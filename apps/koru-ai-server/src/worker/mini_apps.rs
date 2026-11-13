// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::mini_apps::{self, Authority, HostContext};

const SUCCESS_LIMIT: usize = 1024;
pub(super) mod mcp;
struct McpPending {
    attempt: InferenceAttemptId,
    context: AiToolExecutionContext,
    call: AiToolCall,
    reply: SyncSender<ApiResult<koru_ai_wire::MiniAppToolResponse>>,
}
fn scoped_definitions(
    definitions: &[AiToolDefinition],
    host: Option<HostContext>,
) -> Vec<AiToolDefinition> {
    definitions.iter().filter_map(|definition| {
        let native=[mini_apps::CREATE_TOOL,mini_apps::LIST_TOOL,mini_apps::UPDATE_TOOL].contains(&definition.name.as_str());
        if native && host.is_none() { return None; }
        if definition.name == mini_apps::UPDATE_TOOL && host.and_then(|h| h.refinement).is_none() { return None; }
        let mut definition=definition.clone();
        if native && let Some(host)=host {
            definition.description.push_str(&format!(" This live host's current workspace_id is {}. Use that identity for existing document discovery tools.",host.workspace_id));
            if definition.name == mini_apps::UPDATE_TOOL && let Some(target) = host.refinement {
                definition.input_schema["properties"]["updateComponents"]["properties"]["surfaceId"] = serde_json::json!({"const":target.surface_name()});
                definition.description.push_str(&format!(" The current live surfaceId is {}.",target.surface_name()));
            }
        }
        Some(definition)
    }).collect()
}
#[derive(Default)]
pub(super) struct Deliveries {
    pub(super) authorities: HashMap<InferenceAttemptId, Authority>,
    successes: HashSet<(Uuid, Uuid)>,
    capabilities: HashMap<mini_apps::Capability, InferenceAttemptId>,
    pending: HashMap<ToolInvocationId, McpPending>,
}
impl Deliveries {
    fn available(&self, host: HostContext) -> bool {
        self.unavailable_reason(host).is_none()
    }
    fn unavailable_reason(&self, host: HostContext) -> Option<&'static str> {
        if host.renderer_id.is_nil() || host.request_id.is_nil() {
            Some("invalid_host")
        } else if self.successes.len() >= SUCCESS_LIMIT {
            Some("host_capacity_reached")
        } else if self
            .successes
            .contains(&(host.renderer_id, host.request_id))
        {
            Some("host_already_completed")
        } else {
            None
        }
    }
    fn record(&mut self, host: HostContext) -> bool {
        self.available(host) && self.successes.insert((host.renderer_id, host.request_id))
    }
    fn retire(&mut self, attempt: InferenceAttemptId) {
        self.capabilities.retain(|_, owner| *owner != attempt);
        self.pending.retain(|_, pending| {
            if pending.attempt != attempt {
                return true;
            }
            let _ = pending.reply.send(Err(failure(
                "mini_app_unavailable",
                "The live mini-app request ended.",
                false,
            )));
            false
        });
        if let Some(authority) = self.authorities.remove(&attempt) {
            authority.revoke();
        }
    }
}
pub(super) fn correlated(call: &AiToolCall, result: &AiToolResult) -> Result<(), AiError> {
    if call.call_id == result.call_id && call.name == result.name {
        Ok(())
    } else {
        Err(tool_protocol_failure(
            "The tool result did not match its invocation.",
        ))
    }
}
/// Exact bounded stream publication used by the worker, kept independent of
/// repository state so backpressure and lifetime fencing are testable in memory.
pub(super) fn publish_live(
    streams: &mut HashMap<InferenceAttemptId, StreamState>,
    deliveries: &mut Deliveries,
    event: PublicAiEvent,
) {
    let attempt = event.attempt_id();
    let mut remove = event.is_terminal();
    if let Some(stream) = streams.get_mut(&attempt) {
        let public = StreamEvent {
            version: AI_STREAM_VERSION,
            sequence: stream.next_sequence,
            event,
        };
        stream.next_sequence = stream.next_sequence.saturating_add(1);
        if stream.sender.try_send(public).is_err() {
            remove = true;
            tracing::debug!(attempt=%attempt,"AI response stream dropped; native delivery retired");
        }
    } else {
        remove = true;
    }
    if remove {
        streams.remove(&attempt);
        deliveries.retire(attempt);
    }
}

#[cfg(test)]
mod tests;

impl WorkerState {
    pub(super) fn mini_app_tools(
        &mut self,
        attempt: InferenceAttemptId,
        provider: &AiProviderId,
        host: Option<HostContext>,
    ) -> Vec<AiToolDefinition> {
        let managed = managed_tools_available(provider, self.managed_mini_apps);
        let supported = managed
            || matches!(
                provider.as_str(),
                AiProviderId::OPENAI | AiProviderId::OLLAMA | AiProviderId::KORU | "mock"
            );
        let requested = host.is_some();
        let managed_provider_registered = koru_ai_provider_agent::is_agent_provider(provider);
        let reason = match host {
            None => "no_host_supplied",
            Some(_) if !supported && managed_provider_registered => "managed_bridge_unavailable",
            Some(_) if !supported => "provider_unregistered",
            Some(host) => self
                .mini_apps
                .unavailable_reason(host)
                .unwrap_or("admitted"),
        };
        let host = host.filter(|_| reason == "admitted");
        if requested && host.is_none() {
            tracing::warn!(
                event = "ai.mini_apps.withheld",
                attempt = %attempt,
                provider = %provider,
                reason,
                "requested mini-app tools are unavailable"
            );
            self.publish_public(PublicAiEvent::Notice {
                attempt_id: attempt,
                notice: koru_ai_core::AttemptNotice::NativeToolsUnavailable,
                at: Timestamp::now(),
            });
        }
        if let Some(host) = host {
            self.mini_apps
                .authorities
                .insert(attempt, Authority::new(host));
            if managed {
                self.mini_apps
                    .capabilities
                    .insert(mini_apps::Capability::mint(), attempt);
            }
        }
        let definitions = scoped_definitions(&self.tool_definitions, host);
        let native_count = |definitions: &[AiToolDefinition]| {
            definitions
                .iter()
                .filter(|definition| {
                    [
                        mini_apps::CREATE_TOOL,
                        mini_apps::LIST_TOOL,
                        mini_apps::UPDATE_TOOL,
                    ]
                    .contains(&definition.name.as_str())
                })
                .count()
        };
        tracing::debug!(
            event = "ai.mini_apps.tools",
            attempt = %attempt,
            provider = %provider,
            managed_provider_registered,
            managed_tools_available = managed,
            host_requested = requested,
            reason,
            native_tools_configured = native_count(&self.tool_definitions),
            native_tools_admitted = native_count(&definitions),
            tools_admitted = definitions.len(),
            refinement_available = host.and_then(|host| host.refinement).is_some(),
            "mini-app tools scoped for attempt"
        );
        definitions
    }

    pub(super) fn retire_mini_app(&mut self, attempt: InferenceAttemptId) {
        self.mini_apps.retire(attempt);
    }

    pub(super) fn publish_mini_app(
        &mut self,
        attempt_id: InferenceAttemptId,
        invocation_id: ToolInvocationId,
        result: &AiToolResult,
    ) -> Result<(), AiError> {
        if ![mini_apps::CREATE_TOOL, mini_apps::UPDATE_TOOL].contains(&result.name.as_str())
            || result.is_error
        {
            return Ok(());
        }
        let Some(authority) = self
            .mini_apps
            .authorities
            .get(&attempt_id)
            .filter(|a| a.live())
        else {
            return Err(tool_protocol_failure(
                "The native mini-app host is no longer available.",
            ));
        };
        let host = authority.host;
        let chat_id = self
            .repository
            .attempt(attempt_id)?
            .and_then(|a| a.chat_id)
            .ok_or_else(tool_loop_missing)?;
        let profile_id = self
            .repository
            .chat(chat_id)?
            .and_then(|chat| chat.profile_id)
            .ok_or_else(tool_loop_missing)?;
        let context = AiToolExecutionContext {
            chat_id,
            attempt_id,
            invocation_id,
            profile_id: Some(profile_id),
            workspace_ids: vec![host.workspace_id],
            mini_app: Some(authority.clone()),
        };
        // The event contains only the registered bounded result, never an
        // arbitrary tool payload. Stream failure is not a tab acknowledgement.
        publish_validated(&mut self.streams, &mut self.mini_apps, &context, result)
    }
}

fn managed_tools_available(provider: &AiProviderId, bridge_present: bool) -> bool {
    bridge_present && koru_ai_provider_agent::is_agent_provider(provider)
}

fn publish_validated(
    streams: &mut HashMap<InferenceAttemptId, StreamState>,
    deliveries: &mut Deliveries,
    context: &AiToolExecutionContext,
    result: &AiToolResult,
) -> Result<(), AiError> {
    if result.is_error
        || ![mini_apps::CREATE_TOOL, mini_apps::UPDATE_TOOL].contains(&result.name.as_str())
    {
        return Ok(());
    }
    let authority = context
        .mini_app
        .as_ref()
        .filter(|a| a.live())
        .ok_or_else(tool_loop_missing)?;
    let event = crate::mini_apps::creation_event(context, result)?;
    if !deliveries.record(authority.host) {
        return Err(tool_protocol_failure(
            "This logical mini-app request already completed or capacity was reached.",
        ));
    }
    publish_live(streams, deliveries, event);
    if !streams.contains_key(&context.attempt_id) {
        return Err(tool_protocol_failure(
            "The live mini-app offer could not be delivered. Local application is not acknowledged.",
        ));
    }
    Ok(())
}
