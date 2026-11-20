// SPDX-License-Identifier: MPL-2.0

//! Ordinary request composition on the context worker, never on the AI actor.
//! Managed sessions capture their own first input on the provider worker.

use koru_ai_core::AiRequest;
use koru_delegate_preamble::{Resolved, session};

pub(super) type Source = session::Configuration;

pub(super) fn apply(
    request: &mut AiRequest,
    resolve: impl FnOnce() -> Resolved,
) -> Option<Resolved> {
    // The explicit debug override replaces the default instructions, as before.
    if request.prompt_template_version == "debug-override-v1"
        || koru_ai_provider_agent::is_agent_provider(&request.provider)
    {
        return None;
    }
    let snapshot = resolve();
    let facts = if request.tools.is_empty() {
        "Session mode: interactive API chat. Your reply is shown to the person. No application tools are offered in this request; do not claim to have performed an application operation."
    } else {
        "Session mode: interactive API chat. Your reply is shown to the person. Use only the tools offered with this request and their exact schemas; available tools do not imply access beyond their enforced grants."
    };
    request.system = session::compose(&snapshot.text, &request.system, facts);
    Some(snapshot)
}

#[cfg(test)]
mod tests;
