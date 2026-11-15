// SPDX-License-Identifier: MPL-2.0
//! The AI half of the decision wait enrollment handshake (P4.2 stage E).
//!
//! Enroll and Read are finite operations with the Routine bridge's call
//! handle, owner epoch and request identity, on the existing
//! `ProviderAdminPool` lanes: Enroll on the Routine mutation lane (one active,
//! one queued, shared with Routine operations), Read on the bounded Run-read
//! lane. There is no new queue or thread. Exact retained replay precedes any
//! delegate read; the writer transaction is the only enrollment receipt, and
//! no UI card, bus event or tool activity ever stands in for it.
use super::*;
use crate::worker::routine_call::{RoutineCallContext, RoutinePermit, RoutineResponder};
use koru_delegate_api::routines::RoutineBridgeRefusal;
use koru_delegate_core::decisions::{
    BotWaitEnrollmentKey, BotWaitEnrollmentReply, BotWaitRefusal, DecisionValidate,
    EnrollBotWaitCleanup, ReadBotWaitCleanup,
};
use std::collections::BTreeMap;

mod client;
mod driver;
#[cfg(test)]
pub(in crate::worker) mod fixture;
mod jobs;
mod port;
#[cfg(test)]
mod tests;
pub(in crate::worker) use jobs::{DecisionWaitJob, DecisionWaitResult};
pub use port::{DecisionWaitDelegate, DecisionWaitPortError, DecisionWaitSubject};

#[derive(Clone)]
enum Request {
    Enroll(EnrollBotWaitCleanup),
    Read(ReadBotWaitCleanup),
}
impl Request {
    fn key(&self) -> &BotWaitEnrollmentKey {
        match self {
            Self::Enroll(request) => &request.key,
            Self::Read(request) => &request.key,
        }
    }
    fn read(&self) -> bool {
        matches!(self, Self::Read(_))
    }
    fn valid(&self) -> bool {
        match self {
            Self::Enroll(request) => request.validate().is_ok(),
            Self::Read(request) => request.validate().is_ok(),
        }
    }
}

type Reply = RoutineResponder<BotWaitEnrollmentReply, BotWaitRefusal>;

pub(in crate::worker) struct Incoming {
    request: Request,
    reply: Reply,
    context: RoutineCallContext,
    permit: RoutinePermit,
}

#[derive(Clone, PartialEq, Eq)]
struct Key {
    request_id: Uuid,
    serial: u64,
    owner_epoch: u64,
    wait: BotWaitEnrollmentKey,
    read: bool,
    phase: u32,
}

pub(in crate::worker) struct Operation {
    key: Key,
    request: Request,
    reply: Reply,
    context: RoutineCallContext,
    // Survives HTTP drop/timeout through the matching actual completion.
    _permit: RoutinePermit,
}

struct Active {
    key: Key,
    cancellation: Cancellation,
}

/// Owner-epoch/serial registry for decision operations. It follows the same
/// owner epoch as Routine occurrences, so an old completion never reaches a
/// new owner. The delegate port is `None` in production until IPC16 exists.
pub(in crate::worker) struct DecisionWaitDriver {
    delegate: Option<Arc<dyn DecisionWaitDelegate>>,
    owner: Option<BotOwner>,
    epoch: u64,
    serial: u64,
    active: BTreeMap<u64, Active>,
}
impl DecisionWaitDriver {
    pub(in crate::worker) fn new(delegate: Option<Arc<dyn DecisionWaitDelegate>>) -> Self {
        Self {
            delegate,
            owner: None,
            epoch: 0,
            serial: 0,
            active: BTreeMap::new(),
        }
    }
    pub(in crate::worker) fn delegate(&self) -> Option<Arc<dyn DecisionWaitDelegate>> {
        self.delegate.clone()
    }
    pub(in crate::worker) fn cancel_all(&self) {
        for active in self.active.values() {
            active.cancellation.cancel();
        }
    }
    fn synchronize(&mut self, owner: Option<BotOwner>, epoch: u64) {
        if self.owner != owner || self.epoch != epoch {
            self.cancel_all();
            self.owner = owner;
            self.epoch = epoch;
        }
    }
    fn current(&self, key: &Key) -> bool {
        self.owner.is_some_and(|owner| same_owner(owner, &key.wait))
            && self.epoch == key.owner_epoch
            && self
                .active
                .get(&key.serial)
                .is_some_and(|active| active.key == *key)
    }
    fn finish(&mut self, key: &Key) {
        if self
            .active
            .get(&key.serial)
            .is_some_and(|active| active.key == *key)
        {
            self.active.remove(&key.serial);
        }
    }
}
impl Drop for DecisionWaitDriver {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

fn same_owner(owner: BotOwner, key: &BotWaitEnrollmentKey) -> bool {
    owner.user_scope == key.owner.user_scope && owner.device == key.owner.device
}

/// Repository refusals in the decision vocabulary. Unknown storage state is
/// `Unavailable`; nothing here can produce absence.
fn refusal(error: BotError) -> BotWaitRefusal {
    match error {
        BotError::Invalid => BotWaitRefusal::Invalid,
        BotError::Forbidden => BotWaitRefusal::Forbidden,
        BotError::Conflict => BotWaitRefusal::Conflict,
        BotError::NotFound | BotError::RevisionConflict => BotWaitRefusal::Stale,
        _ => BotWaitRefusal::Unavailable,
    }
}
/// The shared call context and gates speak the Routine vocabulary. Capacity
/// is a transient refusal here: unknown completion, never Missing.
fn bridge_refusal(code: RoutineBridgeRefusal) -> BotWaitRefusal {
    match code {
        RoutineBridgeRefusal::Invalid | RoutineBridgeRefusal::ContextTooLarge => {
            BotWaitRefusal::Invalid
        }
        RoutineBridgeRefusal::Forbidden | RoutineBridgeRefusal::Unauthorized => {
            BotWaitRefusal::Forbidden
        }
        RoutineBridgeRefusal::Conflict => BotWaitRefusal::Conflict,
        RoutineBridgeRefusal::Stale => BotWaitRefusal::Stale,
        RoutineBridgeRefusal::Unavailable | RoutineBridgeRefusal::Capacity => {
            BotWaitRefusal::Unavailable
        }
    }
}
