use std::fmt::Debug;
use std::{
    sync::{Arc, Mutex},
    task::Poll,
};

use futures::Stream;
use serde::de::DeserializeOwned;
use tokio::sync::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    OnceCell,
};

use crate::api::{de::deserialize_sentence, Response};

use super::{AsyncCall, CallError};
pub struct StreamingCall<T> {
    inner: Arc<Mutex<InnerStreamingCall<Response<T>>>>,
    // sender: Arc<Mutex<Sender<Response<T>>>>
}

struct InnerStreamingCall<T> {
    receiver: UnboundedReceiver<T>,
    sender: UnboundedSender<T>,
    cell: OnceCell<()>,
}

impl<T> InnerStreamingCall<T> {
    pub fn done(&mut self) -> Result<(), CallError> {
        self.cell
            .set(())
            .map_err(|_| CallError::DoneAlreadyHappened)
    }
}

impl<T> StreamingCall<T> {
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();

        let inner = Arc::new(Mutex::new(InnerStreamingCall {
            sender,
            receiver,
            cell: OnceCell::new(),
        }));

        Self { inner }
    }
}

impl<T: DeserializeOwned + Debug> AsyncCall for StreamingCall<T> {
    fn push_reply(&mut self, sentence: Vec<String>) -> Result<(), CallError> {

        let value = deserialize_sentence(sentence.as_slice())?;

        if let Ok(inner) = self.inner.lock() {
            let _ = inner.sender.send(value);

            return Ok(());
        }

        Err(CallError::BadLock)
    }

    fn done(&mut self) -> Result<(), CallError> {

        if let Ok(mut call) = self.inner.lock() {
            call.done()?;

            return Ok(())
        }

        Err(CallError::BadLock)
    }
}

impl<T> Clone for StreamingCall<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Stream for StreamingCall<T> {
    type Item = Response<T>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if let Ok(mut inner) = self.inner.lock() {

            let next_value = inner.receiver.poll_recv(cx);

            if let Poll::Ready(Some(Response::Done)) = next_value {
                // A !done reply is our End Of Stream.
                return Poll::Ready(None)
            }

            return next_value;
        }

        Poll::Pending
    }
}
