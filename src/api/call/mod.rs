mod array;
mod one_shot;
mod streaming;

use std::{
    fmt::Debug,
    sync::{Arc, Mutex},
    task::Waker,
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
    waker: Option<Waker>,
}

impl<T: Debug> InnerCall<T> {
    pub fn new(value: Option<T>) -> Self {
        Self {
            inner: value,
            done: OnceCell::new(),
            waker: None,
        }
    }

    pub fn register_waker(&mut self, waker: &Waker) {
        match &mut self.waker {
            Some(existing) => existing.clone_from(waker),
            None => self.waker = Some(waker.clone()),
        }
    }

    pub fn done(&mut self) -> Result<(), CallError> {
        if let Some(value) = self.inner.take() {
            self.done
                .set(value)
                .map_err(|_| CallError::DoneAlreadyHappened)?;

            if let Some(waker) = self.waker.take() {
                waker.wake();
            }

            return Ok(());
        }

        Err(CallError::DoneWithoutReply)
    }

    pub fn get_done(&mut self) -> Option<T> {
        self.done.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Response;
    use std::{
        future::Future,
        pin::Pin,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Poll, RawWaker, RawWakerVTable},
    };

    fn counting_waker(counter: Arc<AtomicUsize>) -> Waker {
        unsafe fn clone(data: *const ()) -> RawWaker {
            Arc::increment_strong_count(data as *const AtomicUsize);
            RawWaker::new(data, &VTABLE)
        }
        unsafe fn wake(data: *const ()) {
            let arc = Arc::from_raw(data as *const AtomicUsize);
            arc.fetch_add(1, Ordering::SeqCst);
        }
        unsafe fn wake_by_ref(data: *const ()) {
            let arc = Arc::from_raw(data as *const AtomicUsize);
            arc.fetch_add(1, Ordering::SeqCst);
            std::mem::forget(arc);
        }
        unsafe fn drop(data: *const ()) {
            let _ = Arc::from_raw(data as *const AtomicUsize);
        }

        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);

        let raw = RawWaker::new(Arc::into_raw(counter) as *const (), &VTABLE);
        unsafe { Waker::from_raw(raw) }
    }

    #[test]
    fn test_oneshot_call_does_not_wake_during_pending_poll() {
        let mut call = OneShotCall::<()>::new();
        let wake_count = Arc::new(AtomicUsize::new(0));
        let waker = counting_waker(wake_count.clone());
        let mut cx = Context::from_waker(&waker);

        // First poll while pending must NOT trigger wake
        assert!(matches!(Pin::new(&mut call).poll(&mut cx), Poll::Pending));
        assert_eq!(wake_count.load(Ordering::SeqCst), 0);

        // Push reply (still waiting for done) -> no wake yet
        call.push_reply(vec!["!done".to_string(), ".tag=1".to_string()])
            .unwrap();
        assert_eq!(wake_count.load(Ordering::SeqCst), 0);

        // Trigger done() -> wakes the registered waker exactly once
        call.done().unwrap();
        assert_eq!(wake_count.load(Ordering::SeqCst), 1);

        // Next poll returns Ready
        assert!(matches!(
            Pin::new(&mut call).poll(&mut cx),
            Poll::Ready(Response::Done)
        ));
    }

    #[test]
    fn test_array_call_does_not_wake_during_pending_poll() {
        let mut call = ArrayListCall::<()>::new();
        let wake_count = Arc::new(AtomicUsize::new(0));
        let waker = counting_waker(wake_count.clone());
        let mut cx = Context::from_waker(&waker);

        // First poll while pending must NOT trigger wake
        assert!(matches!(Pin::new(&mut call).poll(&mut cx), Poll::Pending));
        assert_eq!(wake_count.load(Ordering::SeqCst), 0);

        // Push !re and !done replies -> no wake until done()
        call.push_reply(vec!["!re".to_string(), ".tag=1".to_string()])
            .unwrap();
        call.push_reply(vec!["!done".to_string(), ".tag=1".to_string()])
            .unwrap();
        assert_eq!(wake_count.load(Ordering::SeqCst), 0);

        // Trigger done() -> wakes the registered waker exactly once
        call.done().unwrap();
        assert_eq!(wake_count.load(Ordering::SeqCst), 1);

        // Next poll returns Ready with the 1 !re item (!done popped)
        let res = Pin::new(&mut call).poll(&mut cx);
        match res {
            Poll::Ready(items) => assert_eq!(items.len(), 1),
            Poll::Pending => panic!("expected Poll::Ready"),
        }
    }

    #[test]
    fn test_waker_updated_across_polls_and_done_before_poll() {
        // 1. Waker replacement when polled with a new waker
        let mut call = OneShotCall::<()>::new();
        let count1 = Arc::new(AtomicUsize::new(0));
        let count2 = Arc::new(AtomicUsize::new(0));
        let waker1 = counting_waker(count1.clone());
        let waker2 = counting_waker(count2.clone());

        assert!(matches!(
            Pin::new(&mut call).poll(&mut Context::from_waker(&waker1)),
            Poll::Pending
        ));
        assert!(matches!(
            Pin::new(&mut call).poll(&mut Context::from_waker(&waker2)),
            Poll::Pending
        ));

        call.push_reply(vec!["!done".to_string(), ".tag=1".to_string()])
            .unwrap();
        call.done().unwrap();

        assert_eq!(count1.load(Ordering::SeqCst), 0);
        assert_eq!(count2.load(Ordering::SeqCst), 1);

        // 2. Calling done() before the first poll resolves immediately on first poll
        let mut fast_call = OneShotCall::<()>::new();
        fast_call
            .push_reply(vec!["!done".to_string(), ".tag=2".to_string()])
            .unwrap();
        fast_call.done().unwrap();

        let count3 = Arc::new(AtomicUsize::new(0));
        let waker3 = counting_waker(count3.clone());
        assert!(matches!(
            Pin::new(&mut fast_call).poll(&mut Context::from_waker(&waker3)),
            Poll::Ready(Response::Done)
        ));
        assert_eq!(count3.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_tokio_delayed_wakeup_without_busy_spin() {
        let one_shot = OneShotCall::<()>::new();
        let mut one_shot_producer = one_shot.clone();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            one_shot_producer
                .push_reply(vec!["!done".to_string(), ".tag=10".to_string()])
                .unwrap();
            one_shot_producer.done().unwrap();
        });

        let mut polls = 0usize;
        let mut pinned = Box::pin(one_shot);
        let res = std::future::poll_fn(|cx| {
            polls += 1;
            pinned.as_mut().poll(cx)
        })
        .await;

        assert!(matches!(res, Response::Done));
        assert_eq!(
            polls, 2,
            "expected exactly 2 polls across 25ms wait, got {polls}"
        );
    }
}
