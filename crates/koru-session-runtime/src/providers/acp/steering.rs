// SPDX-License-Identifier: MPL-2.0

//! **Text sent into a running turn** (a steer), over the ACP steering
//! extension the Claude wrapper speaks.
//!
//! `claude-agent-acp` 0.76.0 advertises the extension at initialize, at the
//! top level of its answer: `_meta.steering.supported: true`. A steer is the
//! request `_session/steering` — `{sessionId, prompt, _meta}` — whose message
//! the wrapper injects into the turn in flight instead of queuing a second
//! prompt behind it; the steered message's own output streams through
//! `session/update` under the running prompt, and that prompt settles once,
//! when the agent goes idle. With
//! `_meta.steering.idleBehavior: "promptRequired"` an idle session does not
//! start a detached turn of its own: it answers
//! `{"outcome": "promptRequired"}` and leaves the message with the client.
//! Koru always asks for that, so a steer can never start a turn nobody
//! follows; `promptRequired` is answered as the default refusal
//! (`InvalidTransition`), and the person's message is sent as a normal
//! message once the reply has ended. `injected` is the only success.
//!
//! An agent that did not advertise the extension keeps the trait's default
//! refusal. Text only: steering carries no attachment.

use super::*;

const STEER_METHOD: &str = "_session/steering";

/// Whether an initialize answer advertises the steering extension.
pub(super) fn advertised(initialized: &Value) -> bool {
    initialized
        .pointer("/_meta/steering/supported")
        .and_then(Value::as_bool)
        == Some(true)
}

impl AcpControl {
    /// `AgentControl::steer` for an ACP agent: `_session/steering` into the
    /// running turn, where the agent advertised it.
    pub(super) fn steer_turn(&mut self, input: AgentInput) -> Result<(), ProviderFailure> {
        let refused = || {
            failure(
                ErrorCode::InvalidTransition,
                ProviderStage::Submit,
                ProviderDiagnostic::InvalidState,
            )
        };
        self.ensure_usable(ProviderStage::Submit)?;
        input.validate(self.config.maximum_input_bytes)?;
        if !input.attachments.is_empty() {
            return Err(failure(
                ErrorCode::DeliveryUnsupported,
                ProviderStage::Submit,
                ProviderDiagnostic::InvalidInput,
            ));
        }
        if !self.steering {
            return Err(refused());
        }
        // Only the turn running now, and not one being cancelled.
        let running = self.active.filter(|active| {
            *active == input.turn_id
                && self
                    .turns
                    .get(active)
                    .is_some_and(|turn| !turn.terminal && turn.cancelling.is_none())
        });
        if running.is_none() {
            return Err(refused());
        }
        let native_session = self.native_session.clone().ok_or_else(refused)?;
        let params = json!({
            "sessionId": native_session,
            "prompt": [{"type": "text", "text": input.text}],
            "_meta": {"steering": {"idleBehavior": "promptRequired"}},
        });
        let answer = self.call(STEER_METHOD, params)?;
        match answer.get("outcome").and_then(Value::as_str) {
            Some("injected") => {
                tracing::info!(target: "koru::acp", event = "acp.steer.injected");
                Ok(())
            }
            Some("promptRequired") => {
                tracing::info!(target: "koru::acp", event = "acp.steer.prompt_required");
                Err(refused())
            }
            _ => Err(malformed()),
        }
    }
}
