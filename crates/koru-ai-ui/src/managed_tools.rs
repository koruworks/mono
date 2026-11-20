// SPDX-License-Identifier: MPL-2.0
//! Bounded, display-only native tool observations. No AI repository, provider
//! command or approval uses these rows. The actual engine turn UUID occupies
//! ToolActivity's ordinary chat attempt slot solely to reuse its work renderer; it
//! never represents a durable inference attempt. Reopening starts empty.
use koru_ai_core::{
    InferenceAttemptId, MessageId, Timestamp, ToolActivity, ToolActivityStatus, ToolApproval,
    ToolInvocationId,
};
use koru_ai_wire::bot_threads as wire;
use std::collections::{HashSet, VecDeque};
use uuid::Uuid;

const MAX_CALLS: usize = 128;
const MAX_CONTENT: usize = 16 * 1024;
const MAX_BYTES: usize = 512 * 1024;

struct Call {
    turn: wire::Turn,
    bot: Uuid,
    row: ToolActivity,
}
#[derive(Default)]
pub(crate) struct Observed {
    boot: Option<Uuid>,
    cursor: u64,
    calls: VecDeque<Call>,
}

fn same(a: wire::Turn, b: wire::Turn) -> bool {
    a.id == b.id && a.generation == b.generation && a.worker == b.worker && a.session == b.session
}
fn matches(turn: &wire::ActiveTurn, event: &wire::Observation) -> bool {
    turn.id == event.turn.id
        && turn.bot == event.bot
        && turn.generation == event.turn.generation
        && turn.worker == Some(event.turn.worker)
        && turn.session == Some(event.turn.session)
}
fn bounded(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].into()
}
impl Observed {
    pub(crate) fn rows(&self) -> Vec<ToolActivity> {
        self.calls.iter().map(|call| call.row.clone()).collect()
    }

    pub(crate) fn apply(
        &mut self,
        feed: &crate::managed::Feed,
        old: &wire::Snapshot,
        current: &wire::Snapshot,
        published: &HashSet<Uuid>,
    ) -> bool {
        let mut changed = false;
        if feed.gap || self.boot != Some(feed.boot) {
            changed = !self.calls.is_empty();
            self.calls.clear();
            self.cursor = 0;
        }
        self.boot = Some(feed.boot);
        for entry in &feed.entries {
            if entry.cursor <= self.cursor {
                continue;
            }
            self.cursor = entry.cursor;
            let event = &entry.observation;
            if event.thread != current.conversation.chat.id.as_uuid() {
                continue;
            }
            let (id, completed) = match &event.event {
                wire::AgentEvent::ToolStarted { tool_call_id, .. } => {
                    (tool_call_id.as_uuid(), false)
                }
                wire::AgentEvent::ToolCompleted { tool_call_id, .. } => {
                    (tool_call_id.as_uuid(), true)
                }
                _ => continue,
            };
            let known = self.calls.iter().position(|call| {
                call.row.id.as_uuid() == id && same(call.turn, event.turn) && call.bot == event.bot
            });
            // A new incarnation wins over an old observation. Completion can
            // arrive in the same refresh that removes its turn and publishes
            // the exact reply; only an already witnessed call gets that seam.
            let turn = current.turns.iter().find(|turn| turn.id == event.turn.id);
            let valid = turn.filter(|turn| matches(turn, event)).or_else(|| {
                if turn.is_some() || !completed || known.is_none() {
                    return None;
                }
                old.turns.iter().find(|turn| {
                    matches(turn, event)
                        && turn
                            .reply_message
                            .is_some_and(|reply| published.contains(&reply))
                })
            });
            let Some(turn) = valid else {
                continue;
            };
            let Some(reply) = turn.reply_message else {
                continue;
            };
            match &event.event {
                wire::AgentEvent::ToolStarted { label, input, .. } => {
                    if known.is_some() || published.contains(&reply) {
                        continue;
                    }
                    self.calls.push_back(Call {
                        turn: event.turn,
                        bot: event.bot,
                        row: ToolActivity {
                            id: ToolInvocationId::from_uuid(id),
                            attempt_id: InferenceAttemptId::from_uuid(turn.id),
                            chat_id: current.conversation.chat.id,
                            assistant_message_id: MessageId::from_uuid(reply),
                            tool_name: "agent.other".into(),
                            read_only: false,
                            status: ToolActivityStatus::Running,
                            approval: ToolApproval::NotRequired,
                            proposed_at: Timestamp::from_millis(turn.created_at_ms),
                            started_at: None,
                            completed_at: None,
                            result_bytes: None,
                            error_code: None,
                            target_workspace_id: None,
                            target_document_id: None,
                            error_message: None,
                            agent_label: Some(bounded(label, 256)),
                            agent_summary: None,
                            agent_input: input.as_deref().map(|text| bounded(text, MAX_CONTENT)),
                            agent_output: None,
                            text_offset: None,
                            display_target: None,
                            live: None,
                        },
                    });
                    changed = true;
                }
                wire::AgentEvent::ToolCompleted {
                    succeeded,
                    tool,
                    detail,
                    output,
                    ..
                } => {
                    let Some(index) = known else {
                        continue;
                    };
                    let row = &mut self.calls[index].row;
                    if row.status.is_terminal() {
                        continue;
                    }
                    row.status = if *succeeded {
                        ToolActivityStatus::Complete
                    } else {
                        ToolActivityStatus::Failed
                    };
                    row.tool_name = match tool.map(|kind| kind.as_str()) {
                        Some("command_execution") => "agent.command",
                        Some("file_change") => "agent.file_change",
                        Some("web_search") => "agent.search",
                        Some("mcp_tool") => "agent.mcp",
                        _ => "agent.other",
                    }
                    .into();
                    row.error_message = detail.as_deref().map(|text| bounded(text, 1024));
                    row.agent_output = output.as_deref().map(|text| bounded(text, MAX_CONTENT));
                    changed = true;
                }
                _ => {}
            }
            while self.calls.len() > MAX_CALLS || self.bytes() > MAX_BYTES {
                self.calls.pop_front();
            }
        }
        self.cursor = self.cursor.max(feed.cursor);
        // Running calls with no matching current incarnation cannot advertise
        // current activity. Keep only completed observations in that case.
        let before = self.calls.len();
        self.calls.retain(|call| {
            call.row.status.is_terminal()
                || current.turns.iter().any(|turn| {
                    turn.id == call.turn.id
                        && turn.bot == call.bot
                        && turn.generation == call.turn.generation
                        && turn.worker == Some(call.turn.worker)
                        && turn.session == Some(call.turn.session)
                        && !published.contains(&call.row.assistant_message_id.as_uuid())
                })
        });
        changed || before != self.calls.len()
    }

    fn bytes(&self) -> usize {
        self.calls
            .iter()
            .map(|call| {
                [
                    &call.row.agent_label,
                    &call.row.agent_summary,
                    &call.row.error_message,
                    &call.row.agent_input,
                    &call.row.agent_output,
                ]
                .into_iter()
                .map(|part| part.as_ref().map_or(0, String::len))
                .sum::<usize>()
            })
            .sum()
    }
}

#[cfg(test)]
mod tests;
