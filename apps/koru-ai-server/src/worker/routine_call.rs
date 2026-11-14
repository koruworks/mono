// SPDX-License-Identifier: MPL-2.0
//! Finite private calls. A waiter never owns the physical operation permit.
use koru_ai_core::Cancellation;
use koru_delegate_api::routines::{ROUTINE_BRIDGE_DEADLINE, RoutineBridgeRefusal};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Instant,
};
use uuid::Uuid;

#[derive(Clone)]
pub struct RoutineCallContext {
    pub(crate) request_id: Uuid,
    pub(crate) deadline: Instant,
    pub(crate) cancellation: Cancellation,
}
impl RoutineCallContext {
    pub fn new(request_id: Uuid, deadline: Instant, cancellation: Cancellation) -> Self {
        Self {
            request_id,
            deadline,
            cancellation,
        }
    }
    pub(crate) fn validate(&self) -> Result<(), RoutineBridgeRefusal> {
        if self.request_id.get_version() != Some(uuid::Version::Random)
            || self.request_id.get_variant() != uuid::Variant::RFC4122
        {
            return Err(RoutineBridgeRefusal::Invalid);
        }
        let now = Instant::now();
        if self.deadline <= now || self.cancellation.is_cancelled() {
            return Err(RoutineBridgeRefusal::Unavailable);
        }
        if self.deadline.duration_since(now) > ROUTINE_BRIDGE_DEADLINE {
            return Err(RoutineBridgeRefusal::Invalid);
        }
        Ok(())
    }
    pub(crate) fn live(&self) -> bool {
        !self.cancellation.is_cancelled() && Instant::now() < self.deadline
    }
}
impl std::fmt::Debug for RoutineCallContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineCallContext([REDACTED])")
    }
}

struct Reply<T> {
    value: Option<Result<T, RoutineBridgeRefusal>>,
    waker: Option<Waker>,
}
/// Await with the same total transport deadline. Drop requests cooperative
/// cancellation; it does not imply a remote IPC mutation stopped, or release
/// the local background job's owned permit.
/// Cancellation alone neither wakes nor resolves this Future. The transport
/// owns waiter expiration using the same absolute deadline; actual producer
/// completion or teardown publishes the result and wakes a retained waiter.
pub struct RoutineCall<T> {
    state: Arc<Mutex<Reply<T>>>,
    cancellation: Cancellation,
    complete: bool,
}
impl<T> RoutineCall<T> {
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
}
impl<T> Future for RoutineCall<T> {
    type Output = Result<T, RoutineBridgeRefusal>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut state = this.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(value) = state.value.take() {
            this.complete = true;
            Poll::Ready(value)
        } else {
            // Publication and registration share the lock: neither ordering
            // can lose a wakeup. Replace old executors' wakers after migration.
            if state
                .waker
                .as_ref()
                .is_none_or(|w| !w.will_wake(cx.waker()))
            {
                state.waker = Some(cx.waker().clone());
            }
            Poll::Pending
        }
    }
}
impl<T> Drop for RoutineCall<T> {
    fn drop(&mut self) {
        if !self.complete {
            self.cancellation.cancel();
        }
    }
}
impl<T> std::fmt::Debug for RoutineCall<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineCall([REDACTED])")
    }
}

pub(super) struct RoutineResponder<T> {
    state: Arc<Mutex<Reply<T>>>,
    sent: bool,
}
impl<T> RoutineResponder<T> {
    pub(super) fn send(mut self, result: Result<T, RoutineBridgeRefusal>) {
        self.publish(result);
    }
    fn publish(&mut self, result: Result<T, RoutineBridgeRefusal>) {
        if self.sent {
            return;
        }
        self.sent = true;
        let wake = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.value = Some(result);
            state.waker.take()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }
}
impl<T> Drop for RoutineResponder<T> {
    fn drop(&mut self) {
        self.publish(Err(RoutineBridgeRefusal::Unavailable));
    }
}
pub(super) fn channel<T>(cancellation: Cancellation) -> (RoutineResponder<T>, RoutineCall<T>) {
    let state = Arc::new(Mutex::new(Reply {
        value: None,
        waker: None,
    }));
    (
        RoutineResponder {
            state: state.clone(),
            sent: false,
        },
        RoutineCall {
            state,
            cancellation,
            complete: false,
        },
    )
}

#[derive(Clone, Default)]
pub(super) struct RoutineCallGates {
    mutation: Arc<AtomicUsize>,
    read: Arc<AtomicUsize>,
}
impl RoutineCallGates {
    pub(super) fn acquire(&self, read: bool) -> Result<RoutinePermit, RoutineBridgeRefusal> {
        let (counter, limit) = if read {
            (&self.read, 16)
        } else {
            (&self.mutation, 2)
        };
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < limit).then_some(n + 1)
            })
            .map_err(|_| RoutineBridgeRefusal::Capacity)?;
        Ok(RoutinePermit {
            counter: counter.clone(),
        })
    }
}
/// Move through admission, background execution and writer completion. Never
/// store this in RoutineCall: HTTP teardown has no right to release capacity.
pub(super) struct RoutinePermit {
    counter: Arc<AtomicUsize>,
}
impl Drop for RoutinePermit {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests;
