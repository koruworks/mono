// SPDX-License-Identifier: MPL-2.0

//! ACP command listings arrive as notifications, sometimes before the
//! `session/new` answer. A listing belongs to one native session only.

use super::*;
use koru_session_protocol::{AgentCommand, AgentCommandKind, MAX_AGENT_COMMANDS};

fn listing(update: &Value) -> Result<Vec<AgentCommand>, ProviderFailure> {
    let rows = update["availableCommands"]
        .as_array()
        .ok_or_else(malformed)?;
    let mut commands = Vec::with_capacity(rows.len().min(MAX_AGENT_COMMANDS));
    for row in rows.iter().take(MAX_AGENT_COMMANDS) {
        let Some(name) = row["name"].as_str() else {
            continue;
        };
        if !koru_session_protocol::valid_command_name(name) {
            continue;
        }
        commands.push(AgentCommand {
            name: name.to_owned(),
            description: koru_session_protocol::command_text(
                row["description"].as_str().unwrap_or_default(),
            ),
            display_name: None,
            kind: AgentCommandKind::Command,
            input_hint: row["input"]["hint"]
                .as_str()
                .map(koru_session_protocol::command_text)
                .filter(|hint| !hint.is_empty()),
        });
    }
    Ok(koru_session_protocol::ordered_commands(commands))
}

impl AcpControl {
    pub(super) fn command_update(&mut self, params: &Value) -> Result<(), ProviderFailure> {
        let native = params["sessionId"]
            .as_str()
            .filter(|id| valid_native_id(id))
            .ok_or_else(malformed)?;
        let commands = listing(&params["update"])?;
        if self.native_session.is_none() {
            self.early_commands = Some((native.to_owned(), commands));
            return Ok(());
        }
        self.live_commands = Some(commands.clone());
        self.queue(AgentEvent::CommandsUpdated { commands })
    }
}
