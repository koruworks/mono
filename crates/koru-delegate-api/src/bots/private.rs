// SPDX-License-Identifier: MPL-2.0

//! Delegate IPC 18: private operations (M5). Their decision, their one-use
//! consume, their effect intent and receipt, and the dispatcher's announce.
//!
//! All six commands and both replies ride the authenticated private Bot
//! channel. None is a public frame or a model tool. Every value is an
//! identifier, a digest, a closed token, a revision or a size: no account
//! label, item title, vendor reference, link, secret or content that was read
//! can be spelled on this wire. The neutral types live in
//! `koru-delegate-core` (the ledger and the decision binding need them); this
//! module re-exports them, adds the strict wire spelling of the proposal, the
//! 16 KiB bound and the fixed failure texts of the new refusal codes.
use koru_delegate_core::decisions::{DecisionDigest, DecisionRevision};
pub use koru_delegate_core::{
    ConnectionGrantRef, ConnectionId, ConnectionItemId, PrivateDispatcherId,
    PrivateEffectDisposition, PrivateEffectKey, PrivateEffectRefusal, PrivateEffectStage,
    PrivateEffectTransition, PrivateEffectView, PrivateOperationKind, PrivateOperationProposal,
    PrivateRoutineBinding,
};
use serde::{Deserialize, Serialize};

/// The encoded bound of a private command or reply that carries neither a
/// `FreshBotPolicyProof` nor a `DecisionReceiptView`: the announce, the
/// advance, the read, and both replies. The three commands that carry a proof
/// keep the existing frame bound.
pub const PRIVATE_WIRE_LIMIT: usize = 16 * 1024;

/// The largest request frame body that may carry one of the bounded private
/// commands: the 16 KiB command, the frame tag and the bootstrap authority at
/// its longest escaped spelling.
pub const PRIVATE_FRAME_LIMIT: usize = PRIVATE_WIRE_LIMIT + 2048;

/// Most rows one resolution closes; a `PrivateDispatcher` reply never names
/// more.
pub const PRIVATE_RESOLVED_LIMIT: u16 = 64;

/// The strict wire spelling of a proposal: unknown fields refused and the
/// operation id canonical hyphenated v4 text, as `DecisionEditProposal`.
#[derive(Serialize, Deserialize)]
#[serde(remote = "PrivateOperationProposal", deny_unknown_fields)]
pub(super) struct StrictPrivateProposal {
    #[serde(deserialize_with = "super::decisions::operation_uuid")]
    operation_id: uuid::Uuid,
    connection: ConnectionId,
    connection_revision: DecisionRevision,
    account: DecisionDigest,
    item: ConnectionItemId,
    item_ref: DecisionDigest,
    operation: PrivateOperationKind,
    request_fingerprint: [u8; 32],
}

/// The strict wire spelling of an effect key: the operation id canonical
/// hyphenated v4 text.
#[derive(Serialize, Deserialize)]
#[serde(remote = "PrivateEffectKey", deny_unknown_fields)]
pub(super) struct StrictPrivateEffectKey {
    run: koru_delegate_core::RunId,
    #[serde(deserialize_with = "super::decisions::operation_uuid")]
    operation_id: uuid::Uuid,
    attempt: koru_delegate_core::RunAttemptId,
    generation: koru_delegate_core::AttemptGeneration,
    turn: koru_delegate_core::AgentTurnId,
}

pub(super) fn within_wire_limit<T: Serialize + ?Sized>(value: &T) -> bool {
    // Counted while encoding: nothing over the bound is ever buffered.
    struct Bounded(usize);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            self.0 -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Bounded(PRIVATE_WIRE_LIMIT), value).is_ok()
}

fn v4(id: &uuid::Uuid) -> bool {
    id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122
}

pub(super) fn key_valid(key: &PrivateEffectKey) -> bool {
    v4(&key.operation_id)
}

pub(super) fn operation_valid(id: &uuid::Uuid) -> bool {
    v4(id)
}

/// Whether a reported effect is one the ledger could hold: the stage's own
/// columns and no others, the one license only on a fresh `prepared` row, and
/// delivery only for a confirmed read that was not withheld.
pub(super) fn view_valid(effect: &PrivateEffectView) -> bool {
    let revisions = |n: u64| n > 0 && n <= i64::MAX as u64;
    let stage = match effect.stage {
        PrivateEffectStage::Refused => {
            effect.refusal.is_some() && effect.outcome_bytes.is_none() && effect.withheld.is_none()
        }
        PrivateEffectStage::Confirmed => {
            effect.refusal.is_none() && effect.outcome_bytes.is_some() && effect.withheld.is_some()
        }
        PrivateEffectStage::Prepared
        | PrivateEffectStage::Dispatched
        | PrivateEffectStage::Unknown => {
            effect.refusal.is_none() && effect.outcome_bytes.is_none() && effect.withheld.is_none()
        }
    };
    stage
        && key_valid(&effect.key)
        && effect.operation.valid()
        && effect.operation.operation_id == effect.key.operation_id
        && revisions(effect.config_revision)
        && revisions(effect.policy_revision)
        && effect
            .routine
            .is_none_or(|routine| revisions(routine.grant_revision))
        // D6: a Routine Run never has a private decision.
        && !(effect.routine.is_some() && effect.decision.is_some())
        && (effect.disposition != PrivateEffectDisposition::Execute
            || effect.stage == PrivateEffectStage::Prepared)
        && (!effect.deliver
            || (effect.stage == PrivateEffectStage::Confirmed
                && effect.withheld == Some(false)
                && effect.disposition == PrivateEffectDisposition::ReconcileOnly))
}

/// Whether a raw request body names a bounded private command and is over its
/// bound. Runs before the frame is decoded, like
/// [`super::routine_control_frame_oversized`]: the probe borrows the one tag
/// it reads and skips everything else, so nothing oversized is allocated.
pub fn private_frame_oversized(body: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct Frame<'a> {
        #[serde(borrow, default)]
        command: Option<Command<'a>>,
    }
    #[derive(Deserialize)]
    struct Command<'a> {
        #[serde(borrow, default)]
        command: Option<&'a str>,
    }
    body.len() > PRIVATE_FRAME_LIMIT
        && serde_json::from_slice::<Frame<'_>>(body).is_ok_and(|frame| {
            matches!(
                frame.command.and_then(|command| command.command),
                Some(
                    "announce_private_dispatcher"
                        | "advance_private_effect"
                        | "read_private_effect"
                )
            )
        })
}

/// The refusal codes IPC 18 adds, each with its one fixed English text. No
/// text interpolates a value; none is retryable; each means the command's own
/// transaction committed nothing (resolution first may still have closed
/// another process's orphaned rows).
pub const PRIVATE_FAILURES: &[(&str, &str)] = &[
    (
        "bot_private_unavailable",
        "Private operations are not available in this build.",
    ),
    (
        "bot_connection_granted",
        "This Bot already has access to the connection. No question is needed.",
    ),
    (
        "bot_connection_not_granted",
        "This Bot does not have access to the connection.",
    ),
    (
        "bot_private_effect_open",
        "Another private operation is still open for this Run.",
    ),
    (
        "bot_private_effect_stale",
        "The private operation was already resolved.",
    ),
];

/// The fixed failure of one of the new codes; `None` for every other code.
pub fn private_failure(code: &str) -> Option<crate::Failure> {
    PRIVATE_FAILURES
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(known, message)| crate::Failure::new(*known, *message, false))
}
