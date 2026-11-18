// SPDX-License-Identifier: MPL-2.0

//! The bounded worker and the cached projection around the coordinator.
//!
//! The shape is the enrollment module's: the host enqueues commands and reads
//! a whole cached snapshot, one coordinator owns authority/permit effects,
//! retained Profile session threads own document exchanges, and
//! the notification is never invoked while the cached-state lock is held. A
//! slow hosted call therefore cannot make `snapshot()` block.

use std::sync::Arc;

use super::coordinator::Coordinator;
use super::*;
use crate::ports::{Clock, Entropy, SystemClock, SystemEntropy};

pub struct Control {
    wake: mpsc::SyncSender<()>,
    desired: Arc<Mutex<Desired>>,
    visible: Visible,
    foreground: Arc<AtomicBool>,
    cancel: Cancel,
}

impl Control {
    pub fn start<A: Api, H: Host>(
        api: A,
        host: H,
        notify: impl Fn(Snapshot) + Send + 'static,
    ) -> Self {
        Self::start_with_clock(
            api,
            host,
            Arc::new(SystemClock),
            Arc::new(SystemEntropy),
            true,
            notify,
        )
    }

    pub fn start_with_clock<A: Api, H: Host>(
        api: A,
        host: H,
        clock: Arc<dyn Clock>,
        entropy: Arc<dyn Entropy>,
        foreground: bool,
        notify: impl Fn(Snapshot) + Send + 'static,
    ) -> Self {
        Self::start_with_spawn(api, host, clock, entropy, foreground, notify, |worker| {
            std::thread::Builder::new()
                .name("koru-cloud".into())
                .spawn(worker)
                .map(|_| ())
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_with_spawn<A: Api, H: Host>(
        api: A,
        host: H,
        clock: Arc<dyn Clock>,
        entropy: Arc<dyn Entropy>,
        initial_foreground: bool,
        notify: impl Fn(Snapshot) + Send + 'static,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
    ) -> Self {
        let (wake, receiver) = mpsc::sync_channel(WAKE_CAPACITY);
        let desired = Arc::new(Mutex::new(Desired::default()));
        let visible: Visible = Arc::new(Mutex::new(Snapshot::idle()));
        let foreground = Arc::new(AtomicBool::new(initial_foreground));
        let cancel = Cancel::default();
        let worker_desired = desired.clone();
        let worker_visible = visible.clone();
        let worker_foreground = foreground.clone();
        let worker_cancel = cancel.clone();
        // **Told when the snapshot changed, not every time one is made.** The
        // worker makes one between every dataset session and after every
        // dispatch; the Agent turned each into a push of the whole account to
        // every window, about two a second on an idle device (2026-11-17).
        let notify = Arc::new(Mutex::new(Notifier { notify, last: None }));
        let result = spawn(Box::new(move || {
            let mut coordinator = Coordinator::new(api, host, entropy, worker_cancel);
            coordinator.follow_authorization(worker_desired.clone(), worker_foreground.clone());
            let progress_visible = worker_visible.clone();
            let progress_notify = notify.clone();
            coordinator.observe(move |snapshot| {
                *progress_visible
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = snapshot.clone();
                // Publish between blocking dataset sessions as well as after a pass.
                progress_notify
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .publish(snapshot);
            });
            loop {
                let next = {
                    let mut desired = worker_desired
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    std::mem::take(&mut *desired)
                };
                if next.stop {
                    coordinator.apply(Command::Activate(None, None), clock.as_ref());
                    break;
                }
                for command in next.commands {
                    coordinator.apply(command, clock.as_ref());
                }
                let foreground = worker_foreground.load(Ordering::Acquire);
                coordinator.dispatch(clock.as_ref(), foreground);
                let snapshot = coordinator.snapshot();
                {
                    let mut visible = worker_visible
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    *visible = snapshot.clone();
                }
                // Notification is never invoked under the cached-state lock.
                notify
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .publish(snapshot);
                let wait = coordinator
                    .deadline(foreground)
                    .map(|at| at.saturating_duration_since(clock.monotonic()));
                match wait {
                    Some(wait) => match receiver.recv_timeout(wait) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    },
                    None => {
                        if receiver.recv().is_err() {
                            break;
                        }
                    }
                }
            }
        }));
        if result.is_err() {
            let mut visible = visible.lock().unwrap_or_else(|error| error.into_inner());
            visible.failure_code = Some(CLOUD_UNAVAILABLE);
        }
        Self {
            wake,
            desired,
            visible,
            foreground,
            cancel,
        }
    }

    fn enqueue(&self, command: Command) -> bool {
        let mut desired = self
            .desired
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if desired.stop {
            return false;
        }
        // Heartbeats are one pending wake, not thirty queue entries that
        // could prevent a credential refresh during a long dataset session.
        if matches!(&command, Command::Wake)
            && desired
                .commands
                .iter()
                .any(|queued| matches!(queued, Command::Wake))
        {
            return true;
        }
        if desired.commands.len() >= 32 {
            return false;
        }
        desired.commands.push_back(command);
        drop(desired);
        signal(&self.wake)
    }

    pub fn update_authorization(&self, authority: EnrollmentAuthorization) -> bool {
        self.enqueue(Command::Activate(Some(authority), None))
    }

    pub fn activate_owned(
        &self,
        authority: Option<EnrollmentAuthorization>,
        owner_scope: u64,
    ) -> bool {
        self.enqueue(Command::Activate(authority, Some(owner_scope)))
    }

    /// Only an acknowledged same-session refresh. A different session is an
    /// account switch and must arrive through [`Self::update_authorization`].
    pub fn refresh_authorization(
        &self,
        previous: EnrollmentAuthorization,
        next: EnrollmentAuthorization,
    ) -> bool {
        if !same_authority(&previous, &next) {
            return false;
        }
        self.enqueue(Command::Refresh(previous, next))
    }

    /// **Check the account now**: something here may have changed it — a
    /// sign-in, a Profile chosen, a local catalog change, the reconcile.
    pub fn wake(&self) -> bool {
        self.enqueue(Command::Wake)
    }

    /// **The heartbeat.** Renews what is about to expire and keeps the live
    /// sessions' credentials current, and checks the account's catalog only
    /// when the last check is [`ACCOUNT_CHECK_EVERY`] old — so a quiet device
    /// reads the whole catalog every few minutes, not every half minute.
    ///
    /// [`ACCOUNT_CHECK_EVERY`]: super::coordinator::ACCOUNT_CHECK_EVERY
    pub fn tick(&self) -> bool {
        self.enqueue(Command::Tick)
    }

    pub fn cancel(&self) -> bool {
        self.cancel.request();
        self.enqueue(Command::Cancel)
    }

    pub fn set_foreground(&self, foreground: bool) {
        if self.foreground.swap(foreground, Ordering::AcqRel) != foreground {
            signal(&self.wake);
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.visible
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// Run the worker's next iteration without producing a change event. The
    /// scheduled deadlines come from the injected clock, so a test advances
    /// that clock and then pokes; a real host never needs this.
    #[cfg(test)]
    pub(super) fn poke(&self) -> bool {
        signal(&self.wake)
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.cancel.request();
        self.desired
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .stop = true;
        let _ = self.wake.try_send(());
    }
}

impl Driver for Control {
    fn update_authorization(&self, authority: EnrollmentAuthorization) -> bool {
        Control::update_authorization(self, authority)
    }
    fn activate_owned(&self, authority: Option<EnrollmentAuthorization>, owner_scope: u64) -> bool {
        Control::activate_owned(self, authority, owner_scope)
    }
    fn refresh_authorization(
        &self,
        previous: EnrollmentAuthorization,
        next: EnrollmentAuthorization,
    ) -> bool {
        Control::refresh_authorization(self, previous, next)
    }
    fn set_foreground(&self, foreground: bool) {
        Control::set_foreground(self, foreground)
    }
    fn wake(&self) -> bool {
        Control::wake(self)
    }
    fn cancel(&self) -> bool {
        Control::cancel(self)
    }
    fn snapshot(&self) -> Snapshot {
        Control::snapshot(self)
    }
}

/// The control's observer, and what it was last told.
struct Notifier<F> {
    notify: F,
    last: Option<Snapshot>,
}

impl<F: Fn(Snapshot)> Notifier<F> {
    /// Pass `snapshot` on when it differs from the last one passed on.
    fn publish(&mut self, snapshot: Snapshot) {
        if self.last.as_ref() == Some(&snapshot) {
            return;
        }
        self.last = Some(snapshot.clone());
        (self.notify)(snapshot);
    }
}

#[cfg(test)]
mod notifier_tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn an_unchanged_snapshot_is_not_passed_on_again() {
        let told = RefCell::new(0);
        let mut notifier = Notifier {
            notify: |_: Snapshot| *told.borrow_mut() += 1,
            last: None,
        };
        notifier.publish(Snapshot::default());
        notifier.publish(Snapshot::default());
        assert_eq!(*told.borrow(), 1);
        notifier.publish(Snapshot {
            retry_at_millis: Some(1),
            ..Snapshot::default()
        });
        assert_eq!(*told.borrow(), 2, "a change is");
    }
}
