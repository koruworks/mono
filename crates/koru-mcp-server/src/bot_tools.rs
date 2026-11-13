// SPDX-License-Identifier: MPL-2.0

//! External task tools are local bridge operations, never model tools inside
//! a Koru chat or a Bot Run. The gateway owns admission and execution.
use super::*;

pub const BOT_TOOL_NAMES: &[&str] = &[
    "list_bots",
    "start_bot_task",
    "read_bot_task",
    "stop_bot_task",
];
const BOT_SCOPES: &[&str] = &["bots.read", "bots.run", "store.read_documents"];

pub(super) fn allowed(context: &RequestContext) -> bool {
    matches!(context.workspace_access, WorkspaceAccess::All)
        && context.chat_tools == ChatToolAccess::Bridge
}

impl LocalBackend {
    fn bot_connection(&self) -> Result<Client, ToolError> {
        if self.embedded.is_some() {
            return Err(ToolError::permission_denied(
                "Embedded hosts do not provide external Bot task tools",
            ));
        }
        let mut held = self.bot_client.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(client) = held.as_ref() {
            return Ok(client.clone());
        }
        let client = if let Some(dialer) = &self.dialer {
            dialer()?
        } else {
            match self.root.as_deref() {
                Some(root) => Client::open_at_with_scopes(&self.segment, root, BOT_SCOPES)?,
                None => Client::ensure_with_scopes(&self.segment, BOT_SCOPES)?,
            }
        };
        *held = Some(client.clone());
        Ok(client)
    }

    fn bot_call<T: Serialize>(
        &self,
        call: impl FnOnce(&Client) -> koru_api_client::AiResult<T>,
    ) -> Result<Value, ToolError> {
        let client = self.bot_connection()?;
        match call(&client) {
            Ok(value) => serde_json::to_value(value).map_err(|e| ToolError::invalid(e.to_string())),
            Err(error) => {
                if spent_ai(&error) {
                    self.bot_client
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .take();
                }
                // Never mint a fresh request ID on reconnect or uncertainty.
                Err(ai_tool_error(error))
            }
        }
    }

    pub(super) fn bot_tool(
        &self,
        context: &RequestContext,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, ToolError> {
        if !allowed(context) {
            return Err(ToolError::permission_denied(
                "Bot task tools require the trusted local bridge",
            ));
        }
        match tool {
            "list_bots" => {
                let request: ListBotsRequest = from(arguments)?;
                self.bot_call(|client| client.ai().list_bots(&request))
            }
            "start_bot_task" => {
                let request: StartBotTaskRequest = from(arguments)?;
                self.bot_call(|client| client.ai().start_bot_task(&request))
            }
            "read_bot_task" => {
                let request: ReadBotTaskRequest = from(arguments)?;
                self.bot_call(|client| client.ai().read_bot_task(&request))
            }
            "stop_bot_task" => {
                let request: StopBotTaskRequest = from(arguments)?;
                self.bot_call(|client| client.ai().stop_bot_task(&request))
            }
            _ => Err(ToolError::invalid("unknown Bot task tool")),
        }
    }
}

#[cfg(test)]
mod tests;
