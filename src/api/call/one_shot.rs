use std::fmt::Debug;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
};

use serde::de::DeserializeOwned;

use crate::api::{de::deserialize_sentence, Response};

use super::{AsyncCall, CallError, InnerCall, ThreadSafeInnerCall};

pub struct OneShotCall<T>(ThreadSafeInnerCall<Response<T>>);

impl<T: Debug> OneShotCall<T> {
    pub fn new() -> Self {
        let inner = InnerCall::new(None);

        let mutex_inner = Mutex::new(inner);

        OneShotCall(Arc::new(mutex_inner))
    }
}

impl<T> Clone for OneShotCall<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: DeserializeOwned + Debug> AsyncCall for OneShotCall<T> {
    fn push_reply(&mut self, sentence: Vec<String>) -> Result<(), CallError> {
        let value = deserialize_sentence(sentence.as_slice())?;

        if let Ok(mut call) = self.0.lock() {
            if call.inner.is_none() {
                let _ = call.inner.insert(value);
            }
            return Ok(());
        }

        Err(CallError::BadLock)
    }

    fn done(&mut self) -> Result<(), CallError> {
        if let Ok(mut call) = self.0.lock() {
            call.done()?;

            return Ok(());
        }

        Err(CallError::BadLock)
    }
}

impl<T: Debug> Future for OneShotCall<T> {
    type Output = Response<T>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if let Ok(mut call) = self.0.lock() {
            if let Some(value) = call.get_done() {
                return Poll::Ready(value);
            }
        }

        cx.waker().wake_by_ref();
        Poll::Pending
    }
}
