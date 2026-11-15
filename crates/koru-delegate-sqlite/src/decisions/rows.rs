// SPDX-License-Identifier: MPL-2.0

use super::*;
use rusqlite::{Row, types::FromSql};
mod native;
mod resume;
mod wait;
pub(super) use wait::read_wait;

/// Fixed source-owned column descriptions, never caller SQL. SQLite checks
/// every type/byte length in the selected row before Rust allocates any body.
pub(super) fn query(table: &str, fields: &[(&str, &str, usize, bool)], predicate: &str) -> String {
    let guards = fields
        .iter()
        .map(|(name, kind, max, optional)| {
            let check = if *kind == "integer" {
                format!("typeof({name})='integer'")
            } else {
                format!(
                    "typeof({name})='{kind}' AND length(CAST({name} AS BLOB)) BETWEEN 1 AND {max}"
                )
            };
            if *optional {
                format!("({name} IS NULL OR ({check}))")
            } else {
                format!("({check})")
            }
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    format!("SELECT ({guards}) AS bounds_ok, * FROM {table} WHERE {predicate}")
}
pub(super) struct Cells<'a, 'r> {
    pub row: &'a Row<'r>,
    pub table: &'static str,
}
impl Cells<'_, '_> {
    pub fn check(&self) -> Result<(), BotLedgerError> {
        if self.get::<bool>("bounds_ok")? {
            Ok(())
        } else {
            Err(corrupt(self.table, "bounded_columns"))
        }
    }
    pub fn get<T: FromSql>(&self, name: &'static str) -> Result<T, BotLedgerError> {
        self.row.get(name).map_err(|_| corrupt(self.table, name))
    }
    pub fn id<T: DeserializeOwned>(&self, name: &'static str) -> Result<T, BotLedgerError> {
        serde_json::from_value(serde_json::Value::String(self.get(name)?))
            .map_err(|_| corrupt(self.table, name))
    }
    pub fn optional_id<T: DeserializeOwned>(
        &self,
        name: &'static str,
    ) -> Result<Option<T>, BotLedgerError> {
        self.get::<Option<String>>(name)?
            .map(|s| serde_json::from_value(s.into()).map_err(|_| corrupt(self.table, name)))
            .transpose()
    }
    pub fn json<T: DeserializeOwned>(&self, name: &'static str) -> Result<T, BotLedgerError> {
        serde_json::from_str(&self.get::<String>(name)?).map_err(|_| corrupt(self.table, name))
    }
    pub fn optional_json<T: DeserializeOwned>(
        &self,
        name: &'static str,
    ) -> Result<Option<T>, BotLedgerError> {
        self.get::<Option<String>>(name)?
            .map(|s| serde_json::from_str(&s).map_err(|_| corrupt(self.table, name)))
            .transpose()
    }
    pub fn digest(&self, name: &'static str) -> Result<DecisionDigest, BotLedgerError> {
        self.get::<Vec<u8>>(name)?
            .try_into()
            .map(DecisionDigest::from_bytes)
            .map_err(|_| corrupt(self.table, name))
    }
    pub fn revision(&self, name: &'static str) -> Result<DecisionRevision, BotLedgerError> {
        DecisionRevision::new(self.get(name)?).map_err(|_| corrupt(self.table, name))
    }
    pub fn generation(&self, name: &'static str) -> Result<AttemptGeneration, BotLedgerError> {
        AttemptGeneration::from_raw(self.get(name)?).ok_or_else(|| corrupt(self.table, name))
    }
    pub fn same(&self, ok: bool) -> Result<(), BotLedgerError> {
        if ok {
            Ok(())
        } else {
            Err(corrupt(self.table, "correlation"))
        }
    }
}

const DECISION: &[(&str, &str, usize, bool)] = &[
    ("decision_id", "text", 36, false),
    ("run_id", "text", 36, false),
    ("user_scope", "text", 36, false),
    ("device_id", "text", 36, false),
    ("bot_id", "text", 36, false),
    ("origin_attempt_id", "text", 36, false),
    ("origin_generation", "integer", 0, false),
    ("origin_session_id", "text", 36, false),
    ("origin_turn_id", "text", 36, false),
    ("kind", "text", 32, false),
    ("operation_id", "text", 36, true),
    ("native_prompt_id", "text", 4096, true),
    ("human_profile_id", "text", 36, false),
    ("binding_version", "integer", 0, false),
    ("binding_digest", "blob", 32, false),
    ("binding_json", "text", DECISION_FRAME_BYTES, false),
    ("created_boot_id", "text", 36, false),
    ("created_at_ms", "integer", 0, false),
    ("deadline_at_ms", "integer", 0, false),
    ("native_deadline_at_ms", "integer", 0, true),
    ("last_observed_at_ms", "integer", 0, false),
    ("revision", "integer", 0, false),
    ("authorization", "text", 32, false),
    ("phase", "text", 32, false),
    ("answer_request_id", "text", 36, true),
    ("answer_profile_id", "text", 36, true),
    ("answer_choice_json", "text", 16384, true),
    ("answer_at_ms", "integer", 0, true),
    ("answer_proof_digest", "blob", 32, true),
    ("answer_receipt_json", "text", DECISION_RECEIPT_BYTES, true),
    ("consumed_at_ms", "integer", 0, true),
    ("closure_reason", "text", 32, true),
    ("settled_at_ms", "integer", 0, true),
    ("updated_at_ms", "integer", 0, false),
];

pub(super) fn operation(
    conn: &Connection,
    run: RunId,
    operation: uuid::Uuid,
) -> Result<Option<DecisionId>, BotLedgerError> {
    conn.query_row("SELECT CASE WHEN typeof(decision_id)='text' AND length(CAST(decision_id AS BLOB))=36 THEN decision_id END FROM bot_decisions WHERE run_id=?1 AND operation_id=?2 AND kind='koru_edit'",
        params![run.to_storage(),operation.to_string()],|r|Ok(r.get::<_,String>(0)?)).optional().map_err(sql)?
        .map(|s|s.parse().map_err(|_|corrupt("bot_decisions","decision_id"))).transpose()
}
pub(super) fn matching(
    conn: &Connection,
    binding: &DecisionBindingV1,
) -> Result<Option<DecisionId>, BotLedgerError> {
    match binding {
        DecisionBindingV1::KoruEdit(v)=>operation(conn,v.run,v.operation_id),
        DecisionBindingV1::NativeApproval(v)=>conn.query_row("SELECT CASE WHEN typeof(decision_id)='text' AND length(CAST(decision_id AS BLOB))=36 THEN decision_id END FROM bot_decisions WHERE run_id=?1 AND origin_session_id=?2 AND origin_turn_id=?3 AND native_prompt_id=?4 AND kind='native_approval'",
            params![v.run.to_storage(),v.origin_session.to_string(),v.origin_turn.to_string(),v.prompt.to_string()],|r|r.get::<_,String>(0)).optional().map_err(sql)?
            .map(|s|s.parse().map_err(|_|corrupt("bot_decisions","decision_id"))).transpose(),
    }
}
pub(super) fn read(
    conn: &Connection,
    owner: BotOwner,
    run: RunId,
    id: DecisionId,
) -> Result<Option<DecisionRecord>, BotLedgerError> {
    // Identity-only preflight precedes binding allocation, including corrupt or
    // adversarial foreign bodies. The full reader repeats every scalar below.
    let subject=conn.query_row("SELECT CASE WHEN typeof(user_scope)='text' AND length(CAST(user_scope AS BLOB))=36 THEN user_scope END,CASE WHEN typeof(device_id)='text' AND length(CAST(device_id AS BLOB))=36 THEN device_id END,CASE WHEN typeof(run_id)='text' AND length(CAST(run_id AS BLOB))=36 THEN run_id END FROM bot_decisions WHERE decision_id=?1",[id.to_storage()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional().map_err(sql)?;
    let Some((user, device, stored_run)) = subject else {
        return Ok(None);
    };
    if user != owner.user_scope.to_storage()
        || device != owner.device.to_storage()
        || stored_run != run.to_storage()
    {
        return Err(BotLedgerError::Fenced);
    }
    let receipt = conn
        .query_row(
            &query("bot_decisions", DECISION, "decision_id=?1"),
            [id.to_storage()],
            |r| Ok(decode(r)),
        )
        .map_err(sql)??;
    let admission = authority::retained(conn, &receipt.binding)?;
    let wait = read_wait(conn, run, id, &receipt)?;
    let resume = resume::read(conn, &receipt)?;
    let native_response = native::read(conn, &receipt)?;
    wait::correlate(
        conn,
        &receipt,
        &wait,
        resume.as_ref(),
        admission.submission.provenance.ai_attempt_id,
    )?;
    companions(&receipt, &wait, resume.as_ref(), native_response.as_ref())?;
    Ok(Some(DecisionRecord {
        receipt,
        wait,
        resume,
        native_response,
    }))
}

fn companions(
    receipt: &DecisionReceipt,
    wait: &DecisionWaitRecord,
    resume: Option<&DecisionResume>,
    native: Option<&DecisionNativeResponse>,
) -> Result<(), BotLedgerError> {
    use BotWaitPhase as W;
    use DecisionPhase as P;
    let acknowledged =
        resume.is_some_and(|r| matches!(r.state, DecisionResumeState::InputAcknowledged { .. }));
    let agrees = match receipt.state.phase {
        P::Publishing => wait.phase == W::Fenced,
        P::Holding => matches!(wait.phase, W::Fenced | W::HoldRequested),
        P::Held => matches!(wait.phase, W::Held | W::Retired) && resume.is_none(),
        P::NativeWait => wait.phase == W::Fenced && wait.release.is_none(),
        P::ResumePrepared => wait.phase == W::ResumeReserved && resume.is_some(),
        P::AwaitRetry | P::Effect => wait.phase == W::ResumeReserved && acknowledged,
        P::NativeDispatch => wait.phase == W::Fenced && native.is_some(),
        P::NativeUnknown => {
            wait.phase == W::Fenced
                && matches!(native, Some(DecisionNativeResponse::Unconfirmed { .. }))
        }
        P::StopPending => wait.phase == W::CleanupPending,
        // Logical settlement precedes final projection acknowledgment. The
        // retained wait can still own physical/publication work at this point.
        P::Settled => true,
    };
    if !agrees || wait.created_at_ms != receipt.deadline.created_at_ms {
        return Err(corrupt("bot_decisions", "companion_phase"));
    }
    Ok(())
}
fn decode(row: &Row<'_>) -> Result<DecisionReceipt, BotLedgerError> {
    let c = Cells {
        row,
        table: "bot_decisions",
    };
    c.check()?;
    let binding: DecisionBindingV1 = c.json("binding_json")?;
    let origin = binding.origin();
    c.same(
        origin.run == c.id("run_id")?
            && origin.owner.user_scope == c.id("user_scope")?
            && origin.owner.device == c.id("device_id")?
            && origin.bot == c.id("bot_id")?
            && origin.attempt == c.id("origin_attempt_id")?
            && origin.generation == c.generation("origin_generation")?
            && origin.session == c.id("origin_session_id")?
            && origin.turn == c.id("origin_turn_id")?
            && c.get::<i64>("binding_version")? == 1,
    )?;
    let (human, operation, prompt) = match &binding {
        DecisionBindingV1::KoruEdit(v) => (v.human_profile, Some(v.operation_id), None),
        DecisionBindingV1::NativeApproval(v) => (v.human_profile, None, Some(v.prompt)),
    };
    c.same(
        human == c.id("human_profile_id")?
            && operation == c.optional_id("operation_id")?
            && prompt == c.optional_id("native_prompt_id")?,
    )?;
    let answer: Option<DecisionAnswerReceipt> = c.optional_json("answer_receipt_json")?;
    match &answer {
        Some(a) => c.same(
            Some(a.request) == c.optional_id("answer_request_id")?
                && Some(a.human_profile) == c.optional_id("answer_profile_id")?
                && Some(a.choice.clone()) == c.optional_json("answer_choice_json")?
                && Some(a.answered_at_ms) == c.get("answer_at_ms")?
                && a.authority_digest == c.digest("answer_proof_digest")?,
        )?,
        None => c.same(
            c.get::<Option<String>>("answer_request_id")?.is_none()
                && c.get::<Option<String>>("answer_profile_id")?.is_none()
                && c.get::<Option<String>>("answer_choice_json")?.is_none()
                && c.get::<Option<i64>>("answer_at_ms")?.is_none()
                && c.get::<Option<Vec<u8>>>("answer_proof_digest")?.is_none(),
        )?,
    }
    let closure = match (
        c.optional_id("closure_reason")?,
        c.get::<Option<i64>>("settled_at_ms")?,
    ) {
        (Some(reason), Some(settled_at_ms)) => Some(DecisionClosure {
            reason,
            settled_at_ms,
        }),
        (None, None) => None,
        _ => return Err(corrupt(c.table, "closure")),
    };
    let receipt = DecisionReceipt {
        decision: c.id("decision_id")?,
        binding,
        binding_digest: c.digest("binding_digest")?,
        deadline: DecisionDeadline {
            created_boot: c.id("created_boot_id")?,
            created_at_ms: c.get("created_at_ms")?,
            deadline_at_ms: c.get("deadline_at_ms")?,
            native_deadline_at_ms: c.get("native_deadline_at_ms")?,
        },
        state: DecisionState {
            kind: c.id("kind")?,
            authorization: c.id("authorization")?,
            phase: c.id("phase")?,
            revision: c.revision("revision")?,
        },
        answer,
        consumed_at_ms: c.get("consumed_at_ms")?,
        closure,
        last_observed_at_ms: c.get("last_observed_at_ms")?,
        updated_at_ms: c.get("updated_at_ms")?,
    };
    receipt
        .validate()
        .map_err(|_| corrupt(c.table, "receipt"))?;
    c.same(
        receipt.updated_at_ms >= receipt.last_observed_at_ms
            && receipt.last_observed_at_ms >= receipt.deadline.created_at_ms,
    )?;
    Ok(receipt)
}
