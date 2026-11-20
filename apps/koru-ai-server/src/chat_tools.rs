// SPDX-License-Identifier: MPL-2.0

//! Direct read-only access to `ai.db` for provider-requested chat tools.

use std::path::PathBuf;
use std::sync::Mutex;

use koru_ai_core::{
    AiError, AiErrorClass, AiToolCall, AiToolDefinition, AiToolExecutionContext, AiToolExecutor,
    AiToolResult, AiToolSafety, Cancellation, ChatAccessLens, ListChatsToolRequest,
    ReadChatToolRequest, SearchChatsToolRequest, list_chats, read_chat, search_chats,
};
use koru_ai_sqlite::Repository;
use koru_mcp_server::{CHAT_TOOL_NAMES, workspace_tool_catalog};

use crate::StoreToolAdapter;

/// A read-only adapter with its own lazily-opened WAL connection.
///
/// Tool jobs execute while the main AI worker waits for their batch. Calling
/// back through `AiWorkerClient` would deadlock, so this type exposes no worker
/// client and no mutable repository operation.
pub struct ChatToolAdapter {
    root: PathBuf,
    repository: Mutex<Option<Repository>>,
}

impl ChatToolAdapter {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            repository: Mutex::new(None),
        }
    }

    fn invoke_read(
        &self,
        context: &AiToolExecutionContext,
        call: &AiToolCall,
    ) -> Result<serde_json::Value, AiError> {
        let mut repository = self.repository.lock().map_err(|_| {
            AiError::new(
                AiErrorClass::Storage,
                "The local chat database is unavailable.",
                true,
            )
        })?;
        if repository.is_none() {
            *repository = Some(
                Repository::open(&self.root.join(koru_ai_sqlite::DATABASE_FILE)).map_err(|_| {
                    AiError::new(
                        AiErrorClass::Storage,
                        "The local chat database is unavailable.",
                        true,
                    )
                })?,
            );
        }
        let repository = repository.as_ref().expect("repository opened above");
        let lens = ChatAccessLens::ToolSurface {
            calling_chat: Some(context.chat_id),
        };
        // A calling chat sees only its own profile's chats, and a chat of a
        // removed profile sends no turn, so there is no removed profile to
        // leave out here; the worker hides them from every other surface.
        match call.name.as_str() {
            "list_chats" => encode(list_chats(
                repository,
                lens,
                decode::<ListChatsToolRequest>(call.arguments.clone())?,
                &[],
            )?),
            "search_chats" => encode(search_chats(
                repository,
                lens,
                decode::<SearchChatsToolRequest>(call.arguments.clone())?,
                &[],
            )?),
            "read_chat" => encode(read_chat(
                repository,
                lens,
                decode::<ReadChatToolRequest>(call.arguments.clone())?,
            )?),
            _ => Err(AiError::new(
                AiErrorClass::Provider,
                "The provider requested an unknown chat tool.",
                false,
            )),
        }
    }
}

impl AiToolExecutor for ChatToolAdapter {
    fn definitions(&self) -> Vec<AiToolDefinition> {
        catalog_definitions()
            .into_iter()
            .filter(|tool| CHAT_TOOL_NAMES.contains(&tool.name.as_str()))
            .collect()
    }

    fn invoke(
        &self,
        context: &AiToolExecutionContext,
        call: &AiToolCall,
        cancellation: &Cancellation,
    ) -> Result<AiToolResult, AiError> {
        if cancellation.is_cancelled() {
            return Err(AiError::new(
                AiErrorClass::Cancelled,
                "The AI request was cancelled.",
                false,
            ));
        }
        tracing::info!(
            chat = %context.chat_id,
            attempt = %context.attempt_id,
            invocation = %context.invocation_id,
            tool = %call.name,
            "AI chat tool started"
        );
        let content = self.invoke_read(context, call)?;
        tracing::info!(
            chat = %context.chat_id,
            attempt = %context.attempt_id,
            invocation = %context.invocation_id,
            tool = %call.name,
            "AI chat tool completed"
        );
        Ok(AiToolResult {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            content,
            is_error: false,
        })
    }
}

/// Routes one provider-neutral catalog to the Store and chat worlds.
pub struct AiToolDispatcher {
    store: StoreToolAdapter,
    chat: ChatToolAdapter,
    definitions: Vec<AiToolDefinition>,
}

impl AiToolDispatcher {
    pub fn new(store: StoreToolAdapter, chat: ChatToolAdapter) -> Self {
        Self {
            store,
            chat,
            definitions: catalog_definitions(),
        }
    }
}

impl AiToolExecutor for AiToolDispatcher {
    fn definitions(&self) -> Vec<AiToolDefinition> {
        self.definitions.clone()
    }

    fn invoke(
        &self,
        context: &AiToolExecutionContext,
        call: &AiToolCall,
        cancellation: &Cancellation,
    ) -> Result<AiToolResult, AiError> {
        if CHAT_TOOL_NAMES.contains(&call.name.as_str()) {
            self.chat.invoke(context, call, cancellation)
        } else {
            self.store.invoke(context, call, cancellation)
        }
    }
}

fn catalog_definitions() -> Vec<AiToolDefinition> {
    workspace_tool_catalog()
        .into_iter()
        .map(|tool| AiToolDefinition {
            name: tool.name,
            title: tool.title,
            description: tool.description,
            input_schema: tool.input_schema,
            output_schema: tool.output_schema,
            safety: AiToolSafety {
                read_only: tool.safety.read_only,
                destructive: tool.safety.destructive,
                idempotent: tool.safety.idempotent,
                open_world: tool.safety.open_world,
            },
        })
        .collect()
}

fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, AiError> {
    serde_json::from_value(value).map_err(|_| {
        AiError::new(
            AiErrorClass::Provider,
            "The provider supplied invalid chat-tool arguments.",
            false,
        )
    })
}

fn encode<T: serde::Serialize>(value: T) -> Result<serde_json::Value, AiError> {
    serde_json::to_value(value).map_err(|_| {
        AiError::new(
            AiErrorClass::Storage,
            "The chat-tool result could not be encoded.",
            false,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatcher_catalog_is_the_mcp_catalog_byte_for_byte() {
        let store_root = tempfile::tempdir().unwrap();
        let chat_root = tempfile::tempdir().unwrap();
        let worker =
            koru_store_server::StoreWorker::new(koru_store_server::WorkerConfig::embedded(
                store_root.path().to_owned(),
                "internal-debug",
            ))
            .spawn()
            .unwrap();
        let dispatcher = AiToolDispatcher::new(
            StoreToolAdapter::new(worker.client()),
            ChatToolAdapter::new(chat_root.path().to_owned()),
        );

        assert_eq!(dispatcher.definitions(), catalog_definitions());
        let chat_definitions = dispatcher
            .definitions()
            .into_iter()
            .filter(|tool| CHAT_TOOL_NAMES.contains(&tool.name.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(chat_definitions.len(), CHAT_TOOL_NAMES.len());
        assert!(chat_definitions.iter().all(|tool| {
            tool.safety.read_only
                && !tool.safety.destructive
                && tool.safety.idempotent
                && !tool.safety.open_world
        }));

        drop(dispatcher);
        assert_eq!(worker.join(), koru_store_server::WorkerExit::Clean);
    }
}
