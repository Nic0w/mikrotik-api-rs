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

            Fatal => panic!("Fatal error."),
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

fn encode_len(data: &str) -> Vec<u8> {
    let mut res = vec![];

    let len = data.len();

    if len <= 0x7F {
        res.push(len as u8)
    } else if len <= 0x3FFF {
        let bytes = ((len as u16) | 0x8000).to_ne_bytes();
        res.copy_from_slice(&bytes);
    } else if len <= 0x1FFFFF {
        let bytes = ((len as u32) | 0xC00000).to_ne_bytes();
        res.copy_from_slice(&bytes[..3]);
    } else if len <= 0xFFFFFFF {
        let bytes = ((len as u32) | 0xE0000000).to_ne_bytes();
        res.copy_from_slice(&bytes);
    } else if len >= 0x10000000 {
        let bytes = (len as u32).to_ne_bytes();
        res.copy_from_slice(&bytes);
        res.insert(0, 0xF0);
    }

    res
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

fn next_tag(tag_iter: &mut dyn Iterator<Item = u16>, unlocked_map: &MutexGuard<TagMap>) -> u16 {
    for tag in tag_iter {
        if unlocked_map.contains_key(&tag) {
            continue;
        }

        return tag;
    }

    unreachable!()
}
