use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
};

use serde::de::DeserializeOwned;

use std::fmt::Debug;

use crate::api::{de::deserialize_sentence, Response};

use super::{AsyncCall, CallError, InnerCall, ThreadSafeInnerCall};

pub struct ArrayListCall<T>(ThreadSafeInnerCall<Vec<Response<T>>>);

impl<T: Debug> ArrayListCall<T> {
    pub fn new() -> Self {
        let inner_vec = Some(Vec::new());

        let inner = InnerCall::new(inner_vec);

        let mutex_inner = Mutex::new(inner);
        let arc_inner = Arc::new(mutex_inner);

        Self(arc_inner)
    }
}

impl<T> Clone for ArrayListCall<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: DeserializeOwned + Debug> AsyncCall for ArrayListCall<T> {
    fn push_reply(&mut self, sentence: Vec<String>) -> Result<(), CallError> {
        let value = deserialize_sentence(sentence.as_slice())?;

        if let Ok(mut call) = self.0.lock() {
            if let Some(vec) = call.inner.as_mut() {
                vec.push(value);
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

impl<T: Debug> Future for ArrayListCall<T> {
    type Output = Vec<Response<T>>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        if let Ok(mut call) = self.0.lock() {
            if let Some(mut vec) = call.get_done().take() {
                //remove !done response at the end
                vec.pop();

                return Poll::Ready(vec);
            }
        }

        cx.waker().wake_by_ref();
        Poll::Pending
    }
}
