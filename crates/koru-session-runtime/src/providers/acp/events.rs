// SPDX-License-Identifier: MPL-2.0

use super::*;
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

    fn tool(&mut self, turn_id: AgentTurnId, update: &Value) -> Result<(), ProviderFailure> {
        /// A title is a line, not a transcript.
        const TOOL_LABEL_CHARS: usize = 160;
        let native_id = update["toolCallId"]
            .as_str()
            .filter(|value| valid_native_id(value))
            .ok_or_else(malformed)?;
        let kind = match update["kind"].as_str() {
            Some("execute") => ToolKind::CommandExecution,
            Some("edit" | "delete" | "move") => ToolKind::FileChange,
            _ => ToolKind::Other,
        };
        if !self.tools.contains_key(native_id) {
            let id = ToolCallId::mint();
            self.tools.insert(
                native_id.to_owned(),
                Tool {
                    id,
                    kind,
                    completed: false,
                    output: None,
                },
            );
            // The agent's own title for the call (*Read README.md*, *Bash git
            // status*), bounded, and the kind's token when it gave none — a
            // label is what a person reads on the row while the turn is live.
            let title = update["title"]
                .as_str()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(|title| title.chars().take(TOOL_LABEL_CHARS).collect::<String>());
            self.queue(AgentEvent::ToolStarted {
                turn_id,
                tool_call_id: id,
                label: title.unwrap_or_else(|| kind.as_str().to_owned()),
                input: tool_input(update),
            })?;
        }
        let tool = self.tools.get_mut(native_id).unwrap();
        if tool.completed {
            return Ok(());
        }
        if update.get("kind").is_some() {
            tool.kind = kind;
        }
        if let Some(output) = tool_output(update) {
            tool.output = Some(output);
        }
        let status = update["status"].as_str();
        if matches!(status, Some("completed" | "failed")) {
            tool.completed = true;
            let id = tool.id;
            let kind = tool.kind;
            let failed = status == Some("failed");
            let output = tool.output.take();
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

/// What the agent asked the tool to do (UC17): the call's `rawInput`, the
/// string itself when it is one, compact JSON when it is an object.
/// Bounded by the protocol's `TOOL_CONTENT_CHARS`.
fn tool_input(update: &Value) -> Option<String> {
    bound_tool_content(&value_text(update.get("rawInput")?)?)
}

/// What the tool produced (UC17), from the update's content blocks — a
/// text block as it is, a diff as its path over its new text — else the
/// `rawOutput`. Bounded by the protocol's `TOOL_CONTENT_CHARS`.
fn tool_output(update: &Value) -> Option<String> {
    let mut text = String::new();
    for block in update["content"].as_array().into_iter().flatten() {
        let piece = match block["type"].as_str() {
            Some("content") => block["content"]["text"].as_str().map(str::to_owned),
            Some("diff") => block["newText"].as_str().map(|new_text| {
                format!("{}\n{new_text}", block["path"].as_str().unwrap_or_default())
            }),
            _ => None,
        };
        if let Some(piece) = piece {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&piece);
        }
    }
    if text.is_empty() {
        text = value_text(update.get("rawOutput")?)?;
    }
    bound_tool_content(&text)
}

/// A string as it is; anything else as compact JSON; nothing for null.
fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => serde_json::to_string(other).ok(),
    }
}
