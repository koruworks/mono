// SPDX-License-Identifier: MPL-2.0
//! Remove live and expired capability spellings before any event leaves the
//! adapter. Runtime/provider replay memory may retain the original prompt.
use koru_ai_core::AiEvent;
use std::collections::{HashSet, VecDeque};
const PREFIX: &str = "koru-mini-";
const TOKEN_BYTES: usize = 46;

/// Codex exposes a closed generic MCP kind as the label and puts the server
/// and tool in this normalized header. Keep only a known owned mini-app name
/// before withholding its raw arguments. This is display metadata, not authority.
fn owned_mini_app_label(label: &str, input: &str) -> Option<String> {
    if label != koru_session_protocol::ToolKind::McpTool.as_str() {
        return None;
    }
    let (server, rest) = input.split_once(": ")?;
    let server = [
        koru_session_profiles::MCP_SERVER_NAME,
        koru_session_profiles::CODEX_DESKTOP_MCP_SERVER_NAME,
    ]
    .into_iter()
    .find(|known| *known == server)?;
    let (tool, _) = rest.split_once(' ')?;
    let tool = [
        koru_ai_core::mini_apps::CREATE_TOOL,
        koru_ai_core::mini_apps::LIST_TOOL,
        koru_ai_core::mini_apps::UPDATE_TOOL,
    ]
    .into_iter()
    .find(|known| *known == tool)?;
    Some(format!("{server}.{tool}"))
}
#[derive(Default)]
struct Text {
    pending: String,
}
impl Text {
    fn push(&mut self, input: &str) -> String {
        self.pending.push_str(input);
        let mut output = String::new();
        loop {
            if let Some(start) = self.pending.find(PREFIX) {
                output.push_str(&self.pending[..start]);
                self.pending.drain(..start);
                if self.pending.len() < TOKEN_BYTES {
                    break;
                }
                if self
                    .pending
                    .get(..TOKEN_BYTES)
                    .is_some_and(|s| koru_ai_core::mini_apps::Capability(s.into()).valid())
                {
                    self.pending.drain(..TOKEN_BYTES);
                    output.push_str("[redacted mini-app permission]");
                } else {
                    output.push('k');
                    self.pending.drain(..1);
                }
            } else {
                let keep = (1..PREFIX.len())
                    .rev()
                    .find(|i| self.pending.ends_with(&PREFIX[..*i]))
                    .unwrap_or(0);
                let end = self.pending.len() - keep;
                output.push_str(&self.pending[..end]);
                self.pending.drain(..end);
                break;
            }
        }
        output
    }
    fn finish(&mut self) -> String {
        std::mem::take(&mut self.pending)
    }
}
pub(crate) struct Redacted<S> {
    inner: S,
    text: Text,
    reasoning: Text,
    ready: VecDeque<AiEvent>,
    native: HashSet<koru_ai_core::ToolInvocationId>,
    last: Option<koru_ai_core::InferenceAttemptId>,
}
impl<S> Redacted<S> {
    pub(crate) fn new(inner: S) -> Self {
        Self {
            inner,
            text: Text::default(),
            reasoning: Text::default(),
            ready: VecDeque::new(),
            native: HashSet::new(),
            last: None,
        }
    }
}
impl<S: Iterator<Item = AiEvent>> Iterator for Redacted<S> {
    type Item = AiEvent;
    fn next(&mut self) -> Option<AiEvent> {
        loop {
            if let Some(event) = self.ready.pop_front() {
                return Some(event);
            }
            let Some(mut event) = self.inner.next() else {
                let attempt_id = self.last.take()?;
                let at = koru_ai_core::Timestamp::now();
                let text = self.text.finish();
                if !text.is_empty() {
                    self.ready.push_back(AiEvent::TextDelta {
                        attempt_id,
                        text,
                        at,
                    });
                }
                let text = self.reasoning.finish();
                if !text.is_empty() {
                    self.ready.push_back(AiEvent::ReasoningDelta {
                        attempt_id,
                        text,
                        at,
                    });
                }
                return self.ready.pop_front();
            };
            self.last = Some(event.attempt_id());
            if let AiEvent::AgentActivity { activity, .. } = &mut event {
                use koru_ai_core::AgentActivityEvent;
                match activity {
                    AgentActivityEvent::ToolStarted {
                        invocation,
                        label,
                        input,
                    } => {
                        if let Some(known) = input
                            .as_deref()
                            .and_then(|input| owned_mini_app_label(label, input))
                        {
                            *label = known;
                        }
                        if [
                            koru_ai_core::mini_apps::CREATE_TOOL,
                            koru_ai_core::mini_apps::LIST_TOOL,
                            koru_ai_core::mini_apps::UPDATE_TOOL,
                        ]
                        .iter()
                        .any(|tool| label.ends_with(tool))
                            || input
                                .as_deref()
                                .is_some_and(koru_ai_core::mini_apps::Capability::in_text)
                        {
                            self.native.insert(*invocation);
                            *input = None;
                        }
                    }
                    AgentActivityEvent::ToolCompleted {
                        invocation,
                        detail,
                        output,
                        ..
                    } if self.native.remove(invocation) => {
                        *detail = None;
                        *output = None;
                    }
                    _ => {}
                }
            }
            match &mut event {
                AiEvent::TextDelta { text, .. } => {
                    *text = self.text.push(text);
                    if text.is_empty() {
                        continue;
                    }
                }
                AiEvent::ReasoningDelta { text, .. } => {
                    *text = self.reasoning.push(text);
                    if text.is_empty() {
                        continue;
                    }
                }
                _ => {
                    event = koru_ai_core::mini_apps::sanitize_agent_event(event);
                    if event.is_terminal() {
                        let attempt_id = event.attempt_id();
                        let at = koru_ai_core::Timestamp::now();
                        let text = self.text.finish();
                        if !text.is_empty() {
                            self.ready.push_back(AiEvent::TextDelta {
                                attempt_id,
                                text,
                                at,
                            });
                        }
                        let text = self.reasoning.finish();
                        if !text.is_empty() {
                            self.ready.push_back(AiEvent::ReasoningDelta {
                                attempt_id,
                                text,
                                at,
                            });
                        }
                        self.ready.push_back(event);
                        continue;
                    }
                }
            }
            return Some(event);
        }
    }
}
#[cfg(test)]
mod tests;
