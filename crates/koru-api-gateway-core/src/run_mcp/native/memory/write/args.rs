// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::bots::BotMemoryNodeId;
use koru_mcp_server::native::memory::validate_memory_text;
use serde::Deserialize;

const ARGUMENT_BYTES: usize = 24 * 1024;

pub(super) struct Arguments {
    pub operation_id: Uuid,
    pub action: MemoryEditAction,
    pub base: MemoryBaseSelector,
    pub operation: MemoryRecipeOperation,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Remember {
    operation_id: String,
    expected_version: Option<String>,
    summary: String,
    detail: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Correct {
    operation_id: String,
    expected_version: String,
    fact_id: BotMemoryNodeId,
    summary: String,
    detail: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Forget {
    operation_id: String,
    expected_version: String,
    fact_id: BotMemoryNodeId,
}
fn uuid(raw: &str) -> Result<Uuid, ToolFailure> {
    let id = Uuid::parse_str(raw).map_err(|_| failure("invalid_request"))?;
    if id.to_string() != raw || id.is_nil() {
        return Err(failure("invalid_request"));
    }
    Ok(id)
}
fn history(raw: &str) -> Result<MemoryBaseSelector, ToolFailure> {
    Ok(MemoryBaseSelector::History {
        version: DocumentVersionId::from_uuid(uuid(raw)?),
    })
}
// Count serialized bytes without allocating another copy of attacker arguments.
struct Counter(usize);
impl std::io::Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|n| *n <= ARGUMENT_BYTES)
            .ok_or_else(|| std::io::Error::other("Memory arguments exceed limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) fn parse(name: &str, value: Value) -> Result<Arguments, ToolFailure> {
    serde_json::to_writer(Counter(0), &value).map_err(|_| failure("invalid_request"))?;
    if value.get("expected_version").is_some_and(Value::is_null) {
        return Err(failure("invalid_request"));
    }
    let (raw, action, base, operation) = match name {
        "remember_memory" => {
            let a: Remember =
                serde_json::from_value(value).map_err(|_| failure("invalid_request"))?;
            (
                a.operation_id,
                MemoryEditAction::Remember,
                a.expected_version
                    .as_deref()
                    .map(history)
                    .transpose()?
                    .unwrap_or(MemoryBaseSelector::OriginalSeed {}),
                MemoryRecipeOperation::Remember {
                    summary: a.summary,
                    detail: a.detail,
                },
            )
        }
        "correct_memory" => {
            let a: Correct =
                serde_json::from_value(value).map_err(|_| failure("invalid_request"))?;
            (
                a.operation_id,
                MemoryEditAction::Correct,
                history(&a.expected_version)?,
                MemoryRecipeOperation::Correct {
                    fact: a.fact_id.as_uuid(),
                    summary: a.summary,
                    detail: a.detail,
                },
            )
        }
        "forget_memory" => {
            let a: Forget =
                serde_json::from_value(value).map_err(|_| failure("invalid_request"))?;
            (
                a.operation_id,
                MemoryEditAction::Forget,
                history(&a.expected_version)?,
                MemoryRecipeOperation::Forget {
                    fact: a.fact_id.as_uuid(),
                },
            )
        }
        _ => return Err(failure("invalid_request")),
    };
    let operation_id = uuid(&raw)?;
    if operation_id.get_version_num() != 4 || operation_id.get_variant() != uuid::Variant::RFC4122 {
        return Err(failure("invalid_request"));
    }
    if let MemoryRecipeOperation::Remember { summary, detail }
    | MemoryRecipeOperation::Correct {
        summary, detail, ..
    } = &operation
    {
        validate_memory_text(summary, detail).map_err(|_| failure("invalid_memory_text"))?;
    }
    Ok(Arguments {
        operation_id,
        action,
        base,
        operation,
    })
}

pub(super) fn definitions() -> Vec<Tool> {
    ["remember_memory", "correct_memory", "forget_memory"].into_iter().map(|name| {
        let remember = name == "remember_memory";
        let forget = name == "forget_memory";
        let mut properties = json!({
            "operation_id":{"type":"string","format":"uuid","pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"},
            "expected_version":{"type":"string","format":"uuid"}
        });
        let mut required = vec!["operation_id"];
        if !remember {
            properties["fact_id"] = json!({"type":"string","format":"uuid","pattern":"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"});
            required.extend(["expected_version", "fact_id"]);
        }
        if !forget {
            properties["summary"] = json!({"type":"string","minLength":1,"maxLength":1024});
            properties["detail"] = json!({"type":"string","minLength":1,"maxLength":16384});
            required.extend(["summary", "detail"]);
        }
        let description = match name {
            "remember_memory" => "Add a fact to this Bot's Memory. Summary/detail are single paragraphs. Pass an exact history version from list_memory/read_memory; omit it only to initialize and edit the unchanged original empty seed. Omission never means latest. Retry the same operation_id with identical arguments.",
            "correct_memory" => "Replace the selected fact's summary and entire detail/source subtree, preserving its fact ID. Use the ordinary editor for unsupported rich or oversize facts. Pass an exact history version from list_memory/read_memory; stale edits conflict. Summary/detail are single paragraphs. Retry the same operation_id with identical arguments.",
            _ => "Move the selected fact and its descendants to ordinary Trash in this Bot's Memory. History, transcripts and runtime memory are not erased. Pass an exact history version from list_memory/read_memory; stale edits conflict. Retry the same operation_id with identical arguments.",
        };
        serde_json::from_value(json!({"name":name,"description":description,
            "inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false},
            "annotations":{"readOnlyHint":false,"destructiveHint":!remember,"idempotentHint":true,"openWorldHint":false}
        })).expect("fixed Memory write schema")
    }).collect()
}
