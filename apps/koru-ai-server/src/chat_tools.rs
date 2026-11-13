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
    store: Box<dyn AiToolExecutor>,
    chat: Box<dyn AiToolExecutor>,
    definitions: Vec<AiToolDefinition>,
    native: crate::mini_apps::MiniAppTools,
}

impl AiToolDispatcher {
    pub fn new(store: StoreToolAdapter, chat: ChatToolAdapter) -> Self {
        Self {
            native: crate::mini_apps::MiniAppTools::new(std::sync::Arc::new(store.clone())),
            store: Box::new(store),
            chat: Box::new(chat),
            definitions: catalog_definitions(),
        }
    }
}

/// A no-I/O fixture through the real dispatcher, registry validation, native
/// provider tool loop and typed event projection, for a native host's tests.
#[cfg(feature = "test-support")]
pub fn scripted_native_creation(
    context: AiToolExecutionContext,
    declaration: serde_json::Value,
    lists: Vec<(uuid::Uuid, String)>,
    documents: Vec<koru_ai_core::DocumentId>,
) -> Result<Vec<koru_ai_wire::StreamEvent>, AiError> {
    use koru_ai_core::{
        AiEvent, AiProvider, AiRequest, AiRequestOptions, AiToolLoop, AiToolLoopLimits,
        ReasoningChoice, Timestamp,
    };
    struct FakeRegistry {
        profile: Option<koru_ai_core::ProfileId>,
        workspace: koru_ai_core::WorkspaceId,
        lists: Vec<(uuid::Uuid, String)>,
        documents: Vec<koru_ai_core::DocumentId>,
    }
    impl crate::mini_apps::MiniAppRegistry for FakeRegistry {
        fn read(
            &self,
            c: &AiToolExecutionContext,
            _: bool,
            _: bool,
        ) -> Result<crate::mini_apps::Registry, AiError> {
            if c.profile_id != self.profile
                || c.mini_app
                    .as_ref()
                    .is_none_or(|a| a.host.workspace_id != self.workspace)
            {
                return Err(crate::mini_apps::unavailable());
            }
            Ok(crate::mini_apps::Registry {
                lists: self.lists.clone(),
                documents: self.documents.clone(),
            })
        }
    }
    struct Unused;
    impl AiToolExecutor for Unused {
        fn definitions(&self) -> Vec<AiToolDefinition> {
            Vec::new()
        }
        fn invoke(
            &self,
            _: &AiToolExecutionContext,
            _: &AiToolCall,
            _: &Cancellation,
        ) -> Result<AiToolResult, AiError> {
            panic!("fixture unexpectedly requested a non-native tool")
        }
    }
    let host = context
        .mini_app
        .as_ref()
        .ok_or_else(crate::mini_apps::unavailable)?
        .host;
    let dispatcher = AiToolDispatcher {
        store: Box::new(Unused),
        chat: Box::new(Unused),
        definitions: koru_ai_core::mini_apps::definitions(),
        native: crate::mini_apps::MiniAppTools::new(std::sync::Arc::new(FakeRegistry {
            profile: context.profile_id,
            workspace: host.workspace_id,
            lists,
            documents,
        })),
    };
    let provider = koru_ai_provider::DeterministicMockProvider::new(
        "The declaration was offered to the native host.",
    )
    .with_tool_call(koru_ai_core::mini_apps::CREATE_TOOL, declaration);
    let request = AiRequest {
        attempt_id: context.attempt_id,
        provider: koru_ai_core::AiProviderId::parse("mock").unwrap(),
        model: "scripted".into(),
        reasoning: ReasoningChoice::Default,
        prompt_template_version: "fixture".into(),
        system: String::new(),
        messages: Vec::new(),
        context: Vec::new(),
        tools: dispatcher.definitions(),
        tool_history: Vec::new(),
        options: AiRequestOptions::default(),
    };
    let mut tool_loop = AiToolLoop::new(request, AiToolLoopLimits::default())?;
    let calls = provider
        .start(tool_loop.provider_request(), Cancellation::default())?
        .find_map(|event| {
            if let AiEvent::ToolCalls { calls, .. } = event {
                Some(calls)
            } else {
                None
            }
        })
        .ok_or_else(crate::mini_apps::unavailable)?;
    tool_loop.accept_calls(&calls)?;
    let results = calls
        .iter()
        .map(|call| dispatcher.invoke(&context, call, &Cancellation::default()))
        .collect::<Result<Vec<_>, _>>()?;
    let event = crate::mini_apps::creation_event(&context, &results[0])?;
    tool_loop.append_results(calls, results)?;
    // The validated envelope also returns to the same model conversation.
    assert!(
        provider
            .start(tool_loop.provider_request(), Cancellation::default())?
            .any(|event| matches!(event, AiEvent::Completed { .. }))
    );
    let (sender, receiver) = std::sync::mpsc::sync_channel(2);
    sender
        .try_send(koru_ai_wire::StreamEvent {
            version: koru_ai_wire::AI_STREAM_VERSION,
            sequence: 1,
            event,
        })
        .unwrap();
    sender
        .try_send(koru_ai_wire::StreamEvent {
            version: koru_ai_wire::AI_STREAM_VERSION,
            sequence: 2,
            event: koru_ai_wire::PublicAiEvent::Completed {
                attempt_id: context.attempt_id,
                stop_reason: Some("stop".into()),
                at: Timestamp::now(),
            },
        })
        .unwrap();
    drop(sender);
    Ok(receiver.into_iter().collect())
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
        if [
            koru_ai_core::mini_apps::CREATE_TOOL,
            koru_ai_core::mini_apps::LIST_TOOL,
        ]
        .contains(&call.name.as_str())
        {
            self.native.invoke(context, call, cancellation)
        } else if CHAT_TOOL_NAMES.contains(&call.name.as_str()) {
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
        .chain(koru_ai_core::mini_apps::definitions())
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
