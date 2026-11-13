// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::mini_apps::{self, Authority, HostContext};

const SUCCESS_LIMIT: usize = 1024;
fn scoped_definitions(
    definitions: &[AiToolDefinition],
    host: Option<HostContext>,
) -> Vec<AiToolDefinition> {
    definitions.iter().filter_map(|definition| {
        let native=[mini_apps::CREATE_TOOL,mini_apps::LIST_TOOL].contains(&definition.name.as_str());
        if native && host.is_none() { return None; }
        let mut definition=definition.clone();
        if native && let Some(host)=host {
            definition.description.push_str(&format!(" This live host's current workspace_id is {}. Use that identity for existing document discovery tools.",host.workspace_id));
        }
        Some(definition)
    }).collect()
}
#[derive(Default)]
pub(super) struct Deliveries {
    pub(super) authorities: HashMap<InferenceAttemptId, Authority>,
    successes: HashSet<(Uuid, Uuid)>,
}
impl Deliveries {
    fn available(&self, host: HostContext) -> bool {
        !host.renderer_id.is_nil()
            && !host.request_id.is_nil()
            && self.successes.len() < SUCCESS_LIMIT
            && !self
                .successes
                .contains(&(host.renderer_id, host.request_id))
    }
    fn record(&mut self, host: HostContext) -> bool {
        self.available(host) && self.successes.insert((host.renderer_id, host.request_id))
    }
    fn retire(&mut self, attempt: InferenceAttemptId) {
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
        let supported = matches!(
            provider.as_str(),
            AiProviderId::OPENAI | AiProviderId::OLLAMA | AiProviderId::KORU | "mock"
        );
        let requested = host.is_some();
        let host = host.filter(|host| supported && self.mini_apps.available(*host));
        if requested && host.is_none() {
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
        }
        scoped_definitions(&self.tool_definitions, host)
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
        if result.name != mini_apps::CREATE_TOOL || result.is_error {
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
        let event = crate::mini_apps::creation_event(
            &AiToolExecutionContext {
                chat_id,
                attempt_id,
                invocation_id,
                profile_id: Some(profile_id),
                workspace_ids: vec![host.workspace_id],
                mini_app: Some(authority.clone()),
            },
            result,
        )?;
        if !self.mini_apps.record(host) {
            return Err(tool_protocol_failure(
                "This logical request already produced a mini app, or live request capacity was reached.",
            ));
        }
        // The event contains only the registered bounded result, never an
        // arbitrary tool payload. Stream failure is not a tab acknowledgement.
        self.publish_public(event);
        Ok(())
    }
}
