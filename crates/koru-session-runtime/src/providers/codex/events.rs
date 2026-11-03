// SPDX-License-Identifier: MPL-2.0

use koru_session_protocol::{
    AgentEvent, AgentTurnId, ApprovalId, ErrorCode, SessionFailure, ToolCallId,
};
use serde_json::{Value, json};

use super::{
    ApprovalKind, CodexControl, PendingApproval, ProviderDiagnostic, ProviderFailure,
    ProviderStage, RpcId, approval_summary, failure, malformed_read, normalize, provider_kind,
    valid_native_id,
};

impl CodexControl {
    pub(super) fn queue(&mut self, event: AgentEvent) -> Result<(), ProviderFailure> {
        if self.events.len() >= self.config.event_capacity {
            return Err(failure(
                ErrorCode::Backpressure,
                ProviderStage::Read,
                ProviderDiagnostic::Backpressure,
            ));
        }
        self.events.push_back(event);
        self.diagnostics.maximum_queue_depth =
            self.diagnostics.maximum_queue_depth.max(self.events.len());
        Ok(())
    }

    fn mark_unknown(&mut self) {
        self.diagnostics.unknown_events = self.diagnostics.unknown_events.saturating_add(1);
    }

    pub(super) fn handle_frame(&mut self, frame: Value) -> Result<(), ProviderFailure> {
        if frame.get("method").is_none() {
            self.mark_unknown();
            return Ok(());
        }
        if frame.get("id").is_some() {
            self.handle_server_request(&frame)
        } else {
            self.handle_notification(&frame)
        }
    }

    fn handle_server_request(&mut self, frame: &Value) -> Result<(), ProviderFailure> {
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            self.mark_unknown();
            return Ok(());
        };
        let Some(request_id) = frame.get("id").and_then(RpcId::parse) else {
            return Err(failure(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Approval,
                ProviderDiagnostic::MalformedFrame,
            ));
        };
        let params = frame.get("params").unwrap_or(&Value::Null);
        let kind = match method {
            "item/commandExecution/requestApproval" => ApprovalKind::Command,
            "item/fileChange/requestApproval" => ApprovalKind::FileChange,
            "item/permissions/requestApproval" => ApprovalKind::Permissions {
                requested: params
                    .get("permissions")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            },
            _ => {
                self.mark_unknown();
                return self.reply_error(request_id, -32601, "unsupported request");
            }
        };
        let Some(turn_id) = self.koru_turn(params.get("turnId")) else {
            self.mark_unknown();
            return self.reply_error(request_id, -32602, "unknown turn");
        };
        let approval_id = ApprovalId::mint();
        let choices = koru_session_protocol::one_time_approval_choices();
        self.approvals.insert(
            approval_id,
            PendingApproval {
                request_id,
                turn_id,
                kind,
                choices: choices.clone(),
            },
        );
        self.queue(AgentEvent::ApprovalRequired {
            turn_id,
            approval_id,
            summary: approval_summary(method).to_owned(),
            choices,
        })?;
        self.queue(AgentEvent::ProviderState {
            provider: provider_kind(),
            state: koru_session_protocol::ManagedState::WaitingForApproval,
        })
    }

    fn reply_error(
        &mut self,
        request_id: RpcId,
        code: i32,
        message: &'static str,
    ) -> Result<(), ProviderFailure> {
        self.transport()?.send(&json!({
            "id": request_id.value(),
            "error": { "code": code, "message": message },
        }))
    }

    fn handle_notification(&mut self, frame: &Value) -> Result<(), ProviderFailure> {
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            self.mark_unknown();
            return Ok(());
        };
        let params = frame.get("params").unwrap_or(&Value::Null);
        match method {
            // Additive notifications the adapter reads nothing from. Listed
            // so `unknown_events` counts what is actually unknown; each is
            // named in the version's `schema-evidence.json`.
            "turn/started"
            | "thread/started"
            | "thread/status/changed"
            | "mcpServer/startupStatus/updated"
            | "hook/started"
            | "hook/completed"
            | "account/rateLimits/updated"
            | "remoteControl/status/changed" => Ok(()),
            "model/rerouted" => self.handle_rerouted(params),
            "item/agentMessage/delta" => {
                let Some(turn_id) = self.koru_turn(params.get("turnId")) else {
                    self.mark_unknown();
                    return Ok(());
                };
                let text = params
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or_else(malformed_read)?;
                self.queue(AgentEvent::MessageDelta {
                    turn_id,
                    text: text.to_owned(),
                })
            }
            "item/started" => self.handle_item(params, false),
            "item/completed" => self.handle_item(params, true),
            "thread/tokenUsage/updated" => self.handle_usage(params),
            "turn/completed" => self.handle_turn_completed(params),
            "serverRequest/resolved" => self.handle_request_resolved(params),
            _ => {
                self.mark_unknown();
                Ok(())
            }
        }
    }

    fn handle_item(&mut self, params: &Value, completed: bool) -> Result<(), ProviderFailure> {
        let Some(turn_id) = self.koru_turn(params.get("turnId")) else {
            self.mark_unknown();
            return Ok(());
        };
        let item = params.get("item").ok_or_else(malformed_read)?;
        let kind = item
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(malformed_read)?;
        if kind == "imageView" {
            let attached = item
                .get("path")
                .and_then(Value::as_str)
                .map(|path| (turn_id, path.to_owned()))
                .filter(|key| self.attachment_paths.contains_key(key));
            // A picture the person attached, which the agent has now seen.
            if let Some(key) = attached {
                if completed && let Some(attachment_id) = self.attachment_paths.remove(&key) {
                    self.queue(AgentEvent::AttachmentAccepted {
                        turn_id,
                        attachment_id,
                    })?;
                }
                return Ok(());
            }
            return self.handle_image_view(turn_id, item, completed);
        }
        let Some(tool) = normalize::tool_kind(kind) else {
            if !matches!(kind, "agentMessage" | "userMessage" | "reasoning" | "plan") {
                self.mark_unknown();
            }
            return Ok(());
        };
        let label = tool.as_str();
        let native_id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| valid_native_id(value))
            .ok_or_else(malformed_read)?;
        if completed {
            let Some(tool_call_id) = self.tools.remove(&(turn_id, native_id.to_owned())) else {
                self.mark_unknown();
                return Ok(());
            };
            // The classification, and nothing the provider wrote: the
            // item's `error` is read for whether it is there and never for
            // what it says.
            let failure = normalize::item_failure(item);
            self.queue(AgentEvent::ToolCompleted {
                turn_id,
                tool_call_id,
                succeeded: failure.is_none(),
                tool: Some(tool),
                detail: failure.and_then(|_| normalize::item_failure_detail(item)),
                failure,
                output: normalize::item_output(item),
            })
        } else {
            let tool_call_id = ToolCallId::mint();
            self.tools
                .insert((turn_id, native_id.to_owned()), tool_call_id);
            self.queue(AgentEvent::ToolStarted {
                turn_id,
                tool_call_id,
                label: label.to_owned(),
                input: normalize::item_input(item),
            })
        }
    }

    /// **A picture the agent chose to look at** (koru-ai D38): a step,
    /// labelled with the tool's own name and with the file as its input, so
    /// the chat files it as a read and can draw the picture from the disk.
    /// The item carries no status — it is the record of a view that
    /// happened — so it completes as succeeded. A completion whose start
    /// was never seen still gets its step.
    fn handle_image_view(
        &mut self,
        turn_id: AgentTurnId,
        item: &Value,
        completed: bool,
    ) -> Result<(), ProviderFailure> {
        const LABEL: &str = "view_image";
        let native_id = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| valid_native_id(value))
            .ok_or_else(malformed_read)?;
        let key = (turn_id, native_id.to_owned());
        let started = if completed {
            self.tools.remove(&key)
        } else {
            None
        };
        let tool_call_id = match started {
            Some(tool_call_id) => tool_call_id,
            None => {
                let tool_call_id = ToolCallId::mint();
                self.queue(AgentEvent::ToolStarted {
                    turn_id,
                    tool_call_id,
                    label: LABEL.to_owned(),
                    input: normalize::item_input(item),
                })?;
                if !completed {
                    self.tools.insert(key, tool_call_id);
                    return Ok(());
                }
                tool_call_id
            }
        };
        self.queue(AgentEvent::ToolCompleted {
            turn_id,
            tool_call_id,
            succeeded: true,
            tool: Some(koru_session_protocol::ToolKind::Other),
            detail: None,
            failure: None,
            output: None,
        })
    }

    /// `thread/tokenUsage/updated` carries the thread's cumulative total.
    /// The turn's report is that total minus the total when the turn was
    /// submitted — a replacement, so repeating it changes nothing, a later
    /// lower total is a correction, and one arriving after the turn ended
    /// still names the turn it belongs to (§17.2).
    fn handle_usage(&mut self, params: &Value) -> Result<(), ProviderFailure> {
        let Some(turn_id) = self.koru_turn(params.get("turnId")) else {
            self.mark_unknown();
            return Ok(());
        };
        let Some(total) = normalize::usage_totals(params) else {
            return Err(malformed_read());
        };
        self.thread_usage = total;
        let baseline = self
            .usage_baselines
            .get(&turn_id)
            .copied()
            .unwrap_or(normalize::UsageTotals::ZERO);
        // The model the provider says served this turn: a reroute it named
        // for the turn, else the model the thread reported at start, else
        // nothing — never the one that was asked for.
        let model = self
            .rerouted_models
            .get(&turn_id)
            .or(self.thread_model.as_ref())
            .cloned();
        self.queue(AgentEvent::Usage {
            turn_id,
            usage: normalize::turn_usage(total, baseline, model),
        })
    }

    /// `model/rerouted` (0.153.4): the provider served the turn with a model
    /// other than the thread's. Remembered per turn and stamped on that
    /// turn's usage reports; the requested model on the session never moves.
    fn handle_rerouted(&mut self, params: &Value) -> Result<(), ProviderFailure> {
        let Some(turn_id) = self.koru_turn(params.get("turnId")) else {
            self.mark_unknown();
            return Ok(());
        };
        match normalize::model_name(params.get("toModel")) {
            Some(model) => {
                self.rerouted_models.insert(turn_id, model);
                Ok(())
            }
            None => Err(malformed_read()),
        }
    }

    fn handle_request_resolved(&mut self, params: &Value) -> Result<(), ProviderFailure> {
        let thread_matches =
            params.get("threadId").and_then(Value::as_str) == self.thread_id.as_deref();
        if !thread_matches {
            self.mark_unknown();
            return Ok(());
        }
        let request_id = params
            .get("requestId")
            .and_then(RpcId::parse)
            .ok_or_else(malformed_read)?;
        let Some(approval_id) = self.approvals.iter().find_map(|(approval_id, pending)| {
            (pending.request_id == request_id).then_some(*approval_id)
        }) else {
            return Ok(());
        };
        let pending = self
            .approvals
            .remove(&approval_id)
            .expect("approval was found immediately before removal");
        self.queue(AgentEvent::ApprovalExpired {
            turn_id: pending.turn_id,
            approval_id,
        })?;
        if self.approvals.is_empty() {
            self.queue(AgentEvent::ProviderState {
                provider: provider_kind(),
                state: koru_session_protocol::ManagedState::Running,
            })?;
        }
        Ok(())
    }

    fn handle_turn_completed(&mut self, params: &Value) -> Result<(), ProviderFailure> {
        let turn = params.get("turn").ok_or_else(malformed_read)?;
        let native_id = turn
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(malformed_read)?;
        let Some(turn_id) = self.native_turns.get(native_id).copied() else {
            self.mark_unknown();
            return Ok(());
        };
        if let Some(turn) = self.turns.get_mut(&turn_id) {
            turn.terminal = true;
        }
        let expired = self
            .approvals
            .iter()
            .filter_map(|(approval_id, pending)| {
                (pending.turn_id == turn_id).then_some(*approval_id)
            })
            .collect::<Vec<_>>();
        for approval_id in expired {
            self.approvals.remove(&approval_id);
            self.queue(AgentEvent::ApprovalExpired {
                turn_id,
                approval_id,
            })?;
        }
        self.tools
            .retain(|(tool_turn_id, _), _| *tool_turn_id != turn_id);
        self.attachment_paths
            .retain(|(attachment_turn_id, _), _| *attachment_turn_id != turn_id);
        let status = turn
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(malformed_read)?;
        let (event, state) = match status {
            "completed" => (
                AgentEvent::TurnCompleted { turn_id },
                koru_session_protocol::ManagedState::Completed,
            ),
            "interrupted" => (
                // The adapter knows an interruption happened, not why; the
                // session worker that accepted the cancel stamps the reason.
                AgentEvent::TurnCancelled {
                    turn_id,
                    reason: Default::default(),
                },
                koru_session_protocol::ManagedState::Cancelled,
            ),
            _ => (
                AgentEvent::TurnFailed {
                    turn_id,
                    error: SessionFailure {
                        code: ErrorCode::ProviderFailed,
                        message: ErrorCode::ProviderFailed.as_str().to_owned(),
                    },
                },
                koru_session_protocol::ManagedState::Failed,
            ),
        };
        self.queue(event)?;
        self.queue(AgentEvent::ProviderState {
            provider: provider_kind(),
            state,
        })
    }

    fn koru_turn(&self, native: Option<&Value>) -> Option<AgentTurnId> {
        native
            .and_then(Value::as_str)
            .and_then(|value| self.native_turns.get(value))
            .copied()
    }
}
