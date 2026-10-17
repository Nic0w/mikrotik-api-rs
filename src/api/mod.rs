use std::{
    collections::HashMap,
    fmt::Debug,
    sync::{Arc, Mutex, MutexGuard},
};

use futures::Stream;
use log::{debug, trace};
use rand::distributions::{Distribution, Uniform};
use serde::de::DeserializeOwned;
use tokio::{
    io::{AsyncWriteExt, BufWriter},
    net::{tcp::OwnedWriteHalf, TcpStream},
};

use crate::api::call::{ArrayListCall, EmptyCall};

use self::{
    call::{AsyncCall, OneShotCall, StreamingCall},
    error::Error,
    listener::event_loop,
    model::{ActiveUser, Interface, InterfaceChange, Response, SystemResources},
};

mod call;
mod de;
mod error;
mod listener;
pub(crate) mod model;
mod read;

pub trait State {}

/// API in disconnected state: socket is connected but user has not yet completed its authentification.
pub struct Disconnected;

/// API in authenticated state: user has access to the full api.
pub struct Authenticated;

impl State for Disconnected {}
impl State for Authenticated {}

pub type TagMap = HashMap<u16, Box<dyn AsyncCall + Send + Sync>>;

pub type SharedTagMap = Arc<Mutex<TagMap>>;

/// Struct to interact with Mikrotik RouterOS API on port 8728
pub struct MikrotikAPI<S: State> {
    output: BufWriter<OwnedWriteHalf>,
    tag_map: SharedTagMap,
    tag_iter: Box<dyn Iterator<Item = u16>>,

    _state: S,
}

impl<S: State> MikrotikAPI<S> {
    /// Returns the number of currently active calls registered in the tag map.
    pub fn active_calls_count(&self) -> usize {
        self.tag_map.lock().map(|m| m.len()).unwrap_or(0)
    }

    async fn send_command(
        &mut self,
        command: &str,
        attributes: &[(&str, &str)],
    ) -> Result<(), Error> {
        let mut sentence = Vec::with_capacity(1 + attributes.len());

        sentence.push(command.to_owned());

        for (key, value) in attributes {
            if key.starts_with('?') {
                if value.is_empty() {
                    sentence.push(key.to_string());
                } else {
                    sentence.push(format!("{}={}", key, value));
                }
            } else if key.starts_with(&['.', '=']) {
                //.proplist, .tag
                sentence.push(format!("{}={}", key, value));
            } else {
                // everything else (attributes)
                sentence.push(format!("={}={}", key, value));
            }
        }

        let bytes = encode_sentence(sentence.as_slice());

        self.output.write_all(&bytes).await?;
        self.output.flush().await?;

        Ok(())
    }

    async fn do_call<'a, T>(
        &mut self,
        command: &str,
        attributes: Option<&[(&str, &str)]>,
        call_type: T,
        future_tag: Option<&mut u16>,
    ) -> Box<T>
    where
        T: AsyncCall + Clone + Send + Sync + 'static,
    {
        let mut attributes: Vec<(&str, &str)> = attributes.map(<[_]>::to_vec).unwrap_or_default();

        let boxed_call = Box::new(call_type);
        let cloned_call = boxed_call.clone();

        let mut tag = None;

        if let Ok(mut map) = self.tag_map.lock() {
            let new_tag = tag.get_or_insert(next_tag(&mut self.tag_iter, &map));

            map.insert(*new_tag, boxed_call);
        }

        if let Some(mut_tag) = future_tag {
            *mut_tag = tag.unwrap();
        }

        let tag_str = tag.map(|t| t.to_string()).unwrap();

        attributes.insert(0, (".tag", &tag_str));

        debug!("do_call: {}", command);
        trace!("do_call: {:?}", attributes);

        self.send_command(command, attributes.as_slice())
            .await
            .unwrap();

        cloned_call
    }
}

impl MikrotikAPI<Disconnected> {
    pub(crate) fn new(socket: TcpStream) -> Self {
        let (sock_read, sock_write) = socket.into_split();

        let output = BufWriter::new(sock_write);

        let tag_map: TagMap = HashMap::new();

        let tag_range = Uniform::from(1..u16::MAX);

        let rng = rand::thread_rng();

        let tag_iter = Box::new(tag_range.sample_iter(rng));

        let locked_map = Mutex::new(tag_map);
        let shared_map = Arc::new(locked_map);

        let map_clone = shared_map.clone();

        tokio::task::spawn(event_loop(sock_read, map_clone));

        Self {
            tag_iter,
            output,
            tag_map: shared_map,
            _state: Disconnected,
        }
    }

    /// Authenticate user with its login & password
    pub async fn authenticate(
        mut self,
        login: &str,
        password: &str,
    ) -> Result<MikrotikAPI<Authenticated>, Error> {
        let success = self
            .do_call(
                "/login",
                Some(&[("name", login), ("password", password)]),
                EmptyCall::new(),
                None,
            )
            .await;

        use Response::*;
        match success.await {
            Done | Reply(_) => Ok(MikrotikAPI {
                output: self.output,
                tag_map: self.tag_map,
                tag_iter: self.tag_iter,
                _state: Authenticated,
            }),

            Trap { message, .. } => Err(Error::Remote(message)),

            Fatal => Err(Error::EndOfStream),
        }
    }
}

impl MikrotikAPI<Authenticated> {
    /// Get details of the remote router such as architecture, processor, RAM, ...
    pub async fn system_resources(&mut self) -> Result<SystemResources, Error> {
        self.do_call(
            "/system/resource/print",
            None,
            OneShotCall::<SystemResources>::new(),
            None,
        )
        .await
        .await
        .into()
    }

    /// List interfaces and their state in great details
    pub async fn interfaces(&mut self) -> Result<Vec<Interface>, Error> {
        self.do_call("/interface/print", None, ArrayListCall::new(), None)
            .await
            .await
            .into_iter()
            .collect::<Response<Vec<Interface>>>()
            .into()
    }

    /// Listen to user activity in terms of login/logout
    pub async fn active_users(
        &mut self,
        tag: &mut u16,
    ) -> impl Stream<Item = Response<ActiveUser>> {
        self.do_call("/user/active/listen", None, StreamingCall::new(), Some(tag))
            .await
    }

    /// Listen to interface changes (up, down, ...)
    pub async fn interfaces_changes(
        &mut self,
        tag: &mut u16,
    ) -> impl Stream<Item = Response<InterfaceChange>> {
        self.do_call("/interface/listen", None, StreamingCall::new(), Some(tag))
            .await
    }

    /// Alias for [`interfaces_changes`](Self::interfaces_changes) to listen to interface changes (up, down, ...)
    pub async fn interface_changes(
        &mut self,
        tag: &mut u16,
    ) -> impl Stream<Item = Response<InterfaceChange>> {
        self.interfaces_changes(tag).await
    }

    /// Allows to call generic commands returning a one-off response
    pub async fn generic_oneshot_call<T>(
        &mut self,
        command: &str,
        attributes: Option<&[(&str, &str)]>,
    ) -> Result<T, Error>
    where
        T: DeserializeOwned + Debug + Sync + Send + 'static,
    {
        self.do_call(command, attributes, OneShotCall::<T>::new(), None)
            .await
            .await
            .into()
    }

    /// Allows to call generic commands returning a finite amount of items
    pub async fn generic_array_call<T>(
        &mut self,
        command: &str,
        attributes: Option<&[(&str, &str)]>,
    ) -> Result<Vec<T>, Error>
    where
        T: DeserializeOwned + Debug + Sync + Send + 'static,
    {
        self.do_call(command, attributes, ArrayListCall::new(), None)
            .await
            .await
            .into_iter()
            .collect::<Response<Vec<T>>>()
            .into()
    }

    /// Allows to generate a stream of events for `listen` endpoints.
    /// Takes a mutable `tag` argument that allows to stop (cancel) the stream afterwards
    pub async fn generic_streaming_call<T>(
        &mut self,
        command: &str,
        attributes: Option<&[(&str, &str)]>,
        tag: &mut u16,
    ) -> impl Stream<Item = Response<T>>
    where
        T: DeserializeOwned + Debug + Sync + Send + 'static,
    {
        self.do_call(command, attributes, StreamingCall::new(), Some(tag))
            .await
    }

    /// Calls `/cancel` on a specific tag.
    /// Primary usage is to stop `listen` commands
    pub async fn cancel(&mut self, tag: u16) -> Response<()> {
        self.do_call(
            "/cancel",
            Some(&[("tag", tag.to_string().as_str())]),
            EmptyCall::new(),
            None,
        )
        .await
        .await
    }
}

fn encode_len_usize(len: usize) -> Vec<u8> {
    let mut res = Vec::with_capacity(5);

    if len <= 0x7F {
        res.push(len as u8);
    } else if len <= 0x3FFF {
        let bytes = ((len as u16) | 0x8000).to_be_bytes();
        res.extend_from_slice(&bytes);
    } else if len <= 0x1FFFFF {
        let bytes = ((len as u32) | 0xC00000).to_be_bytes();
        res.extend_from_slice(&bytes[1..]);
    } else if len <= 0xFFFFFFF {
        let bytes = ((len as u32) | 0xE0000000).to_be_bytes();
        res.extend_from_slice(&bytes);
    } else {
        res.push(0xF0);
        res.extend_from_slice(&(len as u32).to_be_bytes());
    }

    res
}

fn encode_len(data: &str) -> Vec<u8> {
    encode_len_usize(data.len())
}

fn encode_word(word: &str) -> Vec<u8> {
    let mut res = encode_len(word);

    res.extend_from_slice(word.as_bytes());

    res
}

fn encode_sentence<S: AsRef<str>>(words: &[S]) -> Vec<u8> {
    let mut res = vec![];

    for w in words {
        res.append(&mut encode_word(w.as_ref()));
    }

    //Empty word to close sentence.
    res.push(0x00);

    res
}

const MAX_TAGS: usize = (u16::MAX - 1) as usize; // 65534 valid tags (1..u16::MAX)

fn next_tag(tag_iter: &mut dyn Iterator<Item = u16>, unlocked_map: &MutexGuard<TagMap>) -> u16 {
    // Try sampling random tags from the iterator first.
    for _ in 0..128 {
        if let Some(tag) = tag_iter.next() {
            if !unlocked_map.contains_key(&tag) {
                return tag;
            }
        }
    }

    // If random sampling collided repeatedly or tag space is heavily populated,
    // fall back to a linear scan to find an available tag.
    for tag in 1..u16::MAX {
        if !unlocked_map.contains_key(&tag) {
            return tag;
        }
    }

    panic!(
        "Tag space exhausted: all {} tags are currently in use",
        MAX_TAGS
    );
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        encode_len, encode_len_usize, encode_sentence, encode_word,
        read::{read_len, read_sentence},
    };

    #[test]
    fn test_encode_len_one_byte() {
        assert_eq!(encode_len(""), vec![0x00]);
        assert_eq!(encode_len("a"), vec![0x01]);
        assert_eq!(encode_len(&"a".repeat(0x42)), vec![0x42]);
        assert_eq!(encode_len(&"a".repeat(0x7F)), vec![0x7F]);
    }

    #[test]
    fn test_encode_len_two_bytes() {
        // Lower boundary: 128 (0x80) -> 0x8080
        assert_eq!(encode_len(&"a".repeat(0x80)), vec![0x80, 0x80]);

        // Endianness regression: 256 (0x0100) -> 0x8100 (NOT [0x00, 0x81])
        assert_eq!(encode_len(&"a".repeat(0x100)), vec![0x81, 0x00]);

        // Endianness regression: 258 (0x0102) -> 0x8102 (NOT [0x02, 0x81])
        assert_eq!(encode_len(&"a".repeat(0x102)), vec![0x81, 0x02]);

        // 300 (0x012C) -> 0x812C
        assert_eq!(encode_len(&"a".repeat(300)), vec![0x81, 0x2C]);

        // Upper boundary: 16383 (0x3FFF) -> 0xBFFF
        assert_eq!(encode_len(&"a".repeat(0x3FFF)), vec![0xBF, 0xFF]);
    }

    #[test]
    fn test_encode_len_three_bytes() {
        // Lower boundary & slice range regression: 16384 (0x4000) -> [0xC0, 0x40, 0x00] (NOT [0x00, 0xC0, 0x40])
        assert_eq!(encode_len(&"a".repeat(0x4000)), vec![0xC0, 0x40, 0x00]);

        // 16385 (0x4001) -> [0xC0, 0x40, 0x01]
        assert_eq!(encode_len(&"a".repeat(0x4001)), vec![0xC0, 0x40, 0x01]);

        // 0x4242 -> [0xC0, 0x42, 0x42]
        assert_eq!(encode_len(&"a".repeat(0x4242)), vec![0xC0, 0x42, 0x42]);

        // Distinct bytes across all 3 positions: 0x010203 -> [0xC1, 0x02, 0x03]
        assert_eq!(encode_len(&"a".repeat(0x010203)), vec![0xC1, 0x02, 0x03]);

        // Upper boundary: 0x1FFFFF -> [0xDF, 0xFF, 0xFF]
        assert_eq!(encode_len_usize(0x1FFFFF), vec![0xDF, 0xFF, 0xFF]);
    }

    #[test]
    fn test_encode_len_four_bytes() {
        // Lower boundary: 0x200000 -> [0xE0, 0x20, 0x00, 0x00]
        assert_eq!(encode_len_usize(0x200000), vec![0xE0, 0x20, 0x00, 0x00]);

        // Distinct bytes across all 4 positions: 0x01020304 -> [0xE1, 0x02, 0x03, 0x04]
        assert_eq!(encode_len_usize(0x01020304), vec![0xE1, 0x02, 0x03, 0x04]);

        // Upper boundary: 0xFFFFFFF -> [0xEF, 0xFF, 0xFF, 0xFF]
        assert_eq!(encode_len_usize(0xFFFFFFF), vec![0xEF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn test_encode_len_five_bytes() {
        // Lower boundary: 0x10000000 -> [0xF0, 0x10, 0x00, 0x00, 0x00]
        assert_eq!(
            encode_len_usize(0x10000000),
            vec![0xF0, 0x10, 0x00, 0x00, 0x00]
        );

        // Distinct bytes across all 4 payload bytes: 0xAABBCCDD -> [0xF0, 0xAA, 0xBB, 0xCC, 0xDD]
        assert_eq!(
            encode_len_usize(0xAABBCCDD),
            vec![0xF0, 0xAA, 0xBB, 0xCC, 0xDD]
        );

        // Upper 32-bit boundary: 0xFFFFFFFF -> [0xF0, 0xFF, 0xFF, 0xFF, 0xFF]
        assert_eq!(
            encode_len_usize(0xFFFFFFFF),
            vec![0xF0, 0xFF, 0xFF, 0xFF, 0xFF]
        );
    }

    #[test]
    fn test_encode_decode_len_roundtrip() {
        let test_lengths: &[usize] = &[
            0, 1, 0x42, 0x7E, 0x7F, 0x80, 0x81, 0xFF, 0x100, 0x101, 0x102, 300, 0x3FFE, 0x3FFF,
            0x4000, 0x4001, 0x4242, 0x010203, 0x1FFFFE, 0x1FFFFF, 0x200000, 0x200001, 0x01020304,
            0xFFFFFFE, 0xFFFFFFF, 0x10000000, 0x10000001, 0xAABBCCDD, 0xFFFFFFFE, 0xFFFFFFFF,
        ];

        for &len in test_lengths {
            let encoded = encode_len_usize(len);
            let mut cursor = Cursor::new(encoded.as_slice());
            let decoded = read_len(&mut cursor)
                .unwrap_or_else(|e| panic!("failed to decode length {:#x}: {:?}", len, e));

            assert_eq!(
                decoded, len as u32,
                "round-trip mismatch for length {:#x} (encoded: {:02x?})",
                len, encoded
            );
            assert_eq!(
                cursor.position() as usize,
                encoded.len(),
                "cursor did not consume full encoded buffer for length {:#x}",
                len
            );
        }
    }

    #[test]
    fn test_encode_decode_sentence_roundtrip() {
        let word_1b = "/interface/print".to_string();
        let word_2b_boundary = format!("?name={}", "a".repeat(122)); // 128 bytes
        let word_2b_256 = format!("?name={}", "b".repeat(250)); // 256 bytes (0x0100)
        let word_3b_boundary = format!("?name={}", "c".repeat(16378)); // 16384 bytes (0x4000)
        let word_3b_16385 = format!("?name={}", "d".repeat(16379)); // 16385 bytes (0x4001)

        let input_words = vec![
            word_1b.as_str(),
            word_2b_boundary.as_str(),
            word_2b_256.as_str(),
            word_3b_boundary.as_str(),
            word_3b_16385.as_str(),
        ];

        let encoded = encode_sentence(&input_words);
        let mut cursor = Cursor::new(encoded.as_slice());
        let decoded = read_sentence(&mut cursor).expect("failed to decode multi-tier sentence");

        // `read_sentence` includes the trailing empty word `""` terminator
        let mut expected = input_words;
        expected.push("");

        assert_eq!(decoded, expected);
        assert_eq!(cursor.position() as usize, encoded.len());

        // Also verify `encode_word` prefix + payload layout directly
        let w = encode_word(&word_2b_256);
        assert_eq!(&w[..2], &[0x81, 0x00]);
        assert_eq!(&w[2..], word_2b_256.as_bytes());
    }

    #[test]
    fn test_next_tag_basic_and_collision() {
        use super::{next_tag, EmptyCall, TagMap};
        use std::sync::Mutex;

        let map = Mutex::new(TagMap::new());
        let mut guarded = map.lock().unwrap();

        // Single repeated tag in iterator
        let mut iter = std::iter::repeat(42u16);
        assert_eq!(next_tag(&mut iter, &guarded), 42);

        // Insert 42 into map
        guarded.insert(42, Box::new(EmptyCall::new()));

        // Iterator still yields 42, but next_tag should skip it and fall back to linear scan
        let mut iter2 = std::iter::repeat(42u16);
        let tag = next_tag(&mut iter2, &guarded);
        assert_ne!(tag, 42);
        assert_eq!(tag, 1);
    }

    #[test]
    fn test_next_tag_panics_on_exhaustion() {
        use super::{next_tag, EmptyCall, TagMap, MAX_TAGS};
        use std::sync::Mutex;

        let map = Mutex::new(TagMap::new());
        let mut guarded = map.lock().unwrap();

        let dummy = EmptyCall::new();
        for t in 1..u16::MAX {
            guarded.insert(t, Box::new(dummy.clone()));
        }
        assert_eq!(guarded.len(), MAX_TAGS);

        let mut iter = 1..u16::MAX;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            next_tag(&mut iter, &guarded);
        }));

        assert!(
            result.is_err(),
            "expected next_tag to panic on tag exhaustion"
        );
    }
}
