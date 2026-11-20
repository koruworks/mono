// SPDX-License-Identifier: MPL-2.0

//! An ensured session may still have received no input: attachment validation,
//! model selection or the first send can fail after creation. Only a complete
//! unused event stream proves that its first prompt is still owed. Any active
//! turn, turn fact, non-idle provider state or truncated history is ambiguous
//! or used, and must never cause the instructions to be appended again.

use koru_ai_core::AiRequest;
use koru_delegate_preamble::{Resolved, session};
use koru_session_protocol::{AgentEvent, AgentEventObservation, AgentTurnId, ManagedState};

pub(super) fn needs_initial_prompt(
    active: Option<AgentTurnId>,
    observed: &AgentEventObservation,
) -> bool {
    active.is_none()
        && !observed.earlier_truncated
        && observed.events.iter().all(|row| {
            matches!(
                row.event,
                AgentEvent::CommandsUpdated { .. }
                    | AgentEvent::ContextUsage { .. }
                    | AgentEvent::ProviderState {
                        state: ManagedState::Idle,
                        ..
                    }
            )
        })
}

pub(super) fn prepare(
    request: &AiRequest,
    initial: bool,
    resolve: impl FnOnce() -> Resolved,
) -> (String, Option<Resolved>) {
    if !initial || request.prompt_template_version == "debug-override-v1" {
        return (super::compose_prompt(request, initial), None);
    }
    let snapshot = resolve();
    let mut request = request.clone();
    request.system = session::compose(
        &snapshot.text,
        &request.system,
        "Session mode: interactive managed agent chat. Your conversation reply is shown to the person. Look for Koru operations in the tools actually offered through this agent's MCP connections, using their listed server names and schemas. A missing connection means those application operations are unavailable. The working folder alone grants no Koru workspace access.",
    );
    (super::compose_prompt(&request, true), Some(snapshot))
}

#[cfg(test)]
mod tests;
