// SPDX-License-Identifier: MPL-2.0
use super::*;
use std::task::Wake;
struct Count(AtomicUsize);
impl Wake for Count {
    fn wake(self: Arc<Self>) { self.0.fetch_add(1, Ordering::SeqCst); }
    fn wake_by_ref(self: &Arc<Self>) { self.0.fetch_add(1, Ordering::SeqCst); }
}

#[test]
fn completion_before_poll_and_waker_replacement_cannot_lose_result() {
    let count = Arc::new(Count(AtomicUsize::new(0)));
    let waker = Waker::from(count.clone());
    let mut cx = Context::from_waker(&waker);
    let (reply, mut call) = channel(Cancellation::default());
    reply.send(Ok(7));
    assert_eq!(Pin::new(&mut call).poll(&mut cx), Poll::Ready(Ok(7)));
    let (reply, mut call) = channel(Cancellation::default());
    assert!(Pin::new(&mut call).poll(&mut cx).is_pending());
    let replacement = Arc::new(Count(AtomicUsize::new(0)));
    let waker = Waker::from(replacement.clone());
    let mut cx = Context::from_waker(&waker);
    assert!(Pin::new(&mut call).poll(&mut cx).is_pending());
    reply.send(Ok(9));
    assert_eq!(count.0.load(Ordering::SeqCst), 0);
    assert_eq!(replacement.0.load(Ordering::SeqCst), 1);
    assert_eq!(Pin::new(&mut call).poll(&mut cx), Poll::Ready(Ok(9)));
}

#[test]
fn cancelled_waiter_retains_physical_permit_until_owner_finishes() {
    let gates = RoutineCallGates::default();
    let active = gates.acquire(false).unwrap();
    let queued = gates.acquire(false).unwrap();
    let cancel = Cancellation::default();
    let (reply, call) = channel::<()>(cancel.clone());
    drop(call);
    assert!(cancel.is_cancelled());
    assert!(matches!(gates.acquire(false), Err(RoutineBridgeRefusal::Capacity)));
    assert!(gates.acquire(true).is_ok(), "read capacity is independent");
    drop(reply); // responder teardown alone is not physical completion
    assert!(gates.acquire(false).is_err());
    drop(active);
    let next = gates.acquire(false).unwrap();
    drop(queued);
    drop(next);
    assert_eq!(gates.mutation.load(Ordering::Acquire), 0);
}

#[test]
fn dropped_owner_wakes_waiter_with_unavailable() {
    let count = Arc::new(Count(AtomicUsize::new(0)));
    let waker = Waker::from(count.clone());
    let mut cx = Context::from_waker(&waker);
    let (reply, mut call) = channel::<()>(Cancellation::default());
    assert!(Pin::new(&mut call).poll(&mut cx).is_pending());
    drop(reply);
    assert_eq!(count.0.load(Ordering::SeqCst), 1);
    assert_eq!(Pin::new(&mut call).poll(&mut cx), Poll::Ready(Err(RoutineBridgeRefusal::Unavailable)));
}
