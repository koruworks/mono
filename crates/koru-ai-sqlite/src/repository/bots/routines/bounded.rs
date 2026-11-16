// SPDX-License-Identifier: MPL-2.0
//! Bound SQL values before allocation/JSON decoding; malformed rows are errors.
use super::*;
use koru_delegate_core::{CapturedBotPolicy, RoutineDigest32};

pub(super) struct Receipt {
    fingerprint: RoutineDigest32,
    expected: u64,
    pub stage: String,
    pub outcome: BotRoutineCreationReceipt,
}

pub(super) fn receipt(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    request: BotRequestId,
) -> Result<Option<Receipt>, BotError> {
    // The first projection is only the kind/subject. Cross-kind collision must
    // not decode or allocate that other command's possibly much larger body.
    let header: Option<(String,String)> = conn.query_row("SELECT
        CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END,
        CASE WHEN typeof(command_kind)='text' AND length(CAST(command_kind AS BLOB))<=32 THEN command_kind END
        FROM bot_command_receipts WHERE user_scope_id=?1 AND request_id=?2",
        params![owner.user_scope.to_storage(),request.to_storage()], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(bad)?;
    let Some((subject, kind)) = header else {
        return Ok(None);
    };
    if subject != bot.to_storage() || kind != "create_routine" {
        return Err(BotError::Conflict);
    }
    let (fingerprint,expected,stage,json):(String,u64,String,String) = conn.query_row("SELECT
        CASE WHEN typeof(fingerprint)='text' AND length(CAST(fingerprint AS BLOB))=64 THEN fingerprint END,
        CASE WHEN typeof(expected_revision)='integer' AND expected_revision>0 THEN expected_revision END,
        CASE WHEN typeof(stage)='text' AND length(CAST(stage AS BLOB))<=16 THEN stage END,
        CASE WHEN typeof(outcome_json)='text' AND length(CAST(outcome_json AS BLOB)) BETWEEN 1 AND 16384 THEN outcome_json END
        FROM bot_command_receipts WHERE user_scope_id=?1 AND request_id=?2
        AND planned_run_id IS NULL AND captured_config_revision IS NULL AND captured_policy_revision IS NULL
        AND submission_version IS NULL AND submission_json IS NULL AND desired_pause IS NULL AND pause_ack_stage IS NULL",
        params![owner.user_scope.to_storage(),request.to_storage()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(bad)?;
    Ok(Some(Receipt {
        fingerprint: fingerprint.try_into().map_err(bad)?,
        expected,
        stage,
        outcome: serde_json::from_str(&json).map_err(bad)?,
    }))
}

pub(super) fn by_request(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    request: BotRequestId,
    receipt: &Receipt,
) -> Result<BotRoutineCreation, BotError> {
    let schedule: String = conn.query_row("SELECT CASE WHEN typeof(schedule_id)='text' AND length(CAST(schedule_id AS BLOB))=36 THEN schedule_id END FROM bot_routines WHERE user_scope_id=?1 AND creation_request_id=?2",
        params![owner.user_scope.to_storage(),request.to_storage()], |r| r.get(0)).map_err(bad)?;
    let schedule = schedule.parse().map_err(bad)?;
    decode(conn, owner, bot, schedule, receipt)?.ok_or(BotError::Storage)
}

pub(super) fn by_schedule(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    schedule: ScheduleId,
) -> Result<Option<BotRoutineCreation>, BotError> {
    let header: Option<(String,String,String,String)> = conn.query_row("SELECT
        CASE WHEN typeof(user_scope_id)='text' AND length(CAST(user_scope_id AS BLOB))=36 THEN user_scope_id END,
        CASE WHEN typeof(owner_device_id)='text' AND length(CAST(owner_device_id AS BLOB))=36 THEN owner_device_id END,
        CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END,
        CASE WHEN typeof(creation_request_id)='text' AND length(CAST(creation_request_id AS BLOB))=36 THEN creation_request_id END
        FROM bot_routines WHERE schedule_id=?1", [schedule.to_storage()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(bad)?;
    let Some((scope, device, subject, request)) = header else {
        return Ok(None);
    };
    if scope != owner.user_scope.to_storage()
        || device != owner.device.to_storage()
        || subject != bot.to_storage()
    {
        return Err(BotError::Forbidden);
    }
    let receipt =
        receipt(conn, owner, bot, request.parse().map_err(bad)?)?.ok_or(BotError::Storage)?;
    decode(conn, owner, bot, schedule, &receipt)
}

fn decode(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    schedule: ScheduleId,
    receipt: &Receipt,
) -> Result<Option<BotRoutineCreation>, BotError> {
    let creation = conn.query_row("SELECT
        CASE WHEN typeof(user_scope_id)='text' AND length(CAST(user_scope_id AS BLOB))=36 THEN user_scope_id END,
        CASE WHEN typeof(owner_device_id)='text' AND length(CAST(owner_device_id AS BLOB))=36 THEN owner_device_id END,
        CASE WHEN typeof(bot_id)='text' AND length(CAST(bot_id AS BLOB))=36 THEN bot_id END,
        CASE WHEN typeof(workspace_id)='text' AND length(CAST(workspace_id AS BLOB))=36 THEN workspace_id END,
        CASE WHEN typeof(creation_request_id)='text' AND length(CAST(creation_request_id AS BLOB))=36 THEN creation_request_id END,
        CASE WHEN typeof(store_request_id)='text' AND length(CAST(store_request_id AS BLOB))=36 THEN store_request_id END,
        CASE WHEN typeof(seed_json)='text' AND length(CAST(seed_json AS BLOB)) BETWEEN 1 AND 4194304 THEN seed_json END,
        CASE WHEN typeof(seed_hash)='blob' AND length(seed_hash)=32 THEN seed_hash END,
        CASE WHEN typeof(seed_title)='text' AND length(CAST(seed_title AS BLOB)) BETWEEN 1 AND 256 THEN seed_title END,
        CASE WHEN typeof(source_bot_policy_revision)='integer' AND source_bot_policy_revision>0 THEN source_bot_policy_revision END,
        CASE WHEN typeof(grant_revision)='integer' AND grant_revision>0 THEN grant_revision END,
        CASE WHEN typeof(grant_json)='text' AND length(CAST(grant_json AS BLOB)) BETWEEN 1 AND 65536 THEN grant_json END,
        CASE WHEN typeof(grant_digest)='blob' AND length(grant_digest)=32 THEN grant_digest END,
        CASE WHEN typeof(creation_state)='text' AND length(CAST(creation_state AS BLOB))<=16 THEN creation_state END,
        CASE WHEN typeof(origin_edit_revision)='blob' AND length(origin_edit_revision) BETWEEN 1 AND 65536 THEN origin_edit_revision END,
        CASE WHEN typeof(refusal)='text' AND length(CAST(refusal AS BLOB))<=32 THEN refusal END,
        created_at_ms,updated_at_ms,
        (origin_edit_revision IS NULL OR (typeof(origin_edit_revision)='blob' AND length(origin_edit_revision) BETWEEN 1 AND 65536))
        AND (refusal IS NULL OR (typeof(refusal)='text' AND length(CAST(refusal AS BLOB))<=32))
        FROM bot_routines WHERE schedule_id=?1",[schedule.to_storage()],|r| {
        let invalid = || rusqlite::Error::InvalidQuery;
        if !r.get::<_,bool>(18)? || r.get::<_,String>(0)? != owner.user_scope.to_storage()
            || r.get::<_,String>(1)? != owner.device.to_storage() || r.get::<_,String>(2)? != bot.to_storage() { return Err(invalid()); }
        let seed: BotRoutineSeed = serde_json::from_str(&r.get::<_,String>(6)?).map_err(|_|invalid())?;
        let grant: BotRoutineGrant = serde_json::from_str(&r.get::<_,String>(11)?).map_err(|_|invalid())?;
        let seed_hash: [u8;32] = r.get::<_,Vec<u8>>(7)?.try_into().map_err(|_|invalid())?;
        let grant_hash: [u8;32] = r.get::<_,Vec<u8>>(12)?.try_into().map_err(|_|invalid())?;
        if seed.name.as_str() != r.get::<_,String>(8)? || grant.source_bot_policy_revision != r.get::<_,u64>(9)?
            || grant.grant_revision != r.get::<_,u64>(10)? || grant.digest.as_bytes() != &grant_hash { return Err(invalid()); }
        Ok(BotRoutineCreation {
            identity: BotRoutineCreationIdentity {
                owner: koru_delegate_core::BotOwner { user_scope: owner.user_scope, device: owner.device }, bot,
                home: WorkspaceId::parse(&r.get::<_,String>(3)?).ok_or_else(invalid)?, schedule,
                creation_request: r.get::<_,String>(4)?.parse().map_err(|_|invalid())?,
                store_request: r.get::<_,String>(5)?.parse().map_err(|_|invalid())?,
                expected_bot_revision: receipt.expected, command_fingerprint: receipt.fingerprint,
                seed, grant, seed_hash: RoutineDigest32::from_bytes(seed_hash),
            },
            state: state(&r.get::<_,String>(13)?).map_err(|_|invalid())?,
            origin_edit_revision: r.get(14)?,
            refusal: r.get::<_,Option<String>>(15)?.map(|v| refusal(&v).map_err(|_|invalid())).transpose()?,
            created_at_ms: r.get(16)?, updated_at_ms: r.get(17)?,
        })
    }).optional().map_err(bad)?;
    if let Some(c) = &creation {
        c.validate().map_err(|_| BotError::Storage)?;
        c.identity
            .validate_digests()
            .map_err(|_| BotError::Storage)?;
        receipt
            .outcome
            .validate(&c.identity)
            .map_err(|_| BotError::Storage)?;
        let home: String = conn.query_row(
            "SELECT CASE WHEN typeof(home_workspace_id)='text' AND length(CAST(home_workspace_id AS BLOB))=36 THEN home_workspace_id END FROM bots WHERE bot_id=?1",
            [bot.to_storage()], |r| r.get(0),
        ).map_err(bad)?;
        if c.updated_at_ms < c.created_at_ms
            || home != c.identity.home.to_storage()
            || receipt.stage != receipt_stage(c)
            || receipt.outcome != receipt_for(c)
        {
            return Err(BotError::Storage);
        }
    }
    Ok(creation)
}

pub(super) fn source(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
) -> Result<BotRoutineCreationSource, BotError> {
    let g = admission::guard(conn, owner, bot)?;
    let (provider,model,reasoning,folder,access):(String,String,String,String,String) = conn.query_row("SELECT
        CASE WHEN typeof(provider)='text' AND length(CAST(provider AS BLOB)) BETWEEN 1 AND 256 THEN provider END,
        CASE WHEN typeof(model)='text' AND length(CAST(model AS BLOB)) BETWEEN 1 AND 512 THEN model END,
        CASE WHEN typeof(reasoning)='text' AND length(CAST(reasoning AS BLOB))<=32 THEN reasoning END,
        CASE WHEN typeof(working_folder)='text' AND length(CAST(working_folder AS BLOB)) BETWEEN 1 AND 32768 THEN working_folder END,
        CASE WHEN typeof(access)='text' AND length(CAST(access AS BLOB))<=16 THEN access END
        FROM bots WHERE bot_id=?1",[bot.to_storage()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(bad)?;
    let main = designated_main(conn, bot)?;
    let profile: String = conn.query_row("SELECT CASE WHEN typeof(profile_id)='text' AND length(CAST(profile_id AS BLOB))=36 THEN profile_id END FROM chats WHERE id=?1",[main.to_storage()],|r|r.get(0)).map_err(bad)?;
    let mut query = conn.prepare("SELECT
        CASE WHEN typeof(workspace_id)='text' AND length(CAST(workspace_id AS BLOB))=36 THEN workspace_id END,
        CASE WHEN typeof(document_id)='text' AND length(CAST(document_id AS BLOB))=36 THEN document_id END
        FROM bot_selected_pages WHERE bot_id=?1 ORDER BY workspace_id,document_id LIMIT 1025").map_err(bad)?;
    let mut selected = std::collections::BTreeSet::new();
    for row in query
        .query_map([bot.to_storage()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(bad)?
    {
        let (workspace, document) = row.map_err(bad)?;
        if workspace != g.home.to_storage()
            || !selected.insert(DocumentId::parse(&document).ok_or(BotError::Storage)?)
            || selected.len() > 1024
        {
            return Err(BotError::Storage);
        }
    }
    Ok(BotRoutineCreationSource {
        owner: koru_delegate_core::BotOwner {
            user_scope: owner.user_scope,
            device: owner.device,
        },
        bot,
        home: g.home,
        profile: ProfileId::parse(&profile).ok_or(BotError::Storage)?,
        revision: g.revision,
        policy_revision: g.policy,
        model: ModelSelection {
            provider: AiProviderId::parse(provider).map_err(bad)?,
            requested_model: model,
            default_reasoning: ReasoningChoice::parse(&reasoning).ok_or(BotError::Storage)?,
        },
        folder,
        policy: CapturedBotPolicy {
            workspace: g.home,
            destination: g.home,
            access: match access.as_str() {
                "default" => koru_delegate_core::BotAccess::Default,
                "workspace" => koru_delegate_core::BotAccess::Workspace,
                _ => return Err(BotError::Storage),
            },
            selected_pages: selected,
            // The Bot's own grants, as its policy holds them (M5). A new
            // Routine never starts from them: see the creation source.
            connections: crate::repository::connections::bot_grants_on(conn, bot)?,
        },
    })
}
