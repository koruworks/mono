// SPDX-License-Identifier: MPL-2.0

//! Sole owner of `bot_private_effects` (runs.db 11, M5): the intent and the
//! receipt of every private operation, with or without a decision.
//!
//! A row is written before a vendor is called and closed after. It holds
//! identifiers, digests, codes and sizes; no account label, item title,
//! vendor reference, link, secret or content has a column, and none appears
//! in an error. The rules this module keeps:
//!
//! - **One execution license.** `Execute` is answered once, on the commit
//!   that created the row. Every replay is `ReconcileOnly`.
//! - **Final is final.** `confirmed`, `refused` and `unknown` admit no update,
//!   not even one that changes nothing. Nothing leads back to `prepared`, and
//!   an `unknown` effect is never resent.
//! - **Only the row's dispatcher advances it.** Another process's open rows
//!   are closed by resolution, in a transaction of its own, before the command
//!   that carries the new dispatcher id does anything else.
//! - **Both barriers are bound.** The Bot's policy barrier, and for a Routine
//!   Run the Routine's, are on the row. The dispatch mark commits only while
//!   both are still the latest and allow effects.
//!
//! The decision half (prepare, consume, settlement, cleanup evidence) lives
//! with the decision owner in `decisions/private.rs`; it calls the row
//! primitives here inside its own transaction.

use crate::{BotAdmissionRecord, BotLedgerError, LedgerError, RunsDb, WriteOp};
use koru_delegate_core::{decisions::*, *};
use rusqlite::{Connection, OptionalExtension, params};

mod rows;
pub(crate) use rows::{by_decision_on, insert_prepared_on, open_in_run_on, read_on};
#[cfg(test)]
pub(crate) mod tests;

/// Most rows one resolution closes. The two-Run host bound makes more than a
/// handful impossible; a later command closes any remainder.
pub const PRIVATE_RESOLUTION_LIMIT: usize = 64;

/// Refusals of the private-operation commands. Every variant other than
/// `Ledger(Ledger(..))` means the command's own transaction committed nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum PrivateLedgerError {
    Ledger(BotLedgerError),
    /// The captured and the current policy both hold the grant: no question
    /// is needed.
    ConnectionGranted,
    /// The captured or the current policy lacks the grant.
    ConnectionNotGranted,
    /// The Run still has an open effect of the caller's own dispatcher that
    /// is not this operation.
    EffectOpen,
    /// The transition is not legal from the retained stage and is not an
    /// exact replay, or the row belongs to another dispatcher.
    EffectStale,
    /// Another decision or a cleanup wait owns the Run.
    DecisionOwned,
    /// A Routine Run never has a private decision.
    RoutineDecisionUnsupported,
}
impl PrivateLedgerError {
    /// The wire code of a refusal that has one of its own. The other
    /// refusals are the existing ledger ones and keep their existing mapping.
    pub fn code(&self) -> Option<&'static str> {
        Some(match self {
            Self::Ledger(_) => return None,
            Self::ConnectionGranted => "bot_connection_granted",
            Self::ConnectionNotGranted => "bot_connection_not_granted",
            Self::EffectOpen => "bot_private_effect_open",
            Self::EffectStale => "bot_private_effect_stale",
            Self::DecisionOwned => "bot_decision_owned",
            Self::RoutineDecisionUnsupported => "bot_routine_decision_unsupported",
        })
    }
}
impl From<BotLedgerError> for PrivateLedgerError {
    fn from(error: BotLedgerError) -> Self {
        Self::Ledger(error)
    }
}
impl From<LedgerError> for PrivateLedgerError {
    fn from(error: LedgerError) -> Self {
        Self::Ledger(error.into())
    }
}
impl std::fmt::Display for PrivateLedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Fixed text: no id, digest or stored value is interpolated.
        match self {
            Self::Ledger(error) => error.fmt(f),
            Self::ConnectionGranted => f.write_str("the connection is already granted"),
            Self::ConnectionNotGranted => f.write_str("the connection is not granted"),
            Self::EffectOpen => f.write_str("another private operation is open for this Run"),
            Self::EffectStale => f.write_str("stale private effect transition"),
            Self::DecisionOwned => f.write_str("a decision owns this Run"),
            Self::RoutineDecisionUnsupported => {
                f.write_str("a Routine Run cannot ask a private question")
            }
        }
    }
}
impl std::error::Error for PrivateLedgerError {}

/// The Routine barrier an effect of a Routine Run is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateRoutineFence {
    pub schedule: ScheduleId,
    pub request: NativePolicyRequestId,
    pub grant_revision: u64,
    pub digest: DecisionDigest,
}

/// One retained row, complete. History, not permission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateEffectRecord {
    pub owner: BotOwner,
    pub bot: BotId,
    pub key: PrivateEffectKey,
    pub dispatcher: PrivateDispatcherId,
    pub decision: Option<DecisionId>,
    pub binding_digest: Option<DecisionDigest>,
    pub operation: PrivateOperationProposal,
    pub barrier: NativePolicyRequestId,
    pub config_revision: u64,
    pub policy_revision: u64,
    pub policy_digest: [u8; 32],
    pub routine: Option<PrivateRoutineFence>,
    pub stage: PrivateEffectStage,
    pub refusal: Option<PrivateEffectRefusal>,
    pub outcome_bytes: Option<u32>,
    pub withheld: Option<bool>,
    pub dispatched_at_ms: Option<i64>,
    pub resolved_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
impl PrivateEffectRecord {
    pub fn view(&self, disposition: PrivateEffectDisposition, deliver: bool) -> PrivateEffectView {
        PrivateEffectView {
            key: self.key.clone(),
            dispatcher: self.dispatcher,
            decision: self.decision,
            operation: self.operation.clone(),
            config_revision: self.config_revision,
            policy_revision: self.policy_revision,
            routine: self.routine.as_ref().map(|r| PrivateRoutineBinding {
                schedule: r.schedule,
                grant_revision: r.grant_revision,
            }),
            stage: self.stage,
            refusal: self.refusal,
            outcome_bytes: self.outcome_bytes,
            withheld: self.withheld,
            disposition,
            deliver,
        }
    }

    /// The outcome a consumed decision's settlement or cleanup names for
    /// this row; `None` while the row is open and its dispatcher owns it.
    pub fn retained_effect(&self) -> Option<DecisionRetainedEffect> {
        let key = self.key.clone();
        match self.stage {
            PrivateEffectStage::Prepared | PrivateEffectStage::Dispatched => None,
            PrivateEffectStage::Confirmed => Some(DecisionRetainedEffect::PrivateConfirmed {
                key,
                outcome_bytes: self.outcome_bytes?,
                withheld: self.withheld?,
            }),
            PrivateEffectStage::Refused => Some(DecisionRetainedEffect::PrivateNotApplied {
                key,
                code: self.refusal?,
            }),
            PrivateEffectStage::Unknown => Some(DecisionRetainedEffect::PrivateUnknown { key }),
        }
    }
}

/// The answer to a command about one effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateEffectOutcome {
    pub record: PrivateEffectRecord,
    pub disposition: PrivateEffectDisposition,
    pub deliver: bool,
}
impl PrivateEffectOutcome {
    pub fn view(&self) -> PrivateEffectView {
        self.record.view(self.disposition, self.deliver)
    }
    fn reconcile(record: PrivateEffectRecord) -> Self {
        Self {
            record,
            disposition: PrivateEffectDisposition::ReconcileOnly,
            deliver: false,
        }
    }
    pub(crate) fn execute(record: PrivateEffectRecord) -> Self {
        Self {
            record,
            disposition: PrivateEffectDisposition::Execute,
            deliver: false,
        }
    }
    pub(crate) fn replay(record: PrivateEffectRecord) -> Self {
        Self::reconcile(record)
    }
}

/// What a command that carries a dispatcher id did. `resolved` are the rows
/// of **other** dispatcher instances this call closed first, in a committed
/// transaction of its own: they stay closed even when `outcome` is a refusal.
/// The caller runs the decision settle hook for each resolved row that names
/// a decision, exactly as after an advance.
#[derive(Debug)]
pub struct PrivateResolved<T> {
    pub resolved: Vec<PrivateEffectRecord>,
    pub outcome: Result<T, PrivateLedgerError>,
}

fn sql(error: rusqlite::Error) -> BotLedgerError {
    LedgerError::sql("private effect ledger", error).into()
}
fn corrupt(column: &'static str) -> BotLedgerError {
    LedgerError::Corrupt {
        table: "bot_private_effects",
        column,
    }
    .into()
}

/// The Run's immutable owner and Bot, from its admission row. A Run this
/// ledger never admitted is fenced.
pub(crate) fn subject_on(
    conn: &Connection,
    run: RunId,
) -> Result<(BotOwner, BotId), BotLedgerError> {
    let row = conn
        .query_row(
            "SELECT CASE WHEN typeof(user_scope)='text' AND length(CAST(user_scope AS BLOB))=36 THEN user_scope END,
                    CASE WHEN typeof(device_id)='text' AND length(CAST(device_id AS BLOB))=36 THEN device_id END,
                    CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END
             FROM bot_admissions WHERE run_id=?1",
            [run.to_storage()],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()
        .map_err(sql)?;
    let Some((Some(user), Some(device), Some(bot))) = row else {
        return Err(BotLedgerError::Fenced);
    };
    let owner: BotOwner =
        serde_json::from_value(serde_json::json!({"user_scope":user,"device":device}))
            .map_err(|_| BotLedgerError::Fenced)?;
    let bot = bot.parse().map_err(|_| BotLedgerError::Fenced)?;
    Ok((owner, bot))
}

/// Whether this operation id already names another kind of operation in the
/// Run: a page intent, a Memory allocation or a private effect. A decision
/// under the same id is compared by its owner, which knows the proposal.
pub(crate) fn operation_id_taken_on(
    conn: &Connection,
    run: RunId,
    operation: uuid::Uuid,
) -> Result<bool, BotLedgerError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_result_intents WHERE run_id=?1 AND operation_id=?2)
             OR EXISTS(SELECT 1 FROM bot_memory_edit_allocations WHERE run_id=?1 AND operation_id=?2)
             OR EXISTS(SELECT 1 FROM bot_private_effects WHERE run_id=?1 AND operation_id=?2)",
        params![run.to_storage(), operation.to_string()],
        |r| r.get(0),
    )
    .map_err(sql)
}

/// The mirror check of the page path: a private effect under this id.
pub(crate) fn exists_on(
    conn: &Connection,
    run: RunId,
    operation: uuid::Uuid,
) -> Result<bool, BotLedgerError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bot_private_effects WHERE run_id=?1 AND operation_id=?2)",
        params![run.to_storage(), operation.to_string()],
        |r| r.get(0),
    )
    .map_err(sql)
}

/// Whether both barriers the row is bound to are still the latest ones, in
/// force, and allowing effects. A newer barrier (a removed grant bumps the
/// Bot's policy revision or the Routine's grant revision), a pending one or
/// an effect-forbidding one all answer `false`.
pub(crate) fn barriers_current_on(
    conn: &Connection,
    record: &PrivateEffectRecord,
) -> Result<bool, BotLedgerError> {
    let global = crate::native_results::current_policy(conn, record.owner, record.bot)?;
    let global_ok = global.is_some_and(|current| {
        current.key.request == record.barrier
            && current.config_revision == record.config_revision
            && current.policy_revision == record.policy_revision
            && current.payload_digest == record.policy_digest
            && current.state == BotPolicyBarrierState::Effective
            && current.effects_allowed
    });
    if !global_ok {
        return Ok(false);
    }
    let Some(routine) = &record.routine else {
        return Ok(true);
    };
    let current =
        crate::bots::routines::current_policy(conn, record.owner, record.bot, routine.schedule)?;
    Ok(current.is_some_and(|current| {
        current.request == routine.request
            && current.grant_revision == routine.grant_revision
            && current.payload_digest == routine.digest
            && current.state == BotPolicyBarrierState::Effective
            && current.effects_allowed
    }))
}

/// Resolution: close the open rows of this owner that belong to **another**
/// dispatcher instance. `prepared` becomes `refused/not_dispatched` (the
/// vendor was never called); `dispatched` becomes `unknown` (it may have
/// been). A row of the caller's own instance is never touched.
pub(crate) fn resolve_others_on(
    conn: &Connection,
    owner: BotOwner,
    dispatcher: PrivateDispatcherId,
    now: i64,
) -> Result<Vec<PrivateEffectRecord>, BotLedgerError> {
    let keys = rows::open_of_others(conn, owner, dispatcher, PRIVATE_RESOLUTION_LIMIT)?;
    let mut resolved = Vec::with_capacity(keys.len());
    for (run, operation) in keys {
        let record = read_on(conn, run, operation)?.ok_or_else(|| corrupt("open_index"))?;
        let changed = match record.stage {
            PrivateEffectStage::Prepared => conn.execute(
                "UPDATE bot_private_effects SET stage='refused',refusal='not_dispatched',resolved_at_ms=?3,updated_at_ms=?3
                 WHERE run_id=?1 AND operation_id=?2 AND stage='prepared'",
                params![run.to_storage(), operation.to_string(), now.max(record.updated_at_ms)],
            ),
            PrivateEffectStage::Dispatched => conn.execute(
                "UPDATE bot_private_effects SET stage='unknown',resolved_at_ms=?3,updated_at_ms=?3
                 WHERE run_id=?1 AND operation_id=?2 AND stage='dispatched'",
                params![run.to_storage(), operation.to_string(), now.max(record.updated_at_ms)],
            ),
            _ => return Err(corrupt("open_index")),
        }
        .map_err(sql)?;
        if changed != 1 {
            return Err(BotLedgerError::Stale);
        }
        resolved.push(read_on(conn, run, operation)?.ok_or_else(|| corrupt("resolved"))?);
    }
    Ok(resolved)
}

/// The granted path: an intent without a decision.
fn prepare_granted_on(
    conn: &Connection,
    admitted: &BotAdmissionRecord,
    dispatcher: PrivateDispatcherId,
    proposal: &PrivateOperationProposal,
    fence: &BotExecutionFence,
    proof: &FreshBotPolicyProof,
    now: i64,
) -> Result<PrivateEffectOutcome, PrivateLedgerError> {
    let provenance = &admitted.submission.provenance;
    let run = admitted.submission.request.run;
    if fence.owner != provenance.owner || fence.bot != provenance.bot || fence.run != run {
        return Err(BotLedgerError::Fenced.into());
    }
    // Replay first. The retained row answers before any fresh check, and it
    // never hands out a second license.
    if let Some(old) = read_on(conn, run, proposal.operation_id)? {
        if old.decision.is_some() || old.operation != *proposal {
            return Err(BotLedgerError::Conflict.into());
        }
        return Ok(PrivateEffectOutcome::replay(old));
    }
    let Some(turn) = fence.turn else {
        return Err(BotLedgerError::Stale.into());
    };
    // A decision or a cleanup wait owns the Run: only its exact retry passes.
    let owned: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM bot_decisions WHERE run_id=?1 AND phase<>'settled')
                 OR EXISTS(SELECT 1 FROM bot_wait_runtime WHERE run_id=?1 AND phase<>'settled')",
            [run.to_storage()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if owned {
        return Err(PrivateLedgerError::DecisionOwned);
    }
    // One operation id, one meaning.
    let decided: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM bot_decisions WHERE run_id=?1 AND operation_id=?2)",
            params![run.to_storage(), proposal.operation_id.to_string()],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if decided || operation_id_taken_on(conn, run, proposal.operation_id)? {
        return Err(BotLedgerError::Conflict.into());
    }
    if open_in_run_on(conn, run)?.is_some() {
        return Err(PrivateLedgerError::EffectOpen);
    }
    let key = NativeResultKey {
        run,
        attempt: fence.attempt,
        generation: fence.generation,
        turn,
        operation_id: proposal.operation_id,
    };
    // The page path's own owner: the live attempt and turn, the Routine link,
    // and both current barriers, in this transaction.
    let (global, routine) =
        crate::native_results::fresh_policy_on(conn, &key, admitted, fence, proof)?;
    // The grant, captured and current. For a Routine Run both are the
    // Routine's own grant, never the Bot's ordinary-chat access.
    let grant = proposal.grant();
    let current = proof
        .routine
        .as_ref()
        .map_or(&proof.global.policy, |routine| &routine.policy);
    if !admitted.submission.policy.grants_connection(&grant) || !current.grants_connection(&grant) {
        return Err(PrivateLedgerError::ConnectionNotGranted);
    }
    let record = PrivateEffectRecord {
        owner: provenance.owner,
        bot: provenance.bot,
        key: PrivateEffectKey {
            run,
            operation_id: proposal.operation_id,
            attempt: fence.attempt,
            generation: fence.generation,
            turn,
        },
        dispatcher,
        decision: None,
        binding_digest: None,
        operation: proposal.clone(),
        barrier: global.key.request,
        config_revision: global.config_revision,
        policy_revision: global.policy_revision,
        policy_digest: global.payload_digest,
        routine: routine.map(|r| PrivateRoutineFence {
            schedule: r.schedule,
            request: r.request,
            grant_revision: r.grant_revision,
            digest: r.payload_digest,
        }),
        stage: PrivateEffectStage::Prepared,
        refusal: None,
        outcome_bytes: None,
        withheld: None,
        dispatched_at_ms: None,
        resolved_at_ms: None,
        created_at_ms: now,
        updated_at_ms: now,
    };
    insert_prepared_on(conn, &record)?;
    let record = read_on(conn, run, proposal.operation_id)?.ok_or_else(|| corrupt("inserted"))?;
    Ok(PrivateEffectOutcome::execute(record))
}

/// Whether `transition` is exactly what the row already records.
fn is_replay(record: &PrivateEffectRecord, transition: &PrivateEffectTransition) -> bool {
    match (record.stage, transition) {
        (PrivateEffectStage::Dispatched, PrivateEffectTransition::Dispatched {}) => true,
        // The dispatch mark that met a moved barrier committed this refusal
        // and said so in its reply; its lost reply replays as that refusal.
        (PrivateEffectStage::Refused, PrivateEffectTransition::Dispatched {}) => {
            record.refusal == Some(PrivateEffectRefusal::AccessDenied)
                && record.dispatched_at_ms.is_none()
        }
        (
            PrivateEffectStage::Confirmed,
            PrivateEffectTransition::Confirmed {
                outcome_bytes,
                withheld,
            },
        ) => record.outcome_bytes == Some(*outcome_bytes) && record.withheld == Some(*withheld),
        (PrivateEffectStage::Refused, PrivateEffectTransition::Refused { code }) => {
            record.refusal == Some(*code)
        }
        (PrivateEffectStage::Unknown, PrivateEffectTransition::Unknown {}) => true,
        _ => false,
    }
}

fn advance_on(
    conn: &Connection,
    key: &PrivateEffectKey,
    dispatcher: PrivateDispatcherId,
    transition: &PrivateEffectTransition,
    now: i64,
) -> Result<PrivateEffectOutcome, PrivateLedgerError> {
    let Some(record) = read_on(conn, key.run, key.operation_id)? else {
        return Err(PrivateLedgerError::EffectStale);
    };
    if record.key != *key {
        return Err(BotLedgerError::Fenced.into());
    }
    // Resolution first has already closed a row of another instance.
    if record.dispatcher != dispatcher {
        return Err(PrivateLedgerError::EffectStale);
    }
    if is_replay(&record, transition) {
        // A replay never delivers: the one `deliver` went with the commit.
        return Ok(PrivateEffectOutcome::replay(record));
    }
    let run = key.run.to_storage();
    let operation = key.operation_id.to_string();
    let at = now.max(record.updated_at_ms);
    let mut deliver = false;
    let changed = match (record.stage, transition) {
        (PrivateEffectStage::Prepared, PrivateEffectTransition::Dispatched {}) => {
            if barriers_current_on(conn, &record)? {
                conn.execute(
                    "UPDATE bot_private_effects SET stage='dispatched',dispatched_at_ms=?3,updated_at_ms=?3
                     WHERE run_id=?1 AND operation_id=?2 AND stage='prepared'",
                    params![run, operation, at],
                )
            } else {
                // A fenced dispatch mark is a reply, not a failure: the row
                // is closed and the vendor is never called.
                conn.execute(
                    "UPDATE bot_private_effects SET stage='refused',refusal='access_denied',resolved_at_ms=?3,updated_at_ms=?3
                     WHERE run_id=?1 AND operation_id=?2 AND stage='prepared'",
                    params![run, operation, at],
                )
            }
        }
        (PrivateEffectStage::Prepared, PrivateEffectTransition::Refused { code }) => conn.execute(
            "UPDATE bot_private_effects SET stage='refused',refusal=?4,resolved_at_ms=?3,updated_at_ms=?3
             WHERE run_id=?1 AND operation_id=?2 AND stage='prepared'",
            params![run, operation, at, rows::token(code)?],
        ),
        (
            PrivateEffectStage::Dispatched,
            PrivateEffectTransition::Confirmed {
                outcome_bytes,
                withheld,
            },
        ) => {
            // Content may reach the model only while both bound barriers
            // still stand; a read that completed after a revocation is
            // recorded and its content dropped.
            deliver = !*withheld && barriers_current_on(conn, &record)?;
            conn.execute(
                "UPDATE bot_private_effects SET stage='confirmed',outcome_bytes=?4,withheld=?5,resolved_at_ms=?3,updated_at_ms=?3
                 WHERE run_id=?1 AND operation_id=?2 AND stage='dispatched'",
                params![run, operation, at, outcome_bytes, withheld],
            )
        }
        (PrivateEffectStage::Dispatched, PrivateEffectTransition::Refused { code }) => conn
            .execute(
                "UPDATE bot_private_effects SET stage='refused',refusal=?4,resolved_at_ms=?3,updated_at_ms=?3
                 WHERE run_id=?1 AND operation_id=?2 AND stage='dispatched'",
                params![run, operation, at, rows::token(code)?],
            ),
        (PrivateEffectStage::Dispatched, PrivateEffectTransition::Unknown {}) => conn.execute(
            "UPDATE bot_private_effects SET stage='unknown',resolved_at_ms=?3,updated_at_ms=?3
             WHERE run_id=?1 AND operation_id=?2 AND stage='dispatched'",
            params![run, operation, at],
        ),
        // Not legal from the retained stage: a final row, `prepared` to
        // `confirmed` or `unknown`, or `dispatched` to `dispatched` handled
        // above as a replay.
        _ => return Err(PrivateLedgerError::EffectStale),
    }
    .map_err(sql)?;
    if changed != 1 {
        return Err(BotLedgerError::Stale.into());
    }
    let record = read_on(conn, key.run, key.operation_id)?.ok_or_else(|| corrupt("advanced"))?;
    Ok(PrivateEffectOutcome {
        record,
        disposition: PrivateEffectDisposition::ReconcileOnly,
        deliver,
    })
}

impl RunsDb {
    /// Resolution in a transaction of its own. Private: every public command
    /// below that carries a dispatcher id begins with it.
    pub(crate) fn resolve_private_others(
        &mut self,
        owner: BotOwner,
        dispatcher: PrivateDispatcherId,
    ) -> Result<Vec<PrivateEffectRecord>, PrivateLedgerError> {
        self.write_with_error(WriteOp::ResolvePrivateDispatcher, |tx, now| {
            Ok::<_, PrivateLedgerError>(resolve_others_on(tx, owner, dispatcher, now)?)
        })
    }

    /// `AnnouncePrivateDispatcher`: the AI worker adopted this owner under
    /// this process's dispatcher id. Closes what a dead instance left for the
    /// owner and answers those rows; another owner's rows are untouched.
    /// Idempotent: a second announce finds nothing.
    pub fn announce_private_dispatcher(
        &mut self,
        owner: BotOwner,
        dispatcher: PrivateDispatcherId,
    ) -> Result<Vec<PrivateEffectRecord>, PrivateLedgerError> {
        self.resolve_private_others(owner, dispatcher)
    }

    /// `PreparePrivateEffect`: the intent of a granted read, with no
    /// decision. The first fresh commit answers the one `Execute`.
    pub fn prepare_private_effect(
        &mut self,
        dispatcher: PrivateDispatcherId,
        proposal: &PrivateOperationProposal,
        fence: &BotExecutionFence,
        proof: &FreshBotPolicyProof,
    ) -> Result<PrivateResolved<PrivateEffectOutcome>, PrivateLedgerError> {
        let resolved = self.resolve_private_others(fence.owner, dispatcher)?;
        let outcome = (|| {
            if !proposal.valid() {
                return Err(BotLedgerError::Invalid.into());
            }
            // Immutable capture, decoded through its existing owner.
            let admitted = crate::bots::routines::native_admission(self.conn(), fence.run)?;
            self.write_with_error(WriteOp::PreparePrivateEffect, |tx, now| {
                prepare_granted_on(tx, &admitted, dispatcher, proposal, fence, proof, now)
            })
        })();
        Ok(PrivateResolved { resolved, outcome })
    }

    /// `AdvancePrivateEffect`: the row's own dispatcher records what it knows.
    /// A dispatch mark that meets a moved barrier commits
    /// `refused/access_denied` and answers it as a reply.
    pub fn advance_private_effect(
        &mut self,
        key: &PrivateEffectKey,
        dispatcher: PrivateDispatcherId,
        transition: &PrivateEffectTransition,
    ) -> Result<PrivateResolved<PrivateEffectOutcome>, PrivateLedgerError> {
        let (owner, _) = subject_on(self.conn(), key.run)?;
        let resolved = self.resolve_private_others(owner, dispatcher)?;
        let outcome = self.write_with_error(WriteOp::AdvancePrivateEffect, |tx, now| {
            advance_on(tx, key, dispatcher, transition, now)
        });
        Ok(PrivateResolved { resolved, outcome })
    }

    /// `ReadPrivateEffect`: the retained row, or `None`. It carries a
    /// dispatcher id, so it resolves first; the read itself changes nothing
    /// and never answers `Execute` or `deliver`.
    pub fn read_private_effect(
        &mut self,
        run: RunId,
        operation: uuid::Uuid,
        dispatcher: PrivateDispatcherId,
    ) -> Result<PrivateResolved<Option<PrivateEffectOutcome>>, PrivateLedgerError> {
        let (owner, _) = subject_on(self.conn(), run)?;
        let resolved = self.resolve_private_others(owner, dispatcher)?;
        let outcome = read_on(self.conn(), run, operation)
            .map(|record| record.map(PrivateEffectOutcome::replay))
            .map_err(PrivateLedgerError::from);
        Ok(PrivateResolved { resolved, outcome })
    }

    /// The retained row alone, without a dispatcher and without resolution.
    /// For the decision owner's settle hook and for inspection.
    pub fn private_effect(
        &self,
        run: RunId,
        operation: uuid::Uuid,
    ) -> Result<Option<PrivateEffectRecord>, BotLedgerError> {
        read_on(self.conn(), run, operation)
    }
}
