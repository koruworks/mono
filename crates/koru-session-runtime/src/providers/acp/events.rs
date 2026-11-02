// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::providers::tool_output::bound_tool_output;
use koru_session_protocol::{ToolFailure, bound_tool_content, bound_tool_detail};

impl AcpControl {
    pub(super) fn handle_frame(&mut self, frame: Value) -> Result<(), ProviderFailure> {
        if frame.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(malformed());
        }
        if let Some(method) = frame.get("method").and_then(Value::as_str) {
            if let Some(id) = frame.get("id") {
                if !(id.as_i64().is_some() || id.as_str().is_some_and(valid_native_id)) {
                    return Err(malformed());
                }
                if method == "session/request_permission" {
                    return self.permission(id.clone(), &frame["params"]);
                }
                self.mark_unknown();
                return self.transport()?.send(&json!({"jsonrpc":"2.0","id":id,
                    "error":{"code":-32601,"message":"Method not found"}}));
            }
            if self.config.route.is_claude() && method == "_auth/status_update" {
                self.wrapper_auth = Some(claude::auth_status(&frame["params"]));
                return Ok(());
            }
            if method == "session/update" {
                return self.update(&frame["params"]);
            }
            self.mark_unknown();
            return Ok(());
        }
        let Some(turn_id) = self.active else {
            self.mark_unknown();
            return Ok(());
        };
        if frame.get("id").and_then(Value::as_u64) != Some(self.turns[&turn_id].request_id) {
            self.mark_unknown();
            return Ok(());
        }
        if frame.get("result").is_some() == frame.get("error").is_some() {
            return Err(malformed());
        }
        if frame.get("error").is_some() {
            return self.finish(None);
        }
        // Unknown stop reasons must fail, even if the older pinned enum
        // cannot deserialize them. They are never silently end_turn.
        self.finish(frame.pointer("/result/stopReason").and_then(Value::as_str))
    }

    fn update(&mut self, params: &Value) -> Result<(), ProviderFailure> {
        if self.config.route.is_grok() && self.grok_inventory_update(params)? {
            return Ok(());
        }
        if self.native_session.is_some()
            && params["sessionId"].as_str() != self.native_session.as_deref()
        {
            if params["sessionId"].as_str().is_some() {
                self.log.foreign_session();
            }
            self.mark_unknown();
            return Ok(());
        }
        let update = &params["update"];
        // The agent's mode moves with or without a turn: Claude leaving plan
        // mode once its plan is approved, or answering a set (koru-ai D7).
        match update["sessionUpdate"].as_str() {
            Some("current_mode_update") => {
                let reported = update["currentModeId"]
                    .as_str()
                    .and_then(|id| id.parse().ok());
                let moved = self.follow_mode(reported);
                return self.report_mode(moved);
            }
            Some("config_option_update") => {
                // Claude's SDK may turn Fast mode off itself (D11).
                if update.get("configOptions").is_some() {
                    self.fast = super::models::fast_option(update);
                }
                let moved = self.follow_mode(super::modes::current(update));
                return self.report_mode(moved);
            }
            _ => {}
        }
        let Some(turn_id) = self.active else {
            self.mark_unknown();
            return Ok(());
        };
        let kind = update["sessionUpdate"].as_str().ok_or_else(malformed)?;
        if !matches!(
            kind,
            "agent_message_chunk" | "tool_call" | "tool_call_update"
        ) {
            // Plan, thought and extension updates deliberately have no new
            // orchestration semantics in this adapter.
            self.mark_unknown();
            return Ok(());
        }
        decode::<acp::SessionNotification>(params)?;
        // Late content cannot replenish the event queue while stopping.
        if self.turns[&turn_id].cancelling.is_some() {
            self.mark_unknown();
            return Ok(());
        }
        match kind {
            "agent_message_chunk" => {
                if update["content"]["type"].as_str() != Some("text") {
                    self.mark_unknown();
                    return Ok(());
                }
                let text = update["content"]["text"].as_str().ok_or_else(malformed)?;
                self.queue(AgentEvent::MessageDelta {
                    turn_id,
                    text: text.to_owned(),
                })
            }
            "tool_call" | "tool_call_update" => self.tool(turn_id, update),
            _ => unreachable!(),
        }
    }

    /// A `tool_call` or `tool_call_update`. **What a call does often comes
    /// after the call**: Claude announces a tool as soon as the model starts
    /// writing its input — titled *Terminal*, *Read File* or *Edit*, with
    /// `rawInput: {}` — and sends the command, the path and the real title
    /// in an update once the input is complete. The start is held until an
    /// update says what the call does (any update carrying `rawInput`, a
    /// non-empty input, or a status past *pending*), or until anything else
    /// has to be queued, so the row is written once with what it did.
    fn tool(&mut self, turn_id: AgentTurnId, update: &Value) -> Result<(), ProviderFailure> {
        let native_id = update["toolCallId"]
            .as_str()
            .filter(|value| valid_native_id(value))
            .ok_or_else(malformed)?;
        let kind = tool_kind(update);
        if !self.tools.contains_key(native_id) {
            self.tools.insert(
                native_id.to_owned(),
                Tool {
                    id: ToolCallId::mint(),
                    kind,
                    completed: false,
                    ready: false,
                    title: None,
                    input: None,
                    diff: None,
                    text: None,
                    raw_output: None,
                },
            );
            self.unstarted.push(native_id.to_owned());
        }
        let tool = self.tools.get_mut(native_id).unwrap();
        if tool.completed {
            return Ok(());
        }
        if update.get("kind").is_some() {
            tool.kind = kind;
        }
        refine_tool(tool, update);
        let status = update["status"].as_str();
        // An update that carries an input says what the call does, even an
        // empty one (a tool that takes no arguments); so does a status past
        // pending. The very first `tool_call` says it only with a real input.
        let says_what = match update.get("rawInput") {
            Some(Value::Null) | None => false,
            Some(input) => update["sessionUpdate"] == "tool_call_update" || !empty_input(input),
        } || matches!(status, Some("in_progress" | "completed" | "failed"));
        tool.ready |= says_what;
        // Ready calls start once every call announced before them has, so
        // the rows keep the order the agent made them in.
        let ready = self
            .unstarted
            .iter()
            .take_while(|id| self.tools.get(*id).is_some_and(|tool| tool.ready))
            .count();
        self.flush_starts(ready)?;
        let tool = self.tools.get_mut(native_id).unwrap();
        if matches!(status, Some("completed" | "failed")) {
            tool.completed = true;
            let id = tool.id;
            let kind = tool.kind;
            let failed = status == Some("failed");
            let output = tool_output(tool);
            self.queue(AgentEvent::ToolCompleted {
                turn_id,
                tool_call_id: id,
                succeeded: !failed,
                tool: Some(kind),
                failure: failed.then_some(ToolFailure::Errored),
                detail: failed.then(|| failure_detail(update)).flatten(),
                output,
            })?;
        }
        Ok(())
    }

    fn permission(&mut self, request_id: Value, params: &Value) -> Result<(), ProviderFailure> {
        // The schema's permission-kind enum is closed, but a future kind is an
        // unsupported choice rather than a malformed request. Validate every
        // option's basic fields, then validate the representable request with
        // the pinned schema. Native identities stay in this private map.
        let options = params["options"].as_array().ok_or_else(malformed)?;
        let mut choices = Vec::new();
        let mut native_options = BTreeMap::new();
        let mut native_ids = std::collections::BTreeSet::new();
        let mut represented = Vec::new();
        for option in options {
            let native = option["optionId"]
                .as_str()
                .filter(|value| valid_native_id(value))
                .ok_or_else(malformed)?;
            if !native_ids.insert(native) {
                return Err(malformed());
            }
            let kind = option["kind"].as_str().ok_or_else(malformed)?;
            let label = option["name"].as_str().ok_or_else(malformed)?;
            use koru_session_protocol::ApprovalIntent;
            let intent = match kind {
                "allow_once" => ApprovalIntent::Allow,
                "allow_always" => ApprovalIntent::AllowRemembered,
                "reject_once" => ApprovalIntent::Reject,
                "reject_always" => ApprovalIntent::RejectRemembered,
                _ => continue,
            };
            let id = koru_session_protocol::ApprovalChoiceId::mint();
            choices.push(koru_session_protocol::ApprovalChoice {
                id,
                intent,
                label: label.to_owned(),
            });
            native_options.insert(id, native.to_owned());
            represented.push(option.clone());
        }
        let mut validated = params.clone();
        validated["options"] = Value::Array(represented);
        decode::<acp::RequestPermissionRequest>(&validated)?;
        let Some(turn_id) = self.active else {
            return self.reply_permission(request_id, None);
        };
        if params["sessionId"].as_str() != self.native_session.as_deref() {
            self.log.foreign_session();
            return self.reply_permission(request_id, None);
        }
        if self.turns[&turn_id].cancelling.is_some() {
            return self.reply_permission(request_id, None);
        }
        // A duplicated provider request never opens a second local approval.
        if self
            .approvals
            .values()
            .any(|pending| pending.request_id == request_id)
        {
            return Ok(());
        }
        // Reserve one eventual expiration/resolution per pending request, so
        // content backpressure can never prevent a controlled stop.
        if self.events.len() + self.approvals.len() + 2 > self.config.event_capacity {
            return Err(failure(
                ErrorCode::Backpressure,
                ProviderStage::Approval,
                ProviderDiagnostic::Backpressure,
            ));
        }
        let approval_id = ApprovalId::mint();
        self.approvals.insert(
            approval_id,
            PendingApproval {
                request_id,
                turn_id,
                choices: choices.clone(),
                native_options,
            },
        );
        if choices.is_empty() {
            // No fabricated offer: the minted approval is only an expiration
            // fact, and cancelling owns its response and reserved event slot.
            return self.cancel_active(turn_id, true);
        }
        // The request names the call it is for, with its title and input:
        // a call still held back starts with them, just before the request.
        if let Some(tool) = params["toolCall"]["toolCallId"]
            .as_str()
            .and_then(|native_id| self.tools.get_mut(native_id))
        {
            refine_tool(tool, &params["toolCall"]);
        }
        self.queue(AgentEvent::ApprovalRequired {
            turn_id,
            approval_id,
            choices,
            summary: params["toolCall"]["title"]
                .as_str()
                .unwrap_or("provider_action")
                .to_owned(),
        })
    }

    pub(super) fn reply_permission(
        &mut self,
        request_id: Value,
        option: Option<&str>,
    ) -> Result<(), ProviderFailure> {
        let outcome = match option {
            Some(id) => json!({"outcome":"selected","optionId":id}),
            None => json!({"outcome":"cancelled"}),
        };
        let result = self
            .transport()?
            .send(&json!({"jsonrpc":"2.0","id":request_id,"result":{"outcome":outcome}}));
        if let Err(error) = &result {
            self.fail_transport(error.clone());
        }
        result
    }

    pub(super) fn expire_permissions(
        &mut self,
        turn_id: AgentTurnId,
    ) -> Result<(), ProviderFailure> {
        let ids: Vec<_> = self
            .approvals
            .iter()
            .filter_map(|(id, pending)| (pending.turn_id == turn_id).then_some(*id))
            .collect();
        for approval_id in ids {
            let pending = self.approvals.remove(&approval_id).unwrap();
            self.reply_permission(pending.request_id, None)?;
            self.queue(AgentEvent::ApprovalExpired {
                turn_id,
                approval_id,
            })?;
        }
        Ok(())
    }
}

/// A failed call's own words, and nothing else of its payload: the first
/// text block of the update's `content`, else `rawOutput.message` or
/// `rawOutput.error` when the agent put a string there. Bounded by the
/// protocol's [`TOOL_DETAIL_CHARS`](koru_session_protocol::TOOL_DETAIL_CHARS).
fn failure_detail(update: &Value) -> Option<String> {
    let from_content = update["content"].as_array().and_then(|blocks| {
        blocks.iter().find_map(|block| {
            block["content"]["text"]
                .as_str()
                .or_else(|| block["text"].as_str())
        })
    });
    let from_raw = || {
        ["message", "error"]
            .iter()
            .find_map(|key| update["rawOutput"][key].as_str())
    };
    from_content.or_else(from_raw).and_then(bound_tool_detail)
}

/// The kind Koru files a call under, from ACP's `kind`: a command, a change
/// to files, a reach to the web; everything else is read from the title
/// later (`AgentToolKind::refined`).
fn tool_kind(update: &Value) -> ToolKind {
    match update["kind"].as_str() {
        Some("execute") => ToolKind::CommandExecution,
        Some("edit" | "delete" | "move") => ToolKind::FileChange,
        Some("fetch") => ToolKind::WebSearch,
        _ => ToolKind::Other,
    }
}

/// Whether a `rawInput` says nothing: `{}`, `[]`, `""`.
fn empty_input(input: &Value) -> bool {
    match input {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::String(text) => text.trim().is_empty(),
        _ => false,
    }
}

/// Take what a `tool_call`, a `tool_call_update` or a permission request's
/// `toolCall` says about the call: its latest title and input (UC17), and
/// what it produced so far.
///
/// **Content that comes with an input describes the call, not its result**:
/// Claude's update carrying a command also carries the command's
/// description as text, and an edit's carries the edit as a diff. So text
/// is taken only from updates without an input (a result, a hook's report),
/// and diffs from any update, the latest winning. `rawOutput` is kept apart:
/// it is the result as the model saw it, and an edit's says only *The file
/// was updated*, which must not replace the diff.
fn refine_tool(tool: &mut Tool, update: &Value) {
    /// A title is a line, not a transcript.
    const TOOL_LABEL_CHARS: usize = 160;
    if let Some(title) = update["title"]
        .as_str()
        .map(str::trim)
        .filter(|title| !title.is_empty())
    {
        tool.title = Some(title.chars().take(TOOL_LABEL_CHARS).collect());
    }
    if let Some(input) = update.get("rawInput").filter(|input| !empty_input(input))
        && let Some(text) = value_text(input).and_then(|text| bound_tool_content(&text))
    {
        tool.input = Some(text);
    }
    if let Some(blocks) = update["content"].as_array() {
        if let Some(diff) = diff_text(blocks, update["locations"].as_array()) {
            tool.diff = Some(diff);
        }
        if update.get("rawInput").is_none()
            && let Some(text) = content_text(blocks)
        {
            tool.text = Some(text);
        }
    }
    if let Some(raw) = update.get("rawOutput").and_then(value_text) {
        tool.raw_output = Some(raw);
    }
}

/// What the tool produced (UC17): its diffs and its result text, else its
/// `rawOutput`. Bounded by the protocol's `TOOL_CONTENT_CHARS`; a JSON result
/// too long for it is summarized ([`bound_tool_output`]).
fn tool_output(tool: &mut Tool) -> Option<String> {
    let produced = [tool.diff.take(), tool.text.take()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
    let text = if produced.trim().is_empty() {
        tool.raw_output.take()?
    } else {
        produced
    };
    bound_tool_output(&text)
}

/// A content array's text blocks, joined; `None` when it has none.
fn content_text(blocks: &[Value]) -> Option<String> {
    let text = blocks
        .iter()
        .filter(|block| block["type"] == "content")
        .filter_map(|block| block["content"]["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}

/// A content array's diff blocks, as one unified diff. Claude's edit
/// arrives twice: first built from the call's input (`old_string`,
/// `new_string`), then — from the hook that ran after the edit — once per
/// hunk, with the hunk's first line in the update's `locations`, in the
/// same order. A hunk whose first line is not known has a bare `@@` header
/// and no line numbers.
fn diff_text(blocks: &[Value], locations: Option<&Vec<Value>>) -> Option<String> {
    let diffs = blocks
        .iter()
        .filter(|block| block["type"] == "diff")
        .collect::<Vec<_>>();
    let lines = locations
        .filter(|locations| locations.len() == diffs.len())
        .map(|locations| {
            locations
                .iter()
                .map(|location| location["line"].as_u64())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec![None; diffs.len()]);
    let mut text = String::new();
    let mut path = None;
    for (block, first_line) in diffs.into_iter().zip(lines) {
        let Some(new_text) = block["newText"].as_str() else {
            continue;
        };
        let block_path = block["path"].as_str().unwrap_or_default();
        let old_text = block["oldText"].as_str();
        // The hunks of one file share its header.
        if path != Some(block_path) {
            let from = if old_text.is_some() {
                block_path
            } else {
                "/dev/null"
            };
            text.push_str(&format!("--- {from}\n+++ {block_path}\n"));
            path = Some(block_path);
        }
        unified_hunks(
            old_text.unwrap_or_default(),
            new_text,
            first_line,
            &mut text,
        );
    }
    (!text.is_empty()).then_some(text)
}

/// The hunks between `old` and `new`, with three lines of context, as a
/// unified diff's body. `first_line` is where `old` and `new` start in the
/// file, when known.
fn unified_hunks(old: &str, new: &str, first_line: Option<u64>, text: &mut String) {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(old, new);
    for group in diff.grouped_ops(3) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        match first_line {
            Some(line) => {
                let offset = line.saturating_sub(1) as usize;
                let range = |start: usize, end: usize| {
                    let count = end - start;
                    // An empty side is numbered by the line before it.
                    let start = if count == 0 { start } else { start + 1 } + offset;
                    format!("{start},{count}")
                };
                text.push_str(&format!(
                    "@@ -{} +{} @@\n",
                    range(first.old_range().start, last.old_range().end),
                    range(first.new_range().start, last.new_range().end),
                ));
            }
            None => text.push_str("@@\n"),
        }
        for op in &group {
            for change in diff.iter_changes(op) {
                text.push(match change.tag() {
                    ChangeTag::Equal => ' ',
                    ChangeTag::Delete => '-',
                    ChangeTag::Insert => '+',
                });
                text.push_str(change.value().trim_end_matches(['\n', '\r']));
                text.push('\n');
            }
        }
    }
}

/// A string as it is; anything else as compact JSON; nothing for null.
fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => serde_json::to_string(other).ok(),
    }
}
