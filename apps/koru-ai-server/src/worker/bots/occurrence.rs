// SPDX-License-Identifier: MPL-2.0
//! Private occurrence ownership. No HTTP router, provider or ordinary Submit.
use super::*;
use crate::worker::routine_call::{
    RoutineCall, RoutineCallContext, RoutineCallGates, RoutinePermit, RoutineResponder, channel,
};
use koru_delegate_api::{
    bots::{BotCommand as DelegateCommand, BotDelegatePort, BotReply},
    routines::*,
};
use koru_delegate_core::{
    RoutineClaimIdentity, RoutineClaimObservation, RoutineClaimRecord, RoutineDigest32,
    RoutineLinkAcknowledgement, RoutineLinkOutcome, RoutineLinkState, RoutinePrepared, RoutineSlot,
};
use std::collections::{BTreeMap, VecDeque};

mod client;
mod driver;
mod jobs;
mod preclaim;
mod recovery;
mod replies;
#[cfg(test)]
mod tests;
pub(in crate::worker) use jobs::{OccurrenceJob, OccurrenceResult};

#[derive(Clone)]
enum Request {
    Prepare(PrepareOccurrence),
    Read(ReadPreparation),
    Finalize(FinalizeOccurrence),
    Link(ReadOccurrenceLink),
    Preclaim(SynchronizeRoutinePreclaim),
    Recover(ReadPreparation),
}
impl Request {
    fn slot(&self) -> RoutineSlot {
        match self {
            Self::Prepare(r) => r.slot,
            Self::Read(r) | Self::Recover(r) => r.slot,
            Self::Finalize(r) => r.claim.slot,
            Self::Link(r) => r.claim.slot,
            Self::Preclaim(r) => r.slot,
        }
    }
    fn identity(&self) -> Result<Identity, RoutineBridgeRefusal> {
        Ok(match self {
            Self::Prepare(r) => Identity::Semantic(
                bot_routine_semantic_hash(
                    &r.slot,
                    &r.schedule_definition,
                    r.workspace,
                    r.owner_handoff,
                )
                .map_err(refusal)?,
            ),
            Self::Read(r) | Self::Recover(r) => Identity::Semantic(r.semantic_hash),
            Self::Finalize(r) => Identity::Claim(r.claim.clone()),
            Self::Link(r) => Identity::Claim(r.claim.clone()),
            Self::Preclaim(r) => Identity::Preparation {
                semantic_hash: r.semantic_hash,
                preparation_hash: r.preparation_hash,
            },
        })
    }
}

enum Reply {
    Prepare(RoutineResponder<PrepareOccurrenceReply>),
    Read(RoutineResponder<ReadPreparationReply>),
    Finalize(RoutineResponder<FinalizeOccurrenceReply>),
    Link(RoutineResponder<ReadOccurrenceLinkReply>),
    Preclaim(RoutineResponder<SynchronizeRoutinePreclaimReply>),
    Recovery,
}
impl Reply {
    fn refuse(self, code: RoutineBridgeRefusal) {
        match self {
            Self::Prepare(reply) => reply.send(Ok(PrepareOccurrenceReply::Refused { code })),
            Self::Read(reply) => reply.send(Ok(ReadPreparationReply::Refused { code })),
            Self::Finalize(reply) => reply.send(Ok(FinalizeOccurrenceReply::Refused { code })),
            Self::Link(reply) => reply.send(Ok(ReadOccurrenceLinkReply::Refused { code })),
            Self::Preclaim(reply) => {
                reply.send(Ok(SynchronizeRoutinePreclaimReply::Refused { code }))
            }
            Self::Recovery => {}
        }
    }
}

pub(in crate::worker) struct Incoming {
    request: Request,
    reply: Reply,
    context: RoutineCallContext,
    permit: RoutinePermit,
}

#[derive(Clone, PartialEq, Eq)]
enum Identity {
    Semantic(RoutineDigest32),
    Claim(RoutineClaimIdentity),
    Preparation {
        semantic_hash: RoutineDigest32,
        preparation_hash: RoutineDigest32,
    },
}
#[derive(Clone, PartialEq, Eq)]
struct Key {
    request_id: Uuid,
    serial: u64,
    owner_epoch: u64,
    slot: RoutineSlot,
    identity: Identity,
    phase: u32,
}
struct Operation {
    key: Key,
    request: Request,
    reply: Reply,
    context: RoutineCallContext,
    // Never held by the waiting Future or by the writer's identity registry.
    _permit: RoutinePermit,
}
struct Active {
    key: Key,
    cancellation: Cancellation,
}

pub(in crate::worker) struct OccurrenceDriver {
    gates: RoutineCallGates,
    owner: Option<BotOwner>,
    epoch: u64,
    serial: u64,
    active: BTreeMap<u64, Active>,
    recovery: recovery::Recovery,
}
impl OccurrenceDriver {
    pub(in crate::worker) fn cancel_all(&self) {
        for active in self.active.values() {
            active.cancellation.cancel();
        }
    }
    pub(in crate::worker) fn new(gates: RoutineCallGates) -> Self {
        Self {
            gates,
            owner: None,
            epoch: 0,
            serial: 0,
            active: BTreeMap::new(),
            recovery: Default::default(),
        }
    }
    fn synchronize(&mut self, owner: Option<BotOwner>, epoch: u64) {
        if self.owner != owner || self.epoch != epoch {
            for active in self.active.values() {
                active.cancellation.cancel();
            }
            self.owner = owner;
            self.epoch = epoch;
            self.recovery = Default::default();
        }
    }
    fn current(&self, key: &Key) -> bool {
        self.owner == Some(ai_owner(key.slot))
            && self.epoch == key.owner_epoch
            && self
                .active
                .get(&key.serial)
                .is_some_and(|active| active.key == *key)
    }
    fn finish(&mut self, operation: &Operation, owed: bool) {
        let matched = self
            .active
            .get(&operation.key.serial)
            .is_some_and(|active| active.key == operation.key);
        let current = matched
            && self.owner == Some(ai_owner(operation.key.slot))
            && self.epoch == operation.key.owner_epoch;
        if matched {
            self.active.remove(&operation.key.serial);
        }
        if current && matches!(&operation.request, Request::Recover(_)) {
            self.recovery.finished(owed);
        }
    }
}
impl Drop for OccurrenceDriver {
    fn drop(&mut self) {
        for active in self.active.values() {
            active.cancellation.cancel();
        }
    }
}

fn ai_owner(slot: RoutineSlot) -> BotOwner {
    BotOwner {
        user_scope: slot.owner.user_scope,
        device: slot.owner.device,
    }
}
fn refusal(error: BotError) -> RoutineBridgeRefusal {
    match error {
        BotError::Invalid => RoutineBridgeRefusal::Invalid,
        BotError::Forbidden => RoutineBridgeRefusal::Forbidden,
        BotError::Conflict | BotError::RevisionConflict => RoutineBridgeRefusal::Conflict,
        BotError::NotFound => RoutineBridgeRefusal::Stale,
        _ => RoutineBridgeRefusal::Unavailable,
    }
}

impl WorkerState {
    /// Row 10 of the team pause's admission inventory (M8): while the team
    /// is paused no new occurrence is prepared and none is preclaimed. The
    /// answer is the bridge's existing refusal, so the scheduler retries the
    /// occurrence until its due window closes and then records it expired.
    /// An occurrence already prepared is still read, and one already claimed
    /// is finished: that is work that was admitted before the pause.
    fn refuse_occurrence_under_team_pause(
        &self,
        operation: &Operation,
    ) -> Result<(), RoutineBridgeRefusal> {
        if self
            .team_paused(ai_owner(operation.key.slot))
            .map_err(refusal)?
        {
            return Err(refusal(BotError::TeamPaused));
        }
        Ok(())
    }
}

/// Existing Run reads, Routine observations and decision wait reads share the
/// same bounded thread.
pub(in crate::worker) enum RunReadWork {
    Bot(BotRunReadJob),
    Routine(OccurrenceJob),
    Decision(super::decision_wait::DecisionWaitJob),
}
impl RunReadWork {
    pub(in crate::worker) fn into_admin(self) -> ProviderAdminJob {
        match self {
            Self::Bot(job) => ProviderAdminJob::BotRunRead(job),
            Self::Routine(job) => ProviderAdminJob::RoutineRead(job),
            Self::Decision(job) => ProviderAdminJob::DecisionWaitRead(job),
        }
    }
    pub(in crate::worker) fn run(self, output: &SyncSender<Envelope>, shutdown: &AtomicBool) {
        match self {
            Self::Bot(job) => job.run(),
            Self::Routine(job) => send_admin_envelope(
                output,
                Envelope::RoutineOccurrenceCompleted(Box::new(job.run(None))),
                shutdown,
            ),
            Self::Decision(job) => send_admin_envelope(
                output,
                Envelope::DecisionWaitCompleted(Box::new(job.run())),
                shutdown,
            ),
        }
    }
}

/// The Routine mutation lane's work: Routine occurrence mutations and decision
/// wait enrollment share its one physical thread and its one queued slot.
pub(in crate::worker) enum MutationWork {
    Routine(OccurrenceJob),
    Decision(super::decision_wait::DecisionWaitJob),
}
impl MutationWork {
    pub(in crate::worker) fn into_admin(self) -> ProviderAdminJob {
        match self {
            Self::Routine(job) => ProviderAdminJob::RoutineMutation(job),
            Self::Decision(job) => ProviderAdminJob::DecisionWaitMutation(job),
        }
    }
    pub(in crate::worker) fn run(
        self,
        output: &SyncSender<Envelope>,
        agents: Option<&AgentSessions>,
        shutdown: &AtomicBool,
    ) {
        let envelope = match self {
            Self::Routine(job) => Envelope::RoutineOccurrenceCompleted(Box::new(job.run(agents))),
            Self::Decision(job) => Envelope::DecisionWaitCompleted(Box::new(job.run())),
        };
        send_admin_envelope(output, envelope, shutdown);
    }
}
