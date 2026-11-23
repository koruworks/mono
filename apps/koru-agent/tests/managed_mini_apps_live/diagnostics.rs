// SPDX-License-Identifier: MPL-2.0
//! Failure-only diagnostics for this synthetic, allowlisted live turn. Never
//! collect prompts, reasoning, tool arguments or tool bodies.
use super::*;
use std::collections::BTreeMap;
const INPUT_BYTES: usize = 8192;
const OUTPUT_BYTES: usize = 1900;

#[derive(Default)]
pub(super) struct AssistantReply {
    text: String,
    truncated: bool,
}
impl AssistantReply {
    pub(super) fn push(&mut self, text: &str) {
        if self.truncated {
            return;
        }
        let available = INPUT_BYTES - self.text.len();
        let end = text
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(text.len()))
            .take_while(|i| *i <= available)
            .last()
            .unwrap_or(0);
        self.text.push_str(&text[..end]);
        if end < text.len() {
            self.truncated = true;
            // Do not turn a clipped credential-like span into a short word.
            let end = self.text.rfind(char::is_whitespace).unwrap_or(0);
            self.text.truncate(end);
        }
    }
    fn scrubbed(&self) -> String {
        let mut output = String::new();
        for raw in self.text.split_whitespace() {
            let lower = raw.to_ascii_lowercase();
            let credential_label = [
                "password",
                "passwd",
                "secret",
                "authorization",
                "bearer",
                "api_key",
                "apikey",
                "token",
                "credential",
            ]
            .iter()
            .any(|label| lower.contains(label));
            // A separator such as '=' must not consume a one-word secret
            // flag. Conservatively withhold the remaining ambiguous span.
            if credential_label {
                if output.len() + 11 <= OUTPUT_BYTES {
                    if !output.is_empty() {
                        output.push(' ');
                    }
                    output.push_str("[redacted]");
                }
                break;
            }
            let word = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-');
            let known = [
                mini_apps::CREATE_TOOL,
                mini_apps::LIST_TOOL,
                mini_apps::UPDATE_TOOL,
                "invalid_request",
                "mini_app_unavailable",
                "unauthenticated",
                "forbidden",
                "internal_error",
                "service_unavailable",
                "ai_configuration",
                "ai_authentication",
                "ai_rate_limit",
                "ai_context_too_large",
                "ai_model_unavailable",
                "ai_network",
                "ai_provider",
                "ai_cancelled",
                "ai_storage",
            ]
            .contains(&word);
            let safe = if !known
                && (word.len() > 24
                    || word
                        .chars()
                        .any(|c| !c.is_alphabetic() && c != '-' && c != '\'')
                    || raw
                        .chars()
                        .any(|c| matches!(c, '/' | '\\' | ':' | '=' | '@' | '\u{1b}'))
                    || word.contains('.')
                    || lower.starts_with("sk-")
                    || lower.starts_with("pk-"))
            {
                "[redacted]"
            } else {
                word
            };
            if safe.is_empty() {
                continue;
            }
            let extra = safe.len() + usize::from(!output.is_empty());
            if output.len() + extra > OUTPUT_BYTES {
                break;
            }
            if !output.is_empty() {
                output.push(' ');
            }
            output.push_str(safe);
        }
        output
    }
    pub(super) fn report(&self) {
        let text = self.scrubbed();
        if !text.is_empty() {
            eprintln!(
                "managed_mini_app_assistant_failure: scrubbed=true, input_truncated={}, text={text}",
                self.truncated
            );
        }
    }
}

#[derive(Default)]
pub(super) struct ActivitySummary {
    values: BTreeMap<&'static str, ([u64; 7], [u64; 3])>,
    seen: BTreeSet<(koru_ai_core::ToolInvocationId, u8)>,
}
impl ActivitySummary {
    pub(super) fn observe(&mut self, activity: &ToolActivity) {
        let name = activity
            .agent_label
            .as_deref()
            .and_then(CallShape::of)
            .and_then(|shape| {
                if let CallShape::Koru(name) = shape {
                    Some(name)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| activity.tool_name.clone());
        let Some(name) = [
            mini_apps::CREATE_TOOL,
            mini_apps::LIST_TOOL,
            mini_apps::UPDATE_TOOL,
        ]
        .into_iter()
        .find(|known| *known == name) else {
            return;
        };
        let status = match activity.status {
            ToolActivityStatus::Proposed => 0,
            ToolActivityStatus::ApprovalRequired => 1,
            ToolActivityStatus::Running => 2,
            ToolActivityStatus::Complete => 3,
            ToolActivityStatus::Denied => 4,
            ToolActivityStatus::Failed => 5,
            ToolActivityStatus::Cancelled => 6,
        };
        if !self.seen.insert((activity.id, status)) {
            return;
        }
        let (statuses, failures) = self.values.entry(name).or_default();
        statuses[status as usize] += 1;
        if let Some(error) = activity.error_code {
            let index = match error {
                koru_ai_core::ToolErrorCode::PermissionDenied => 0,
                koru_ai_core::ToolErrorCode::ExecutionFailed => 1,
                koru_ai_core::ToolErrorCode::Cancelled => 2,
            };
            failures[index] += 1;
        }
    }
    pub(super) fn report(&self) {
        for (name, (counts, failures)) in &self.values {
            eprintln!(
                "managed_mini_app_tool_summary: tool={name}, proposed={}, approval_required={}, running={}, complete={}, denied={}, failed={}, cancelled={}, permission_failures={}, execution_failures={}, cancellation_failures={}",
                counts[0],
                counts[1],
                counts[2],
                counts[3],
                counts[4],
                counts[5],
                counts[6],
                failures[0],
                failures[1],
                failures[2]
            );
        }
    }
}
#[cfg(test)]
mod tests;
