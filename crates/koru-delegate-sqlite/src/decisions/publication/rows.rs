// SPDX-License-Identifier: MPL-2.0

use super::super::rows::{Cells, query};
use super::*;

const TABLE: &str = "bot_decision_publications";
const FIELDS: &[(&str, &str, usize, bool)] = &[
    ("transition_id", "text", 36, false),
    ("run_id", "text", 36, false),
    ("wait_id", "text", 36, false),
    ("decision_id", "text", 36, true),
    ("run_sequence", "integer", 0, false),
    ("decision_revision", "integer", 0, true),
    ("workspace_id", "text", 36, false),
    ("document_id", "text", 36, false),
    ("source_schema", "integer", 0, false),
    ("subject_digest", "blob", 32, false),
    ("projection_kind", "text", 16, false),
    ("target_json", "text", DECISION_PAYLOAD_BYTES, false),
    ("target_digest", "blob", 32, false),
    ("state", "text", 16, false),
    ("base_version", "blob", 65_536, true),
    ("result_version", "blob", 65_536, true),
    ("prepared_delta", "blob", DECISION_DELTA_BYTES, true),
    ("delta_digest", "blob", 32, true),
    ("superseded_by_sequence", "integer", 0, true),
    ("observed_version", "blob", 65_536, true),
    ("created_at_ms", "integer", 0, false),
    ("completed_at_ms", "integer", 0, true),
];

pub(super) fn read(
    conn: &Connection,
    run: RunId,
    transition: DecisionTransitionId,
) -> Result<Option<DecisionProjectionRecord>, BotLedgerError> {
    // Filter exact subject before any variable-size body is allocated. The
    // caller separately checks owner even for absence and repeats correlation.
    conn.query_row(
        &query(TABLE, FIELDS, "run_id=?1 AND transition_id=?2"),
        params![run.to_storage(), transition.to_storage()],
        |row| Ok(decode(row)),
    )
    .optional()
    .map_err(sql)?
    .transpose()
}

fn bytes<const N: usize>(
    c: &Cells<'_, '_>,
    name: &'static str,
) -> Result<Option<DecisionBytes<N>>, BotLedgerError> {
    c.get::<Option<Vec<u8>>>(name)?
        .map(|v| DecisionBytes::new(v).map_err(|_| corrupt(TABLE, name)))
        .transpose()
}

fn decode(row: &rusqlite::Row<'_>) -> Result<DecisionProjectionRecord, BotLedgerError> {
    let c = Cells { row, table: TABLE };
    c.check()?;
    let decision = match (
        c.optional_id("decision_id")?,
        c.get::<Option<u64>>("decision_revision")?,
    ) {
        (None, None) => None,
        (Some(decision), Some(revision)) => Some(DecisionProjectionRevision {
            decision,
            revision: DecisionRevision::new(revision)
                .map_err(|_| corrupt(TABLE, "decision_revision"))?,
        }),
        _ => return Err(corrupt(TABLE, "decision_revision")),
    };
    let identity = DecisionProjectionIdentity {
        transition: c.id("transition_id")?,
        run: c.id("run_id")?,
        wait: c.id("wait_id")?,
        decision,
        sequence: c.revision("run_sequence")?,
        workspace: c.id("workspace_id")?,
        document: c.id("document_id")?,
        source_schema: DecisionSourceSchema::try_from(c.get::<u8>("source_schema")?)
            .map_err(|_| corrupt(TABLE, "source_schema"))?,
        subject_digest: c.digest("subject_digest")?,
        kind: c.id("projection_kind")?,
        target_digest: c.digest("target_digest")?,
    };
    let raw = c.get::<String>("target_json")?;
    let target =
        RunProjectionTargetV1::decode(raw.as_bytes()).map_err(|_| corrupt(TABLE, "target_json"))?;
    target
        .matches_identity(&identity)
        .map_err(|_| corrupt(TABLE, "target_digest"))?;
    let canonical = target
        .canonical_bytes()
        .map_err(|_| corrupt(TABLE, "target_json"))?;
    c.same(canonical.as_slice() == raw.as_bytes())?;
    let base = bytes(&c, "base_version")?;
    let result = bytes(&c, "result_version")?;
    let delta = bytes(&c, "prepared_delta")?;
    let digest = c
        .get::<Option<Vec<u8>>>("delta_digest")?
        .map(|v| {
            <[u8; 32]>::try_from(v)
                .map(DecisionDigest::from_bytes)
                .map_err(|_| corrupt(TABLE, "delta_digest"))
        })
        .transpose()?;
    let prepared = match (base, result, delta, digest) {
        (None, None, None, None) => None,
        (Some(base_version), Some(result_version), Some(delta), Some(delta_digest)) => {
            c.same(
                decision_projection_delta_digest(&delta)
                    .map_err(|_| corrupt(TABLE, "delta_digest"))?
                    == delta_digest,
            )?;
            let p = DecisionPreparedProjection {
                identity: identity.clone(),
                target: canonical,
                base_version,
                result_version,
                delta,
                delta_digest,
            };
            p.validate().map_err(|_| corrupt(TABLE, "prepared_delta"))?;
            Some(p)
        }
        _ => return Err(corrupt(TABLE, "prepared_group")),
    };
    let observed = bytes(&c, "observed_version")?;
    let created_at_ms = c.get::<i64>("created_at_ms")?;
    let completed = c.get::<Option<i64>>("completed_at_ms")?;
    c.same(completed.is_none_or(|n| n >= created_at_ms))?;
    let by = c
        .get::<Option<u64>>("superseded_by_sequence")?
        .map(|n| DecisionRevision::new(n).map_err(|_| corrupt(TABLE, "superseded_by_sequence")))
        .transpose()?;
    let state = match (
        c.get::<String>("state")?.as_str(),
        prepared,
        observed,
        completed,
        by,
    ) {
        ("pending", None, None, None, None) => DecisionProjectionState::Pending,
        ("prepared", Some(prepared), None, None, None) => {
            DecisionProjectionState::Prepared { prepared }
        }
        ("applied", Some(prepared), Some(observed_version), Some(completed_at_ms), None) => {
            DecisionProjectionState::Applied {
                prepared,
                observed_version,
                completed_at_ms,
            }
        }
        ("superseded", None, None, Some(completed_at_ms), Some(by_sequence))
            if identity.kind == DecisionProjectionKind::Control
                && by_sequence > identity.sequence =>
        {
            DecisionProjectionState::Superseded {
                by_sequence,
                completed_at_ms,
            }
        }
        _ => return Err(corrupt(TABLE, "state")),
    };
    Ok(DecisionProjectionRecord {
        identity,
        target,
        state,
        created_at_ms,
    })
}

pub(super) fn open(
    conn: &Connection,
    run: RunId,
) -> Result<Vec<DecisionTransitionId>, BotLedgerError> {
    let mut statement = conn.prepare(
        "SELECT CASE WHEN typeof(transition_id)='text' AND length(CAST(transition_id AS BLOB))=36 THEN transition_id END,
                CASE WHEN typeof(run_sequence)='integer' THEN run_sequence END,
                projection_kind,state
         FROM (
             SELECT transition_id,run_sequence,projection_kind,state FROM bot_decision_publications
             INDEXED BY bot_decision_publications_pending_source
             WHERE run_id=?1 AND state='pending' AND projection_kind='source'
             UNION ALL
             SELECT transition_id,run_sequence,projection_kind,state FROM bot_decision_publications
             INDEXED BY bot_decision_publications_pending_control
             WHERE run_id=?1 AND state='pending' AND projection_kind='control'
             UNION ALL
             SELECT transition_id,run_sequence,projection_kind,state FROM bot_decision_publications
             INDEXED BY bot_decision_publications_prepared
             WHERE run_id=?1 AND state='prepared'
         )
         ORDER BY run_sequence LIMIT 3",
    ).map_err(sql)?;
    let mut cursor = statement.query([run.to_storage()]).map_err(sql)?;
    let mut keys = Vec::new();
    let mut shape = Vec::new();
    while let Some(row) = cursor.next().map_err(sql)? {
        let id = row
            .get::<_, String>(0)
            .map_err(sql)?
            .parse()
            .map_err(|_| corrupt(TABLE, "transition_id"))?;
        let sequence = DecisionRevision::new(row.get(1).map_err(sql)?)
            .map_err(|_| corrupt(TABLE, "run_sequence"))?;
        let kind = match row.get::<_, String>(2).map_err(sql)?.as_str() {
            "source" => DecisionProjectionKind::Source,
            "control" => DecisionProjectionKind::Control,
            _ => return Err(corrupt(TABLE, "projection_kind")),
        };
        let phase = match row.get::<_, String>(3).map_err(sql)?.as_str() {
            "pending" => DecisionProjectionPhase::Pending,
            "prepared" => DecisionProjectionPhase::Prepared,
            _ => return Err(corrupt(TABLE, "state")),
        };
        keys.push(id);
        shape.push(DecisionOpenProjection {
            run,
            sequence,
            kind,
            phase,
        });
    }
    validate_open_decision_projections(&shape).map_err(|_| corrupt(TABLE, "open_shape"))?;
    Ok(keys)
}

pub(super) fn latest(
    conn: &Connection,
    run: RunId,
) -> Result<Option<DecisionTransitionId>, BotLedgerError> {
    conn.query_row(
        "SELECT CASE WHEN typeof(transition_id)='text' AND length(CAST(transition_id AS BLOB))=36 THEN transition_id END
         FROM bot_decision_publications WHERE run_id=?1 ORDER BY run_sequence DESC LIMIT 1",
        [run.to_storage()], |r| r.get::<_, String>(0),
    ).optional().map_err(sql)?
        .map(|v| v.parse().map_err(|_| corrupt(TABLE, "transition_id"))).transpose()
}
