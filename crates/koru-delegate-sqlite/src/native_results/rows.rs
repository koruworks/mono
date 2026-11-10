// SPDX-License-Identifier: MPL-2.0

use super::*;
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::str::FromStr;

pub(super) fn sql(error: rusqlite::Error) -> BotLedgerError {
    crate::LedgerError::sql("native result ledger", error).into()
}
pub(super) fn corrupt(column: &'static str) -> BotLedgerError {
    crate::LedgerError::Corrupt {
        table: "native_result_intents",
        column,
    }
    .into()
}
fn cell<T: rusqlite::types::FromSql>(
    row: &Row<'_>,
    name: &'static str,
) -> Result<T, BotLedgerError> {
    row.get(name).map_err(|_| corrupt(name))
}
fn id<T: FromStr>(row: &Row<'_>, name: &'static str) -> Result<T, BotLedgerError> {
    cell::<String>(row, name)?
        .parse()
        .map_err(|_| corrupt(name))
}
fn positive(row: &Row<'_>, name: &'static str) -> Result<u64, BotLedgerError> {
    let value: i64 = cell(row, name)?;
    u64::try_from(value)
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| corrupt(name))
}
fn digest(row: &Row<'_>, name: &'static str) -> Result<[u8; 32], BotLedgerError> {
    cell::<Vec<u8>>(row, name)?
        .try_into()
        .map_err(|_| corrupt(name))
}
fn uuid(row: &Row<'_>, name: &'static str) -> Result<Uuid, BotLedgerError> {
    let value = id::<Uuid>(row, name)?;
    if value.get_version() != Some(uuid::Version::Random)
        || value.get_variant() != uuid::Variant::RFC4122
    {
        return Err(corrupt(name));
    }
    Ok(value)
}
pub(super) fn policy(row: &Row<'_>) -> Result<NativePolicyRecord, BotLedgerError> {
    Ok(NativePolicyRecord {
        key: BotPolicyBarrierKey {
            owner: BotOwner {
                user_scope: serde_json::from_value(cell::<String>(row, "user_scope")?.into())
                    .map_err(|_| corrupt("user_scope"))?,
                device: serde_json::from_value(cell::<String>(row, "device_id")?.into())
                    .map_err(|_| corrupt("device_id"))?,
            },
            bot: id(row, "bot_id")?,
            request: id(row, "request_id")?,
        },
        config_revision: positive(row, "config_revision")?,
        policy_revision: positive(row, "policy_revision")?,
        payload_digest: digest(row, "payload_digest")?,
        effects_allowed: match cell::<i64>(row, "effects_allowed")? {
            0 => false,
            1 => true,
            _ => return Err(corrupt("effects_allowed")),
        },
        state: match cell::<String>(row, "state")?.as_str() {
            "pending" => BotPolicyBarrierState::Pending,
            "effective" => BotPolicyBarrierState::Effective,
            _ => return Err(corrupt("state")),
        },
    })
}
pub(super) fn read_policy(
    conn: &Connection,
    key: &BotPolicyBarrierKey,
) -> Result<Option<NativePolicyRecord>, BotLedgerError> {
    conn.query_row("SELECT * FROM bot_policy_barriers WHERE user_scope=?1 AND device_id=?2 AND bot_id=?3 AND request_id=?4", params![key.owner.user_scope.to_storage(),key.owner.device.to_storage(),key.bot.to_storage(),key.request.to_storage()], |row| Ok(policy(row))).optional().map_err(sql)?.transpose()
}
pub(super) fn current_policy(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
) -> Result<Option<NativePolicyRecord>, BotLedgerError> {
    conn.query_row("SELECT * FROM bot_policy_barriers WHERE user_scope=?1 AND device_id=?2 AND bot_id=?3 ORDER BY config_revision DESC LIMIT 1",params![owner.user_scope.to_storage(),owner.device.to_storage(),bot.to_storage()],|row|Ok(policy(row))).optional().map_err(sql)?.transpose()
}
pub(super) fn held(conn: &Connection, owner: BotOwner, bot: BotId) -> Result<bool, BotLedgerError> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM native_result_intents WHERE lease_user_scope=?1 AND lease_device_id=?2 AND lease_bot_id=?3 AND lease_state='held')",params![owner.user_scope.to_storage(),owner.device.to_storage(),bot.to_storage()],|r|r.get(0)).map_err(sql)
}
pub(super) fn stage(value: &str) -> Result<NativeResultStage, BotLedgerError> {
    Ok(match value {
        "prepared" => NativeResultStage::Prepared,
        "unconfirmed" => NativeResultStage::Unconfirmed,
        "verified" => NativeResultStage::Verified,
        "registered" => NativeResultStage::Registered,
        "refused" => NativeResultStage::Refused,
        _ => return Err(corrupt("stage")),
    })
}
pub(super) fn lease_state(value: &str) -> Result<NativeLeaseState, BotLedgerError> {
    Ok(match value {
        "none" => NativeLeaseState::None,
        "held" => NativeLeaseState::Held,
        "settled" => NativeLeaseState::Settled,
        _ => return Err(corrupt("lease_state")),
    })
}
pub(super) fn refusal(value: &str) -> Result<NativeResultRefusal, BotLedgerError> {
    Ok(match value {
        "access_denied" => NativeResultRefusal::AccessDenied,
        "invalid_input" => NativeResultRefusal::InvalidInput,
        "conflict" => NativeResultRefusal::Conflict,
        "unavailable" => NativeResultRefusal::Unavailable,
        _ => return Err(corrupt("refusal")),
    })
}
pub(super) fn refusal_name(value: NativeResultRefusal) -> &'static str {
    match value {
        NativeResultRefusal::AccessDenied => "access_denied",
        NativeResultRefusal::InvalidInput => "invalid_input",
        NativeResultRefusal::Conflict => "conflict",
        NativeResultRefusal::Unavailable => "unavailable",
    }
}
pub(super) fn key(row: &Row<'_>) -> Result<NativeResultKey, BotLedgerError> {
    Ok(NativeResultKey {
        run: id(row, "run_id")?,
        attempt: id(row, "attempt_id")?,
        generation: u32::try_from(positive(row, "generation")?)
            .ok()
            .and_then(AttemptGeneration::from_raw)
            .ok_or_else(|| corrupt("generation"))?,
        turn: id(row, "turn_id")?,
        operation_id: uuid(row, "operation_id")?,
    })
}
fn decode(row: &Row<'_>) -> Result<NativeIntentRecord, BotLedgerError> {
    let key = key(row)?;
    let kind = match cell::<String>(row, "kind")?.as_str() {
        "create_page" => NativeResultOperation::CreatePage,
        "edit_page" => NativeResultOperation::EditPage,
        _ => return Err(corrupt("kind")),
    };
    let stage = stage(&cell::<String>(row, "stage")?)?;
    let lease_state = lease_state(&cell::<String>(row, "lease_state")?)?;
    let destination =
        serde_json::from_value(cell::<String>(row, "destination_workspace_id")?.into())
            .map_err(|_| corrupt("destination_workspace_id"))?;
    let target = serde_json::from_value(cell::<String>(row, "target_document_id")?.into())
        .map_err(|_| corrupt("target_document_id"))?;
    let lease = match cell::<Option<String>>(row, "lease_id")? {
        None => None,
        Some(_) => Some(NativeEffectLease {
            id: uuid(row, "lease_id")?,
            policy: NativePolicyRecord {
                key: BotPolicyBarrierKey {
                    owner: BotOwner {
                        user_scope: serde_json::from_value(
                            cell::<String>(row, "lease_user_scope")?.into(),
                        )
                        .map_err(|_| corrupt("lease_user_scope"))?,
                        device: serde_json::from_value(
                            cell::<String>(row, "lease_device_id")?.into(),
                        )
                        .map_err(|_| corrupt("lease_device_id"))?,
                    },
                    bot: id(row, "lease_bot_id")?,
                    request: id(row, "lease_barrier_request_id")?,
                },
                config_revision: positive(row, "lease_config_revision")?,
                policy_revision: positive(row, "lease_policy_revision")?,
                payload_digest: digest(row, "lease_payload_digest")?,
                effects_allowed: true,
                state: BotPolicyBarrierState::Effective,
            },
        }),
    };
    if lease.is_none() {
        for column in [
            "lease_user_scope",
            "lease_device_id",
            "lease_bot_id",
            "lease_barrier_request_id",
            "lease_config_revision",
            "lease_policy_revision",
            "lease_payload_digest",
        ] {
            if cell::<Option<rusqlite::types::Value>>(row, column)?.is_some() {
                return Err(corrupt("lease_cohort"));
            }
        }
    }
    let version: Option<Vec<u8>> = cell(row, "verified_version")?;
    if version.is_none()
        && (cell::<Option<String>>(row, "verified_title")?.is_some()
            || cell::<Option<i64>>(row, "confirmed_at_ms")?.is_some())
    {
        return Err(corrupt("verified_cohort"));
    }

    let verified = version
        .map(|version| {
            Ok::<_, BotLedgerError>(NativeResultReference {
                operation_id: key.operation_id,
                workspace: destination,
                document: target,
                version,
                title: cell(row, "verified_title")?,
                confirmed_at_ms: cell(row, "confirmed_at_ms")?,
                kind: match kind {
                    NativeResultOperation::CreatePage => NativeResultKind::CreatedPage,
                    NativeResultOperation::EditPage => NativeResultKind::EditedPage,
                },
            })
        })
        .transpose()?;
    let record = NativeIntentRecord {
        key,
        kind,
        destination,
        target,
        store_request: uuid(row, "store_request_id")?,
        fingerprint: digest(row, "request_fingerprint")?,
        expected_revision: cell(row, "expected_revision")?,
        requested_title: cell(row, "requested_title")?,
        stage,
        lease,
        lease_state,
        verified,
        refusal: cell::<Option<String>>(row, "refusal")?
            .map(|s| refusal(&s))
            .transpose()?,
        registered_at_ms: cell(row, "registered_at_ms")?,
        created_at_ms: cell(row, "created_at_ms")?,
        updated_at_ms: cell(row, "updated_at_ms")?,
    };
    let request = PrepareNativeResult {
        key: record.key.clone(),
        kind: record.kind,
        destination: record.destination,
        target: match record.kind {
            NativeResultOperation::CreatePage => None,
            NativeResultOperation::EditPage => Some(record.target),
        },
        request_fingerprint: record.fingerprint,
        expected_revision: record.expected_revision.clone(),
        config_revision: 1,
        policy_revision: 1,
        title: record.requested_title.clone(),
    };
    let coherent = match record.stage {
        NativeResultStage::Prepared => matches!(
            record.lease_state,
            NativeLeaseState::None | NativeLeaseState::Held
        ),
        NativeResultStage::Unconfirmed => record.lease_state == NativeLeaseState::Held,
        NativeResultStage::Verified | NativeResultStage::Registered => {
            record.lease_state == NativeLeaseState::Settled
        }
        NativeResultStage::Refused => matches!(
            record.lease_state,
            NativeLeaseState::None | NativeLeaseState::Settled
        ),
    };
    if !request.valid()
        || !coherent
        || (record.lease.is_none() != (record.lease_state == NativeLeaseState::None))
        || record.verified.as_ref().is_some_and(|v| !v.valid())
        || (record.verified.is_some()
            != matches!(
                record.stage,
                NativeResultStage::Verified | NativeResultStage::Registered
            ))
        || (record.refusal.is_some() != (record.stage == NativeResultStage::Refused))
        || (record.registered_at_ms.is_some() != (record.stage == NativeResultStage::Registered))
    {
        return Err(corrupt("cohort"));
    }
    Ok(record)
}
pub(super) fn read(
    conn: &Connection,
    key: &NativeResultKey,
) -> Result<Option<NativeIntentRecord>, BotLedgerError> {
    let record = conn
        .query_row(
            "SELECT * FROM native_result_intents WHERE run_id=?1 AND operation_id=?2",
            params![key.run.to_storage(), key.operation_id.to_string()],
            |row| Ok(decode(row)),
        )
        .optional()
        .map_err(sql)?
        .transpose()?;
    if let Some(record) = &record {
        if record.key != *key {
            return Err(BotLedgerError::Conflict);
        }
        let attempt: Option<String> = conn
            .query_row(
                "SELECT attempt_id FROM run_attempts WHERE run_id=?1 AND generation=?2",
                params![key.run.to_storage(), key.generation.get()],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        if attempt.as_deref() != Some(key.attempt.to_storage().as_str()) {
            return Err(corrupt("attempt_binding"));
        }

        if let Some(lease) = &record.lease {
            if read_policy(conn, &lease.policy.key)?.as_ref() != Some(&lease.policy) {
                return Err(corrupt("lease_policy"));
            }
        }
    }
    Ok(record)
}
pub(super) fn required(
    conn: &Connection,
    key: &NativeResultKey,
) -> Result<NativeIntentRecord, BotLedgerError> {
    read(conn, key)?.ok_or(BotLedgerError::Invalid)
}
pub(super) fn created(
    conn: &Connection,
    owner: BotOwner,
    bot: BotId,
    workspace: WorkspaceId,
    document: DocumentId,
) -> Result<bool, BotLedgerError> {
    let found = conn.query_row("SELECT n.run_id,n.operation_id,n.attempt_id,n.generation,n.turn_id FROM native_result_intents n JOIN bot_admissions b ON b.run_id=n.run_id WHERE n.destination_workspace_id=?1 AND n.target_document_id=?2 AND n.kind='create_page' AND n.stage IN ('verified','registered') AND b.user_scope=?3 AND b.device_id=?4 AND b.bot_id=?5",params![workspace.to_storage(),document.to_storage(),owner.user_scope.to_storage(),owner.device.to_storage(),bot.to_storage()],|row|Ok(key(row))).optional().map_err(sql)?.transpose()?;
    let Some(key) = found else {
        return Ok(false);
    };
    // The index locates one unique creation; it is not itself authority.
    // Validate its full evidence and exact retained lease-owner binding.
    let record = required(conn, &key)?;
    if record
        .lease
        .as_ref()
        .is_none_or(|lease| lease.policy.key.owner != owner || lease.policy.key.bot != bot)
    {
        return Err(corrupt("creation_owner"));
    }
    Ok(true)
}
