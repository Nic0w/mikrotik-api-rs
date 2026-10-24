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
    ChannelClosed,
}

impl From<DeserializerError> for CallError {
    fn from(e: DeserializerError) -> Self {
        CallError::BadSentence(e)
    }
}

pub trait AsyncCall {
    fn push_reply(&mut self, sentence: Vec<String>) -> Result<(), CallError>;

    fn done(&mut self, sentence: Vec<String>) -> Result<(), CallError>;

    fn disconnect(&mut self);
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
    use crate::TrapCategory;
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

        // Trigger done() without prior !re reply -> for () resolves to Response::Reply(()) and wakes once
        call.done(vec!["!done".to_string()]).unwrap();
        assert_eq!(wake_count.load(Ordering::SeqCst), 1);

        // Next poll returns Ready(Response::Reply(()))
        assert!(matches!(
            Pin::new(&mut call).poll(&mut cx),
            Poll::Ready(Ok(Response::Reply(())))
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

        // Push !re reply -> no wake until done()
        call.push_reply(vec!["!re".to_string(), ".tag=1".to_string()])
            .unwrap();
        assert_eq!(wake_count.load(Ordering::SeqCst), 0);

        // Trigger done() -> wakes the registered waker exactly once
        call.done(vec!["!done".to_string()]).unwrap();
        assert_eq!(wake_count.load(Ordering::SeqCst), 1);

        // Next poll returns Ready with the 1 !re item
        let res = Pin::new(&mut call).poll(&mut cx);
        match res {
            Poll::Ready(Ok(items)) => assert_eq!(items.len(), 1),
            Poll::Ready(Err(e)) => panic!("expected Poll::Ready(Ok), got Err({e:?})"),
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

        call.done(vec!["!done".to_string()]).unwrap();

        assert_eq!(count1.load(Ordering::SeqCst), 0);
        assert_eq!(count2.load(Ordering::SeqCst), 1);

        // 2. Calling done() before the first poll resolves immediately on first poll
        let mut fast_call = OneShotCall::<()>::new();
        fast_call.done(vec!["!done".to_string()]).unwrap();

        let count3 = Arc::new(AtomicUsize::new(0));
        let waker3 = counting_waker(count3.clone());
        assert!(matches!(
            Pin::new(&mut fast_call).poll(&mut Context::from_waker(&waker3)),
            Poll::Ready(Ok(Response::Reply(())))
        ));
        assert_eq!(count3.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_tokio_delayed_wakeup_without_busy_spin() {
        let one_shot = OneShotCall::<()>::new();
        let mut one_shot_producer = one_shot.clone();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            one_shot_producer.done(vec!["!done".to_string()]).unwrap();
        });

        let mut polls = 0usize;
        let mut pinned = Box::pin(one_shot);
        let res = std::future::poll_fn(|cx| {
            polls += 1;
            pinned.as_mut().poll(cx)
        })
        .await;

        assert!(matches!(res, Ok(Response::Reply(()))));
        assert_eq!(
            polls, 2,
            "expected exactly 2 polls across 25ms wait, got {polls}"
        );
    }

    #[tokio::test]
    async fn test_oneshot_unit_type_succeeds_on_bare_done() {
        let call = OneShotCall::<()>::new();
        let mut caller: Box<dyn AsyncCall + Send + Sync> = Box::new(call.clone());

        caller
            .done(vec![
                "!done".to_string(),
                ".tag=1".to_string(),
                "".to_string(),
            ])
            .unwrap();

        let res = call.await.expect("deserialization failed");
        assert_eq!(res, Response::Reply(()));
        let result: Result<(), crate::api::Error> = res.into();
        assert_eq!(result.unwrap(), ());
    }

    #[tokio::test]
    async fn test_oneshot_done_with_ret_attribute_deserializes_struct() {
        #[derive(Debug, serde::Deserialize, PartialEq, Eq)]
        struct CreatedItem {
            ret: String,
        }

        let call = OneShotCall::<CreatedItem>::new();
        let mut caller: Box<dyn AsyncCall + Send + Sync> = Box::new(call.clone());

        caller
            .done(vec![
                "!done".to_string(),
                ".tag=1".to_string(),
                "=ret=*1A".to_string(),
                "".to_string(),
            ])
            .unwrap();

        let res = call.await.expect("deserialization failed");
        assert_eq!(
            res,
            Response::Reply(CreatedItem {
                ret: "*1A".to_string()
            })
        );
        let result: Result<CreatedItem, crate::api::Error> = res.into();
        assert_eq!(
            result.unwrap(),
            CreatedItem {
                ret: "*1A".to_string()
            }
        );

        // Also test that invalid attribute payload on !done returns CallError::BadSentence and DeserializerError
        #[derive(Debug, serde::Deserialize, PartialEq, Eq)]
        struct BadRet {
            ret: u32,
        }

        let bad_call = OneShotCall::<BadRet>::new();
        let mut bad_caller: Box<dyn AsyncCall + Send + Sync> = Box::new(bad_call.clone());

        let done_res = bad_caller.done(vec![
            "!done".to_string(),
            ".tag=2".to_string(),
            "=ret=*1A".to_string(), // non-integer string fails to parse into u32
            "".to_string(),
        ]);

        assert!(matches!(done_res, Err(CallError::BadSentence(_))));
        let bad_res = bad_call.await;
        assert!(bad_res.is_err());
        let err: crate::api::Error = bad_res.unwrap_err().into();
        assert!(matches!(err, crate::api::Error::Deserialize(_)));
    }

    #[tokio::test]
    async fn test_oneshot_empty_response_for_non_unit_struct_on_bare_done() {
        #[derive(Debug, serde::Deserialize, PartialEq, Eq)]
        struct Dummy {
            name: String,
        }

        let call = OneShotCall::<Dummy>::new();
        let mut caller: Box<dyn AsyncCall + Send + Sync> = Box::new(call.clone());

        // Bare !done without data attributes
        caller
            .done(vec![
                "!done".to_string(),
                ".tag=1".to_string(),
                "".to_string(),
            ])
            .unwrap();

        let res = call.await.expect("call failed");
        assert_eq!(res, Response::Done);
        let result: Result<Dummy, crate::api::Error> = res.into();
        assert!(matches!(result, Err(crate::api::Error::EmptyResponse)));
    }

    #[tokio::test]
    async fn test_oneshot_trap_overwrites_previous_re() {
        #[derive(Debug, serde::Deserialize, PartialEq, Eq)]
        struct Item {
            name: String,
        }

        let call = OneShotCall::<Item>::new();
        let mut caller: Box<dyn AsyncCall + Send + Sync> = Box::new(call.clone());

        // 1. Push a valid !re sentence
        caller
            .push_reply(vec![
                "!re".to_string(),
                "=name=foo".to_string(),
                ".tag=1".to_string(),
                "".to_string(),
            ])
            .unwrap();

        // 2. Push a !trap sentence
        caller
            .push_reply(vec![
                "!trap".to_string(),
                "=category=4".to_string(),
                "=message=interrupted".to_string(),
                ".tag=1".to_string(),
                "".to_string(),
            ])
            .unwrap();

        // 3. Complete with !done
        caller
            .done(vec![
                "!done".to_string(),
                ".tag=1".to_string(),
                "".to_string(),
            ])
            .unwrap();

        let res = call.await.expect("call failed");
        match res {
            Response::Trap { category, message } => {
                assert_eq!(category, Some(TrapCategory::GeneralFailure));
                assert_eq!(message, "interrupted");
            }
            other => panic!("expected Response::Trap, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_streaming_call_dropped_receiver_returns_channel_closed() {
        let call = StreamingCall::<()>::new();
        let mut producer = call.clone();

        // While receiver is alive, push_reply succeeds
        let res = producer.push_reply(vec![
            "!re".to_string(),
            ".tag=1".to_string(),
            "".to_string(),
        ]);
        assert!(res.is_ok());

        // Drop the consumer stream
        drop(call);

        // Subsequent push_reply fails with ChannelClosed without attempting deserialization
        let res_after_drop = producer.push_reply(vec![
            "!re".to_string(),
            ".tag=1".to_string(),
            "".to_string(),
        ]);
        assert!(matches!(res_after_drop, Err(CallError::ChannelClosed)));
    }

    #[tokio::test]
    async fn test_streaming_call_disconnect_emits_fatal_then_none() {
        use futures::StreamExt;

        let mut call = StreamingCall::<()>::new();
        let mut producer = call.clone();

        producer.disconnect();

        // First poll yields Some(Response::Fatal)
        let item1 = call.next().await;
        assert_eq!(item1, Some(Response::Fatal));

        // Second poll yields None (end of stream)
        let item2 = call.next().await;
        assert_eq!(item2, None);

        // Calling disconnect again does not emit duplicate events
        producer.disconnect();
        let item3 = call.next().await;
        assert_eq!(item3, None);
    }
}
