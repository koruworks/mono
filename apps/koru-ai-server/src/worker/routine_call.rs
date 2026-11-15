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

/// The closed refusal vocabulary one private bridge domain answers with. The
/// finite call machinery is shared; the vocabulary is not: a decision call
/// refuses with `BotWaitRefusal`, never by widening the Routine enum.
pub trait CallRefusal: Copy + Send + 'static {
    /// What a lost producer resolves to: unknown completion, never absence.
    const UNAVAILABLE: Self;
}
impl CallRefusal for RoutineBridgeRefusal {
    const UNAVAILABLE: Self = RoutineBridgeRefusal::Unavailable;
}
impl CallRefusal for koru_delegate_core::decisions::BotWaitRefusal {
    const UNAVAILABLE: Self = koru_delegate_core::decisions::BotWaitRefusal::Unavailable;
}

struct Reply<T, R> {
    value: Option<Result<T, R>>,
    waker: Option<Waker>,
}
/// Await with the same total transport deadline. Drop requests cooperative
/// cancellation; it does not imply a remote IPC mutation stopped, or release
/// the local background job's owned permit.
/// Cancellation alone neither wakes nor resolves this Future. The transport
/// owns waiter expiration using the same absolute deadline; actual producer
/// completion or teardown publishes the result and wakes a retained waiter.
pub struct RoutineCall<T, R = RoutineBridgeRefusal> {
    state: Arc<Mutex<Reply<T, R>>>,
    cancellation: Cancellation,
    complete: bool,
}
impl<T, R> RoutineCall<T, R> {
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }
}
impl<T, R> Future for RoutineCall<T, R> {
    type Output = Result<T, R>;
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
impl<T, R> Drop for RoutineCall<T, R> {
    fn drop(&mut self) {
        if !self.complete {
            self.cancellation.cancel();
        }
    }
}
impl<T, R> std::fmt::Debug for RoutineCall<T, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineCall([REDACTED])")
    }
}

pub(super) struct RoutineResponder<T, R: CallRefusal = RoutineBridgeRefusal> {
    state: Arc<Mutex<Reply<T, R>>>,
    sent: bool,
}
impl<T, R: CallRefusal> RoutineResponder<T, R> {
    pub(super) fn send(mut self, result: Result<T, R>) {
        self.publish(result);
    }
    fn publish(&mut self, result: Result<T, R>) {
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
impl<T, R: CallRefusal> Drop for RoutineResponder<T, R> {
    fn drop(&mut self) {
        self.publish(Err(R::UNAVAILABLE));
    }
}
pub(super) fn channel<T>(cancellation: Cancellation) -> (RoutineResponder<T>, RoutineCall<T>) {
    refusal_channel(cancellation)
}
/// The same one-shot handle for another bridge domain's refusal vocabulary.
pub(super) fn refusal_channel<T, R: CallRefusal>(
    cancellation: Cancellation,
) -> (RoutineResponder<T, R>, RoutineCall<T, R>) {
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
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
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
