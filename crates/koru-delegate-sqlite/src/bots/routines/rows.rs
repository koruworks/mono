// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_delegate_core::*;
use rusqlite::Row;

pub(super) fn bad(column: &'static str) -> BotLedgerError {
    LedgerError::Corrupt {
        table: "bot_routine_links",
        column,
    }
    .into()
}
pub(super) fn parse_cell<T: std::str::FromStr>(
    row: &Row<'_>,
    index: usize,
) -> Result<T, BotLedgerError> {
    row.get::<_, String>(index)
        .map_err(sql)?
        .parse()
        .map_err(|_| bad("identity"))
}
pub(super) fn digest_cell(row: &Row<'_>, index: usize) -> Result<RoutineDigest32, BotLedgerError> {
    Ok(RoutineDigest32::from_bytes(
        row.get::<_, Vec<u8>>(index)
            .map_err(sql)?
            .try_into()
            .map_err(|_| bad("digest"))?,
    ))
}

/// Call before reading any occurrence: a Schedule slot alone is not a subject.
pub(super) fn subject(
    conn: &Connection,
    slot: &RoutineSlot,
) -> Result<RoutineRouteIdentity, BotLedgerError> {
    if !slot.valid() {
        return Err(BotLedgerError::Invalid);
    }
    let route = super::read(conn, slot.schedule)?.ok_or(BotLedgerError::Fenced)?;
    if !route.matches_slot(slot) {
        return Err(BotLedgerError::Fenced);
    }
    Ok(route)
}

// Column names and limits below are fixed source strings, never SQL input.
fn bounded(column: &str, size: usize, kind: &str) -> String {
    format!(
        "CASE WHEN typeof({column})='{kind}' AND length(CAST({column} AS BLOB))={size} THEN {column} END AS {column}"
    )
}

pub(crate) fn bounded_admission(conn: &Connection, run: RunId) -> Result<(), BotLedgerError> {
    let mut conditions=vec!["typeof(payload_json)='text' AND length(CAST(payload_json AS BLOB))<=4194304".to_owned(),
        "delivery_outcome IN ('unconfirmed','confirmed')".to_owned(),"state IN ('queued','reserved','executing','settled','refused')".to_owned(),
        "initial_send_fingerprint IS NULL OR (typeof(initial_send_fingerprint)='blob' AND length(initial_send_fingerprint)=32)".to_owned()];
    for column in [
        "run_id",
        "user_scope",
        "device_id",
        "bot_id",
        "request_id",
        "initial_send_key",
        "initial_send_attempt_id",
        "initial_send_turn_id",
        "delivery_attempt_id",
        "delivery_chat_id",
        "delivery_ai_attempt_id",
        "delivery_assistant_message_id",
        "delivery_publication_event_id",
    ] {
        conditions.push(format!(
            "{column} IS NULL OR (typeof({column})='text' AND length(CAST({column} AS BLOB))=36)"
        ));
    }
    let query = format!(
        "SELECT {} FROM bot_admissions WHERE run_id=?1",
        conditions
            .iter()
            .map(|c| format!("({c})"))
            .collect::<Vec<_>>()
            .join(" AND ")
    );
    let valid = conn
        .query_row(&query, [run.to_storage()], |r| r.get::<_, bool>(0))
        .optional()
        .map_err(sql)?
        .unwrap_or(false);
    if !valid {
        return Err(bad("admission_cells"));
    }
    Ok(())
}

pub(super) fn observation(
    conn: &Connection,
    slot: &RoutineSlot,
    now: i64,
) -> Result<RoutineClaimObservation, BotLedgerError> {
    observation_with_admission(conn, slot, now, None)
}

pub(super) fn observation_with_admission(
    conn: &Connection,
    slot: &RoutineSlot,
    now: i64,
    admitted: Option<&crate::BotAdmissionRecord>,
) -> Result<RoutineClaimObservation, BotLedgerError> {
    let route = subject(conn, slot)?;
    let query = format!(
        "SELECT revision,{},{},busy_holder_run_id IS NULL,{},claimed_at_ms FROM schedule_claims WHERE schedule_id=?1 AND occurrence_ms=?2",
        bounded("run_id", 36, "text"),
        "CASE WHEN outcome IN ('claimed','fired','skipped_missed','skipped_busy','skipped_retired') THEN outcome END",
        bounded("busy_holder_run_id", 36, "text")
    );
    let claim = conn
        .query_row(
            &query,
            params![slot.schedule.to_storage(), slot.occurrence_ms],
            |r| {
                Ok((|| {
                    let revision: u32 = r.get(0).map_err(sql)?;
                    let run: RunId = parse_cell(r, 1)?;
                    let outcome: String = r.get(2).map_err(sql)?;
                    let holder = if r.get::<_, bool>(3).map_err(sql)? {
                        None
                    } else {
                        Some(parse_cell::<RunId>(r, 4)?)
                    };
                    let claimed_at: i64 = r.get(5).map_err(sql)?;
                    if revision == 0 {
                        return Err(bad("revision"));
                    }
                    Ok((revision, run, outcome, holder, claimed_at))
                })())
            },
        )
        .optional()
        .map_err(sql)?
        .transpose()?;
    let Some((revision, run, outcome, holder, claimed_at)) = claim else {
        let orphan: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM bot_routine_links WHERE schedule_id=?1 AND occurrence_ms=?2)",params![slot.schedule.to_storage(),slot.occurrence_ms],|r|r.get(0)).map_err(sql)?;
        if orphan {
            return Err(bad("orphan_link"));
        }
        return Ok(RoutineClaimObservation::Absent {
            slot: *slot,
            observed_at_ms: now,
        });
    };
    if matches!(outcome.as_str(), "skipped_missed" | "skipped_busy") {
        if let Some(holder) = holder {
            // Admissions are retained. The holder may since have settled, but
            // it must still be an actual admission of this exact owner/Bot.
            let same_subject: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM bot_admissions WHERE run_id=?1 AND user_scope=?2 AND device_id=?3 AND bot_id=?4)",params![holder.to_storage(),slot.owner.user_scope.to_storage(),slot.owner.device.to_storage(),slot.bot.to_storage()],|r|r.get(0)).map_err(sql)?;
            if !same_subject {
                return Err(bad("busy_holder_subject"));
            }
        }
        let record = RoutineSkippedClaim {
            slot: *slot,
            definition_revision: revision,
            planned_run: run,
            outcome: match (outcome.as_str(), holder) {
                ("skipped_missed", None) => RoutineSkipOutcome::SkippedMissed {},
                ("skipped_busy", Some(holder)) => RoutineSkipOutcome::SkippedBusy { holder },
                _ => return Err(bad("skip_holder")),
            },
            recorded_at_ms: claimed_at,
        };
        let executable: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM run_intents WHERE run_id=?1) OR EXISTS(SELECT 1 FROM bot_admissions WHERE run_id=?1) OR EXISTS(SELECT 1 FROM bot_routine_links WHERE schedule_id=?2 AND occurrence_ms=?3)",params![run.to_storage(),slot.schedule.to_storage(),slot.occurrence_ms],|r|r.get(0)).map_err(sql)?;
        if !record.valid() || executable {
            return Err(bad("skip"));
        }
        return Ok(RoutineClaimObservation::Skipped { record });
    }
    if holder.is_some() {
        return Err(bad("holder"));
    }
    // Bound payload before the existing exact v1/v2 admission decoder allocates.
    let loaded;
    let admission = if let Some(admission) = admitted {
        if admission.submission.request.run != run {
            return Err(bad("admission_run"));
        }
        admission
    } else {
        bounded_admission(conn, run)?;
        loaded = super::super::repository::read(conn, run)?.ok_or_else(|| bad("admission"))?;
        &loaded
    };
    let mut fields = vec![
        "definition_revision".to_owned(),
        bounded("definition_digest", 32, "blob"),
        "owner_handoff IS NULL".to_owned(),
        bounded("owner_handoff", 36, "text"),
        "grant_revision".to_owned(),
        bounded("preparation_hash", 32, "blob"),
    ];
    fields.extend(
        [
            "submission_request_id",
            "chat_id",
            "attempt_id",
            "user_message_id",
            "assistant_message_id",
        ]
        .map(|c| bounded(c, 36, "text")),
    );
    fields.extend(["CASE WHEN state IN ('held','linked','terminal') THEN state END".to_owned(),
        "terminal_reason IS NULL".to_owned(),"CASE WHEN terminal_reason IN ('stopped','configuration_changed','authority_changed','already_terminal') THEN terminal_reason END".to_owned(),
        "created_at_ms".to_owned(),"updated_at_ms".to_owned()]);
    let query = format!(
        "SELECT {} FROM bot_routine_links WHERE run_id=?1 AND schedule_id=?2 AND occurrence_ms=?3 AND user_scope=?4 AND device_id=?5 AND bot_id=?6",
        fields.join(",")
    );
    let record = conn
        .query_row(
            &query,
            params![
                run.to_storage(),
                slot.schedule.to_storage(),
                slot.occurrence_ms,
                slot.owner.user_scope.to_storage(),
                slot.owner.device.to_storage(),
                slot.bot.to_storage()
            ],
            |r| {
                Ok((|| {
                    let definition = RoutineDefinitionIdentity {
                        revision: r.get(0).map_err(sql)?,
                        digest: digest_cell(r, 1)?,
                        owner_handoff: if r.get::<_, bool>(2).map_err(sql)? {
                            None
                        } else {
                            Some(parse_cell(r, 3)?)
                        },
                    };
                    let grant_revision: u64 = r.get(4).map_err(sql)?;
                    let ids = RoutineExecutionIds {
                        submission: parse_cell(r, 6)?,
                        run,
                        chat: parse_cell(r, 7)?,
                        attempt: parse_cell(r, 8)?,
                        user_message: parse_cell(r, 9)?,
                        assistant_message: parse_cell(r, 10)?,
                    };
                    let reason_null: bool = r.get(12).map_err(sql)?;
                    let link_state =
                        match (r.get::<_, String>(11).map_err(sql)?.as_str(), reason_null) {
                            ("held", true) => RoutineLinkState::Held {},
                            ("linked", true) => RoutineLinkState::Linked {},
                            ("terminal", false) => RoutineLinkState::Terminal {
                                reason: match r.get::<_, String>(13).map_err(sql)?.as_str() {
                                    "stopped" => RoutineTerminalReason::Stopped,
                                    "configuration_changed" => {
                                        RoutineTerminalReason::ConfigurationChanged
                                    }
                                    "authority_changed" => RoutineTerminalReason::AuthorityChanged,
                                    "already_terminal" => RoutineTerminalReason::AlreadyTerminal,
                                    _ => return Err(bad("terminal_reason")),
                                },
                            },
                            _ => return Err(bad("link_state")),
                        };
                    let created: i64 = r.get(14).map_err(sql)?;
                    let updated: i64 = r.get(15).map_err(sql)?;
                    let terminal = matches!(
                        admission.state,
                        BotAdmissionState::Settled | BotAdmissionState::Refused
                    );
                    if definition.revision != revision
                        || created != claimed_at
                        || updated < created
                        || admission.created_at_ms != claimed_at
                        || admission.submission.workspace != route.workspace
                        || admission
                            .submission
                            .provenance
                            .routine
                            .as_ref()
                            .is_none_or(|p| p.grant_revision != grant_revision)
                        || matches!(link_state, RoutineLinkState::Terminal { .. }) != terminal
                        || (matches!(link_state, RoutineLinkState::Held {})
                            && admission.state != BotAdmissionState::Queued)
                    {
                        return Err(bad("repeated_identity"));
                    }
                    let record = RoutineClaimRecord {
                        claim: RoutineClaimIdentity {
                            slot: *slot,
                            definition,
                            ids,
                            preparation_hash: digest_cell(r, 5)?,
                            claimed_at_ms: claimed_at,
                        },
                        submission_version: admission.submission_version,
                        submission: admission.submission.clone(),
                        link_state,
                        updated_at_ms: r.get(15).map_err(sql)?,
                    };
                    if !record.valid() {
                        return Err(bad("identity"));
                    }
                    Ok(record)
                })())
            },
        )
        .optional()
        .map_err(sql)?
        .transpose()?
        .ok_or_else(|| bad("link"))?;
    let intent: Option<(String,String,String,Option<String>,bool)> = conn.query_row("SELECT CASE WHEN typeof(payload_json)='text' AND length(CAST(payload_json AS BLOB))<=4194304 THEN payload_json END,CASE WHEN typeof(workspace_id)='text' AND length(CAST(workspace_id AS BLOB))=36 THEN workspace_id END,CASE WHEN state IN ('pending','satisfied','abandoned') THEN state END,CASE WHEN typeof(run_document_id)='text' AND length(CAST(run_document_id AS BLOB))=36 THEN run_document_id END,run_document_id IS NULL FROM run_intents WHERE run_id=?1",[run.to_storage()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(sql)?;
    let (payload, workspace, state, document, document_null) =
        intent.ok_or_else(|| bad("intent"))?;
    if (!document_null
        && document
            .as_ref()
            .and_then(|d| DocumentId::parse(d))
            .is_none())
        || (state == "satisfied" && document_null)
        || !match record.link_state {
            RoutineLinkState::Held {} => state == "pending",
            RoutineLinkState::Linked {} => matches!(state.as_str(), "pending" | "satisfied"),
            RoutineLinkState::Terminal { .. } => {
                matches!(state.as_str(), "abandoned" | "satisfied")
            }
        }
    {
        return Err(bad("intent_state"));
    }
    let request: DelegationRequest =
        serde_json::from_str(&payload).map_err(|_| bad("intent_payload"))?;
    if request != record.submission.request || workspace != route.workspace.to_storage() {
        return Err(bad("intent_identity"));
    }
    Ok(RoutineClaimObservation::Claimed { record })
}
