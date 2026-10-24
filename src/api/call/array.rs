use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
};

use serde::de::DeserializeOwned;

use std::fmt::Debug;

use crate::api::{
    de::{deserialize_sentence, DeserializerError},
    Response,
};

use super::{AsyncCall, CallError, InnerCall, ThreadSafeInnerCall};

pub struct ArrayListCall<T>(ThreadSafeInnerCall<Result<Vec<Response<T>>, DeserializerError>>);

impl<T: Debug> ArrayListCall<T> {
    pub fn new() -> Self {
        let inner = InnerCall::new(Some(Ok(Vec::new())));

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
        let res = deserialize_sentence(sentence.as_slice());

        if let Ok(mut call) = self.0.lock() {
            match (&mut call.inner, res) {
                (Some(Ok(vec)), Ok(value)) => {
                    vec.push(value);
                    return Ok(());
                }
                (Some(Ok(_)), Err(e)) => {
                    call.inner = Some(Err(e.clone()));
                    return Err(CallError::BadSentence(e));
                }
                (Some(Err(_)), Ok(_)) | (None, Ok(_)) => {
                    return Ok(());
                }
                (Some(Err(_)), Err(e)) | (None, Err(e)) => {
                    return Err(CallError::BadSentence(e));
                }
            }
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

    fn disconnect(&mut self) {
        if let Ok(mut call) = self.0.lock() {
            if let Some(Ok(vec)) = call.inner.as_mut() {
                vec.push(Response::Fatal);
            }
            let _ = call.done();
        }
    }
}

impl<T: Debug> Future for ArrayListCall<T> {
    type Output = Result<Vec<Response<T>>, DeserializerError>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        if let Ok(mut call) = self.0.lock() {
            if let Some(vec) = call.get_done() {
                return Poll::Ready(vec);
            }

            call.register_waker(cx.waker());
        }

        Poll::Pending
    }
}
