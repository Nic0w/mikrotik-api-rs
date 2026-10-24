use std::fmt::Debug;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
};

use serde::de::DeserializeOwned;

use crate::api::{
    de::{deserialize_done_payload, deserialize_sentence, has_done_attributes, DeserializerError},
    Response,
};

use super::{AsyncCall, CallError, InnerCall, ThreadSafeInnerCall};

pub struct OneShotCall<T>(ThreadSafeInnerCall<Result<Response<T>, DeserializerError>>);

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
        let res = deserialize_sentence(sentence.as_slice());

        if let Ok(mut call) = self.0.lock() {
            match res {
                Ok(value @ Response::Trap { .. }) => {
                    if !matches!(call.inner, Some(Ok(Response::Trap { .. }))) {
                        call.inner = Some(Ok(value));
                    }
                    return Ok(());
                }
                Ok(value) => {
                    if call.inner.is_none() {
                        let _ = call.inner.insert(Ok(value));
                    }
                    return Ok(());
                }
                Err(e) => {
                    if call.inner.is_none() {
                        let _ = call.inner.insert(Err(e.clone()));
                    }
                    return Err(CallError::BadSentence(e));
                }
            }
        }

        Err(CallError::BadLock)
    }

    fn done(&mut self, sentence: Vec<String>) -> Result<(), CallError> {
        if let Ok(mut call) = self.0.lock() {
            let mut bad_sentence_err = None;

            if call.inner.is_none() {
                let has_attributes = has_done_attributes(sentence.as_slice());

                match deserialize_done_payload::<T>(sentence.as_slice()) {
                    Ok(value) => {
                        call.inner = Some(Ok(Response::Reply(value)));
                    }
                    Err(e) if has_attributes => {
                        call.inner = Some(Err(e.clone()));
                        bad_sentence_err = Some(e);
                    }
                    Err(_) => {
                        call.inner = Some(Ok(Response::Done));
                    }
                }
            }

            call.done()?;

            if let Some(e) = bad_sentence_err {
                return Err(CallError::BadSentence(e));
            }

            return Ok(());
        }

        Err(CallError::BadLock)
    }

    fn disconnect(&mut self) {
        if let Ok(mut call) = self.0.lock() {
            if call.inner.is_none() {
                call.inner = Some(Ok(Response::Fatal));
            }
            let _ = call.done();
        }
    }
}

impl<T: Debug> Future for OneShotCall<T> {
    type Output = Result<Response<T>, DeserializerError>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if let Ok(mut call) = self.0.lock() {
            if let Some(value) = call.get_done() {
                return Poll::Ready(value);
            }

            call.register_waker(cx.waker());
        }

        Poll::Pending
    }
}
