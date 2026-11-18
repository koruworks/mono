// SPDX-License-Identifier: MPL-2.0

//! **The Store saying a workspace's tasks changed** — the Tasks surfaces'
//! one trigger from outside this window.
//!
//! The list page and the rail's counts read the Store's task read model.
//! They asked again when this window wrote something, when the view changed
//! and on a half-minute timer, so a task moved on another device — or in
//! another window — sat in its old place until the timer came round
//! (maintainer, 2026-11-17: *I need to switch views to see the position
//! change*). The Store now says so itself: `projection.tasks.changed`,
//! derived as the read model is written, whoever made the change. This
//! listens for it and remembers which workspaces it named; the window asks
//! on its maintenance turn and calls its own `tasks_changed`, the door a
//! local write already uses.
//!
//! **A trigger, never truth.** The event carries a workspace and nothing
//! else, and what the surfaces draw is still only ever the Store's answer.
//! The timer stays as the net under an event that was missed — a tail that
//! reconnected, a Store that restarted — so the worst case is what the
//! ordinary case used to be.
//!
//! One live tail per window, filtered by name at the Store, on a thread of
//! its own; nothing here runs on the frame.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Waker = Arc<dyn Fn() + Send + Sync>;

/// The event's name, and the prefix the tail filters on.
const TASKS_CHANGED: &str = "projection.tasks.changed";

/// The workspaces whose tasks changed since the window last asked.
type Changed = Arc<Mutex<HashSet<uuid::Uuid>>>;

#[derive(Default)]
pub(super) struct TaskEvents {
    client: Option<koru_api_client::Client>,
    wake: Option<Waker>,
    changed: Changed,
    started: bool,
    arriving: Option<Receiver<koru_api_client::Subscription>>,
    /// Held for as long as the window lives; dropping it stops the tail.
    subscription: Option<koru_api_client::Subscription>,
    stop: Option<Arc<AtomicBool>>,
}

impl TaskEvents {
    /// Cache a clone of the already-discovered client. No I/O here.
    pub(super) fn set_client(&mut self, client: Option<koru_api_client::Client>) {
        if self.client.is_none() {
            self.client = client;
        }
        self.ensure_subscription();
    }

    pub(super) fn set_waker(&mut self, wake: Waker) {
        self.wake = Some(wake);
        self.ensure_subscription();
    }

    /// Whether `workspace`'s tasks changed since this was last asked.
    /// Answers once per batch: what arrived is forgotten on the way out,
    /// another workspace's news included — a window that later shows that
    /// workspace asks afresh when it does.
    pub(super) fn take_changed(&mut self, workspace: Option<koru_core::WorkspaceId>) -> bool {
        if let Some(receiver) = &self.arriving
            && let Ok(subscription) = receiver.try_recv()
        {
            self.arriving = None;
            self.subscription = Some(subscription);
        }
        let named = match self.changed.lock() {
            Ok(mut changed) => std::mem::take(&mut *changed),
            Err(_) => return false,
        };
        workspace.is_some_and(|workspace| named.contains(&workspace.as_uuid()))
    }

    /// Whether the tail is up: the Store's events are being heard, so the
    /// surfaces' own timers can stand back to being a safety net.
    pub(super) fn live(&self) -> bool {
        self.subscription.is_some()
    }

    fn ensure_subscription(&mut self) {
        if self.started || self.subscription.is_some() {
            return;
        }
        let (Some(client), Some(wake)) = (self.client.clone(), self.wake.clone()) else {
            return;
        };
        if !client.events().available() {
            return;
        }
        let (send, receive) = mpsc::channel();
        let changed = Arc::clone(&self.changed);
        let stop = Arc::new(AtomicBool::new(false));
        let setup_stop = Arc::clone(&stop);
        let setup_wake = Arc::clone(&wake);
        let spawned = std::thread::Builder::new()
            .name("koru-task-events-setup".into())
            .spawn(move || {
                let mut delay = Duration::from_millis(250);
                let subscription = loop {
                    if setup_stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let changed = Arc::clone(&changed);
                    let event_wake = Arc::clone(&wake);
                    // From *now*: what the window draws next is a fresh
                    // answer anyway, so history would only repeat it.
                    match client.events().on(
                        None,
                        koru_api_client::Filter::names(TASKS_CHANGED),
                        move |event| {
                            if note(
                                &changed,
                                event.envelope.name.as_str(),
                                event.envelope.scope.workspace_id.map(|id| id.0),
                            ) {
                                event_wake();
                            }
                        },
                    ) {
                        Ok(subscription) => break subscription,
                        Err(_) => {
                            std::thread::sleep(delay);
                            delay = (delay * 2).min(Duration::from_secs(5));
                        }
                    }
                };
                if send.send(subscription).is_ok() {
                    setup_wake();
                }
            });
        if spawned.is_ok() {
            self.started = true;
            self.arriving = Some(receive);
            self.stop = Some(stop);
        }
    }
}

impl Drop for TaskEvents {
    fn drop(&mut self) {
        if let Some(stop) = &self.stop {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

/// Record one event. Answers whether it was news — the right name, about a
/// workspace, and not one already waiting to be asked about, so a burst
/// wakes the window once.
fn note(changed: &Mutex<HashSet<uuid::Uuid>>, name: &str, workspace: Option<uuid::Uuid>) -> bool {
    if name != TASKS_CHANGED {
        return false;
    }
    let Some(workspace) = workspace else {
        return false;
    };
    changed
        .lock()
        .is_ok_and(|mut changed| changed.insert(workspace))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> koru_core::WorkspaceId {
        koru_core::WorkspaceId::from_uuid(uuid::Uuid::new_v4())
    }

    #[test]
    fn the_event_marks_its_workspace_once_and_other_names_mark_nothing() {
        let mut events = TaskEvents::default();
        let (here, elsewhere) = (workspace(), workspace());
        assert!(!events.take_changed(Some(here)), "nothing has arrived");

        assert!(note(&events.changed, TASKS_CHANGED, Some(here.as_uuid())));
        assert!(
            !note(&events.changed, TASKS_CHANGED, Some(here.as_uuid())),
            "a burst wakes the window once"
        );
        assert!(!note(&events.changed, "task.created", Some(here.as_uuid())));
        assert!(!note(&events.changed, TASKS_CHANGED, None));

        assert!(events.take_changed(Some(here)));
        assert!(!events.take_changed(Some(here)), "answered once per batch");

        // News of another workspace is not this window's, and does not wait
        // around to be mistaken for it later.
        assert!(note(
            &events.changed,
            TASKS_CHANGED,
            Some(elsewhere.as_uuid())
        ));
        assert!(!events.take_changed(Some(here)));
        assert!(!events.take_changed(Some(elsewhere)));
        assert!(!events.take_changed(None));
    }

    /// Without a client or a waker there is nothing to listen with, and
    /// nothing is started.
    #[test]
    fn nothing_is_started_without_a_client_and_a_waker() {
        let mut events = TaskEvents::default();
        events.set_client(None);
        assert!(!events.started);
        events.set_waker(Arc::new(|| {}));
        assert!(!events.started);
    }
}
