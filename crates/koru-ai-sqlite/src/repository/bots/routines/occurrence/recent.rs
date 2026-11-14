// SPDX-License-Identifier: MPL-2.0
//! Normalized local materialization references, never capture/claim authority.
use super::*;
use koru_delegate_core::HandoffId;

#[derive(Clone, PartialEq, Eq)]
struct Candidate {
    occurrence: i64,
    definition_revision: u32,
    definition_digest: RoutineDigest32,
    handoff: Option<HandoffId>,
    semantic: RoutineDigest32,
    preparation: RoutineDigest32,
    ids: RoutineExecutionIds,
    config: u64,
    policy: u64,
    grant: u64,
    created: i64,
    updated: i64,
}
#[derive(Clone, PartialEq, Eq)]
struct Reference {
    candidate: Candidate,
    stage: BotReceiptStage,
    outcome: Option<BotExecutionOutcome>,
    receipt_created: i64,
    receipt_updated: i64,
    profile: ProfileId,
    visibility: BotRoutineConversationVisibility,
}
#[derive(Clone, PartialEq, Eq)]
pub(in super::super) struct RecentSnapshot(Vec<Reference>);
impl RecentSnapshot {
    pub(in super::super) fn observation(&self) -> BotRoutineLocalConversations {
        BotRoutineLocalConversations::Available {
            more: self.0.len() == 5,
            items: self
                .0
                .iter()
                .take(4)
                .map(|r| BotRoutineLocalConversation {
                    occurrence_ms: r.candidate.occurrence,
                    run_id: r.candidate.ids.run,
                    chat_id: r.candidate.ids.chat,
                    attempt_id: r.candidate.ids.attempt,
                    conversation: r.visibility,
                })
                .collect(),
        }
    }
}

// All identifiers here are static column names owned in this module, never
// request text. CASE bounds allocation even when CHECK constraints are bypassed.
fn text(column: &str, bytes: usize) -> String {
    format!(
        "CASE WHEN typeof({column})='text' AND length(CAST({column} AS BLOB))={bytes} THEN {column} END"
    )
}
fn integer(column: &str, minimum: i64) -> String {
    format!("CASE WHEN typeof({column})='integer' AND {column}>={minimum} THEN {column} END")
}
fn digest(column: &str) -> String {
    format!("CASE WHEN typeof({column})='blob' AND length({column})=32 THEN {column} END")
}
pub(super) fn candidates_sql() -> String {
    let columns = [
        integer("occurrence_ms", i64::MIN), integer("definition_revision", 1), digest("definition_digest"),
        "CASE WHEN owner_handoff IS NULL OR (typeof(owner_handoff)='text' AND length(CAST(owner_handoff AS BLOB))=36) THEN owner_handoff ELSE 1 END".into(),
        digest("semantic_hash"), digest("preparation_hash"),
        text("submission_request_id",36), text("run_id",36), text("chat_id",36), text("attempt_id",36),
        text("user_message_id",36), text("assistant_message_id",36),
        integer("config_revision",1), integer("policy_revision",1), integer("grant_revision",1),
        text("linked_request_id",36),text("linked_chat_id",36), "close_reason IS NULL".into(),
        integer("created_at_ms",i64::MIN),integer("updated_at_ms",i64::MIN),
    ];
    format!(
        "SELECT {} FROM bot_routine_preparations INDEXED BY bot_routine_conversations WHERE user_scope_id=?1 AND owner_device_id=?2 AND bot_id=?3 AND schedule_id=?4 AND stage='linked' ORDER BY occurrence_ms DESC LIMIT 5",
        columns.join(",")
    )
}

pub(in super::super) fn read(
    repository: &Repository,
    owner: BotOwner,
    bot: BotId,
    schedule: ScheduleId,
    home: WorkspaceId,
) -> Result<RecentSnapshot, BotError> {
    let conn = &repository.connection;
    let mut query = conn.prepare_cached(&candidates_sql()).map_err(bad)?;
    let mut rows = query
        .query(params![
            owner.user_scope.to_storage(),
            owner.device.to_storage(),
            bot.to_storage(),
            schedule.to_storage()
        ])
        .map_err(bad)?;
    let mut candidates = Vec::with_capacity(5);
    // Select the five candidates before any joins. Nothing below can filter a
    // corrupt candidate out or backfill an older materialization.
    while let Some(r) = rows.next().map_err(bad)? {
        let s = |i| r.get::<_, String>(i).map_err(bad);
        let hash = |i| -> Result<RoutineDigest32, BotError> {
            Ok(RoutineDigest32::from_bytes(
                r.get::<_, Vec<u8>>(i)
                    .map_err(bad)?
                    .try_into()
                    .map_err(bad)?,
            ))
        };
        let ids = RoutineExecutionIds {
            submission: s(6)?.parse().map_err(bad)?,
            run: s(7)?.parse().map_err(bad)?,
            chat: s(8)?.parse().map_err(bad)?,
            attempt: s(9)?.parse().map_err(bad)?,
            user_message: s(10)?.parse().map_err(bad)?,
            assistant_message: s(11)?.parse().map_err(bad)?,
        };
        let c = Candidate {
            occurrence: r.get(0).map_err(bad)?,
            definition_revision: r.get(1).map_err(bad)?,
            definition_digest: hash(2)?,
            handoff: r
                .get::<_, Option<String>>(3)
                .map_err(bad)?
                .map(|s| s.parse().map_err(bad))
                .transpose()?,
            semantic: hash(4)?,
            preparation: hash(5)?,
            ids,
            config: r.get(12).map_err(bad)?,
            policy: r.get(13).map_err(bad)?,
            grant: r.get(14).map_err(bad)?,
            created: r.get(18).map_err(bad)?,
            updated: r.get(19).map_err(bad)?,
        };
        if !c.ids.valid()
            || c.updated < c.created
            || s(15)? != c.ids.submission.to_storage()
            || s(16)? != c.ids.chat.to_string()
            || !r.get::<_, bool>(17).map_err(bad)?
        {
            return Err(BotError::Storage);
        }
        candidates.push(c);
    }
    drop(rows);
    let mut result = Vec::with_capacity(candidates.len());
    for c in candidates {
        let (stage,outcome,created,updated):(String,Option<String>,i64,i64) = conn.query_row("SELECT
            CASE WHEN typeof(stage)='text' AND length(CAST(stage AS BLOB))<=16 THEN stage END,
            CASE WHEN outcome_json IS NULL OR (typeof(outcome_json)='text' AND length(CAST(outcome_json AS BLOB)) BETWEEN 1 AND 1048576) THEN outcome_json ELSE 1 END,
            CASE WHEN typeof(created_at_ms)='integer' THEN created_at_ms END,
            CASE WHEN typeof(updated_at_ms)='integer' AND updated_at_ms>=created_at_ms THEN updated_at_ms END
            FROM bot_command_receipts WHERE user_scope_id=?1 AND request_id=?2 AND bot_id=?3
            AND command_kind='routine_occurrence' AND typeof(submission_version)='integer' AND submission_version=2
            AND planned_run_id=?4 AND typeof(captured_config_revision)='integer' AND captured_config_revision=?5
            AND typeof(captured_policy_revision)='integer' AND captured_policy_revision=?6 AND fingerprint=?7
            AND expected_revision IS NULL AND desired_pause IS NULL AND pause_ack_stage IS NULL",
            params![owner.user_scope.to_storage(),c.ids.submission.to_storage(),bot.to_storage(),c.ids.run.to_storage(),c.config,c.policy,String::from(c.semantic)],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(bad)?;
        let stage = BotReceiptStage::parse(&stage).ok_or(BotError::Storage)?;
        let outcome: Option<BotExecutionOutcome> = outcome
            .map(|s| serde_json::from_str(&s).map_err(bad))
            .transpose()?;
        if let Some(value) = &outcome {
            value
                .validate_identity(
                    koru_delegate_core::BotOwner {
                        user_scope: owner.user_scope,
                        device: owner.device,
                    },
                    bot,
                    c.ids.submission,
                    c.ids.run,
                    c.config,
                )
                .map_err(bad)?;
        }
        admission::validate_outcome_stage(stage, &outcome)?;
        let correlations:i64 = conn.query_row("SELECT count(*) FROM bot_chat_runs WHERE run_id=?1 OR (user_scope_id=?2 AND submission_id=?3) OR ai_attempt_id=?4",
            params![c.ids.run.to_storage(),owner.user_scope.to_storage(),c.ids.submission.to_storage(),c.ids.attempt.to_string()], |r|r.get(0)).map_err(bad)?;
        if correlations != 1 {
            return Err(BotError::Storage);
        }
        let association = "SELECT CASE WHEN typeof(h.profile_id)='text' AND length(CAST(h.profile_id AS BLOB))=36 THEN h.profile_id END
            FROM bot_chats c JOIN chats h ON h.id=c.chat_id JOIN bot_chat_runs b ON b.chat_id=c.chat_id AND b.bot_id=c.bot_id
            JOIN inference_attempts a ON a.id=b.ai_attempt_id JOIN messages u ON u.id=?11 JOIN messages m ON m.id=b.assistant_message_id
            WHERE c.bot_id=?1 AND c.chat_id=?2 AND b.user_scope_id=?3 AND b.owner_device_id=?4 AND b.submission_id=?5
            AND b.run_id=?6 AND b.ai_attempt_id=?7 AND b.assistant_message_id=?8 AND b.record_workspace_id=?9
            AND b.continued_from_run_id IS NULL AND a.operation_id=?10 AND a.chat_id=c.chat_id
            AND a.assistant_message_id=b.assistant_message_id AND u.chat_id=c.chat_id AND u.role='user'
            AND m.chat_id=c.chat_id AND m.role='assistant' LIMIT 2";
        let mut q = conn.prepare_cached(association).map_err(bad)?;
        let mut r = q
            .query(params![
                bot.to_storage(),
                c.ids.chat.to_string(),
                owner.user_scope.to_storage(),
                owner.device.to_storage(),
                c.ids.submission.to_storage(),
                c.ids.run.to_storage(),
                c.ids.attempt.to_string(),
                c.ids.assistant_message.to_string(),
                home.to_storage(),
                c.ids.submission.as_uuid().to_string(),
                c.ids.user_message.to_string()
            ])
            .map_err(bad)?;
        let profile: String = r
            .next()
            .map_err(bad)?
            .ok_or(BotError::Storage)?
            .get(0)
            .map_err(bad)?;
        let profile = ProfileId::parse(&profile).ok_or(BotError::Storage)?;
        if r.next().map_err(bad)?.is_some() {
            return Err(BotError::Storage);
        }
        let chat = ChatId::parse(&c.ids.chat.to_string()).ok_or(BotError::Storage)?;
        let visibility = if repository.bot_chat_hold(chat)?.is_none() {
            BotRoutineConversationVisibility::Available
        } else {
            BotRoutineConversationVisibility::Unavailable
        };
        result.push(Reference {
            candidate: c,
            stage,
            outcome,
            receipt_created: created,
            receipt_updated: updated,
            profile,
            visibility,
        });
    }
    Ok(RecentSnapshot(result))
}
