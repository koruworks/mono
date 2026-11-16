// SPDX-License-Identifier: MPL-2.0

//! The native companion owns the immutable first consuming key. Its decoder
//! never substitutes the latest attempt, and grants no fresh execution.
use super::*;

const FIELDS: &[(&str, &str, usize, bool)] = &[
    ("run_id", "text", 36, false),
    ("operation_id", "text", 36, false),
    ("decision_id", "text", 36, false),
    ("binding_digest", "blob", 32, false),
    ("consumer_attempt_id", "text", 36, false),
    ("consumer_generation", "integer", 0, false),
    ("consumer_turn_id", "text", 36, false),
    ("consumed_at_ms", "integer", 0, false),
];

pub(super) fn retained_key(
    conn: &Connection,
    receipt: &DecisionReceipt,
    resume: Option<&DecisionResume>,
) -> Result<Option<NativeResultKey>, BotLedgerError> {
    let found = conn
        .query_row(
            &rows::query("native_decision_leases", FIELDS, "decision_id=?1"),
            [receipt.decision.to_storage()],
            |row| {
                let c = rows::Cells {
                    row,
                    table: "native_decision_leases",
                };
                Ok((|| -> Result<_, BotLedgerError> {
                    c.check()?;
                    require(
                        c.id::<DecisionId>("decision_id")? == receipt.decision
                            && c.digest("binding_digest")? == receipt.binding_digest
                            && Some(c.get::<i64>("consumed_at_ms")?) == receipt.consumed_at_ms,
                    )?;
                    Ok(NativeResultKey {
                        run: c.id("run_id")?,
                        operation_id: c.id("operation_id")?,
                        attempt: c.id("consumer_attempt_id")?,
                        generation: c.generation("consumer_generation")?,
                        turn: c.id("consumer_turn_id")?,
                    })
                })())
            },
        )
        .optional()
        .map_err(sql)?
        .transpose()?;
    let DecisionBindingV1::KoruEdit(binding) = &receipt.binding else {
        require(found.is_none())?;
        return Ok(None);
    };
    require(found.is_some() == (receipt.state.authorization == DecisionAuthorization::Consumed))?;
    let Some(key) = found else { return Ok(None) };
    let resume = resume.ok_or_else(|| corrupt("native_decision_leases", "resume"))?;
    let (attempt, generation, _, _) = resume.consumer_identity();
    let acknowledgment = match &resume.state {
        DecisionResumeState::InputAcknowledged { acknowledgment }
        | DecisionResumeState::Closed {
            acknowledgment: Some(acknowledgment),
        } => acknowledgment,
        _ => return Err(corrupt("native_decision_leases", "acknowledgment")),
    };
    require(
        key.run == binding.run
            && key.operation_id == binding.operation_id
            && key.attempt == attempt
            && key.generation == generation
            && key.turn == acknowledgment.turn,
    )?;
    Ok(Some(key))
}

/// The retained outcome final cleanup must name for a consumed Koru edit, read
/// through the companion's immutable first consuming key. `None` means no
/// outcome is owed (not a consumed Koru edit). An unresolved effect is Fenced:
/// it is never reported as NotApplied, and cleanup keeps waiting.
pub(super) fn retained_outcome_on(
    conn: &Connection,
    owner: BotOwner,
    run: RunId,
    decision: DecisionId,
) -> Result<Option<DecisionRetainedEffect>, BotLedgerError> {
    authority::owner(conn, owner, run)?;
    let Some(record) = rows::read(conn, owner, run, decision)? else {
        return Ok(None);
    };
    if record.receipt.state.authorization != DecisionAuthorization::Consumed {
        return Ok(None);
    }
    match record.receipt.state.kind {
        DecisionKind::KoruEdit => {}
        // A consumed private read's outcome is its own effect row; an open
        // row is Fenced, exactly as an unresolved page effect is.
        DecisionKind::PrivateOperation => {
            return super::private::retained_outcome(conn, &record).map(Some);
        }
        DecisionKind::NativeApproval => return Ok(None),
    }
    let key = retained_key(conn, &record.receipt, record.resume.as_ref())?
        .ok_or_else(|| corrupt("native_decision_leases", "consumed"))?;
    let intent = crate::native_results::retained_result_on(conn, &key)?
        .ok_or_else(|| corrupt("native_decision_leases", "intent"))?;
    if intent.lease_state != crate::NativeLeaseState::Settled {
        return Err(BotLedgerError::Fenced);
    }
    let bad = || corrupt("native_result_intents", "outcome");
    Ok(Some(match intent.stage {
        NativeResultStage::Refused => DecisionRetainedEffect::NotApplied {
            code: intent.refusal.ok_or_else(bad)?,
            key,
        },
        NativeResultStage::Verified | NativeResultStage::Registered => {
            DecisionRetainedEffect::Durable {
                reference: intent.verified.ok_or_else(bad)?,
                key,
            }
        }
        NativeResultStage::SettledUnknown => DecisionRetainedEffect::DrainedUnknown {
            drain: intent.drain.ok_or_else(bad)?,
            key,
        },
        _ => return Err(BotLedgerError::Fenced),
    }))
}

/// Whether the Run still has a native effect that blocks final cleanup: a
/// held lease (it may still commit) or verified evidence that still owes its
/// registration. The one exception (O152) is this decision's own consumed
/// effect when its retained outcome is positively Durable: exact verified
/// evidence under the companion's immutable first consuming key, with a
/// settled lease. Final cleanup carries that outcome, the drain already
/// accepts it, and its registration follows once the handle is ordinary
/// again; it is never cleared. Any other held or verified effect, and this
/// one while unresolved, still blocks exactly as before.
pub(super) fn other_effects_pending_on(
    conn: &Connection,
    owner: BotOwner,
    run: RunId,
    decision: DecisionId,
) -> Result<bool, BotLedgerError> {
    let own = match retained_outcome_on(conn, owner, run, decision) {
        Ok(Some(DecisionRetainedEffect::Durable { key, .. })) => Some(key),
        Ok(_) | Err(BotLedgerError::Fenced) => None,
        Err(error) => return Err(error),
    };
    let query = "SELECT EXISTS(SELECT 1 FROM native_result_intents WHERE run_id=?1
        AND (lease_state='held' OR stage='verified')
        AND NOT (?2 IS NOT NULL AND operation_id=?2 AND attempt_id=?3 AND generation=?4
                 AND turn_id=?5 AND lease_state='settled'))";
    let (operation, attempt, generation, turn) = match &own {
        Some(key) => (
            Some(key.operation_id.to_string()),
            Some(key.attempt.to_storage()),
            Some(key.generation.get()),
            Some(key.turn.to_string()),
        ),
        None => (None, None, None, None),
    };
    conn.query_row(
        query,
        params![run.to_storage(), operation, attempt, generation, turn],
        |r| r.get(0),
    )
    .map_err(sql)
}

impl RunsDb {
    /// The exact retained effect outcome a consumed Koru edit's final cleanup
    /// must carry. Read-only; it grants no cleanup or settlement by itself.
    pub fn decision_retained_effect(
        &self,
        owner: BotOwner,
        run: RunId,
        decision: DecisionId,
    ) -> Result<Option<DecisionRetainedEffect>, BotLedgerError> {
        retained_outcome_on(self.conn(), owner, run, decision)
    }

    /// [`other_effects_pending_on`]: whether a native effect other than this
    /// decision's own positively Durable save still blocks final cleanup.
    /// Read-only; it grants no cleanup by itself.
    pub fn decision_other_effects_pending(
        &self,
        owner: BotOwner,
        run: RunId,
        decision: DecisionId,
    ) -> Result<bool, BotLedgerError> {
        other_effects_pending_on(self.conn(), owner, run, decision)
    }
}

/// Runs after the ordinary full-key native decoder and its policy/Memory/
/// Routine checks. This does not recurse into another native intent read.
pub(crate) fn validate_native_on(
    conn: &Connection,
    intent: &crate::NativeIntentRecord,
) -> Result<(), BotLedgerError> {
    let id = rows::operation(conn, intent.key.run, intent.key.operation_id)?;
    let Some(id) = id else {
        let exists:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM native_decision_leases WHERE run_id=?1 AND operation_id=?2)",params![intent.key.run.to_storage(),intent.key.operation_id.to_string()],|r|r.get(0)).map_err(sql)?;
        return require(!exists);
    };
    let lease = intent
        .lease
        .as_ref()
        .ok_or_else(|| corrupt("native_decision_leases", "unleased"))?;
    let record = rows::read(conn, lease.policy.key.owner, intent.key.run, id)?
        .ok_or_else(|| corrupt("native_decision_leases", "decision"))?;
    require(
        retained_key(conn, &record.receipt, record.resume.as_ref())?.as_ref() == Some(&intent.key),
    )?;
    let DecisionBindingV1::KoruEdit(binding) = &record.receipt.binding else {
        return Err(BotLedgerError::Invalid);
    };
    // The lease's Routine policy (request, grant revision, payload digest)
    // equals the binding's `routine_policy`, exactly as the global policy is
    // compared below; an interactive decision's lease carries none.
    let routine = match (&binding.routine, &binding.routine_policy, &lease.routine) {
        (None, None, None) => true,
        (Some(subject), Some(bound), Some(leased)) => {
            leased.owner == bound.owner
                && leased.bot == bound.bot
                && leased.schedule == bound.schedule
                && leased.schedule == subject.provenance.schedule
                && leased.request == bound.request
                && leased.grant_revision == bound.grant_revision.get()
                && leased.payload_digest == bound.digest
        }
        _ => false,
    };
    require(
        matches!(intent.purpose, NativeEffectPurpose::Result {})
            && routine
            && intent.kind == NativeResultOperation::EditPage
            && intent.target == binding.target
            && intent.destination == binding.workspace
            && intent.fingerprint == binding.edit_fingerprint
            && lease.policy.key == binding.global_policy.key
            && lease.policy.config_revision == binding.global_policy.config_revision.get()
            && lease.policy.policy_revision == binding.global_policy.policy_revision.get()
            && &lease.policy.payload_digest == binding.global_policy.digest.as_bytes(),
    )?;
    let revision = intent
        .expected_revision
        .as_deref()
        .ok_or(BotLedgerError::Invalid)?;
    require(
        same_native_edit_revision(revision, binding.expected_revision.as_slice())
            .map_err(|_| BotLedgerError::Invalid)?,
    )
}
