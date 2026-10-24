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
    receiver: Option<UnboundedReceiver<Response<T>>>,
}

struct InnerStreamingCall<T> {
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
            cell: OnceCell::new(),
        }));

        Self {
            inner,
            receiver: Some(receiver),
        }
    }
}

impl<T> Default for StreamingCall<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: DeserializeOwned + Debug> AsyncCall for StreamingCall<T> {
    fn push_reply(&mut self, sentence: Vec<String>) -> Result<(), CallError> {
        if let Ok(inner) = self.inner.lock() {
            if inner.sender.is_closed() {
                return Err(CallError::ChannelClosed);
            }
            let value = deserialize_sentence(sentence.as_slice())?;
            inner
                .sender
                .send(value)
                .map_err(|_| CallError::ChannelClosed)?;

            return Ok(());
        }

        Err(CallError::BadLock)
    }

    fn done(&mut self, _sentence: Vec<String>) -> Result<(), CallError> {
        if let Ok(mut call) = self.inner.lock() {
            call.done()?;
            let _ = call.sender.send(Response::Done);

            return Ok(());
        }

        Err(CallError::BadLock)
    }

    fn disconnect(&mut self) {
        if let Ok(mut call) = self.inner.lock() {
            if call.done().is_ok() {
                let _ = call.sender.send(Response::Fatal);
                let _ = call.sender.send(Response::Done);
            }
        }
    }
}

impl<T> Clone for StreamingCall<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            receiver: None,
        }
    }
}

impl<T> Stream for StreamingCall<T> {
    type Item = Response<T>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(receiver) = this.receiver.as_mut() {
            let next_value = receiver.poll_recv(cx);

            if let Poll::Ready(Some(Response::Done)) = next_value {
                // A !done reply is our End Of Stream.
                this.receiver = None;
                return Poll::Ready(None);
            }

            if let Poll::Ready(None) = next_value {
                this.receiver = None;
                return Poll::Ready(None);
            }

            return next_value;
        }

        Poll::Ready(None)
    }
}
