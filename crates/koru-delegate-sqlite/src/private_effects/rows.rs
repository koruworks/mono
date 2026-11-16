// SPDX-License-Identifier: MPL-2.0

//! The one decoder and the one insert of `bot_private_effects`. Every cell is
//! checked for type and byte length in SQL before Rust reads it.
use super::*;
use rusqlite::Row;
use serde::{Serialize, de::DeserializeOwned};

/// `(column, type, exact or maximum bytes, nullable)`. Text ids are exactly
/// 36 bytes and digests exactly 32; tokens are short closed words.
const FIELDS: &[(&str, &str, usize, bool)] = &[
    ("run_id", "text", 36, false),
    ("operation_id", "text", 36, false),
    ("user_scope", "text", 36, false),
    ("device_id", "text", 36, false),
    ("bot_id", "text", 36, false),
    ("attempt_id", "text", 36, false),
    ("generation", "integer", 0, false),
    ("turn_id", "text", 36, false),
    ("dispatcher_instance", "text", 36, false),
    ("decision_id", "text", 36, true),
    ("binding_digest", "blob", 32, true),
    ("connection_id", "text", 36, false),
    ("connection_revision", "integer", 0, false),
    ("account_digest", "blob", 32, false),
    ("item_id", "text", 36, false),
    ("item_ref_digest", "blob", 32, false),
    ("operation", "text", 32, false),
    ("request_fingerprint", "blob", 32, false),
    ("barrier_request_id", "text", 36, false),
    ("config_revision", "integer", 0, false),
    ("policy_revision", "integer", 0, false),
    ("policy_digest", "blob", 32, false),
    ("schedule_id", "text", 36, true),
    ("routine_barrier_request_id", "text", 36, true),
    ("grant_revision", "integer", 0, true),
    ("routine_policy_digest", "blob", 32, true),
    ("stage", "text", 32, false),
    ("refusal", "text", 32, true),
    ("outcome_bytes", "integer", 0, true),
    ("withheld", "integer", 0, true),
    ("dispatched_at_ms", "integer", 0, true),
    ("resolved_at_ms", "integer", 0, true),
    ("created_at_ms", "integer", 0, false),
    ("updated_at_ms", "integer", 0, false),
];

fn select(predicate: &str) -> String {
    let guards = FIELDS
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
    format!("SELECT ({guards}) AS bounds_ok, * FROM bot_private_effects WHERE {predicate}")
}

pub(super) fn token<T: Serialize>(value: T) -> Result<String, BotLedgerError> {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or(BotLedgerError::Invalid)
}

struct Cells<'a, 'r>(&'a Row<'r>);
impl Cells<'_, '_> {
    fn get<T: rusqlite::types::FromSql>(&self, name: &'static str) -> Result<T, BotLedgerError> {
        self.0.get(name).map_err(|_| corrupt(name))
    }
    fn id<T: DeserializeOwned>(&self, name: &'static str) -> Result<T, BotLedgerError> {
        serde_json::from_value(serde_json::Value::String(self.get(name)?))
            .map_err(|_| corrupt(name))
    }
    fn optional_id<T: DeserializeOwned>(
        &self,
        name: &'static str,
    ) -> Result<Option<T>, BotLedgerError> {
        self.get::<Option<String>>(name)?
            .map(|s| serde_json::from_value(s.into()).map_err(|_| corrupt(name)))
            .transpose()
    }
    fn bytes(&self, name: &'static str) -> Result<[u8; 32], BotLedgerError> {
        self.get::<Vec<u8>>(name)?
            .try_into()
            .map_err(|_| corrupt(name))
    }
    fn digest(&self, name: &'static str) -> Result<DecisionDigest, BotLedgerError> {
        self.bytes(name).map(DecisionDigest::from_bytes)
    }
    fn optional_digest(
        &self,
        name: &'static str,
    ) -> Result<Option<DecisionDigest>, BotLedgerError> {
        self.get::<Option<Vec<u8>>>(name)?
            .map(|bytes| {
                bytes
                    .try_into()
                    .map(DecisionDigest::from_bytes)
                    .map_err(|_| corrupt(name))
            })
            .transpose()
    }
    fn revision(&self, name: &'static str) -> Result<u64, BotLedgerError> {
        let value: i64 = self.get(name)?;
        u64::try_from(value)
            .ok()
            .filter(|v| *v > 0)
            .ok_or_else(|| corrupt(name))
    }
}

fn decode(row: &Row<'_>) -> Result<PrivateEffectRecord, BotLedgerError> {
    let c = Cells(row);
    if !c.get::<bool>("bounds_ok")? {
        return Err(corrupt("bounded_columns"));
    }
    let owner: BotOwner = serde_json::from_value(serde_json::json!({
        "user_scope": c.get::<String>("user_scope")?,
        "device": c.get::<String>("device_id")?,
    }))
    .map_err(|_| corrupt("owner"))?;
    let operation = PrivateOperationProposal {
        operation_id: c.id("operation_id")?,
        connection: c.id("connection_id")?,
        connection_revision: DecisionRevision::new(c.revision("connection_revision")?)
            .map_err(|_| corrupt("connection_revision"))?,
        account: c.digest("account_digest")?,
        item: c.id("item_id")?,
        item_ref: c.digest("item_ref_digest")?,
        operation: c.id("operation")?,
        request_fingerprint: c.bytes("request_fingerprint")?,
    };
    // The fingerprint is recomputed on every read, as it is on every write.
    if !operation.valid() {
        return Err(corrupt("request_fingerprint"));
    }
    let routine = match (
        c.optional_id::<ScheduleId>("schedule_id")?,
        c.optional_id::<NativePolicyRequestId>("routine_barrier_request_id")?,
        c.get::<Option<i64>>("grant_revision")?,
        c.optional_digest("routine_policy_digest")?,
    ) {
        (None, None, None, None) => None,
        (Some(schedule), Some(request), Some(_), Some(digest)) => Some(PrivateRoutineFence {
            schedule,
            request,
            grant_revision: c.revision("grant_revision")?,
            digest,
        }),
        _ => return Err(corrupt("routine_barrier")),
    };
    let decision = c.optional_id::<DecisionId>("decision_id")?;
    let binding_digest = c.optional_digest("binding_digest")?;
    if decision.is_some() != binding_digest.is_some() || (decision.is_some() && routine.is_some()) {
        return Err(corrupt("decision_id"));
    }
    let stage: PrivateEffectStage = c.id("stage")?;
    let refusal: Option<PrivateEffectRefusal> = c.optional_id("refusal")?;
    let outcome_bytes = c
        .get::<Option<i64>>("outcome_bytes")?
        .map(|v| u32::try_from(v).map_err(|_| corrupt("outcome_bytes")))
        .transpose()?;
    let withheld = match c.get::<Option<i64>>("withheld")? {
        None => None,
        Some(0) => Some(false),
        Some(1) => Some(true),
        Some(_) => return Err(corrupt("withheld")),
    };
    let dispatched_at_ms: Option<i64> = c.get("dispatched_at_ms")?;
    let resolved_at_ms: Option<i64> = c.get("resolved_at_ms")?;
    // The stage's companions, repeated from the table's CHECKs.
    let shaped = (stage == PrivateEffectStage::Refused) == refusal.is_some()
        && (stage == PrivateEffectStage::Confirmed) == outcome_bytes.is_some()
        && outcome_bytes.is_some() == withheld.is_some()
        && stage.is_final() == resolved_at_ms.is_some()
        && match stage {
            PrivateEffectStage::Prepared => dispatched_at_ms.is_none(),
            PrivateEffectStage::Dispatched
            | PrivateEffectStage::Confirmed
            | PrivateEffectStage::Unknown => dispatched_at_ms.is_some(),
            PrivateEffectStage::Refused => true,
        };
    if !shaped {
        return Err(corrupt("stage"));
    }
    Ok(PrivateEffectRecord {
        owner,
        bot: c.id("bot_id")?,
        key: PrivateEffectKey {
            run: c.id("run_id")?,
            operation_id: operation.operation_id,
            attempt: c.id("attempt_id")?,
            generation: AttemptGeneration::from_raw(c.get("generation")?)
                .ok_or_else(|| corrupt("generation"))?,
            turn: c.id("turn_id")?,
        },
        dispatcher: c.id("dispatcher_instance")?,
        decision,
        binding_digest,
        operation,
        barrier: c.id("barrier_request_id")?,
        config_revision: c.revision("config_revision")?,
        policy_revision: c.revision("policy_revision")?,
        policy_digest: c.bytes("policy_digest")?,
        routine,
        stage,
        refusal,
        outcome_bytes,
        withheld,
        dispatched_at_ms,
        resolved_at_ms,
        created_at_ms: c.get("created_at_ms")?,
        updated_at_ms: c.get("updated_at_ms")?,
    })
}

pub(crate) fn read_on(
    conn: &Connection,
    run: RunId,
    operation: uuid::Uuid,
) -> Result<Option<PrivateEffectRecord>, BotLedgerError> {
    conn.query_row(
        &select("run_id=?1 AND operation_id=?2"),
        params![run.to_storage(), operation.to_string()],
        |row| Ok(decode(row)),
    )
    .optional()
    .map_err(sql)?
    .transpose()
}

/// The effect a consumed decision owns: at most one, by the unique index.
pub(crate) fn by_decision_on(
    conn: &Connection,
    decision: DecisionId,
) -> Result<Option<PrivateEffectRecord>, BotLedgerError> {
    conn.query_row(&select("decision_id=?1"), [decision.to_storage()], |row| {
        Ok(decode(row))
    })
    .optional()
    .map_err(sql)?
    .transpose()
}

/// The Run's one open effect, if any (`bot_private_effects_open_run`).
pub(crate) fn open_in_run_on(
    conn: &Connection,
    run: RunId,
) -> Result<Option<PrivateEffectRecord>, BotLedgerError> {
    conn.query_row(
        &select("run_id=?1 AND stage IN ('prepared','dispatched')"),
        [run.to_storage()],
        |row| Ok(decode(row)),
    )
    .optional()
    .map_err(sql)?
    .transpose()
}

/// Identity-only bounded probe of one owner's open rows that another
/// dispatcher instance holds, through `bot_private_effects_open`.
pub(super) fn open_of_others(
    conn: &Connection,
    owner: BotOwner,
    dispatcher: PrivateDispatcherId,
    limit: usize,
) -> Result<Vec<(RunId, uuid::Uuid)>, BotLedgerError> {
    let mut query = conn
        .prepare(
            "SELECT CASE WHEN typeof(run_id)='text' AND length(CAST(run_id AS BLOB))=36 THEN run_id END,
                    CASE WHEN typeof(operation_id)='text' AND length(CAST(operation_id AS BLOB))=36 THEN operation_id END
             FROM bot_private_effects INDEXED BY bot_private_effects_open
             WHERE user_scope=?1 AND device_id=?2 AND dispatcher_instance<>?3
               AND stage IN ('prepared','dispatched')
             ORDER BY dispatcher_instance,run_id,operation_id LIMIT ?4",
        )
        .map_err(sql)?;
    let rows = query
        .query_map(
            params![
                owner.user_scope.to_storage(),
                owner.device.to_storage(),
                dispatcher.to_storage(),
                limit as i64
            ],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .map_err(sql)?;
    let mut keys = Vec::new();
    for row in rows {
        let (Some(run), Some(operation)) = row.map_err(sql)? else {
            return Err(corrupt("open_index"));
        };
        keys.push((
            run.parse().map_err(|_| corrupt("run_id"))?,
            operation.parse().map_err(|_| corrupt("operation_id"))?,
        ));
    }
    Ok(keys)
}

/// The one insert. The caller has proved every authority in this transaction;
/// the row always starts at `prepared`.
pub(crate) fn insert_prepared_on(
    conn: &Connection,
    record: &PrivateEffectRecord,
) -> Result<(), BotLedgerError> {
    if record.stage != PrivateEffectStage::Prepared || !record.operation.valid() {
        return Err(BotLedgerError::Invalid);
    }
    let routine = record.routine.as_ref();
    conn.execute(
        "INSERT INTO bot_private_effects(run_id,operation_id,user_scope,device_id,bot_id,attempt_id,generation,turn_id,
            dispatcher_instance,decision_id,binding_digest,connection_id,connection_revision,account_digest,item_id,
            item_ref_digest,operation,request_fingerprint,barrier_request_id,config_revision,policy_revision,policy_digest,
            schedule_id,routine_barrier_request_id,grant_revision,routine_policy_digest,stage,created_at_ms,updated_at_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,'prepared',?27,?27)",
        params![
            record.key.run.to_storage(),
            record.key.operation_id.to_string(),
            record.owner.user_scope.to_storage(),
            record.owner.device.to_storage(),
            record.bot.to_storage(),
            record.key.attempt.to_storage(),
            record.key.generation.get(),
            record.key.turn.to_string(),
            record.dispatcher.to_storage(),
            record.decision.map(|d| d.to_storage()),
            record.binding_digest.map(|d| d.as_bytes().to_vec()),
            record.operation.connection.to_storage(),
            record.operation.connection_revision.get() as i64,
            record.operation.account.as_bytes().as_slice(),
            record.operation.item.to_storage(),
            record.operation.item_ref.as_bytes().as_slice(),
            token(record.operation.operation)?,
            record.operation.request_fingerprint.as_slice(),
            record.barrier.to_storage(),
            record.config_revision as i64,
            record.policy_revision as i64,
            record.policy_digest.as_slice(),
            routine.map(|r| r.schedule.to_storage()),
            routine.map(|r| r.request.to_storage()),
            routine.map(|r| r.grant_revision as i64),
            routine.map(|r| r.digest.as_bytes().to_vec()),
            record.created_at_ms,
        ],
    )
    .map_err(sql)?;
    Ok(())
}
