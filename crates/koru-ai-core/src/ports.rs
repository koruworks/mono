// SPDX-License-Identifier: MPL-2.0

//! Application ports implemented by persistence, provider, and Store adapters.

use uuid::Uuid;

use crate::{
    AiError, AiErrorClass, AiEvent, AiRequest, AiSearchPage, AiSearchQuery, AiSettings, AiToolCall,
    AiToolDefinition, AiToolResult, BranchCopy, Cancellation, Chat, ChatId, ChatOverview,
    ChatToolPolicy, ContextCommit, DocumentContext, DocumentDestination, DocumentExport,
    DocumentExportId, DocumentId, FeedbackEvent, InferenceAttempt, InferenceAttemptId, Message,
    MessageBlock, MessageBlockContext, MessageId, ModelSelection, PendingRetry, PendingTurn,
    PinnedDocument, ProviderStream, RecoveryCounts, SourceLink, Timestamp, ToolActivity,
    ToolInvocationId, ToolMode, UsageClearResult, UsageReport, WorkspaceId,
};

/// The only persistence surface visible to AI application behavior.
pub trait AiPersistence: Send {
    fn settings(&self) -> Result<Option<AiSettings>, AiError>;
    fn save_settings(&mut self, settings: &AiSettings) -> Result<(), AiError>;
    fn create_chat(&mut self, chat: &Chat) -> Result<(), AiError>;
    /// The title only: a rename is not activity, so the chat keeps its place
    /// in a list ordered by recency.
    fn rename_chat(&mut self, chat: ChatId, title: &str) -> Result<bool, AiError>;
    fn update_chat_model(
        &mut self,
        chat: ChatId,
        model: &ModelSelection,
        updated_at: Timestamp,
    ) -> Result<bool, AiError>;
    fn set_chat_private(
        &mut self,
        chat: ChatId,
        private: bool,
        updated_at: Timestamp,
    ) -> Result<Option<Chat>, AiError>;
    /// The folder a local agent works in for this chat; `None` returns the
    /// chat to the home-folder default.
    fn set_chat_working_folder(
        &mut self,
        chat: ChatId,
        folder: Option<&str>,
        updated_at: Timestamp,
    ) -> Result<Option<Chat>, AiError>;
    /// The approval mode a local agent runs the chat's next message in
    /// (koru-ai D7); `None` is the agent's own default. Like a rename, it
    /// leaves the chat where it is in the list.
    fn set_chat_agent_mode(
        &mut self,
        chat: ChatId,
        mode: Option<&str>,
    ) -> Result<Option<Chat>, AiError>;
    /// Whether a local agent runs the chat's next message in Fast mode
    /// (koru-ai D11); `None` is the model's own default. Leaves the chat
    /// where it is in the list.
    fn set_chat_agent_fast(
        &mut self,
        chat: ChatId,
        fast: Option<bool>,
    ) -> Result<Option<Chat>, AiError>;
    fn chat_tool_policy(&self, chat: ChatId) -> Result<Option<ChatToolPolicy>, AiError>;
    fn replace_chat_tool_policy(
        &mut self,
        chat: ChatId,
        mode: ToolMode,
        workspace_ids: &[WorkspaceId],
        updated_at: Timestamp,
    ) -> Result<Option<ChatToolPolicy>, AiError>;
    fn chat(&self, id: ChatId) -> Result<Option<Chat>, AiError>;
    fn chats(&self) -> Result<Vec<Chat>, AiError>;
    fn chat_overviews(&self) -> Result<Vec<ChatOverview>, AiError>;
    fn messages(&self, chat: ChatId) -> Result<Vec<Message>, AiError>;
    fn blocks(&self, chat: ChatId) -> Result<Vec<MessageBlock>, AiError>;
    fn search(&self, query: &AiSearchQuery) -> Result<AiSearchPage, AiError>;
    fn pinned_documents(&self, chat: ChatId) -> Result<Vec<PinnedDocument>, AiError>;
    fn feedback(&self, chat: ChatId) -> Result<Vec<FeedbackEvent>, AiError>;
    fn citations(&self, chat: ChatId) -> Result<Vec<(MessageId, SourceLink)>, AiError>;
    fn append_feedback(&mut self, event: &FeedbackEvent) -> Result<FeedbackEvent, AiError>;
    fn attempt_for_message(&self, message: MessageId) -> Result<Option<InferenceAttempt>, AiError>;
    fn usage(&self, chat: Option<ChatId>) -> Result<UsageReport, AiError>;
    fn delete_chat(&mut self, chat: ChatId) -> Result<bool, AiError>;
    fn clear_history(&mut self) -> Result<u64, AiError>;
    fn clear_usage(&mut self) -> Result<UsageClearResult, AiError>;
    fn message_block_context(
        &self,
        block: crate::MessageBlockId,
    ) -> Result<Option<MessageBlockContext>, AiError>;
    fn attempt(&self, id: InferenceAttemptId) -> Result<Option<InferenceAttempt>, AiError>;
    fn attempt_by_operation(&self, id: Uuid) -> Result<Option<InferenceAttempt>, AiError>;
    fn tool_activity(&self, id: ToolInvocationId) -> Result<Option<ToolActivity>, AiError>;
    fn tool_activities(&self, chat: ChatId) -> Result<Vec<ToolActivity>, AiError>;
    fn create_tool_activity(&mut self, activity: &ToolActivity) -> Result<(), AiError>;
    fn update_tool_activity(&mut self, activity: &ToolActivity) -> Result<bool, AiError>;
    /// Move a local agent's activity row through its lifecycle. Unlike
    /// `update_tool_activity`, which treats `tool_name` as identity, this
    /// may reclassify the row (an agent names a tool's kind only when it
    /// completes) and it fails when no row changed, so a stale or duplicate
    /// event cannot pass silently. The same timestamp invariants hold.
    fn advance_agent_activity(&mut self, activity: &ToolActivity) -> Result<(), AiError>;
    /// Delete exactly these chats. Clear history deletes its admission
    /// snapshot through this rather than a blanket delete, so a chat that
    /// slipped in while the clear ran could never be swept up.
    fn delete_chats(&mut self, chats: &[ChatId]) -> Result<u64, AiError>;
    /// Atomically insert the user message, pending assistant, and attempt.
    fn begin_turn(&mut self, turn: &PendingTurn) -> Result<InferenceAttempt, AiError>;
    fn begin_retry(&mut self, retry: &PendingRetry) -> Result<InferenceAttempt, AiError>;
    fn create_branch(&mut self, branch: &BranchCopy) -> Result<(), AiError>;
    fn begin_export(&mut self, export: &DocumentExport) -> Result<DocumentExport, AiError>;
    fn export(&self, id: DocumentExportId) -> Result<Option<DocumentExport>, AiError>;
    fn export_by_key(&self, key: Uuid) -> Result<Option<DocumentExport>, AiError>;
    fn finish_export(
        &mut self,
        id: DocumentExportId,
        document: Option<DocumentId>,
        error: Option<AiErrorClass>,
        completed_at: Timestamp,
    ) -> Result<DocumentExport, AiError>;
    fn message(&self, id: MessageId) -> Result<Option<Message>, AiError>;
    /// Atomically record context timings/measurements and immutable provenance
    /// before provider I/O begins.
    fn commit_context(&mut self, context: &ContextCommit) -> Result<InferenceAttempt, AiError>;
    /// Apply one normalized event; terminal persistence remains absorbing.
    ///
    /// `finalized_blocks` is non-empty only for `Completed`. Keeping the blocks
    /// beside the terminal event makes message completion and its immutable
    /// deep-link/search projection one crash-safe transaction without putting
    /// block content on the public provider-event contract.
    fn apply_event(
        &mut self,
        event: &AiEvent,
        finalized_blocks: &[MessageBlock],
    ) -> Result<InferenceAttempt, AiError>;
    fn recover_interrupted(&mut self, now: Timestamp) -> Result<RecoveryCounts, AiError>;
}

/// Provider execution boundary. SDK and credential types stay behind it.
pub trait AiProvider: Send + Sync {
    fn start(
        &self,
        request: AiRequest,
        cancellation: Cancellation,
    ) -> Result<ProviderStream, AiError>;
}

/// Semantic tool execution boundary. Provider adapters never implement tools.
pub trait AiToolExecutor: Send + Sync {
    fn definitions(&self) -> Vec<AiToolDefinition>;
    fn invoke(
        &self,
        context: &AiToolExecutionContext,
        call: &AiToolCall,
        cancellation: &Cancellation,
    ) -> Result<AiToolResult, AiError>;
}

/// Durable identity and explicit workspace authority for one tool invocation.
///
/// This travels beside the provider's opaque call id. Provider adapters never
/// mint Store authority, and an empty workspace set deliberately grants none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiToolExecutionContext {
    pub chat_id: ChatId,
    pub attempt_id: InferenceAttemptId,
    pub invocation_id: ToolInvocationId,
    pub workspace_ids: Vec<WorkspaceId>,
}

/// Narrow Store authority used for exact context and retry-safe export.
pub trait StoreContextPort: Send + Sync {
    fn resolve_document(&self, document: DocumentId) -> Result<DocumentContext, AiError>;
    fn create_document(
        &self,
        idempotency_key: Uuid,
        destination: &DocumentDestination,
        markdown: &str,
    ) -> Result<DocumentId, AiError>;
}
