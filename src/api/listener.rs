use std::io::Cursor;

use bytes::{Buf, BytesMut};
use log::{debug, error, trace, warn};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::{error::Error, read::read_sentence, SharedTagMap};

async fn try_read_sentence<R: AsyncRead + Unpin>(
    reader: &mut R,
    buffer: &mut BytesMut,
) -> Result<Vec<String>, Error> {
    loop {
        let mut cursor = Cursor::new(&buffer[..]);

        if let Ok(sentence) = read_sentence(&mut cursor) {
            let res = sentence.iter().map(|t| t.to_string()).collect();

            let consumed = cursor.position() as usize;

            debug!("try_read_sentence: read new sentence ({} bytes).", consumed);
            trace!("try_read_sentence: {:?}", sentence);

            buffer.advance(consumed);

            return Ok(res);
        }

        let new_bytes = reader.read_buf(buffer).await?;

        trace!(
            "try_read_sentence: filling buffer with {} new bytes.",
            new_bytes
        );

        if new_bytes == 0 {
            return Err(Error::EndOfStream);
        }
    }
}

pub async fn event_loop<R: AsyncRead + Unpin>(mut socket: R, tags: SharedTagMap) {
    let mut buffer = BytesMut::with_capacity(16384);

    debug!("event_loop: running!");

    loop {
        match try_read_sentence(&mut socket, &mut buffer).await {
            Ok(sentence) => {
                let mut iter = sentence.iter();

                let first = iter.next().map(String::as_str);
                let second = iter.next().map(String::as_str);

                let both = first.zip(second);

                enum FrameType {
                    Reply,
                    Done,
                }

                use FrameType::*;
                let tuple = match both {
                    Some(("!re", tag)) | Some(("!trap", tag)) if tag.starts_with(".tag") => {
                        Some((Reply, tag))
                    }
                    Some(("!done", tag)) => Some((Done, tag)),

                    Some(("!fatal", message)) => {
                        error!("received !fatal from the router: {}", message);
                        break;
                    }

                    unknown => {
                        warn!("unknown frame type: {:?}", unknown);
                        None
                    }
                }
                .map(|(f_type, tag)| {
                    let (_, id) = tag.split_at(5);

                    let id: u16 = id.parse().unwrap();

                    (f_type, id)
                });

                if let Some((frame_type, id)) = tuple {
                    if let Ok(mut guarded_map) = tags.lock() {
                        if let Some(caller) = guarded_map.get_mut(&id) {
                            match frame_type {
                                Reply => {
                                    if let Err(e) = caller.push_reply(sentence) {
                                        error!("on push_reply: {:?}", e);
                                        break;
                                    }
                                }
                                Done => {
                                    if let Err(e) = caller.done() {
                                        error!("on done: {:?}", e);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(Error::EndOfStream) => {
                debug!("event_loop: reached EOF on socket, exiting.");
                break;
            }
            Err(e) => {
                error!("event_loop: read error on socket: {:?}", e);
                break;
            }
        }
    }

    if let Ok(mut guarded_map) = tags.lock() {
        for (_, mut caller) in guarded_map.drain() {
            caller.disconnect();
        }
    }

    warn!("event_loop: exiting!");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{
        call::{ArrayListCall, OneShotCall, StreamingCall},
        Response, TagMap,
    };
    use futures::StreamExt;
    use std::{
        io,
        pin::Pin,
        sync::{Arc, Mutex},
        task::{Context, Poll},
        time::Duration,
    };
    use tokio::{
        io::{AsyncWriteExt, ReadBuf},
        time::timeout,
    };

    /// Mock reader that yields `WouldBlock` once, then either data or a fatal I/O error.
    struct MockReader {
        yielded_would_block: bool,
        error_kind: Option<io::ErrorKind>,
    }

    impl AsyncRead for MockReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if !self.yielded_would_block {
                self.yielded_would_block = true;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            match self.error_kind {
                Some(kind) => Poll::Ready(Err(io::Error::new(kind, "simulated socket error"))),
                None => Poll::Ready(Ok(())), // EOF (0 bytes appended)
            }
        }
    }

    #[tokio::test]
    async fn test_event_loop_exits_and_notifies_callers_on_eof() {
        let (mut client_write, server_read) = tokio::io::duplex(256);

        let oneshot = OneShotCall::<()>::new();
        let array = ArrayListCall::<()>::new();
        let mut streaming = StreamingCall::<()>::new();

        let mut map = TagMap::new();
        map.insert(1, Box::new(oneshot.clone()));
        map.insert(2, Box::new(array.clone()));
        map.insert(3, Box::new(streaming.clone()));
        let shared_map = Arc::new(Mutex::new(map));

        let loop_handle = tokio::spawn(event_loop(server_read, shared_map.clone()));

        // Send a partial/incomplete word (declared length 10, only 3 bytes sent) then close (EOF)
        client_write
            .write_all(&[10, b'a', b'b', b'c'])
            .await
            .unwrap();
        drop(client_write);

        // 1. event_loop must exit promptly instead of spinning in an infinite loop
        timeout(Duration::from_secs(1), loop_handle)
            .await
            .expect("event_loop hung on EOF!")
            .expect("event_loop task panicked");

        // 2. SharedTagMap must be drained
        assert!(shared_map.lock().unwrap().is_empty());

        // 3. In-flight OneShotCall wakes up and converts to Err(Error::EndOfStream)
        let oneshot_res: Result<(), Error> = timeout(Duration::from_secs(1), oneshot)
            .await
            .expect("OneShotCall hung after EOF!")
            .into();
        assert!(matches!(oneshot_res, Err(Error::EndOfStream)));

        // 4. In-flight ArrayListCall wakes up and converts to Err(Error::EndOfStream)
        let array_res: Result<Vec<()>, Error> = timeout(Duration::from_secs(1), array)
            .await
            .expect("ArrayListCall hung after EOF!")
            .into_iter()
            .collect::<Response<Vec<()>>>()
            .into();
        assert!(matches!(array_res, Err(Error::EndOfStream)));

        // 5. Active StreamingCall wakes up and terminates stream (returns None)
        let stream_item = timeout(Duration::from_secs(1), streaming.next())
            .await
            .expect("StreamingCall hung after EOF!");
        assert!(stream_item.is_none());
    }

    #[tokio::test]
    async fn test_event_loop_exits_and_notifies_callers_on_io_error() {
        let reader = MockReader {
            yielded_would_block: false,
            error_kind: Some(io::ErrorKind::ConnectionReset),
        };

        let oneshot = OneShotCall::<()>::new();
        let mut map = TagMap::new();
        map.insert(42, Box::new(oneshot.clone()));
        let shared_map = Arc::new(Mutex::new(map));

        let loop_handle = tokio::spawn(event_loop(reader, shared_map.clone()));

        timeout(Duration::from_secs(1), loop_handle)
            .await
            .expect("event_loop hung on I/O error!")
            .expect("event_loop task panicked");

        assert!(shared_map.lock().unwrap().is_empty());

        let oneshot_res: Result<(), Error> = timeout(Duration::from_secs(1), oneshot)
            .await
            .expect("OneShotCall hung after I/O error!")
            .into();
        assert!(matches!(oneshot_res, Err(Error::EndOfStream)));
    }

    #[tokio::test]
    async fn test_event_loop_handles_done_only_without_re() {
        let (mut client_write, server_read) = tokio::io::duplex(256);

        let oneshot = OneShotCall::<u32>::new();
        let array = ArrayListCall::<u32>::new();
        let mut streaming = StreamingCall::<u32>::new();

        let mut map = TagMap::new();
        map.insert(1, Box::new(oneshot.clone()));
        map.insert(2, Box::new(array.clone()));
        map.insert(3, Box::new(streaming.clone()));
        let shared_map = Arc::new(Mutex::new(map));

        let _loop_handle = tokio::spawn(event_loop(server_read, shared_map));

        // Helper to write a `!done` sentence for a given tag
        for tag in [".tag=1", ".tag=2", ".tag=3"] {
            client_write.write_all(&[5]).await.unwrap();
            client_write.write_all(b"!done").await.unwrap();
            client_write.write_all(&[tag.len() as u8]).await.unwrap();
            client_write.write_all(tag.as_bytes()).await.unwrap();
            client_write.write_all(&[0]).await.unwrap();
        }

        // 1. OneShotCall resolves to Response::Done, and converts to Err(Error::EmptyResponse) without panicking
        let oneshot_resp = timeout(Duration::from_secs(1), oneshot)
            .await
            .expect("OneShotCall timed out on !done-only frame");
        assert!(matches!(oneshot_resp, Response::Done));
        let oneshot_res: Result<u32, Error> = oneshot_resp.into();
        assert!(matches!(oneshot_res, Err(Error::EmptyResponse)));

        // 2. ArrayListCall resolves to an empty Vec without panicking
        let array_res: Result<Vec<u32>, Error> = timeout(Duration::from_secs(1), array)
            .await
            .expect("ArrayListCall timed out on !done-only frame")
            .into_iter()
            .collect::<Response<Vec<u32>>>()
            .into();
        assert_eq!(array_res.unwrap(), Vec::<u32>::new());

        // 3. StreamingCall terminates cleanly (returns None)
        let stream_item = timeout(Duration::from_secs(1), streaming.next())
            .await
            .expect("StreamingCall timed out on !done-only frame");
        assert!(stream_item.is_none());
    }
}
