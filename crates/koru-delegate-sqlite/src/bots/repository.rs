// SPDX-License-Identifier: MPL-2.0

//! The Bot journal uses the existing connection and transaction boundary.
use super::*;
use crate::{RunsDb, WriteOp, attempts};
use koru_delegate_core::DeviceId;
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::str::FromStr;
mod boot;

pub(super) const BOT_BOOT_ROWS_SQL: &str = "SELECT run_id,bot_id,queue_sequence
    FROM bot_admissions INDEXED BY bot_admissions_queue
    WHERE state=?1 AND user_scope=?2 AND device_id=?3
    AND queue_sequence>?4 AND queue_sequence<=?5 ORDER BY queue_sequence LIMIT ?6";
pub(super) const BOT_BOOT_HIGH_WATER_SQL: &str = "SELECT COALESCE(MAX(queue_sequence),0)
    FROM bot_admissions INDEXED BY bot_admissions_queue
    WHERE state=?1 AND user_scope=?2 AND device_id=?3";

fn sql(error: rusqlite::Error) -> LedgerError {
    LedgerError::sql("access the Bot ledger", error)
}
fn corrupt(column: &'static str) -> LedgerError {
    LedgerError::Corrupt {
        table: "bot_admissions",
        column,
    }
}
fn cell<T: rusqlite::types::FromSql>(row: &Row<'_>, name: &'static str) -> Result<T, LedgerError> {
    row.get(name).map_err(sql)
}
fn id<T: FromStr>(value: &str, column: &'static str) -> Result<T, LedgerError> {
    value.parse().map_err(|_| corrupt(column))
}
fn generation(value: i64) -> Result<AttemptGeneration, LedgerError> {
    u32::try_from(value)
        .ok()
        .and_then(AttemptGeneration::from_raw)
        .ok_or_else(|| corrupt("generation"))
}
fn decode_state(row: &Row<'_>) -> Result<BotAdmissionState, LedgerError> {
    Ok(match cell::<String>(row, "state")?.as_str() {
        "queued" => BotAdmissionState::Queued,
        "reserved" => BotAdmissionState::Reserved,
        "executing" => BotAdmissionState::Executing,
        "settled" => BotAdmissionState::Settled,
        "refused" => BotAdmissionState::Refused,
        _ => return Err(corrupt("state")),
    })
}
fn decode_initial_send(row: &Row<'_>) -> Result<Option<BotInitialSend>, LedgerError> {
    Ok(match cell::<Option<String>>(row, "initial_send_key")? {
        None => None,
        Some(key) => Some(BotInitialSend {
            key: id(&key, "initial_send_key")?,
            attempt: id(
                &cell::<String>(row, "initial_send_attempt_id")?,
                "initial_send_attempt_id",
            )?,
            generation: generation(cell(row, "initial_send_generation")?)?,
            fingerprint: cell::<Vec<u8>>(row, "initial_send_fingerprint")?
                .try_into()
                .map_err(|_| corrupt("initial_send_fingerprint"))?,
            turn: cell::<Option<String>>(row, "initial_send_turn_id")?
                .map(|v| id(&v, "initial_send_turn_id"))
                .transpose()?,
        }),
    })
}
fn decode(row: &Row<'_>) -> Result<BotAdmissionRecord, LedgerError> {
    let payload: String = cell(row, "payload_json")?;
    let submission_version: u8 = cell(row, "submission_version")?;
    if payload.len() > 4 * 1024 * 1024 {
        return Err(corrupt("payload_json"));
    }
    if !matches!(submission_version, 1 | 2) {
        return Err(corrupt("submission_version"));
    }
    let submission: BotSubmission =
        serde_json::from_str(&payload).map_err(|_| corrupt("payload_json"))?;
    let p = &submission.provenance;
    if !submission.valid_for_version(u32::from(submission_version))
        || cell::<String>(row, "run_id")? != submission.request.run.to_storage()
        || cell::<String>(row, "user_scope")? != p.owner.user_scope.to_storage()
        || cell::<String>(row, "device_id")? != p.owner.device.to_storage()
        || cell::<String>(row, "bot_id")? != p.bot.to_storage()
        || cell::<String>(row, "request_id")? != p.submission.to_storage()
        || cell::<u64>(row, "config_revision")? != p.config_revision
        || cell::<u64>(row, "policy_revision")? != p.policy_revision
    {
        return Err(corrupt("payload_json"));
    }
    let state = decode_state(row)?;
    let initial_send = decode_initial_send(row)?;
    let delivery = match cell::<String>(row, "delivery_outcome")?.as_str() {
        "unconfirmed" => None,
        "confirmed" => {
            let receipt = BotDeliveryReceipt {
                run: submission.request.run,
                attempt: id(
                    &cell::<String>(row, "delivery_attempt_id")?,
                    "delivery_attempt_id",
                )?,
                generation: generation(cell(row, "delivery_generation")?)?,
                chat_id: id(
                    &cell::<String>(row, "delivery_chat_id")?,
                    "delivery_chat_id",
                )?,
                ai_attempt_id: id(
                    &cell::<String>(row, "delivery_ai_attempt_id")?,
                    "delivery_ai_attempt_id",
                )?,
                assistant_message_id: id(
                    &cell::<String>(row, "delivery_assistant_message_id")?,
                    "delivery_assistant_message_id",
                )?,
                confirmed_at_ms: cell(row, "delivery_confirmed_at_ms")?,
            };
            if !receipt.valid()
                || receipt.chat_id != p.chat_id
                || Some(receipt.ai_attempt_id) != p.ai_attempt_id
                || Some(receipt.assistant_message_id) != p.assistant_message_id
            {
                return Err(corrupt("delivery_outcome"));
            }
            Some(receipt)
        }
        _ => return Err(corrupt("delivery_outcome")),
    };
    let delivery_publication = cell::<Option<String>>(row, "delivery_publication_event_id")?
        .map(|value| {
            let uuid = uuid::Uuid::parse_str(&value)
                .map_err(|_| corrupt("delivery_publication_event_id"))?;
            if uuid.hyphenated().to_string() != value
                || uuid.get_version() != Some(uuid::Version::Random)
                || uuid.get_variant() != uuid::Variant::RFC4122
                || delivery.is_none()
            {
                return Err(corrupt("delivery_publication_event_id"));
            }
            Ok(koru_delegate_core::EventId(uuid))
        })
        .transpose()?;
    let sequence = cell(row, "queue_sequence")?;
    let active_since_ms: Option<i64> = cell(row, "active_since_ms")?;
    if sequence == 0 || (active_since_ms.is_some() && state != BotAdmissionState::Executing) {
        return Err(corrupt("state"));
    }
    Ok(BotAdmissionRecord {
        submission_version,
        submission,
        sequence,
        state,
        initial_send,
        delivery,
        delivery_publication,
        active_since_ms,
        active_elapsed_ms: cell(row, "active_elapsed_ms")?,
        created_at_ms: cell(row, "created_at_ms")?,
        updated_at_ms: cell(row, "updated_at_ms")?,
    })
}

const EXECUTION_COLUMNS: &str = "run_id,submission_version,user_scope,device_id,bot_id,request_id,config_revision,policy_revision,queue_sequence,state,active_elapsed_ms,active_since_ms,initial_send_key,initial_send_fingerprint,initial_send_attempt_id,initial_send_generation,initial_send_turn_id";
/// The two per-Bot reads behind [`BotLedger::unsettled_bot`]. Both name
/// their index: without statistics the planner would prefer the
/// (user_scope, device_id, bot_id, request_id) uniqueness index, which walks
/// every settled admission the Bot ever had. Plans are pinned by test.
pub(crate) fn unsettled_queries() -> (String, String) {
    (
        format!(
            "SELECT {EXECUTION_COLUMNS} FROM bot_admissions INDEXED BY bot_admissions_execution
             WHERE user_scope=?1 AND device_id=?2 AND bot_id=?3 AND state IN ('reserved','executing')"
        ),
        format!(
            "SELECT {EXECUTION_COLUMNS} FROM bot_admissions INDEXED BY bot_admissions_queue
             WHERE state='queued' AND user_scope=?1 AND device_id=?2 AND bot_id=?3
             ORDER BY queue_sequence LIMIT ?4"
        ),
    )
}
fn decode_execution(row: &Row<'_>) -> Result<BotExecutionRecord, LedgerError> {
    let record = BotExecutionRecord {
        run: id(&cell::<String>(row, "run_id")?, "run_id")?,
        submission_version: cell(row, "submission_version")?,
        owner: BotOwner {
            user_scope: serde_json::from_value(serde_json::Value::String(cell(row, "user_scope")?))
                .map_err(|_| corrupt("user_scope"))?,
            device: DeviceId::from_uuid(id(&cell::<String>(row, "device_id")?, "device_id")?),
        },
        bot: id(&cell::<String>(row, "bot_id")?, "bot_id")?,
        request: id(&cell::<String>(row, "request_id")?, "request_id")?,
        config_revision: cell(row, "config_revision")?,
        policy_revision: cell(row, "policy_revision")?,
        sequence: cell(row, "queue_sequence")?,
        state: decode_state(row)?,
        active_elapsed_ms: cell(row, "active_elapsed_ms")?,
        active_since_ms: cell(row, "active_since_ms")?,
        initial_send: decode_initial_send(row)?,
    };
    if !matches!(record.submission_version, 1 | 2)
        || record.sequence == 0
        || record.config_revision == 0
        || record.policy_revision == 0
        || (record.active_since_ms.is_some() && record.state != BotAdmissionState::Executing)
    {
        return Err(corrupt("state"));
    }
    Ok(record)
}
fn validate_execution(
    conn: &Connection,
    record: &BotExecutionRecord,
) -> Result<(), BotLedgerError> {
    match record.submission_version {
        1 => super::routines::deny_run_columns(conn, record.run).map_err(Into::into),
        // This projection carries no authority. Consumers that read a subject
        // or execute work invoke the sole full admission/occurrence owner.
        2 => Ok(()),
        _ => Err(BotLedgerError::Fenced),
    }
}

/// Same execution decoder, but guard every projected scalar before allocating
/// strings. The 53rd row is an impossible-population sentinel, not a page.
pub(crate) fn bounded_owned_executions(
    conn: &Connection,
    device: DeviceId,
) -> Result<Vec<BotExecutionRecord>, BotLedgerError> {
    let mut guards = Vec::new();
    for column in ["run_id", "user_scope", "device_id", "bot_id", "request_id"] {
        guards.push(format!(
            "typeof({column})='text' AND length(CAST({column} AS BLOB))=36"
        ));
    }
    for column in [
        "initial_send_key",
        "initial_send_attempt_id",
        "initial_send_turn_id",
    ] {
        guards.push(format!(
            "({column} IS NULL OR (typeof({column})='text' AND length(CAST({column} AS BLOB))=36))"
        ));
    }
    for column in [
        "submission_version",
        "config_revision",
        "policy_revision",
        "queue_sequence",
        "active_elapsed_ms",
    ] {
        guards.push(format!("typeof({column})='integer'"));
    }
    for column in ["active_since_ms", "initial_send_generation"] {
        guards.push(format!("({column} IS NULL OR typeof({column})='integer')"));
    }
    guards.push("(initial_send_fingerprint IS NULL OR (typeof(initial_send_fingerprint)='blob' AND length(initial_send_fingerprint)=32))".into());
    guards.push("typeof(state)='text' AND state IN ('reserved','executing')".into());
    let query = format!(
        "SELECT ({}) AS bounds_ok,{EXECUTION_COLUMNS} FROM bot_admissions INDEXED BY bot_admissions_execution WHERE device_id=?1 AND state IN ('reserved','executing') ORDER BY queue_sequence LIMIT 53",
        guards.join(" AND ")
    );
    let mut statement = conn.prepare(&query).map_err(sql)?;
    let mut rows = statement.query([device.to_storage()]).map_err(sql)?;
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(sql)? {
        if !row.get::<_, bool>("bounds_ok").map_err(sql)? {
            return Err(corrupt("bounded_execution").into());
        }
        let record = decode_execution(row)?;
        validate_execution(conn, &record)?;
        result.push(record);
    }
    if result.len() > 52 {
        return Err(corrupt("owned_capacity").into());
    }
    Ok(result)
}
fn execution_projection(
    conn: &Connection,
    run: RunId,
) -> Result<Option<BotExecutionRecord>, BotLedgerError> {
    let record = conn
        .query_row(
            &format!("SELECT {EXECUTION_COLUMNS} FROM bot_admissions WHERE run_id=?1"),
            [run.to_storage()],
            |row| Ok(decode_execution(row)),
        )
        .optional()
        .map_err(sql)?
        .transpose()?;
    if let Some(record) = &record {
        validate_execution(conn, record)?;
    }
    Ok(record)
}
pub(super) fn read(
    conn: &Connection,
    run: RunId,
) -> Result<Option<BotAdmissionRecord>, BotLedgerError> {
    Ok(conn
        .query_row(
            "SELECT * FROM bot_admissions WHERE run_id=?1",
            [run.to_storage()],
            |r| Ok(decode(r)),
        )
        .optional()
        .map_err(sql)?
        .transpose()?)
}
fn required(conn: &Connection, run: RunId) -> Result<BotAdmissionRecord, BotLedgerError> {
    read(conn, run)?.ok_or(LedgerError::UnknownRun.into())
}
fn reconcile(
    conn: &Connection,
    key: &BotSubmissionKey,
) -> Result<Option<BotAdmissionRecord>, BotLedgerError> {
    Ok(conn.query_row("SELECT * FROM bot_admissions WHERE user_scope=?1 AND device_id=?2 AND bot_id=?3 AND request_id=?4",
        params![key.owner.user_scope.to_storage(),key.owner.device.to_storage(),key.bot.to_storage(),key.request.to_storage()],|r|Ok(decode(r)))
        .optional().map_err(sql)?.transpose()?)
}
pub(super) fn barrier(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
) -> Result<Option<BotAdmissionBarrier>, BotLedgerError> {
    // Project invalid storage to NULL before String allocation. Affinity alone
    // does not prove integer values or a bounded canonical request spelling.
    let raw:Option<(i64,i64,String)>=conn.query_row("SELECT CASE WHEN typeof(epoch)='integer' THEN epoch END, CASE WHEN typeof(paused)='integer' THEN paused END, CASE WHEN typeof(request_id)='text' AND length(CAST(request_id AS BLOB))=36 THEN request_id END FROM bot_admission_barriers WHERE user_scope=?1 AND device_id=?2 AND bot_id=?3",
        params![owner.user_scope.to_storage(),owner.device.to_storage(),bot.to_storage()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sql)?;
    raw.map(|(epoch, paused, request)| {
        if epoch <= 0 || !matches!(paused, 0 | 1) {
            return Err(corrupt("barrier").into());
        }
        Ok(BotAdmissionBarrier {
            owner,
            bot,
            epoch: epoch as u64,
            paused: paused == 1,
            request: id(&request, "request_id")?,
        })
    })
    .transpose()
}
fn current(
    conn: &Connection,
    run: RunId,
    attempt: RunAttemptId,
    g: AttemptGeneration,
    live: bool,
) -> Result<(), BotLedgerError> {
    let row = attempts::latest(conn, run)?.ok_or(BotLedgerError::Fenced)?;
    if row.attempt != attempt
        || row.generation != g
        || (g != AttemptGeneration::FIRST
            && !crate::decisions::retained_consumer_on(conn, run, &row)?)
        || (live && (row.retired || row.phase.is_terminal()))
    {
        return Err(BotLedgerError::Fenced);
    }
    Ok(())
}
fn charge(record: &BotAdmissionRecord, now: i64) -> i64 {
    record.active_elapsed_at(now).min(i64::MAX as u64) as i64
}

impl BotLedger for RunsDb {
    fn begin_bot_wait_cleanup(
        &mut self,
        fence: &DecisionFence,
        reason: BotWaitReason,
        observed: koru_delegate_core::StreamSequence,
        transition: DecisionTransitionId,
        base: &DecisionPublicationBase,
    ) -> Result<crate::DecisionWaitRecord, BotLedgerError> {
        RunsDb::begin_bot_wait_cleanup(self, fence, reason, observed, transition, base)
    }
    fn record_nondecision_cleanup(
        &mut self,
        expected: &DecisionWaitExpected,
        cleanup_projection: DecisionTransitionId,
        transition: DecisionTransitionId,
        cleanup: &ConsumerCleanup,
    ) -> Result<crate::DecisionWaitRecord, BotLedgerError> {
        RunsDb::record_nondecision_cleanup(self, expected, cleanup_projection, transition, cleanup)
    }
    fn settle_decision_wait(
        &mut self,
        expected: &DecisionWaitExpected,
        ack: &DecisionProjectionAcknowledgment,
    ) -> Result<crate::DecisionWaitRecord, BotLedgerError> {
        RunsDb::settle_decision_wait(self, expected, ack)
    }
    fn consume_decision_edit(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        request: &koru_delegate_core::PrepareNativeResult,
        payload: &[u8],
        fence: &koru_delegate_core::BotExecutionFence,
        proof: &FreshBotPolicyProof,
        boot: koru_delegate_core::DelegateBootId,
    ) -> Result<crate::NativeLeaseOutcome, BotLedgerError> {
        RunsDb::consume_decision_edit(
            self, expected, transition, request, payload, fence, proof, boot,
        )
    }
    fn settle_decision_effect(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        outcome: &DecisionRetainedEffect,
    ) -> Result<crate::DecisionRecord, BotLedgerError> {
        RunsDb::settle_decision_effect(self, expected, transition, outcome)
    }
    fn decision_cleanup_ownership(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<crate::DecisionCleanupOwnership>, BotLedgerError> {
        RunsDb::decision_cleanup_ownership(self, owner, run)
    }
    fn answer_bot_decision(
        &mut self,
        input: &crate::AnswerDecision,
        transition: DecisionTransitionId,
        proof: &FreshBotPolicyProof,
        boot: koru_delegate_core::DelegateBootId,
    ) -> Result<crate::DecisionRecord, BotLedgerError> {
        RunsDb::answer_bot_decision(self, input, transition, proof, boot)
    }
    fn record_decision_native_answer(
        &mut self,
        owner: BotOwner,
        run: RunId,
        transition: DecisionTransitionId,
        response: &DecisionNativeResponse,
    ) -> Result<crate::DecisionRecord, BotLedgerError> {
        RunsDb::record_decision_native_answer(self, owner, run, transition, response)
    }
    fn record_decision_consumer_cleanup(
        &mut self,
        expected: &DecisionWaitExpected,
        cleanup_projection: DecisionTransitionId,
        transition: DecisionTransitionId,
        observation: &ConsumerCleanup,
    ) -> Result<crate::DecisionRecord, BotLedgerError> {
        RunsDb::record_decision_consumer_cleanup(
            self,
            expected,
            cleanup_projection,
            transition,
            observation,
        )
    }
    fn enroll_decision_cleanup(
        &mut self,
        expected: &DecisionWaitExpected,
        transition: DecisionTransitionId,
    ) -> Result<crate::DecisionRecord, BotLedgerError> {
        RunsDb::enroll_decision_cleanup(self, expected, transition)
    }
    fn record_decision_enrollment(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        reply: &BotWaitEnrollmentReply,
    ) -> Result<crate::DecisionRecord, BotLedgerError> {
        RunsDb::record_decision_enrollment(self, expected, transition, reply)
    }
    fn record_decision_origin_stop(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        release: &OriginWaitStop,
    ) -> Result<crate::DecisionRecord, BotLedgerError> {
        RunsDb::record_decision_origin_stop(self, expected, transition, release)
    }
    fn reserve_decision_successor(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        host: koru_delegate_core::HostInstanceId,
        fingerprint: [u8; 32],
        proof: &FreshBotPolicyProof,
        boot: koru_delegate_core::DelegateBootId,
    ) -> Result<crate::DecisionRecord, BotLedgerError> {
        RunsDb::reserve_decision_successor(
            self,
            expected,
            transition,
            host,
            fingerprint,
            proof,
            boot,
        )
    }
    fn bind_decision_successor(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        creation: IdempotencyKey,
        host: koru_delegate_core::HostInstanceId,
        session: koru_delegate_core::SessionId,
    ) -> Result<crate::DecisionRecord, BotLedgerError> {
        RunsDb::bind_decision_successor(self, expected, transition, creation, host, session)
    }
    fn record_decision_resume_input(
        &mut self,
        expected: &DecisionExpected,
        transition: DecisionTransitionId,
        input: IdempotencyKey,
        fingerprint: [u8; 32],
        acknowledgment: Option<&DecisionInputAcknowledgment>,
    ) -> Result<crate::DecisionRecord, BotLedgerError> {
        RunsDb::record_decision_resume_input(
            self,
            expected,
            transition,
            input,
            fingerprint,
            acknowledgment,
        )
    }
    fn prepare_bot_decision(
        &mut self,
        binding: &DecisionBindingV1,
        proof: &FreshBotPolicyProof,
        boot: koru_delegate_core::DelegateBootId,
        base: &DecisionPublicationBase,
    ) -> Result<crate::DecisionRecord, BotLedgerError> {
        RunsDb::prepare_bot_decision(self, binding, proof, boot, base)
    }
    fn request_decision_cleanup(
        &mut self,
        expected: &DecisionWaitExpected,
        transition: DecisionTransitionId,
        cause: DecisionClosureReason,
    ) -> Result<DecisionProjectionRecord, BotLedgerError> {
        RunsDb::request_decision_cleanup(self, expected, transition, cause)
    }
    fn decision_wait_execution(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<crate::DecisionWaitExecution>, BotLedgerError> {
        RunsDb::decision_wait_execution(self, owner, run)
    }
    fn record_decision_source(
        &mut self,
        owner: BotOwner,
        transition: DecisionTransitionId,
        wait: BotWaitId,
        base: &DecisionPublicationBase,
        target: &RunProjectionTargetV1,
    ) -> Result<DecisionProjectionRecord, BotLedgerError> {
        RunsDb::record_decision_source(self, owner, transition, wait, base, target)
    }
    fn decision_projection(
        &self,
        owner: BotOwner,
        run: RunId,
        transition: DecisionTransitionId,
    ) -> Result<Option<DecisionProjectionRecord>, BotLedgerError> {
        RunsDb::decision_projection(self, owner, run, transition)
    }
    fn next_decision_projection(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionProjectionRecord>, BotLedgerError> {
        RunsDb::next_decision_projection(self, owner, run)
    }
    fn decision_projection_prefix(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Vec<DecisionProjectionRecord>, BotLedgerError> {
        RunsDb::decision_projection_prefix(self, owner, run)
    }
    fn latest_decision_projection(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionProjectionRecord>, BotLedgerError> {
        RunsDb::latest_decision_projection(self, owner, run)
    }
    fn latest_applied_decision_projection(
        &self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionProjectionRecord>, BotLedgerError> {
        RunsDb::latest_applied_decision_projection(self, owner, run)
    }
    fn record_decision_preparation(
        &mut self,
        owner: BotOwner,
        prepared: &DecisionPreparedProjection,
    ) -> Result<DecisionProjectionRecord, BotLedgerError> {
        RunsDb::record_decision_preparation(self, owner, prepared)
    }
    fn acknowledge_decision_projection(
        &mut self,
        owner: BotOwner,
        acknowledgment: &DecisionProjectionAcknowledgment,
    ) -> Result<DecisionProjectionRecord, BotLedgerError> {
        RunsDb::acknowledge_decision_projection(self, owner, acknowledgment)
    }
    fn read_routine_claim(
        &self,
        slot: &koru_delegate_core::RoutineSlot,
    ) -> Result<koru_delegate_core::RoutineClaimObservation, BotLedgerError> {
        RunsDb::read_routine_claim(self, slot)
    }
    fn acknowledge_routine_link(
        &mut self,
        acknowledgement: &koru_delegate_core::RoutineLinkAcknowledgement,
    ) -> Result<koru_delegate_core::RoutineClaimRecord, BotLedgerError> {
        RunsDb::acknowledge_routine_link(self, acknowledgement)
    }
    fn register_routine_route(
        &mut self,
        identity: &koru_delegate_core::RoutineRouteIdentity,
    ) -> Result<koru_delegate_core::RoutineRouteIdentity, BotLedgerError> {
        RunsDb::register_routine_route(self, identity)
    }
    fn stop_unstarted_routine(
        &mut self,
        key: &BotRunKey,
    ) -> Result<Option<koru_delegate_core::RoutineClaimRecord>, BotLedgerError> {
        self.write_with_error(WriteOp::StopUnstartedRoutine, |tx, now| {
            super::routines::stop(tx, key, now)
        })
    }
    fn bot_boot_inventory(
        &self,
        owner: BotOwner,
        after: u64,
        through: Option<u64>,
    ) -> Result<BotBootPage, BotLedgerError> {
        boot::inventory(self, owner, after, through)
    }
    fn bot_has_unsettled(&self, owner: BotOwner, bot: BotId) -> Result<bool, BotLedgerError> {
        boot::has_unsettled(self, owner, bot)
    }
    fn recover_bot_reservation(&mut self, key: &BotRunKey) -> Result<bool, BotLedgerError> {
        boot::recover(self, key)
    }
    fn active_bot_executions(
        &self,
        device: koru_delegate_core::DeviceId,
    ) -> Result<Vec<BotExecutionRecord>, BotLedgerError> {
        // Compatibility ownership view. Its length is NOT host occupancy.
        // Dispatch uses bot_execution_inventory; boot uses its existing page.
        Ok(RunsDb::bot_execution_inventory(self, device)?
            .owned
            .into_iter()
            .map(|r| r.execution)
            .collect())
    }
    fn bot_execution_inventory(
        &self,
        device: DeviceId,
    ) -> Result<BotExecutionInventory, BotLedgerError> {
        RunsDb::bot_execution_inventory(self, device)
    }
    fn owned_bot_execution_page(
        &self,
        device: DeviceId,
        after: u64,
        through: Option<u64>,
    ) -> Result<BotBootPage, BotLedgerError> {
        boot::owned_page(self, device, after, through)
    }
    fn admit_bot(
        &mut self,
        submission: &BotSubmission,
    ) -> Result<BotAdmissionRecord, BotLedgerError> {
        let submission_version = if submission.provenance.routine.is_some() {
            2
        } else {
            1
        };
        if !submission.valid_for_version(submission_version) {
            return Err(BotLedgerError::Invalid);
        }
        let payload = serde_json::to_string(submission).map_err(|_| BotLedgerError::Invalid)?;
        if payload.len() > 4 * 1024 * 1024 {
            return Err(BotLedgerError::Invalid);
        }
        let ordinary =
            serde_json::to_string(&submission.request).map_err(|_| BotLedgerError::Invalid)?;
        self.write_with_error(WriteOp::AdmitBot,|tx,now| {
            let p=&submission.provenance;
            let key=BotSubmissionKey {owner:p.owner,bot:p.bot,request:p.submission};
            if let Some(existing)=reconcile(tx,&key)? {
                return if existing.submission==*submission {Ok(existing)} else {Err(BotLedgerError::Conflict)};
            }
            // Generic admission can replay retained facts but cannot create Routine work.
            if submission_version != 1 { return Err(BotLedgerError::Fenced); }
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM run_intents WHERE run_id=?1)",[submission.request.run.to_storage()],|r|r.get(0)).map_err(sql)?;
            if exists { return Err(BotLedgerError::Conflict); }
            if let Some(b)=barrier(tx,p.owner,p.bot)? {
                if p.config_revision<b.epoch { return Err(BotLedgerError::Stale); }
                if b.paused { return Err(BotLedgerError::Fenced); }
            }
            let pending:i64=tx.query_row("SELECT count(*) FROM bot_admissions WHERE user_scope=?1 AND device_id=?2 AND bot_id=?3 AND state='queued'",
                params![p.owner.user_scope.to_storage(),p.owner.device.to_storage(),p.bot.to_storage()],|r|r.get(0)).map_err(sql)?;
            if pending>=BOT_PENDING_LIMIT as i64 { return Err(BotLedgerError::Capacity); }
            let sequence:i64=tx.query_row("SELECT COALESCE(MAX(queue_sequence),0)+1 FROM bot_admissions",[],|r|r.get(0)).map_err(sql)?;
            tx.execute("INSERT INTO run_intents(run_id,workspace_id,payload_json,state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,'pending',?4,?4)",
                params![submission.request.run.to_storage(),submission.workspace.to_storage(),ordinary,now]).map_err(sql)?;
            tx.execute("INSERT INTO bot_admissions(run_id,queue_sequence,user_scope,device_id,bot_id,request_id,config_revision,policy_revision,payload_json,state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'queued',?10,?10)",
                params![submission.request.run.to_storage(),sequence,p.owner.user_scope.to_storage(),p.owner.device.to_storage(),p.bot.to_storage(),p.submission.to_storage(),p.config_revision,p.policy_revision,payload,now]).map_err(sql)?;
            required(tx,submission.request.run)
        })
    }
    fn bot_execution(&self, run: RunId) -> Result<Option<BotExecutionRecord>, BotLedgerError> {
        execution_projection(self.conn(), run)
    }
    fn reconcile_bot_execution(
        &self,
        key: &BotSubmissionKey,
    ) -> Result<Option<BotExecutionRecord>, BotLedgerError> {
        let record = self.conn().query_row(&format!("SELECT {EXECUTION_COLUMNS} FROM bot_admissions WHERE user_scope=?1 AND device_id=?2 AND bot_id=?3 AND request_id=?4"),
            params![key.owner.user_scope.to_storage(),key.owner.device.to_storage(),key.bot.to_storage(),key.request.to_storage()],
            |row| Ok(decode_execution(row))).optional().map_err(sql)?.transpose()?;
        if let Some(record) = &record {
            validate_execution(self.conn(), record)?;
        }
        Ok(record)
    }
    fn bot_admission(&self, run: RunId) -> Result<Option<BotAdmissionRecord>, BotLedgerError> {
        let version: Option<i64> = self
            .conn()
            .query_row(
                "SELECT submission_version FROM bot_admissions WHERE run_id=?1",
                [run.to_storage()],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        if version == Some(2) {
            super::routines::bounded_admission(self.conn(), run)?;
        }
        let record = read(self.conn(), run)?;
        if let Some(record) = &record {
            super::routines::execution::retained(self.conn(), record)?;
        } else {
            // Absence of the admission does not erase retained Routine origin,
            // route or link markers at a tool-authority boundary.
            super::routines::deny_run(self.conn(), run)?;
        }
        Ok(record)
    }
    fn reconcile_bot(
        &self,
        key: &BotSubmissionKey,
    ) -> Result<Option<BotAdmissionRecord>, BotLedgerError> {
        let Some(record) = self.reconcile_bot_execution(key)? else {
            return Ok(None);
        };
        self.bot_admission(record.run)
    }
    fn queued_bots(
        &self,
        after: u64,
        limit: u16,
    ) -> Result<Vec<QueuedBotAdmission>, BotLedgerError> {
        let Ok(after) = i64::try_from(after) else {
            return Ok(vec![]);
        };
        let mut stmt=self.conn().prepare("SELECT run_id,user_scope,device_id,bot_id,queue_sequence,config_revision,policy_revision FROM bot_admissions WHERE state IN ('queued','reserved') AND queue_sequence>?1 ORDER BY queue_sequence LIMIT ?2").map_err(sql)?;
        let rows = stmt
            .query_map(params![after, limit.clamp(1, 1000)], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, u64>(4)?,
                    r.get::<_, u64>(5)?,
                    r.get::<_, u64>(6)?,
                ))
            })
            .map_err(sql)?;
        rows.map(|row| {
            let (run, scope, device, bot, sequence, config_revision, policy_revision) =
                row.map_err(sql)?;
            Ok(QueuedBotAdmission {
                run: id(&run, "run_id")?,
                owner: BotOwner {
                    user_scope: serde_json::from_value(serde_json::Value::String(scope))
                        .map_err(|_| corrupt("user_scope"))?,
                    device: DeviceId::from_uuid(id(&device, "device_id")?),
                },
                bot: id(&bot, "bot_id")?,
                sequence,
                config_revision,
                policy_revision,
            })
        })
        .collect()
    }
    fn unsettled_bot(
        &self,
        owner: BotOwner,
        bot: BotId,
        limit: u16,
    ) -> Result<(Vec<BotExecutionRecord>, bool), BotLedgerError> {
        let conn = self.conn();
        let decode_rows = |statement: &mut rusqlite::Statement<'_>,
                           params: &[&dyn rusqlite::ToSql]|
         -> Result<Vec<BotExecutionRecord>, BotLedgerError> {
            statement
                .query_map(params, |row| Ok(decode_execution(row)))
                .map_err(sql)?
                .map(|row| row.map_err(sql)?.map_err(BotLedgerError::from))
                .collect()
        };
        let (scope, device, bot) = (
            owner.user_scope.to_storage(),
            owner.device.to_storage(),
            bot.to_storage(),
        );
        let (executing, queued) = unsettled_queries();
        let mut executing = conn.prepare(&executing).map_err(sql)?;
        let mut records = decode_rows(&mut executing, &[&scope, &device, &bot])?;
        if records.len() > 1 {
            return Err(corrupt("bot_capacity").into());
        }
        let limit = i64::from(limit.clamp(1, 64));
        let mut queued = conn.prepare(&queued).map_err(sql)?;
        let mut page = decode_rows(&mut queued, &[&scope, &device, &bot, &(limit + 1)])?;
        let more = page.len() as i64 > limit;
        page.truncate(limit as usize);
        records.extend(page);
        Ok((records, more))
    }
    fn bot_barrier(
        &self,
        owner: BotOwner,
        bot: BotId,
    ) -> Result<Option<BotAdmissionBarrier>, BotLedgerError> {
        barrier(self.conn(), owner, bot)
    }
    fn apply_bot_barrier(
        &mut self,
        b: &BotAdmissionBarrier,
    ) -> Result<BotAdmissionBarrier, BotLedgerError> {
        if b.epoch == 0 || b.epoch > i64::MAX as u64 {
            return Err(BotLedgerError::Invalid);
        }
        self.write_with_error(WriteOp::ApplyBotBarrier,|tx,now| {
            if let Some(old)=barrier(tx,b.owner,b.bot)? {
                if b.epoch<old.epoch {return Err(BotLedgerError::Stale)}
                if b.epoch==old.epoch {return if old==*b {Ok(old)} else {Err(BotLedgerError::Conflict)}}
            }
            tx.execute("INSERT INTO bot_admission_barriers(user_scope,device_id,bot_id,epoch,paused,request_id,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?7) ON CONFLICT(user_scope,device_id,bot_id) DO UPDATE SET epoch=excluded.epoch,paused=excluded.paused,request_id=excluded.request_id,updated_at_ms=excluded.updated_at_ms",
                params![b.owner.user_scope.to_storage(),b.owner.device.to_storage(),b.bot.to_storage(),b.epoch,b.paused,b.request.to_storage(),now]).map_err(sql)?;
            Ok(b.clone())
        })
    }
    fn reserve_bot(
        &mut self,
        run: RunId,
        proof: Option<&FreshBotPolicyProof>,
    ) -> Result<BotReservation, BotLedgerError> {
        self.write_with_error(WriteOp::ReserveBot,|tx,now| {
            let record=super::routines::native_admission(tx,run)?;
            super::routines::execution::retained(tx,&record)?;
            match record.state {
                BotAdmissionState::Reserved|BotAdmissionState::Executing=>return Ok(BotReservation::AlreadyOwned),
                BotAdmissionState::Settled|BotAdmissionState::Refused=>return Ok(BotReservation::Settled),
                BotAdmissionState::Queued=>{}
            }
            super::routines::execution::fresh(tx,&record,proof,true)?;
            let p=&record.submission.provenance;
            if barrier(tx,p.owner,p.bot)?.is_some_and(|b|b.paused) {return Ok(BotReservation::Paused)}
            let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bot_admissions WHERE user_scope=?1 AND device_id=?2 AND bot_id=?3 AND state IN ('reserved','executing'))",
                params![p.owner.user_scope.to_storage(),p.owner.device.to_storage(),p.bot.to_storage()],|r|r.get(0)).map_err(sql)?;
            if busy {return Ok(BotReservation::BotBusy)}
            let hosts=crate::decisions::execution_inventory_on(tx,p.owner.device)?.occupied;
            if hosts>=BOT_HOST_EXECUTION_LIMIT {return Ok(BotReservation::HostBusy)}
            tx.execute("UPDATE bot_admissions SET state='reserved',updated_at_ms=?2 WHERE run_id=?1",params![run.to_storage(),now]).map_err(sql)?;
            Ok(BotReservation::Acquired)
        })
    }
    fn open_bot_attempt(
        &mut self,
        run: RunId,
        proof: Option<&FreshBotPolicyProof>,
    ) -> Result<crate::AttemptRow, BotLedgerError> {
        self.write_with_error(WriteOp::OpenAttempt, |tx, now| {
            let record = super::routines::native_admission(tx, run)?;
            if record.state != BotAdmissionState::Reserved {
                return Err(BotLedgerError::Fenced);
            }
            super::routines::execution::fresh(tx, &record, proof, true)?;
            if record.submission_version == 2 && attempts::latest(tx, run)?.is_some() {
                return Err(BotLedgerError::Fenced);
            }
            Ok(attempts::open_on(tx, run, now)?)
        })
    }
    fn activate_bot(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        g: AttemptGeneration,
        proof: Option<&FreshBotPolicyProof>,
    ) -> Result<(), BotLedgerError> {
        self.write_with_error(WriteOp::ActivateBot,|tx,now| {
            let record=super::routines::native_admission(tx,run)?;
            super::routines::execution::fresh(tx,&record,proof,true)?;
            let inventory=crate::decisions::execution_inventory_on(tx,record.submission.provenance.owner.device)?;
            if !inventory.owned.iter().any(|r|r.execution.run==run && r.capacity==BotExecutionCapacity::Occupied) {return Err(BotLedgerError::Fenced)}
            current(tx,run,attempt,g,true)?;
            crate::decisions::fresh_activity_on(tx,&record,attempt,g,proof,now)?;
            if record.state==BotAdmissionState::Executing {return Ok(())}
            if record.state!=BotAdmissionState::Reserved || record.active_elapsed_at(now)>=BOT_ACTIVE_BUDGET_MS {return Err(BotLedgerError::Fenced)}
            tx.execute("UPDATE bot_admissions SET state='executing',active_since_ms=?2,updated_at_ms=?2 WHERE run_id=?1",params![run.to_storage(),now]).map_err(sql)?;
            Ok(())
        })
    }
    fn set_bot_active(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        g: AttemptGeneration,
        active: bool,
        proof: Option<&FreshBotPolicyProof>,
    ) -> Result<(), BotLedgerError> {
        self.write_with_error(WriteOp::SetBotActive,|tx,now| {
            let projection=execution_projection(tx,run)?.ok_or(BotLedgerError::Fenced)?;
            current(tx,run,attempt,g,true)?;
            if projection.state!=BotAdmissionState::Executing {return Err(BotLedgerError::Fenced)}
            if projection.active_since_ms.is_some()==active {return Ok(())}
            let record=super::routines::native_admission(tx,run)?;
            super::routines::execution::retained(tx,&record)?;
            if active {
                super::routines::execution::fresh(tx,&record,proof,false)?;
                crate::decisions::fresh_activity_on(tx,&record,attempt,g,proof,now)?;
            }
            let elapsed=charge(&record,now);
            if active && elapsed>=BOT_ACTIVE_BUDGET_MS as i64 {return Err(BotLedgerError::Fenced)}
            tx.execute("UPDATE bot_admissions SET active_elapsed_ms=?2,active_since_ms=?3,updated_at_ms=?4 WHERE run_id=?1",
                params![run.to_storage(),elapsed,active.then_some(now),now]).map_err(sql)?;
            Ok(())
        })
    }
    fn settle_bot(&mut self, run: RunId, outcome: BotSettlement) -> Result<(), BotLedgerError> {
        self.write_with_error(WriteOp::SettleBot,|tx,now| {
            let wait:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bot_wait_runtime WHERE run_id=?1 AND phase<>'settled')",[run.to_storage()],|r|r.get(0)).map_err(sql)?;
            if wait {return Err(BotLedgerError::Fenced)}
            let record=required(tx,run)?;
            let target=match outcome {BotSettlement::Settled=>BotAdmissionState::Settled,BotSettlement::Refused=>BotAdmissionState::Refused};
            super::routines::settle_link(tx,&record,now)?;
            if matches!(record.state,BotAdmissionState::Settled|BotAdmissionState::Refused) {
                return if record.state==target {Ok(())} else {Err(BotLedgerError::Conflict)};
            }
            if attempts::latest(tx,run)?.is_some_and(|a|!a.retired || !a.phase.is_terminal()) {return Err(BotLedgerError::Fenced)}
            tx.execute("UPDATE bot_admissions SET state=?2,active_elapsed_ms=?3,active_since_ms=NULL,updated_at_ms=?4 WHERE run_id=?1",
                params![run.to_storage(),if outcome==BotSettlement::Settled {"settled"} else {"refused"},charge(&record,now),now]).map_err(sql)?;
            // Cancelled requests must not be recovered as ordinary runnable intents.
            tx.execute("UPDATE run_intents SET state='abandoned',updated_at_ms=?2 WHERE run_id=?1 AND state='pending'",params![run.to_storage(),now]).map_err(sql)?;
            Ok(())
        })
    }
    fn prepare_bot_initial_send(
        &mut self,
        run: RunId,
        send: &BotInitialSend,
        proof: Option<&FreshBotPolicyProof>,
    ) -> Result<BotInitialSend, BotLedgerError> {
        if send.turn.is_some() {
            return Err(BotLedgerError::Invalid);
        }
        self.write_with_error(WriteOp::PrepareBotInitialSend,|tx,now| {
            let record=super::routines::native_admission(tx,run)?;
            super::routines::execution::retained(tx,&record)?;
            if let Some(old)=record.initial_send.as_ref() {
                if old.attempt==send.attempt && old.generation==send.generation && old.key==send.key && old.fingerprint==send.fingerprint {return Ok(old.clone())}
                return Err(BotLedgerError::Fenced);
            }
            super::routines::execution::fresh(tx,&record,proof,false)?;
            current(tx,run,send.attempt,send.generation,true)?;
            if send.generation!=AttemptGeneration::FIRST || record.state!=BotAdmissionState::Executing || record.active_elapsed_at(now)>=BOT_ACTIVE_BUDGET_MS {return Err(BotLedgerError::Fenced)}
            tx.execute("UPDATE bot_admissions SET initial_send_key=?2,initial_send_fingerprint=?3,initial_send_attempt_id=?4,initial_send_generation=?5,updated_at_ms=?6 WHERE run_id=?1",
                params![run.to_storage(),send.key.to_string(),send.fingerprint.as_slice(),send.attempt.to_storage(),send.generation.get(),now]).map_err(sql)?;
            Ok(send.clone())
        })
    }
    fn acknowledge_bot_initial_send(
        &mut self,
        run: RunId,
        send: &BotInitialSend,
    ) -> Result<BotInitialSend, BotLedgerError> {
        if send.turn.is_none() {
            return Err(BotLedgerError::Invalid);
        }
        self.write_with_error(WriteOp::AcknowledgeBotInitialSend,|tx,now| {
            let record=super::routines::native_admission(tx,run)?;
            super::routines::execution::retained(tx,&record)?;
            let old=record.initial_send.as_ref().ok_or(BotLedgerError::Fenced)?;
            if old.attempt!=send.attempt || old.generation!=send.generation || old.key!=send.key || old.fingerprint!=send.fingerprint {return Err(BotLedgerError::Fenced)}
            if old.turn.is_some() {
                if old!=send { return Err(BotLedgerError::Conflict); }
                super::routines::execution::acknowledged(tx,&record,send)?;
                return Ok(old.clone());
            }
            current(tx,run,send.attempt,send.generation,false)?;
            tx.execute("UPDATE bot_admissions SET initial_send_turn_id=?2,updated_at_ms=?3 WHERE run_id=?1",params![run.to_storage(),send.turn.map(|t|t.to_string()),now]).map_err(sql)?;
            super::routines::execution::acknowledged(tx,&record,send)?;
            Ok(send.clone())
        })
    }
    fn record_bot_delivery(
        &mut self,
        receipt: &BotDeliveryReceipt,
    ) -> Result<koru_delegate_core::EventId, BotLedgerError> {
        if !receipt.valid() {
            return Err(BotLedgerError::Invalid);
        }
        self.write_with_error(WriteOp::RecordBotDelivery, |tx, now| {
            let record = required(tx, receipt.run)?;
            if let Some(old) = record.delivery {
                if old != *receipt { return Err(BotLedgerError::Conflict); }
                if let Some(event) = record.delivery_publication { return Ok(event); }
                // A runs5 confirmed receipt has no publication ID yet. Repair
                // only this exact identity, under the same transaction/crash seam.
                let event = koru_delegate_core::EventId::mint();
                tx.execute("UPDATE bot_admissions SET delivery_publication_event_id=?2, updated_at_ms=?3 WHERE run_id=?1",
                    params![receipt.run.to_storage(), event.to_string(), now]).map_err(sql)?;
                return Ok(event);
            }
            let p = &record.submission.provenance;
            if receipt.chat_id != p.chat_id || Some(receipt.ai_attempt_id) != p.ai_attempt_id
                || Some(receipt.assistant_message_id) != p.assistant_message_id {
                return Err(BotLedgerError::Fenced);
            }
            current(tx, receipt.run, receipt.attempt, receipt.generation, false)?;
            let send = record.initial_send.ok_or(BotLedgerError::Fenced)?;
            if send.turn.is_none() || record.state == BotAdmissionState::Refused {
                return Err(BotLedgerError::Fenced);
            }
            // The immutable original send, or the exact recorded successor whose
            // continuation input was acknowledged. The original InitialSend is
            // never rewritten and a newer generation alone never qualifies.
            let original = send.attempt == receipt.attempt && send.generation == receipt.generation;
            if !original {
                let latest = attempts::latest(tx, receipt.run)?.ok_or(BotLedgerError::Fenced)?;
                if !crate::decisions::acknowledged_continuation_on(tx, receipt.run, &latest)? {
                    return Err(BotLedgerError::Fenced);
                }
            }
            let event = koru_delegate_core::EventId::mint();
            tx.execute("UPDATE bot_admissions SET delivery_outcome='confirmed',delivery_attempt_id=?2,delivery_generation=?3,delivery_chat_id=?4,delivery_ai_attempt_id=?5,delivery_assistant_message_id=?6,delivery_confirmed_at_ms=?7,delivery_publication_event_id=?8,updated_at_ms=?9 WHERE run_id=?1",
                params![receipt.run.to_storage(), receipt.attempt.to_storage(), receipt.generation.get(),
                    receipt.chat_id.to_string(), receipt.ai_attempt_id.to_string(), receipt.assistant_message_id.to_string(),
                    receipt.confirmed_at_ms, event.to_string(), now]).map_err(sql)?;
            Ok(event)
        })
    }
}
