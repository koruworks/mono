// SPDX-License-Identifier: MPL-2.0

//! One selector-only inspection admission per native host. Content is computed
//! only by an idle existing session, outside Store, and retained until bounded
//! delivery. Dropping a caller never releases a still-running replay's permit.
use crate::cloud_sync::driver::{Command, Driver};
use koru_device::cloud::{CloudId, CloudReplicaPrincipalV1};
use koru_replication::cloud::transfer::Scope;
use koru_store_wire::cloud_document::{
    CloudDocumentAction, CloudDocumentFailure, CloudDocumentOutcome,
};
use koru_sync_control::inspection::{InspectionPoll, InspectionRequest};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch};
use tokio::time::Instant;
use uuid::Uuid;

pub type Live = Arc<dyn Fn() -> bool + Send + Sync>;
const RUN_LIMIT: Duration = Duration::from_secs(15);
const RETAIN_LIMIT: Duration = Duration::from_secs(30);
const SESSION_LIMIT: usize = koru_sync_control::cloud::MAX_CLOUD_PERMITS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owner {
    pub control_session: Uuid,
    pub owner_scope: u64,
    pub account: CloudId,
    pub account_generation: u64,
}

/// Installed by a real ordinary Full-stage session, never by an inventory read.
#[derive(Clone, Copy)]
pub struct Session {
    pub control_session: Uuid,
    pub account: CloudId,
    pub account_generation: u64,
    pub scope: Scope,
}

struct Registered {
    session: Session,
    commands: mpsc::Sender<Command>,
    live: Live,
}
struct Slot {
    registration: Uuid,
    token: Uuid,
    owner: Owner,
    cancel: watch::Sender<bool>,
    live: Live,
    result: InspectionPoll,
    _retention: Arc<OwnedSemaphorePermit>,
}

pub struct InspectionHub {
    runtime: tokio::runtime::Handle,
    admission: Arc<Semaphore>,
    sessions: Mutex<BTreeMap<Uuid, Registered>>,
    slot: Mutex<Option<Slot>>,
}
pub struct Registration {
    hub: Weak<InspectionHub>,
    id: Uuid,
}
impl Registration {
    /// A consumed inspection carrier ends normally. Keep its current host
    /// session installed until the account worker consumes the bounded result,
    /// so reconnect cannot invalidate an otherwise successful reply.
    pub async fn finish_delivery(&self) {
        let Some(hub) = self.hub.upgrade() else {
            return;
        };
        let cancelled = {
            let slot = lock(&hub.slot);
            slot.as_ref()
                .filter(|s| s.registration == self.id)
                .map(|s| s.cancel.subscribe())
        };
        if let Some(mut cancelled) = cancelled {
            if !*cancelled.borrow_and_update() {
                let _ = tokio::time::timeout(RETAIN_LIMIT, cancelled.changed()).await;
            }
        }
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(hub) = self.hub.upgrade() {
            lock(&hub.sessions).remove(&self.id);
        }
    }
}
pub(crate) struct Job {
    pub request: InspectionRequest,
    pub session: Session,
    pub live: Live,
    pub cancelled: watch::Receiver<bool>,
    pub deadline: Instant,
    pub retention: Arc<OwnedSemaphorePermit>,
    pub reply: oneshot::Sender<Result<CloudDocumentOutcome, CloudDocumentFailure>>,
}
impl InspectionHub {
    pub fn new(runtime: tokio::runtime::Handle) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            admission: Arc::new(Semaphore::new(1)),
            sessions: Mutex::new(BTreeMap::new()),
            slot: Mutex::new(None),
        })
    }
    pub fn register(
        self: &Arc<Self>,
        session: Session,
        driver: &Driver,
        live: Live,
    ) -> Option<Registration> {
        if !matches!(
            session.scope.principal,
            CloudReplicaPrincipalV1::Workspace { .. }
        ) {
            return None;
        }
        let mut sessions = lock(&self.sessions);
        if sessions.len() >= SESSION_LIMIT {
            return None;
        }
        let id = Uuid::new_v4();
        sessions.insert(
            id,
            Registered {
                session,
                commands: driver.inspection_commands(),
                live,
            },
        );
        Some(Registration {
            hub: Arc::downgrade(self),
            id,
        })
    }
    pub fn start(
        self: &Arc<Self>,
        owner: Owner,
        token: Uuid,
        request: InspectionRequest,
    ) -> Result<Uuid, CloudDocumentFailure> {
        let bad = CloudDocumentFailure::Unavailable;
        if token.get_version_num() != 4
            || owner.owner_scope == 0
            || owner.account_generation == 0
            || !request.validate()
            || request.selection.account_id != owner.account.to_string()
        {
            return Err(bad);
        }
        // Admission precedes all capture/replay. A completed but undelivered
        // result also holds this slot; callers explicitly cancel or consume it.
        let retention = Arc::new(
            self.admission
                .clone()
                .try_acquire_owned()
                .map_err(|_| bad)?,
        );
        let candidates = {
            let sessions = lock(&self.sessions);
            sessions.iter().filter(|(_, r)| {
                r.session.control_session == owner.control_session
                    && r.session.account == owner.account
                    && r.session.account_generation == owner.account_generation
                    && matches!(r.session.scope.principal, CloudReplicaPrincipalV1::Workspace { workspace_id, incarnation_id, .. }
                        if workspace_id.to_string() == request.selection.workspace_id
                        && incarnation_id.to_string() == request.selection.incarnation_id)
            }).map(|(id, r)| (*id, r.session, r.commands.clone(), r.live.clone())).collect::<Vec<_>>()
        };
        // Authorization callbacks can acquire the platform's control lock.
        // Never invoke one while holding either hub mutex.
        let mut matching = candidates.into_iter().filter(|(_, _, _, live)| live());
        let (registration, session, commands, live) = matching.next().ok_or(bad)?;
        if matching.next().is_some() {
            return Err(bad);
        }
        let started = Instant::now();
        let (cancel, cancelled) = watch::channel(false);
        let (reply, answer) = oneshot::channel();
        let removal = matches!(request.action, CloudDocumentAction::RemoveFile { .. });
        let job = Job {
            request,
            session,
            live: live.clone(),
            cancelled,
            deadline: started + RUN_LIMIT,
            retention: retention.clone(),
            reply,
        };
        {
            let mut slot = lock(&self.slot);
            if slot.is_some() {
                return Err(bad);
            }
            commands.try_send(Command::Inspect(job)).map_err(|_| bad)?;
            *slot = Some(Slot {
                registration,
                token,
                owner,
                cancel: cancel.clone(),
                live: live.clone(),
                result: InspectionPoll::Pending,
                _retention: retention.clone(),
            });
        }
        let weak = Arc::downgrade(self);
        self.runtime.spawn(async move {
            // This also prevents accumulating TTL tasks by rapidly consuming
            // and replacing results before the preceding observer exits.
            let _retention = retention;
            monitor(weak, token, cancel, live, removal, answer, started).await;
        });
        Ok(token)
    }
    pub fn poll(&self, owner: Owner, token: Uuid) -> InspectionPoll {
        let live = lock(&self.slot)
            .as_ref()
            .filter(|s| s.token == token && s.owner == owner)
            .map(|s| s.live.clone());
        let valid = live.is_some_and(|live| live());
        let mut slot = lock(&self.slot);
        let Some(held) = slot
            .as_ref()
            .filter(|s| s.token == token && s.owner == owner)
        else {
            return failed(CloudDocumentFailure::Unavailable);
        };
        if !valid {
            clear(&mut slot);
            return failed(CloudDocumentFailure::Unavailable);
        }
        let result = held.result.clone();
        if !matches!(result, InspectionPoll::Pending) {
            clear(&mut slot);
        }
        result
    }
    pub fn cancel(&self, owner: Owner, token: Uuid) {
        let mut slot = lock(&self.slot);
        if slot
            .as_ref()
            .is_some_and(|s| s.token == token && s.owner == owner)
        {
            clear(&mut slot);
        }
    }
    pub fn close_session(&self, session: Uuid) {
        let mut slot = lock(&self.slot);
        if slot
            .as_ref()
            .is_some_and(|s| s.owner.control_session == session)
        {
            clear(&mut slot);
        }
    }
    pub fn revalidate(&self) {
        let held = lock(&self.slot).as_ref().map(|s| (s.token, s.live.clone()));
        let Some((token, live)) = held else {
            return;
        };
        if live() {
            return;
        }
        let mut slot = lock(&self.slot);
        if slot.as_ref().is_some_and(|s| s.token == token) {
            clear(&mut slot);
        }
    }
}
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
fn clear(slot: &mut Option<Slot>) {
    if let Some(slot) = slot.take() {
        slot.cancel.send_replace(true);
    }
}
fn failed(failure: CloudDocumentFailure) -> InspectionPoll {
    InspectionPoll::Failed { failure }
}
fn encode(result: Result<CloudDocumentOutcome, CloudDocumentFailure>) -> InspectionPoll {
    match result {
        Ok(CloudDocumentOutcome::Removed) => InspectionPoll::Removed,
        Ok(CloudDocumentOutcome::View(view)) => match view.to_json() {
            Ok(json) => InspectionPoll::View { json },
            Err(_) => failed(CloudDocumentFailure::Unavailable),
        },
        Err(failure) => failed(failure),
    }
}
async fn monitor(
    hub: Weak<InspectionHub>,
    token: Uuid,
    cancel: watch::Sender<bool>,
    live: Live,
    removal: bool,
    mut answer: oneshot::Receiver<Result<CloudDocumentOutcome, CloudDocumentFailure>>,
    started: Instant,
) {
    let mut cancelled = cancel.subscribe();
    if *cancelled.borrow() {
        return;
    }
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let unknown = if removal {
        CloudDocumentFailure::OutcomeUnknown
    } else {
        CloudDocumentFailure::Unavailable
    };
    let result = loop {
        tokio::select! {
            biased;
            _ = cancelled.changed() => return,
            result = &mut answer => break encode(result.unwrap_or(Err(unknown))),
            _ = tokio::time::sleep_until(started + RUN_LIMIT) => {
                cancel.send_replace(true);
                break failed(unknown);
            },
            _ = tick.tick() => if !(live)() {
                if let Some(hub) = hub.upgrade() { hub.revalidate(); }
                return;
            },
        }
    };
    cancelled.borrow_and_update();
    if let Some(hub) = hub.upgrade() {
        let mut slot = lock(&hub.slot);
        if let Some(slot) = slot.as_mut().filter(|s| s.token == token) {
            slot.result = result;
        } else {
            return;
        }
    } else {
        return;
    }
    // Completion retention expires even when the originating UI disappears.
    tokio::select! {
        _ = cancelled.changed() => {},
        _ = tokio::time::sleep_until(started + RETAIN_LIMIT) => {
            if let Some(hub) = hub.upgrade() {
                let mut slot = lock(&hub.slot);
                if slot.as_ref().is_some_and(|s| s.token == token) { clear(&mut slot); }
            }
        },
    }
}

#[cfg(test)]
mod tests;
