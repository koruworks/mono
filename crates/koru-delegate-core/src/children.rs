// SPDX-License-Identifier: MPL-2.0

//! Neutral records for one Bot Run asking another Bot and waiting for its
//! result (M6). Shape, identity and encoding only.
//!
//! Nothing here admits a Run, opens a wait, releases capacity or reads a
//! page. The ledger checks retained rows and the live fence in its own
//! transaction; the AI owner checks who may be asked. No type in this module
//! holds task text, a title, a path or page content: a request is named by
//! ids, digests, closed tokens, numbers and times.

use crate::decisions::{
    BotWaitCleanupCause, BotWaitId, BotWaitPhase, BotWaitRefusal, ConsumerCleanup,
    DECISION_EVIDENCE_BYTES, DecisionCleanupTarget, DecisionClosureReason, DecisionConsumer,
    DecisionDigest, DecisionError, DecisionFence, DecisionResumeState, DecisionRevision,
    DecisionValidate, DecisionVersionV1, FreshWaitStopEligibility, OriginWaitStop,
    encode_decision_value,
};
use crate::{
    ActorRef, AgentTurnId, AttemptGeneration, BotAdmissionState, BotExecutionFence, BotId,
    BotOwner, BotRunKey, BotSubmission, CapturedBotPolicy, ChildParent, ChildRequestId, DocumentId,
    IdempotencyKey, ProfileId, RoutineProvenance, RunAttemptId, RunId, SessionId, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod binding;
mod enrollment;
mod receipt;
pub use binding::*;
pub use enrollment::*;
pub use receipt::*;

/// How long one request may be waited for, in counted host time.
pub const BOT_CHILD_WAIT_MS: u64 = 900_000;
/// The active budget a parent must still have when it asks, so that its
/// successor is not resumed with nothing left.
pub const BOT_CHILD_RESUME_FLOOR_MS: u64 = 120_000;
/// Child requests per parent Run, for good; also the most one ask may hold.
pub const CHILD_REQUEST_LIMIT: usize = 3;
/// Pages one request may name.
pub const CHILD_DOCUMENT_LIMIT: usize = 16;
/// The canonical binding's byte bound; the ledger column holds the same.
pub const CHILD_BINDING_BYTES: usize = 65_536;

fn require(value: bool) -> Result<(), DecisionError> {
    value.then_some(()).ok_or(DecisionError::Invalid)
}
fn v4(id: Uuid) -> bool {
    id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122
}
fn generation<'de, D: serde::Deserializer<'de>>(d: D) -> Result<AttemptGeneration, D::Error> {
    AttemptGeneration::from_raw(u32::deserialize(d)?)
        .ok_or_else(|| serde::de::Error::custom(DecisionError::Invalid))
}

#[cfg(test)]
mod tests;
