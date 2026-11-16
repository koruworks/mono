// SPDX-License-Identifier: MPL-2.0

//! Transaction bodies only. No callback can opt out of C3's publication write.
use super::*;
use rusqlite::{params_from_iter, types::Value};
mod answer;
mod cleanup;
mod consume;
pub(super) use consume::{consume_on, settle_effect_on};
mod native;
mod phase;
mod resume;
pub use answer::AnswerDecision;
pub(super) use answer::answer_on;
pub(super) use cleanup::validate_cleanup_on;
pub(super) use cleanup::{consumer_cleanup_on, origin_stop_on};
pub(super) use native::record_native_answer_on;
pub(super) use phase::{DecisionStopCause, phase_on};
pub(super) use phase::{enroll_on, record_enrollment_on};
pub(super) use resume::{bind_resume_session_on, record_resume_input_on, reserve_successor_on};

#[derive(Debug)]
pub(crate) struct DecisionMutation {
    pub record: DecisionRecord,
    pub outcome: crate::WriteOutcome,
}

fn text(v: impl Into<String>) -> Value {
    Value::Text(v.into())
}
fn number(v: i64) -> Value {
    Value::Integer(v)
}
fn blob(v: &[u8]) -> Value {
    Value::Blob(v.to_vec())
}
fn insert(
    tx: &Transaction<'_>,
    table: &str,
    values: Vec<(&str, Value)>,
) -> Result<(), BotLedgerError> {
    let names = values.iter().map(|v| v.0).collect::<Vec<_>>().join(",");
    let placeholders = vec!["?"; values.len()].join(",");
    tx.execute(
        &format!("INSERT INTO {table}({names}) VALUES({placeholders})"),
        params_from_iter(values.iter().map(|v| &v.1)),
    )
    .map_err(sql)?;
    Ok(())
}
fn required(
    tx: &Connection,
    owner: BotOwner,
    run: RunId,
    id: DecisionId,
) -> Result<DecisionRecord, BotLedgerError> {
    rows::read(tx, owner, run, id)?.ok_or(BotLedgerError::Stale)
}
fn done(
    tx: &Connection,
    owner: BotOwner,
    run: RunId,
    id: DecisionId,
    outcome: crate::WriteOutcome,
) -> Result<DecisionMutation, BotLedgerError> {
    Ok(DecisionMutation {
        record: required(tx, owner, run, id)?,
        outcome,
    })
}
pub(super) fn cap(tx: &Connection, owner: BotOwner) -> Result<(), BotLedgerError> {
    // Identity-only bounded device probe, including nondecision cleanup. The
    // 53rd row detects overflow of 50 retained waits + two occupied Runs. Do
    // not scan an unbounded excluded prefix to find this owner's decisions.
    let mut q=tx.prepare("SELECT CASE WHEN typeof(user_scope)='text' AND length(CAST(user_scope AS BLOB))=36 THEN user_scope END,CASE WHEN reason IN ('decision','capacity','native_expired','overlapping_prompt') THEN reason END FROM bot_wait_runtime INDEXED BY bot_wait_runtime_recovery WHERE device_id=?1 AND phase<>'settled' ORDER BY updated_at_ms,wait_id LIMIT 53").map_err(sql)?;
    let mut rows = q.query([owner.device.to_storage()]).map_err(sql)?;
    let (mut total, mut device, mut subject) = (0, 0, 0);
    while let Some(row) = rows.next().map_err(sql)? {
        total += 1;
        let user: String = row.get(0).map_err(sql)?;
        let parsed: BotOwner =
            serde_json::from_value(serde_json::json!({"user_scope":user,"device":owner.device}))
                .map_err(|_| corrupt("bot_wait_runtime", "user_scope"))?;
        if row.get::<_, String>(1).map_err(sql)? == "decision" {
            device += 1;
            if parsed.user_scope == owner.user_scope {
                subject += 1;
            }
        }
    }
    if total >= 53
        || total - device > 2
        || device >= DECISION_WAIT_LIMIT
        || subject >= DECISION_WAIT_LIMIT
    {
        return Err(BotLedgerError::Capacity);
    }
    Ok(())
}

/// Current human/Store qualification remains an off-writer obligation. This
/// body proves local retained identity/policy and the selected-write predicate.
/// The public owner must compose the initial control target before committing.
pub(crate) fn prepare_on(
    tx: &Transaction<'_>,
    binding: &DecisionBindingV1,
    proof: &FreshBotPolicyProof,
    boot: DelegateBootId,
    now: i64,
) -> Result<DecisionMutation, BotLedgerError> {
    valid(binding)?;
    let binding_json = encoded(binding, DECISION_FRAME_BYTES)?;
    let f = binding.origin();
    authority::owner(tx, f.owner, f.run)?;
    if let Some(id) = rows::matching(tx, binding)? {
        let old = required(tx, f.owner, f.run, id)?;
        require(old.receipt.binding == *binding)?;
        return Ok(DecisionMutation {
            record: old,
            outcome: crate::WriteOutcome::AlreadyApplied,
        });
    }
    encoded(&(binding, proof), DECISION_FRAME_BYTES)?;
    let admitted = authority::fresh(tx, binding, proof, true)?;
    authority::selected_denial(tx, binding, proof, &admitted)?;
    // One operation id, one meaning. An edit refuses an id a page intent or a
    // private effect already holds; a private read also refuses one a Memory
    // allocation holds.
    match binding {
        DecisionBindingV1::KoruEdit(edit) => {
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM native_result_intents WHERE run_id=?1 AND operation_id=?2) OR EXISTS(SELECT 1 FROM bot_private_effects WHERE run_id=?1 AND operation_id=?2)",params![f.run.to_storage(),edit.operation_id.to_string()],|r|r.get(0)).map_err(sql)?;
            if exists {
                return Err(BotLedgerError::Conflict);
            }
        }
        DecisionBindingV1::PrivateOperation(private) => {
            if crate::private_effects::operation_id_taken_on(tx, f.run, private.operation_id)? {
                return Err(BotLedgerError::Conflict);
            }
        }
        DecisionBindingV1::NativeApproval(_) => {}
    }
    let open:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bot_wait_runtime WHERE run_id=?1 AND phase<>'settled') OR EXISTS(SELECT 1 FROM bot_decisions WHERE run_id=?1 AND phase<>'settled')",[f.run.to_storage()],|r|r.get(0)).map_err(sql)?;
    if open {
        return Err(BotLedgerError::Conflict);
    }
    cap(tx, f.owner)?;
    let human = binding.human_profile();
    let operation = binding.operation_id().map(|id| id.to_string());
    let (prompt, native_deadline) = match binding {
        DecisionBindingV1::NativeApproval(v) => {
            (Some(v.prompt.to_string()), v.native_deadline_at_ms)
        }
        DecisionBindingV1::KoruEdit(_) | DecisionBindingV1::PrivateOperation(_) => (None, None),
    };
    let full = now
        .checked_add(DECISION_WAIT_MILLIS)
        .ok_or(BotLedgerError::Invalid)?;
    let deadline = DecisionDeadline {
        created_boot: boot,
        created_at_ms: now,
        deadline_at_ms: native_deadline.map_or(full, |d| d.min(full)),
        native_deadline_at_ms: native_deadline,
    };
    valid(&deadline)?;
    // No ID is allocated until replay, complete authority, both caps and clock
    // validity pass. There is no caller-supplied replacement identity.
    let id = DecisionId::mint();
    let wait = BotWaitId::mint();
    let digest = binding.digest().map_err(|_| BotLedgerError::Invalid)?;
    insert(
        tx,
        "bot_decisions",
        vec![
            ("decision_id", text(id.to_storage())),
            ("run_id", text(f.run.to_storage())),
            ("user_scope", text(f.owner.user_scope.to_storage())),
            ("device_id", text(f.owner.device.to_storage())),
            ("bot_id", text(f.bot.to_storage())),
            ("origin_attempt_id", text(f.attempt.to_storage())),
            ("origin_generation", number(i64::from(f.generation.get()))),
            ("origin_session_id", text(f.session.to_string())),
            ("origin_turn_id", text(f.turn.to_string())),
            ("kind", text(token(binding.kind())?)),
            ("operation_id", operation.map(text).unwrap_or(Value::Null)),
            ("native_prompt_id", prompt.map(text).unwrap_or(Value::Null)),
            ("human_profile_id", text(human.to_storage())),
            ("binding_version", number(1)),
            ("binding_digest", blob(digest.as_bytes())),
            ("binding_json", text(binding_json)),
            ("created_boot_id", text(boot.to_string())),
            ("created_at_ms", number(now)),
            ("deadline_at_ms", number(deadline.deadline_at_ms)),
            (
                "native_deadline_at_ms",
                native_deadline.map(number).unwrap_or(Value::Null),
            ),
            ("last_observed_at_ms", number(now)),
            ("revision", number(1)),
            ("authorization", text("pending")),
            ("phase", text("publishing")),
            ("updated_at_ms", number(now)),
        ],
    )?;
    insert(
        tx,
        "bot_wait_runtime",
        vec![
            ("wait_id", text(wait.to_storage())),
            ("run_id", text(f.run.to_storage())),
            ("decision_id", text(id.to_storage())),
            ("user_scope", text(f.owner.user_scope.to_storage())),
            ("device_id", text(f.owner.device.to_storage())),
            ("bot_id", text(f.bot.to_storage())),
            ("attempt_id", text(f.attempt.to_storage())),
            ("generation", number(i64::from(f.generation.get()))),
            ("session_id", text(f.session.to_string())),
            ("turn_id", text(f.turn.to_string())),
            ("reason", text("decision")),
            ("phase", text("fenced")),
            ("revision", number(1)),
            ("created_at_ms", number(now)),
            ("updated_at_ms", number(now)),
        ],
    )?;
    done(tx, f.owner, f.run, id, crate::WriteOutcome::Applied)
}

pub(super) fn clock(
    receipt: &DecisionReceipt,
    boot: DelegateBootId,
    now: i64,
) -> Result<(), BotLedgerError> {
    if receipt.deadline.created_boot != boot
        || now < receipt.last_observed_at_ms
        || now < receipt.deadline.created_at_ms
        || now >= receipt.deadline.deadline_at_ms
    {
        return Err(BotLedgerError::Fenced);
    }
    // This is a necessary wall/boot check, never proof of elapsed sleep time
    // or a previous backward sample. E also owns the original Instant deadline.
    Ok(())
}
