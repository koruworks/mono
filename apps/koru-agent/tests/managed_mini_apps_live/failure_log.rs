// SPDX-License-Identifier: MPL-2.0

//! Read only bounded tails from this fixture's own session-host logs. Parse
//! three known support events; reconstruct output from closed fields only.

use koru_session_protocol::{ErrorCode, SessionId};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

const MAX_ENTRIES: usize = 64;
const MAX_FILES: usize = 8;
const MAX_BYTES: u64 = 64 * 1024;
const MAX_LINE: usize = 4096;
const MAX_RECORDS: usize = 32;
const STAGES: &[&str] = &[
    "Discovery",
    "Version",
    "Spawn",
    "Initialize",
    "Read",
    "Write",
    "Submit",
    "Cancel",
    "Approval",
    "Shutdown",
];
const DIAGNOSTICS: &[&str] = &[
    "MissingExecutable",
    "AuthenticationRequired",
    "RuntimeUnavailable",
    "InvalidVersion",
    "VersionRejected",
    "HandshakeRejected",
    "Timeout",
    "Spawn",
    "Io",
    "MalformedFrame",
    "FrameTooLarge",
    "Backpressure",
    "UnexpectedExit",
    "InvalidState",
    "InvalidInput",
    "ReasoningRejected",
];

#[derive(Debug, PartialEq, Eq)]
enum Record {
    McpFailed {
        code: &'static str,
        elapsed_ms: u64,
        reason: Option<&'static str>,
    },
    AcpStart(LogSession),
    Launch {
        code: ErrorCode,
        stage: &'static str,
        diagnostic: &'static str,
        os_code: i32,
    },
    AcpFailed {
        operation: &'static str,
        code: ErrorCode,
        stage: &'static str,
        diagnostic: &'static str,
    },
    AcpTurn {
        session: LogSession,
        outcome: &'static str,
        code: Option<ErrorCode>,
        counts: [u64; 7],
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum LogSession {
    Prefix(u32),
    Full(SessionId),
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let mut values = line.split_ascii_whitespace().filter_map(|token| {
        let (name, value) = token.split_once('=')?;
        (name == key).then_some(value)
    });
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    if value.starts_with('"') {
        value.strip_prefix('"')?.strip_suffix('"')
    } else {
        Some(value)
    }
}

fn known(value: &str, whitelist: &'static [&'static str]) -> Option<&'static str> {
    whitelist.iter().copied().find(|known| *known == value)
}

fn code(value: &str) -> Option<ErrorCode> {
    // ErrorCode accepts unknown future strings. This diagnostic deliberately
    // does not: only declared variants can ever be printed.
    ErrorCode::ALL
        .iter()
        .find(|known| known.as_str() == value)
        .cloned()
}

fn validation_reason(line: &str) -> Option<&'static str> {
    // Read only the fixed leading category, never the message or arguments.
    let mut errors = line
        .split_ascii_whitespace()
        .filter_map(|field| field.strip_prefix("error="));
    let value = errors.next()?;
    if errors.next().is_some() {
        return None;
    }
    let value = value.strip_prefix('"').unwrap_or(value).strip_suffix(':')?;
    [
        "mini_app_validation_missing_field",
        "mini_app_validation_unknown_field",
        "mini_app_validation_duplicate_field",
        "mini_app_validation_field_type",
        "mini_app_validation_field_value",
        "mini_app_validation_json",
        "mini_app_validation_shape",
        "mini_app_validation_size",
        "mini_app_validation_surface",
        "mini_app_validation_components",
        "mini_app_validation_children",
        "mini_app_validation_arrangement",
        "mini_app_validation_task_selection",
        "mini_app_validation_document_scope",
        "mini_app_validation_refinement",
        "mini_app_validation_list_query",
    ]
    .into_iter()
    .find(|category| value == *category)
}

fn session(line: &str) -> Option<LogSession> {
    // SessionId asks its UUID formatter for eight characters, but uuid's
    // Hyphenated formatter ignores precision and emits the complete UUID.
    // Accept that actual format and the bounded prefix spelling. Both stay
    // memory-only for correlation; neither is ever printed by this reader.
    let value = field(line, "session_id")?
        .strip_prefix("Some(session(")?
        .strip_suffix("))")?;
    if value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return u32::from_str_radix(value, 16).ok().map(LogSession::Prefix);
    }
    if value.len() == 36 {
        return value.parse().ok().map(LogSession::Full);
    }
    None
}

fn parse(line: &str, provider: &str) -> Option<Record> {
    if line.len() > MAX_LINE {
        return None;
    }
    if line.contains("tool failed") && field(line, "tool") == Some("mini_app_tool") {
        let code = known(
            field(line, "code")?,
            &[
                "invalid_request",
                "mini_app_unavailable",
                "unauthenticated",
                "forbidden",
                "internal_error",
                "service_unavailable",
                "worker_saturated",
                "ai_unavailable",
                "ai_timeout",
                "request_timeout",
                "network_error",
                "chat_not_found",
                "profiles_unavailable",
                "ai_configuration",
                "ai_authentication",
                "ai_rate_limit",
                "ai_context_too_large",
                "ai_model_unavailable",
                "ai_network",
                "ai_provider",
                "ai_cancelled",
                "ai_storage",
            ],
        )?;
        let elapsed = field(line, "elapsed_ms")?.parse::<f64>().ok()?;
        if !elapsed.is_finite() || !(0.0..=600_000.0).contains(&elapsed) {
            return None;
        }
        return Some(Record::McpFailed {
            code,
            elapsed_ms: elapsed.round() as u64,
            reason: validation_reason(line),
        });
    }
    match field(line, "event")? {
        "terminal.session.launch_failed" => {
            if field(line, "mode")? != "managed" || field(line, "provider")? != provider {
                return None;
            }
            Some(Record::Launch {
                code: code(field(line, "code")?)?,
                stage: known(field(line, "stage")?, STAGES)?,
                diagnostic: known(field(line, "diagnostic")?, DIAGNOSTICS)?,
                os_code: field(line, "os_code")?.parse().ok()?,
            })
        }
        "acp.starting" if provider == "claude" && field(line, "provider")? == provider => {
            Some(Record::AcpStart(session(line)?))
        }
        "acp.failed" if provider == "claude" && field(line, "provider")? == provider => {
            Some(Record::AcpFailed {
                operation: known(
                    field(line, "operation")?,
                    &["start", "transport", "shutdown"],
                )?,
                code: code(field(line, "code")?)?,
                stage: known(field(line, "stage")?, STAGES)?,
                diagnostic: known(field(line, "diagnostic")?, DIAGNOSTICS)?,
            })
        }
        "acp.turn_finished" if provider == "claude" => {
            let value = field(line, "error_code")?;
            Some(Record::AcpTurn {
                session: session(line)?,
                outcome: known(
                    field(line, "outcome")?,
                    &["completed", "cancelled", "failed", "shutdown"],
                )?,
                code: if value == "none" {
                    None
                } else {
                    Some(code(value)?)
                },
                counts: [
                    field(line, "message_chunks")?.parse().ok()?,
                    field(line, "tools")?.parse().ok()?,
                    field(line, "failed_tools")?.parse().ok()?,
                    field(line, "approvals")?.parse().ok()?,
                    field(line, "session_unknown_events")?.parse().ok()?,
                    field(line, "session_foreign_signals")?.parse().ok()?,
                    field(line, "session_queue_high_water")?.parse().ok()?,
                ],
            })
        }
        _ => None,
    }
}

fn filename(name: &str) -> bool {
    let Some(date) = name
        .strip_prefix("koru-session-host.log.")
        .or_else(|| name.strip_prefix("koru-mcp.log."))
    else {
        return false;
    };
    date.len() == 10
        && date.bytes().enumerate().all(|(index, byte)| {
            if index == 4 || index == 7 {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}

pub(super) fn report(root: &Path, provider: &str) {
    let Some(provider) = provider
        .strip_prefix("agent-")
        .filter(|kind| matches!(*kind, "codex" | "claude"))
    else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(root.join("store/logs")) else {
        eprintln!("managed_mini_app_failure_log: unavailable=true");
        return;
    };
    let mut paths = Vec::new();
    for entry in entries.take(MAX_ENTRIES).flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_file())
            && entry.file_name().to_str().is_some_and(filename)
        {
            paths.push(entry.path());
            if paths.len() >= MAX_FILES {
                break;
            }
        }
    }
    paths.sort();
    let mut sessions = BTreeSet::new();
    let mut reported = 0;
    let mut read_files = 0;
    for path in paths {
        let Ok(mut file) = File::open(path) else {
            continue;
        };
        let Ok(metadata) = file.metadata() else {
            continue;
        };
        let start = metadata.len().saturating_sub(MAX_BYTES);
        if file.seek(SeekFrom::Start(start)).is_err() {
            continue;
        }
        let mut bytes = Vec::with_capacity(MAX_BYTES as usize);
        if file.take(MAX_BYTES).read_to_end(&mut bytes).is_err() {
            continue;
        }
        read_files += 1;
        for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
            if (start != 0 && index == 0) || line.len() > MAX_LINE {
                continue;
            }
            let Ok(line) = std::str::from_utf8(line) else {
                continue;
            };
            let Some(record) = parse(line, provider) else {
                continue;
            };
            match record {
                Record::McpFailed {
                    code,
                    elapsed_ms,
                    reason,
                } => {
                    eprintln!(
                        "managed_mini_app_mcp_failure: tool=mini_app_tool, code={code}, elapsed_ms={elapsed_ms}, validation={}",
                        reason.unwrap_or("withheld")
                    );
                    reported += 1;
                }
                Record::AcpStart(session) => {
                    if sessions.len() < MAX_RECORDS {
                        sessions.insert(session);
                    }
                }
                Record::Launch {
                    code,
                    stage,
                    diagnostic,
                    os_code,
                } => {
                    eprintln!(
                        "managed_mini_app_launch_failure: provider={provider}, code={code}, stage={stage}, diagnostic={diagnostic}, os_code={os_code}"
                    );
                    reported += 1;
                }
                Record::AcpFailed {
                    operation,
                    code,
                    stage,
                    diagnostic,
                } => {
                    eprintln!(
                        "managed_mini_app_acp_failure: operation={operation}, code={code}, stage={stage}, diagnostic={diagnostic}"
                    );
                    reported += 1;
                }
                Record::AcpTurn {
                    session,
                    outcome,
                    code,
                    counts,
                } if sessions.contains(&session) => {
                    eprintln!(
                        "managed_mini_app_acp_turn: outcome={outcome}, code={code:?}, message_chunks={}, tools={}, failed_tools={}, approvals={}, unknown_events={}, foreign_signals={}, queue_high_water={}",
                        counts[0], counts[1], counts[2], counts[3], counts[4], counts[5], counts[6]
                    );
                    reported += 1;
                }
                _ => {}
            }
            if reported >= MAX_RECORDS {
                break;
            }
        }
        if reported >= MAX_RECORDS {
            break;
        }
    }
    eprintln!(
        "managed_mini_app_failure_log: files_read={read_files}, known_events={reported}, bounded_tail=true"
    );
}

#[cfg(test)]
mod tests;
