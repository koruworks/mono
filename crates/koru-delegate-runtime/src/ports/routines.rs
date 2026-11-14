// SPDX-License-Identifier: MPL-2.0

//! Neutral scheduler composition seams; executable authority belongs to the
//! consuming live document, ledger and Bot execution owners.
use super::{PortError, PortResult, ScheduleEventRef};
use koru_delegate_core::{
    BotAdmissionBarrier, BotRoutineCloseReason, FreshBotPolicyProof, HandoffId,
    RoutineClaimIdentity, RoutineClaimRecord, RoutineDigest32, RoutineLinkOutcome, RoutinePrepared,
    RoutineRouteIdentity, RoutineSlot, WorkspaceId,
};
use koru_delegate_schedule::{ScheduleDefinition, ScheduleHeader};
use std::time::Instant;

#[derive(Debug)]
pub enum RoutineBridgeFailure {
    NotSent(PortError),
    Unknown(PortError),
    Refused(PortError),
}
pub type RoutineBridgeResult<T> = Result<T, RoutineBridgeFailure>;

#[derive(Debug)]
pub enum RoutinePreparationObservation {
    Prepared(RoutinePrepared),
    Linked { claim: RoutineClaimIdentity },
    Closed { reason: BotRoutineCloseReason },
    Missing,
}
#[derive(Debug)]
pub enum RoutinePreparedReply {
    Prepared(RoutinePrepared),
    Closed { reason: BotRoutineCloseReason },
}

/// The host must correlate the existing strict envelope before projecting a reply.
/// None/Missing are authenticated positive observations, never transport fallback.
pub trait RoutineBridge: Send {
    fn stopped(&self) -> bool;
    fn prepare(
        &mut self,
        slot: &RoutineSlot,
        workspace: WorkspaceId,
        definition: &ScheduleDefinition,
        owner_handoff: Option<HandoffId>,
        deadline: Instant,
    ) -> RoutineBridgeResult<RoutinePreparedReply>;
    fn read_preparation(
        &mut self,
        slot: &RoutineSlot,
        semantic_hash: RoutineDigest32,
        deadline: Instant,
    ) -> RoutineBridgeResult<RoutinePreparationObservation>;
    fn synchronize_preclaim(
        &mut self,
        prepared: &RoutinePrepared,
        deadline: Instant,
    ) -> RoutineBridgeResult<(BotAdmissionBarrier, FreshBotPolicyProof)>;
    fn finalize(
        &mut self,
        claim: &RoutineClaimIdentity,
        deadline: Instant,
    ) -> RoutineBridgeResult<RoutineLinkOutcome>;
    fn read_link(
        &mut self,
        claim: &RoutineClaimIdentity,
        deadline: Instant,
    ) -> RoutineBridgeResult<Option<RoutineLinkOutcome>>;
}

/// The same host event-authority gate, not a new execution authority or lock.
pub trait SchedulePhaseGuard {}
pub trait SchedulePhaseGate: Send {
    fn enter_until(&self, deadline: Instant) -> PortResult<Box<dyn SchedulePhaseGuard>>;
}

/// Owned correlation facts only; cannot carry a live document token or lease.
pub struct RoutineWork {
    pub evaluation_serial: u64,
    pub candidate: RoutineCandidate,
}
pub enum RoutineCandidate {
    Due {
        route: RoutineRouteIdentity,
        location: ScheduleEventRef,
        header: ScheduleHeader,
        owner_handoff: Option<HandoffId>,
        slot: RoutineSlot,
        semantic_hash: RoutineDigest32,
        representative_miss: Option<i64>,
    },
    Recover {
        route: RoutineRouteIdentity,
        record: RoutineClaimRecord,
    },
}
impl std::fmt::Debug for RoutineWork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineWork([REDACTED])")
    }
}
impl std::fmt::Debug for RoutineCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineCandidate([REDACTED])")
    }
}
