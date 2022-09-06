mod array;
mod one_shot;
mod streaming;

use std::{
    fmt::Debug,
    sync::{Arc, Mutex},
};

pub use array::ArrayListCall;
pub use one_shot::OneShotCall;
pub use streaming::StreamingCall;

use tokio::sync::OnceCell;

use super::de::DeserializerError;

pub type EmptyCall = OneShotCall<()>;

type ThreadSafeInnerCall<T> = Arc<Mutex<InnerCall<T>>>;

#[derive(Debug)]
pub enum CallError {
    DoneAlreadyHappened,
    DoneWithoutReply,
    BadLock,
    BadSentence(DeserializerError),
}

impl From<DeserializerError> for CallError {
    fn from(e: DeserializerError) -> Self {
        CallError::BadSentence(e)
    }
}

pub trait AsyncCall {
    fn push_reply(&mut self, sentence: Vec<String>) -> Result<(), CallError>;

    fn done(&mut self) -> Result<(), CallError>;
}

struct InnerCall<T> {
    inner: Option<T>,
    done: OnceCell<T>,
}

impl<T: Debug> InnerCall<T> {
    pub fn new(value: Option<T>) -> Self {
        Self {
            inner: value,
            done: OnceCell::new(),
        }
    }

    pub fn done(&mut self) -> Result<(), CallError> {
        if let Some(value) = self.inner.take() {
            self.done
                .set(value)
                .map_err(|_| CallError::DoneAlreadyHappened)?;

            return Ok(());
        }

        Err(CallError::DoneWithoutReply)
    }

    pub fn get_done(&mut self) -> Option<T> {
        self.done.take()
    }
}
