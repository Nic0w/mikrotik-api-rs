use std::{io::Cursor, time::Duration};

use bytes::{Buf, BytesMut};
use log::{debug, error, trace, warn};
use tokio::{io::Interest, net::tcp::OwnedReadHalf};

use super::{error::Error, read::read_sentence, SharedTagMap};

async fn try_read_sentence(
    reader: &mut OwnedReadHalf,
    buffer: &mut BytesMut,
) -> Result<Vec<String>, Error> {
    let _sleepy_time = Duration::from_millis(20);

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

        if reader.ready(Interest::READABLE).await?.is_readable() {
            let new_bytes = reader.try_read_buf(buffer)?;

            trace!(
                "try_read_sentence: filling buffer with {} new bytes.",
                new_bytes
            );

            if new_bytes == 0 {
                return Err(Error::EndOfStream);
            }
        }
        //tokio::time::sleep(sleepy_time).await;
    }
}

pub async fn event_loop(mut socket: OwnedReadHalf, tags: SharedTagMap) {
    let mut buffer = BytesMut::with_capacity(16384);

    debug!("event_loop: running!");

    loop {
        if let Ok(sentence) = try_read_sentence(&mut socket, &mut buffer).await {
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
                        if let Err(e) = caller.push_reply(sentence) {
                            error!("on push_reply: {:?}", e);
                            break;
                        }

                        if let Done = frame_type {
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

    warn!("event_loop: exiting!");
}
