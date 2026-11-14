// SPDX-License-Identifier: MPL-2.0
//! Stable Memory identities on the existing writer; no Store effects or content.
use super::{repository, rows, *};
use crate::{RunsDb, WriteOp};
use rusqlite::{Connection, OptionalExtension, Row, params};

fn corrupt(column: &'static str) -> BotLedgerError {
    crate::LedgerError::Corrupt {
        table: "bot_memory_edit_allocations",
        column,
    }
    .into()
}
fn cell<T: rusqlite::types::FromSql>(
    row: &Row<'_>,
    column: &'static str,
) -> Result<T, BotLedgerError> {
    row.get(column).map_err(|_| corrupt(column))
}
fn node(raw: &str, column: &'static str) -> Result<Uuid, BotLedgerError> {
    let id = Uuid::parse_str(raw).map_err(|_| corrupt(column))?;
    if id.to_string() != raw {
        return Err(corrupt(column));
    }
    Ok(id)
}
fn id<T: std::str::FromStr>(row: &Row<'_>, column: &'static str) -> Result<T, BotLedgerError> {
    let raw: String = cell(row, column)?;
    node(&raw, column)?;
    raw.parse().map_err(|_| corrupt(column))
}
fn bounded_json(raw: String, column: &'static str) -> Result<String, BotLedgerError> {
    if raw.is_empty() || raw.len() > 4096 {
        return Err(corrupt(column));
    }
    Ok(raw)
}
fn encoded(value: Result<String, serde_json::Error>) -> Result<String, BotLedgerError> {
    let raw = value.map_err(|_| BotLedgerError::Invalid)?;
    if raw.len() > 4096 {
        return Err(BotLedgerError::Invalid);
    }
    Ok(raw)
}
fn action(value: MemoryEditAction) -> &'static str {
    match value {
        MemoryEditAction::Remember => "remember",
        MemoryEditAction::Correct => "correct",
        MemoryEditAction::Forget => "forget",
    }
}
fn decode(row: &Row<'_>) -> Result<MemoryEditAllocation, BotLedgerError> {
    let subject: MemoryEditSubjectV1 =
        serde_json::from_str(&bounded_json(cell(row, "subject_json")?, "subject_json")?)
            .map_err(|_| corrupt("subject_json"))?;
    if cell::<String>(row, "action")? != action(subject.action) {
        return Err(corrupt("action"));
    }
    let requested: Option<String> = cell(row, "requested_version")?;
    let base = match (cell::<String>(row, "base_kind")?.as_str(), requested) {
        ("history", Some(raw)) => {
            node(&raw, "requested_version")?;
            MemoryBaseSelector::History {
                version: serde_json::from_value(raw.into())
                    .map_err(|_| corrupt("requested_version"))?,
            }
        }
        ("original_seed", None) => MemoryBaseSelector::OriginalSeed {},
        _ => return Err(corrupt("base_kind")),
    };
    let ensure = match (
        cell::<Option<String>>(row, "ensure_request_id")?,
        cell::<Option<i64>>(row, "ensure_expected_revision")?,
        cell::<Option<String>>(row, "ensure_title")?,
    ) {
        (None, None, None) => None,
        (Some(raw), Some(revision), Some(title)) => {
            node(&raw, "ensure_request_id")?;
            Some(MemoryEnsureIdentity {
                request_id: serde_json::from_value(raw.into())
                    .map_err(|_| corrupt("ensure_request_id"))?,
                expected_revision: u64::try_from(revision)
                    .map_err(|_| corrupt("ensure_expected_revision"))?,
                title,
            })
        }
        _ => return Err(corrupt("ensure_cohort")),
    };
    let resolution = match (
        cell::<String>(row, "resolution_state")?.as_str(),
        cell::<Option<String>>(row, "authority_json")?,
        cell::<Option<String>>(row, "expected_version")?,
        cell::<Option<Vec<u8>>>(row, "expected_edit_revision")?,
    ) {
        ("unresolved", None, None, None) => MemoryBaseResolution::Unresolved {},
        ("resolved", Some(authority), Some(version), Some(expected_revision)) => {
            node(&version, "expected_version")?;
            MemoryBaseResolution::Resolved {
                authority: serde_json::from_str(&bounded_json(authority, "authority_json")?)
                    .map_err(|_| corrupt("authority_json"))?,
                version: serde_json::from_value(version.into())
                    .map_err(|_| corrupt("expected_version"))?,
                expected_revision,
            }
        }
        _ => return Err(corrupt("resolution_cohort")),
    };
    let value = MemoryEditAllocation {
        key: NativeResultKey {
            run: id(row, "run_id")?,
            attempt: id(row, "attempt_id")?,
            generation: u32::try_from(cell::<i64>(row, "generation")?)
                .ok()
                .and_then(AttemptGeneration::from_raw)
                .ok_or_else(|| corrupt("generation"))?,
            turn: id(row, "turn_id")?,
            operation_id: node(&cell::<String>(row, "operation_id")?, "operation_id")?,
        },
        caller_fingerprint: cell::<Vec<u8>>(row, "caller_fingerprint")?
            .try_into()
            .map_err(|_| corrupt("caller_fingerprint"))?,
        subject,
        base,
        ensure,
        resolution,
        source_run_document: DocumentId::from_uuid(node(
            &cell::<String>(row, "source_run_document_id")?,
            "source_run_document_id",
        )?),
        fact_id: node(&cell::<String>(row, "fact_node_id")?, "fact_node_id")?,
        detail_id: cell::<Option<String>>(row, "detail_node_id")?
            .map(|s| node(&s, "detail_node_id"))
            .transpose()?,
        source_id: cell::<Option<String>>(row, "source_node_id")?
            .map(|s| node(&s, "source_node_id"))
            .transpose()?,
        created_at_ms: cell(row, "created_at_ms")?,
    };
    if !value.valid() {
        return Err(corrupt("allocation_cohort"));
    }
    Ok(value)
}

pub(super) fn exists(
    conn: &Connection,
    run: RunId,
    operation: Uuid,
) -> Result<bool, BotLedgerError> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM bot_memory_edit_allocations WHERE run_id=?1 AND operation_id=?2)", params![run.to_storage(), operation.to_string()], |r| r.get(0)).map_err(rows::sql)
}
fn native_exists(conn: &Connection, run: RunId, operation: Uuid) -> Result<bool, BotLedgerError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_result_intents WHERE run_id=?1 AND operation_id=?2)",
        params![run.to_storage(), operation.to_string()],
        |r| r.get(0),
    )
    .map_err(rows::sql)
}
pub(super) fn read(
    conn: &Connection,
    run: RunId,
    operation: Uuid,
) -> Result<Option<MemoryEditAllocation>, BotLedgerError> {
    let value = conn
        .query_row(
            "SELECT * FROM bot_memory_edit_allocations WHERE run_id=?1 AND operation_id=?2",
            params![run.to_storage(), operation.to_string()],
            |r| Ok(decode(r)),
        )
        .optional()
        .map_err(rows::sql)?
        .transpose()?;
    if let Some(value) = &value {
        let subject = &value.subject;
        let matches: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM bot_admissions b JOIN run_intents i ON i.run_id=b.run_id JOIN run_attempts a ON a.run_id=b.run_id AND a.generation=?3 WHERE b.run_id=?1 AND b.user_scope=?4 AND b.device_id=?5 AND b.bot_id=?6 AND b.request_id=?7 AND b.config_revision=?8 AND i.state='satisfied' AND i.run_document_id=?9 AND a.attempt_id=?2 AND b.initial_send_attempt_id=?2 AND b.initial_send_generation=?3 AND b.initial_send_turn_id=?10)",
            params![run.to_storage(),value.key.attempt.to_storage(),value.key.generation.get(),subject.owner.user_scope.to_storage(),subject.owner.device.to_storage(),subject.bot.to_storage(),subject.submission.to_storage(),subject.captured_config_revision,value.source_run_document.to_storage(),value.key.turn.to_string()], |r|r.get(0),
        ).map_err(rows::sql)?;
        if !matches {
            return Err(corrupt("admission_binding"));
        }
        if let MemoryBaseResolution::Resolved { authority, .. } = &value.resolution {
            let matches: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM run_intents WHERE run_id=?1 AND workspace_id=?2)",
                    params![run.to_storage(), authority.workspace.to_storage()],
                    |r| r.get(0),
                )
                .map_err(rows::sql)?;
            if !matches {
                return Err(corrupt("workspace_binding"));
            }
        }
    }
    Ok(value)
}
pub(super) fn lookup(
    conn: &Connection,
    run: RunId,
    operation: Uuid,
    fingerprint: [u8; 32],
) -> Result<Option<MemoryEditAllocation>, BotLedgerError> {
    if operation.get_version_num() != 4 || operation.get_variant() != uuid::Variant::RFC4122 {
        return Err(BotLedgerError::Invalid);
    }
    let value = read(conn, run, operation)?;
    if value
        .as_ref()
        .is_some_and(|v| v.caller_fingerprint != fingerprint)
        || value.is_none() && native_exists(conn, run, operation)?
    {
        return Err(BotLedgerError::Conflict);
    }
    Ok(value)
}
fn exact(
    conn: &Connection,
    key: &NativeResultKey,
    fingerprint: [u8; 32],
) -> Result<MemoryEditAllocation, BotLedgerError> {
    let value =
        lookup(conn, key.run, key.operation_id, fingerprint)?.ok_or(BotLedgerError::Invalid)?;
    if value.key != *key {
        return Err(BotLedgerError::Conflict);
    }
    Ok(value)
}

pub(super) fn allocate(
    db: &mut RunsDb,
    request: &AllocateMemoryEditRequest,
    source: DocumentId,
    fence: &BotExecutionFence,
    fresh: &FreshBotPolicyProof,
) -> Result<MemoryEditAllocation, BotLedgerError> {
    if !request.valid() || source.as_uuid().is_nil() {
        return Err(BotLedgerError::Invalid);
    }
    let admitted = repository::admission(db, request.key.run)?;
    if !request.subject.matches(&admitted.submission.provenance) {
        return Err(BotLedgerError::Fenced);
    }
    db.write_with_error(WriteOp::AllocateMemoryEdit,|tx,now|{
        if let Some(old)=lookup(tx,request.key.run,request.key.operation_id,request.caller_fingerprint)? {
            if old.key!=request.key || old.subject!=request.subject || old.base!=request.base
                || old.source_run_document!=source
                || request.existing_fact_id.is_some_and(|id|id!=old.fact_id)
            {return Err(BotLedgerError::Conflict);}
            return Ok(old);
        }
        repository::fresh_policy(tx,&request.key,&admitted,fence,fresh)?;
        if request.current_config_revision!=fresh.global.config_revision || request.current_policy_revision!=fresh.global.policy_revision {return Err(BotLedgerError::Stale);}
        let source_matches:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM run_intents WHERE run_id=?1 AND workspace_id=?2 AND state='satisfied' AND run_document_id=?3)",params![request.key.run.to_storage(),admitted.submission.workspace.to_storage(),source.to_storage()],|r|r.get(0)).map_err(rows::sql)?;
        if !source_matches {return Err(BotLedgerError::Fenced);}
        let (all,unlinked):(i64,i64)=tx.query_row("SELECT count(*),coalesce(sum(n.run_id IS NULL),0) FROM bot_memory_edit_allocations a LEFT JOIN native_result_intents n ON n.run_id=a.run_id AND n.operation_id=a.operation_id WHERE a.run_id=?1",[request.key.run.to_storage()],|r|Ok((r.get(0)?,r.get(1)?))).map_err(rows::sql)?;
        if all>=MEMORY_EDIT_ALLOCATION_LIMIT as i64 || unlinked>=MEMORY_EDIT_UNLINKED_LIMIT as i64 {return Err(BotLedgerError::Capacity);}
        let ensure=matches!(request.base,MemoryBaseSelector::OriginalSeed {}).then(||MemoryEnsureIdentity { request_id:BotRequestId::mint(), expected_revision:request.current_config_revision,title:MEMORY_ENSURE_TITLE.into() });
        let has_children=request.subject.action!=MemoryEditAction::Forget;
        let value=MemoryEditAllocation {
            key:request.key.clone(),caller_fingerprint:request.caller_fingerprint,subject:request.subject.clone(),base:request.base.clone(),ensure,
            resolution:MemoryBaseResolution::Unresolved {},source_run_document:source,
            fact_id:request.existing_fact_id.unwrap_or_else(Uuid::new_v4),
            detail_id:has_children.then(Uuid::new_v4),source_id:has_children.then(Uuid::new_v4),created_at_ms:now,
        };
        if !value.valid() {return Err(BotLedgerError::Invalid);}
        let (base,version)=match value.base {MemoryBaseSelector::History{version}=>("history",Some(version.to_storage())),MemoryBaseSelector::OriginalSeed {}=>("original_seed",None)};
        tx.execute("INSERT INTO bot_memory_edit_allocations(run_id,operation_id,attempt_id,generation,turn_id,caller_fingerprint,subject_json,action,base_kind,requested_version,ensure_request_id,ensure_expected_revision,ensure_title,resolution_state,source_run_document_id,fact_node_id,detail_node_id,source_node_id,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'unresolved',?14,?15,?16,?17,?18)",params![value.key.run.to_storage(),value.key.operation_id.to_string(),value.key.attempt.to_storage(),value.key.generation.get(),value.key.turn.to_string(),value.caller_fingerprint.as_slice(),encoded(serde_json::to_string(&value.subject))?,action(value.subject.action),base,version,value.ensure.as_ref().map(|e|e.request_id.to_storage()),value.ensure.as_ref().map(|e|e.expected_revision),value.ensure.as_ref().map(|e|e.title.as_str()),source.to_storage(),value.fact_id.to_string(),value.detail_id.map(|id|id.to_string()),value.source_id.map(|id|id.to_string()),now]).map_err(rows::sql)?;
        exact(tx,&value.key,value.caller_fingerprint)
    })
}

pub(super) fn resolve(
    db: &mut RunsDb,
    key: &NativeResultKey,
    fingerprint: [u8; 32],
    resolution: &MemoryBaseResolution,
    fence: &BotExecutionFence,
    fresh: &FreshBotPolicyProof,
) -> Result<MemoryEditAllocation, BotLedgerError> {
    let MemoryBaseResolution::Resolved {
        authority,
        version,
        expected_revision,
    } = resolution
    else {
        return Err(BotLedgerError::Invalid);
    };
    if !authority.valid()
        || version.as_uuid().is_nil()
        || !same_native_edit_revision(expected_revision, expected_revision)
            .map_err(|_| BotLedgerError::Invalid)?
    {
        return Err(BotLedgerError::Invalid);
    }
    let admitted = repository::admission(db, key.run)?;
    db.write_with_error(WriteOp::ResolveMemoryEditBase,|tx,_|{
        let old=exact(tx,key,fingerprint)?;
        if authority.subject()!=old.subject || authority.workspace!=admitted.submission.workspace {return Err(BotLedgerError::Conflict);}
        if let MemoryBaseResolution::Resolved {authority:prior,version:prior_version,expected_revision:prior_revision}=&old.resolution {
            return if prior==authority && prior_version==version && same_native_edit_revision(prior_revision,expected_revision).map_err(|_|corrupt("expected_edit_revision"))? {Ok(old)} else {Err(BotLedgerError::Conflict)};
        }
        let mut proposed=old.clone(); proposed.resolution=resolution.clone();
        if !proposed.valid() {return Err(BotLedgerError::Invalid);}
        repository::fresh_policy(tx,key,&admitted,fence,fresh)?;
        let updated=tx.execute("UPDATE bot_memory_edit_allocations SET resolution_state='resolved',authority_json=?3,expected_version=?4,expected_edit_revision=?5 WHERE run_id=?1 AND operation_id=?2 AND resolution_state='unresolved'",params![key.run.to_storage(),key.operation_id.to_string(),encoded(serde_json::to_string(authority))?,version.to_storage(),expected_revision]).map_err(rows::sql)?;
        if updated!=1 {return Err(BotLedgerError::Conflict);}
        exact(tx,key,fingerprint)
    })
}

pub(super) fn prepare(
    db: &mut RunsDb,
    request: &PrepareNativeResult,
    fingerprint: [u8; 32],
    authority: &MemoryEditAuthorityV1,
) -> Result<NativeIntentRecord, BotLedgerError> {
    if !request.valid() || request.kind != NativeResultOperation::EditPage || !authority.valid() {
        return Err(BotLedgerError::Invalid);
    }
    let admitted = repository::admission(db, request.key.run)?;
    db.write_with_error(WriteOp::PrepareMemoryEdit,|tx,now|{
        let allocation=exact(tx,&request.key,fingerprint)?;
        let MemoryBaseResolution::Resolved{authority:resolved,expected_revision,..}=&allocation.resolution else{return Err(BotLedgerError::Invalid);};
        if resolved!=authority || request.destination!=authority.workspace || request.target!=Some(authority.memory_document)
            || request.expected_revision.as_ref()!=Some(expected_revision) {return Err(BotLedgerError::Conflict);}
        if let Some(old)=rows::read(tx,&request.key)?{
            return if old.purpose==(NativeEffectPurpose::Memory{authority:authority.clone()}) && repository::same_request(&old,request){Ok(old)}else{Err(BotLedgerError::Conflict)};
        }
        repository::current_attempt(tx,&request.key,&admitted)?;
        tx.execute("INSERT INTO native_result_intents(run_id,operation_id,attempt_id,generation,turn_id,kind,request_fingerprint,destination_workspace_id,target_document_id,store_request_id,expected_revision,requested_title,stage,lease_state,created_at_ms,updated_at_ms,effect_purpose,memory_authority_json) VALUES(?1,?2,?3,?4,?5,'edit_page',?6,?7,?8,?9,?10,?11,'prepared','none',?12,?12,'memory',?13)",params![request.key.run.to_storage(),request.key.operation_id.to_string(),request.key.attempt.to_storage(),request.key.generation.get(),request.key.turn.to_string(),request.request_fingerprint.as_slice(),request.destination.to_storage(),authority.memory_document.to_storage(),Uuid::new_v4().to_string(),expected_revision,request.title,now,encoded(serde_json::to_string(authority))?]).map_err(rows::sql)?;
        rows::required(tx,&request.key)
    })
}

pub(super) fn validate_record(
    conn: &Connection,
    record: &NativeIntentRecord,
) -> Result<(), BotLedgerError> {
    let NativeEffectPurpose::Memory { authority } = &record.purpose else {
        return Ok(());
    };
    let allocation = read(conn, record.key.run, record.key.operation_id)?
        .ok_or_else(|| corrupt("missing_allocation"))?;
    let MemoryBaseResolution::Resolved {
        authority: resolved,
        expected_revision,
        ..
    } = &allocation.resolution
    else {
        return Err(corrupt("resolution_state"));
    };
    if allocation.key != record.key
        || resolved != authority
        || record.kind != NativeResultOperation::EditPage
        || record.destination != authority.workspace
        || record.target != authority.memory_document
        || record.expected_revision.as_ref() != Some(expected_revision)
        || record.lease.as_ref().is_some_and(|lease| {
            lease.policy.key.owner != authority.owner || lease.policy.key.bot != authority.bot
        })
    {
        return Err(corrupt("effect_binding"));
    }
    Ok(())
}
