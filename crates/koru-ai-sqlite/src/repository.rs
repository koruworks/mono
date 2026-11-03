// SPDX-License-Identifier: MPL-2.0

//! Production `AiPersistence` implementation.

use std::fmt;
use std::path::Path;
use std::time::Instant;

use koru_ai_core::{
    AgentToolKind, AiError, AiErrorClass, AiEvent, AiIntent, AiPersistence, AiProviderId,
    AiSearchHit, AiSearchPage, AiSearchQuery, AiSearchSpan, AiSettings, AiSurface, AiUsage,
    AttemptStatus, BranchCopy, BranchOrigin, Chat, ChatAccessLens, ChatId, ChatOverview, ChatRole,
    ChatToolPolicy, ContextCommit, DocumentDestination, DocumentExport, DocumentExportId,
    DocumentId, DocumentRelation, FeedbackEvent, FeedbackEventId, FeedbackValue, InferenceAttempt,
    InferenceAttemptId, Message, MessageBlock, MessageBlockContext, MessageBlockId,
    MessageBlockKind, MessageId, MessageState, ModelSelection, OllamaSettings, OpenAiSettings,
    PendingRetry, PendingTurn, PinnedDocument, ReasoningChoice, RecoveryCounts, SourceLink,
    Timestamp, ToolActivity, ToolActivityStatus, ToolApproval, ToolErrorCode, ToolInvocationId,
    ToolMode, UsageClearResult, UsageGroup, UsageReport, UsageSource, UsageTotals, WebSearchPhase,
    WorkspaceId,
};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};
use uuid::Uuid;

use crate::db::{self, OpenError};

mod attachments;

pub use attachments::{KnownAttachmentDirs, RemovedAttachment};

pub struct Repository {
    connection: Connection,
}

impl Repository {
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        Ok(Self {
            connection: db::open(path)?,
        })
    }

    #[cfg(test)]
    fn in_memory() -> Result<Self, OpenError> {
        Ok(Self {
            connection: db::open_in_memory()?,
        })
    }

    pub fn schema_version(&self) -> Result<i64, OpenError> {
        db::user_version(&self.connection)
    }

    pub fn quick_check(&self) -> Result<bool, OpenError> {
        let result: String = self
            .connection
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
            .map_err(|error| OpenError::Storage {
                database: crate::DATABASE_FILE,
                operation: "check database integrity",
                detail: error.to_string(),
            })?;
        Ok(result == "ok")
    }

    pub fn attempts_for_chat(&self, chat: ChatId) -> Result<Vec<InferenceAttempt>, AiError> {
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT {ATTEMPT_COLUMNS} FROM inference_attempts
                  WHERE chat_id = ?1 AND assistant_message_id IS NOT NULL
                  ORDER BY queued_at_ms, rowid"
            ))
            .map_err(storage_failed)?;
        let rows = statement
            .query_map([chat.to_storage()], attempt_from_row)
            .map_err(storage_failed)?;
        collect_rows(rows)
    }

    #[cfg(test)]
    fn connection(&self) -> &Connection {
        &self.connection
    }
}

impl AiPersistence for Repository {
    fn settings(&self) -> Result<Option<AiSettings>, AiError> {
        self.connection
            .query_row(
                "SELECT selected_provider, openai_model, openai_key_account,
                        openai_reasoning, ollama_endpoint, ollama_model,
                        ollama_reasoning, created_at_ms, updated_at_ms
                   FROM ai_settings WHERE singleton = 1",
                [],
                settings_from_row,
            )
            .optional()
            .map_err(storage_failed)
    }

    fn save_settings(&mut self, settings: &AiSettings) -> Result<(), AiError> {
        validate_settings(settings)?;
        self.connection
            .execute(
                "INSERT INTO ai_settings (
                    singleton, selected_provider, openai_model, openai_key_account,
                    openai_reasoning, ollama_endpoint, ollama_model, ollama_reasoning,
                    created_at_ms, updated_at_ms
                 ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(singleton) DO UPDATE SET
                    selected_provider = excluded.selected_provider,
                    openai_model = excluded.openai_model,
                    openai_key_account = excluded.openai_key_account,
                    openai_reasoning = excluded.openai_reasoning,
                    ollama_endpoint = excluded.ollama_endpoint,
                    ollama_model = excluded.ollama_model,
                    ollama_reasoning = excluded.ollama_reasoning,
                    updated_at_ms = excluded.updated_at_ms",
                params![
                    settings
                        .selected_provider
                        .as_ref()
                        .map(AiProviderId::as_str),
                    settings.openai.model,
                    settings.openai.key_account,
                    settings.openai.reasoning.as_token(),
                    settings.ollama.endpoint.as_str(),
                    settings.ollama.model,
                    settings.ollama.reasoning.as_token(),
                    settings.created_at.millis(),
                    settings.updated_at.millis(),
                ],
            )
            .map_err(storage_failed)?;
        Ok(())
    }

    fn create_chat(&mut self, chat: &Chat) -> Result<(), AiError> {
        if chat.current_model.requested_model.is_empty() {
            return Err(invariant_failed());
        }
        let (branch_chat, branch_message) = chat
            .branched_from
            .as_ref()
            .map(|origin| {
                (
                    Some(origin.chat_id.to_storage()),
                    Some(origin.through_message_id.to_storage()),
                )
            })
            .unwrap_or((None, None));
        self.connection
            .execute(
                "INSERT INTO chats (
                    id, title, current_provider, current_model, current_reasoning,
                    branched_from_chat_id, branched_from_message_id, created_at_ms, updated_at_ms,
                    private, agent_working_folder, agent_mode, agent_fast
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    chat.id.to_storage(),
                    chat.title,
                    chat.current_model.provider.as_str(),
                    chat.current_model.requested_model,
                    chat.current_model.default_reasoning.as_token(),
                    branch_chat,
                    branch_message,
                    chat.created_at.millis(),
                    chat.updated_at.millis(),
                    chat.private,
                    chat.agent_working_folder,
                    chat.agent_mode,
                    chat.agent_fast,
                ],
            )
            .map_err(storage_failed)?;
        Ok(())
    }

    fn rename_chat(&mut self, chat: ChatId, title: &str) -> Result<bool, AiError> {
        // **Not `updated_at_ms`** (the user's call, 2026-10-31): the Chats
        // list is ordered by it, and a renamed chat stays where it was.
        self.connection
            .execute(
                "UPDATE chats SET title = ?2 WHERE id = ?1",
                params![chat.to_storage(), title],
            )
            .map(|updated| updated != 0)
            .map_err(storage_failed)
    }

    fn update_chat_model(
        &mut self,
        chat: ChatId,
        model: &ModelSelection,
        updated_at: Timestamp,
    ) -> Result<bool, AiError> {
        self.connection
            .execute(
                "UPDATE chats
                    SET current_provider = ?2,
                        current_model = ?3,
                        current_reasoning = ?4,
                        updated_at_ms = max(updated_at_ms, ?5)
                  WHERE id = ?1",
                params![
                    chat.to_storage(),
                    model.provider.as_str(),
                    model.requested_model,
                    model.default_reasoning.as_token(),
                    updated_at.millis(),
                ],
            )
            .map(|updated| updated != 0)
            .map_err(storage_failed)
    }

    fn set_chat_private(
        &mut self,
        chat: ChatId,
        private: bool,
        updated_at: Timestamp,
    ) -> Result<Option<Chat>, AiError> {
        let updated = self
            .connection
            .execute(
                "UPDATE chats
                    SET private = ?2, updated_at_ms = max(updated_at_ms, ?3)
                  WHERE id = ?1",
                params![chat.to_storage(), private, updated_at.millis()],
            )
            .map_err(storage_failed)?;
        if updated == 0 {
            return Ok(None);
        }
        query_chat(&self.connection, chat)
    }

    fn set_chat_working_folder(
        &mut self,
        chat: ChatId,
        folder: Option<&str>,
        updated_at: Timestamp,
    ) -> Result<Option<Chat>, AiError> {
        let updated = self
            .connection
            .execute(
                "UPDATE chats
                    SET agent_working_folder = ?2, updated_at_ms = max(updated_at_ms, ?3)
                  WHERE id = ?1",
                params![chat.to_storage(), folder, updated_at.millis()],
            )
            .map_err(storage_failed)?;
        if updated == 0 {
            return Ok(None);
        }
        query_chat(&self.connection, chat)
    }

    fn set_chat_agent_mode(
        &mut self,
        chat: ChatId,
        mode: Option<&str>,
    ) -> Result<Option<Chat>, AiError> {
        let updated = self
            .connection
            .execute(
                "UPDATE chats SET agent_mode = ?2 WHERE id = ?1",
                params![chat.to_storage(), mode],
            )
            .map_err(storage_failed)?;
        if updated == 0 {
            return Ok(None);
        }
        query_chat(&self.connection, chat)
    }

    fn set_chat_agent_fast(
        &mut self,
        chat: ChatId,
        fast: Option<bool>,
    ) -> Result<Option<Chat>, AiError> {
        let updated = self
            .connection
            .execute(
                "UPDATE chats SET agent_fast = ?2 WHERE id = ?1",
                params![chat.to_storage(), fast],
            )
            .map_err(storage_failed)?;
        if updated == 0 {
            return Ok(None);
        }
        query_chat(&self.connection, chat)
    }

    fn chat_tool_policy(&self, chat: ChatId) -> Result<Option<ChatToolPolicy>, AiError> {
        query_chat_tool_policy(&self.connection, chat)
    }

    fn replace_chat_tool_policy(
        &mut self,
        chat: ChatId,
        mode: ToolMode,
        workspace_ids: &[WorkspaceId],
        updated_at: Timestamp,
    ) -> Result<Option<ChatToolPolicy>, AiError> {
        let mut workspace_ids = workspace_ids.to_vec();
        workspace_ids.sort_unstable();
        workspace_ids.dedup();
        let transaction = self.connection.transaction().map_err(storage_failed)?;
        let updated = transaction
            .execute(
                "UPDATE chats
                    SET tool_mode = ?2, updated_at_ms = max(updated_at_ms, ?3)
                  WHERE id = ?1",
                params![
                    chat.to_storage(),
                    tool_mode_token(mode),
                    updated_at.millis()
                ],
            )
            .map_err(storage_failed)?;
        if updated == 0 {
            return Ok(None);
        }
        transaction
            .execute(
                "DELETE FROM chat_workspace_grants WHERE chat_id = ?1",
                [chat.to_storage()],
            )
            .map_err(storage_failed)?;
        for workspace in workspace_ids {
            transaction
                .execute(
                    "INSERT INTO chat_workspace_grants
                        (chat_id, workspace_id, granted_at_ms)
                     VALUES (?1, ?2, ?3)",
                    params![
                        chat.to_storage(),
                        workspace.to_storage(),
                        updated_at.millis()
                    ],
                )
                .map_err(storage_failed)?;
        }
        let policy = query_chat_tool_policy(&transaction, chat)?.ok_or_else(storage_missing)?;
        transaction.commit().map_err(storage_failed)?;
        Ok(Some(policy))
    }

    fn chat(&self, id: ChatId) -> Result<Option<Chat>, AiError> {
        query_chat(&self.connection, id)
    }

    fn chats(&self) -> Result<Vec<Chat>, AiError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, title, current_provider, current_model, current_reasoning,
                        branched_from_chat_id, branched_from_message_id,
                        created_at_ms, updated_at_ms, private, agent_working_folder, agent_mode,
                        agent_fast
                   FROM chats
                  ORDER BY updated_at_ms DESC, id",
            )
            .map_err(storage_failed)?;
        let rows = statement
            .query_map([], chat_from_row)
            .map_err(storage_failed)?;
        collect_rows(rows)
    }

    fn chat_overviews(&self) -> Result<Vec<ChatOverview>, AiError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT chat.id, chat.title, chat.private,
                        count(message.id), chat.created_at_ms, chat.updated_at_ms
                   FROM chats AS chat
                   LEFT JOIN messages AS message
                     ON message.chat_id = chat.id AND message.state = 'complete'
                  GROUP BY chat.id
                  ORDER BY chat.updated_at_ms DESC, chat.id",
            )
            .map_err(storage_failed)?;
        let rows = statement
            .query_map([], |row| {
                Ok(ChatOverview {
                    chat_id: required_id(row, 0, ChatId::parse, "ChatId")?,
                    title: row.get(1)?,
                    private: row.get(2)?,
                    message_count: nonnegative_u32(row, 3)?,
                    created_at: Timestamp::from_millis(row.get(4)?),
                    updated_at: Timestamp::from_millis(row.get(5)?),
                })
            })
            .map_err(storage_failed)?;
        collect_rows(rows)
    }

    fn messages(&self, chat: ChatId) -> Result<Vec<Message>, AiError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, chat_id, ordinal, role, state, markdown, origin_message_id,
                        provider, requested_model, resolved_model, error_class,
                        created_at_ms, completed_at_ms
                   FROM messages
                  WHERE chat_id = ?1
                  ORDER BY ordinal",
            )
            .map_err(storage_failed)?;
        let rows = statement
            .query_map([chat.to_storage()], message_from_row)
            .map_err(storage_failed)?;
        collect_rows(rows)
    }

    fn blocks(&self, chat: ChatId) -> Result<Vec<MessageBlock>, AiError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT block.id, block.message_id, block.ordinal, block.kind,
                        block.markdown, block.plain_text
                   FROM message_blocks AS block
                   JOIN messages AS message ON message.id = block.message_id
                  WHERE message.chat_id = ?1
                  ORDER BY message.ordinal, block.ordinal",
            )
            .map_err(storage_failed)?;
        let rows = statement
            .query_map([chat.to_storage()], block_from_row)
            .map_err(storage_failed)?;
        collect_rows(rows)
    }

    fn search(&self, query: &AiSearchQuery) -> Result<AiSearchPage, AiError> {
        let text = fts_query(&query.text);
        if text.is_empty() || query.limit == 0 {
            return Ok(AiSearchPage {
                hits: Vec::new(),
                next_cursor: None,
            });
        }
        let offset = query
            .cursor
            .as_deref()
            .and_then(|cursor| cursor.parse::<usize>().ok())
            .unwrap_or_default();
        let limit = usize::from(query.limit.min(200));
        let fetch = limit.saturating_add(1);
        let mut statement = self
            .connection
            .prepare(
                "WITH matches AS (
                    SELECT chat.id AS chat_id, message.id AS message_id,
                           block.id AS block_id, chat.title AS chat_title,
                           snippet(message_blocks_fts, 0, char(1), char(2), ' ... ', 24) AS snippet,
                           bm25(message_blocks_fts) AS rank,
                           message.ordinal AS message_ordinal,
                           block.ordinal AS block_ordinal
                      FROM message_blocks_fts
                      JOIN message_blocks AS block ON block.rowid = message_blocks_fts.rowid
                      JOIN messages AS message ON message.id = block.message_id
                      JOIN chats AS chat ON chat.id = message.chat_id
                     WHERE message_blocks_fts MATCH ?1
                       AND message.state = 'complete'
                       AND (?4 = 0 OR chat.private = 0 OR chat.id = ?5)
                ), ranked AS (
                    SELECT *,
                           row_number() OVER (
                               PARTITION BY message_id
                               ORDER BY rank, block_ordinal, block_id
                           ) AS response_rank
                      FROM matches
                )
                SELECT chat_id, message_id, block_id, chat_title, snippet, rank
                  FROM ranked
                 WHERE response_rank = 1
                 ORDER BY rank, message_ordinal DESC, block_ordinal, block_id
                 LIMIT ?2 OFFSET ?3",
            )
            .map_err(storage_failed)?;
        let calling_chat = match query.visibility {
            ChatAccessLens::ToolSurface { calling_chat } => {
                calling_chat.map(|chat| chat.to_storage())
            }
            ChatAccessLens::UserSurface => None,
        };
        let tool_surface = matches!(query.visibility, ChatAccessLens::ToolSurface { .. });
        let rows = statement
            .query_map(
                params![
                    text,
                    i64::try_from(fetch).unwrap_or(i64::MAX),
                    i64::try_from(offset).unwrap_or(i64::MAX),
                    tool_surface,
                    calling_chat,
                ],
                |row| {
                    let rank: f64 = row.get(5)?;
                    let marked: String = row.get(4)?;
                    let (snippet, spans) = highlighted_snippet(&marked);
                    Ok(AiSearchHit {
                        chat_id: required_id(row, 0, ChatId::parse, "ChatId")?,
                        message_id: required_id(row, 1, MessageId::parse, "MessageId")?,
                        block_id: required_id(row, 2, MessageBlockId::parse, "MessageBlockId")?,
                        chat_title: row.get(3)?,
                        snippet,
                        spans,
                        score: (-rank) as f32,
                    })
                },
            )
            .map_err(storage_failed)?;
        let mut hits = collect_rows(rows)?;
        let has_more = hits.len() > limit;
        hits.truncate(limit);
        Ok(AiSearchPage {
            next_cursor: has_more.then(|| offset.saturating_add(hits.len()).to_string()),
            hits,
        })
    }

    fn pinned_documents(&self, chat: ChatId) -> Result<Vec<PinnedDocument>, AiError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT chat_id, document_id, display_title, pinned,
                        first_used_at_ms, last_used_at_ms
                   FROM chat_documents
                  WHERE chat_id = ?1 AND pinned = 1
                  ORDER BY first_used_at_ms, document_id",
            )
            .map_err(storage_failed)?;
        let rows = statement
            .query_map([chat.to_storage()], |row| {
                Ok(PinnedDocument {
                    chat_id: required_id(row, 0, ChatId::parse, "ChatId")?,
                    document_id: required_id(
                        row,
                        1,
                        koru_ai_core::DocumentId::parse,
                        "DocumentId",
                    )?,
                    display_title: row.get(2)?,
                    pinned: row.get(3)?,
                    first_used_at: Timestamp::from_millis(row.get(4)?),
                    last_used_at: Timestamp::from_millis(row.get(5)?),
                })
            })
            .map_err(storage_failed)?;
        collect_rows(rows)
    }

    fn feedback(&self, chat: ChatId) -> Result<Vec<FeedbackEvent>, AiError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, chat_id, message_id, attempt_id, value, surface, created_at_ms
               FROM current_message_feedback
              WHERE chat_id = ?1
              ORDER BY created_at_ms, id",
            )
            .map_err(storage_failed)?;
        let rows = statement
            .query_map([chat.to_storage()], feedback_from_row)
            .map_err(storage_failed)?;
        collect_rows(rows)
    }

    fn citations(&self, chat: ChatId) -> Result<Vec<(MessageId, SourceLink)>, AiError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT citation.message_id, citation.url, citation.title
                   FROM message_citations AS citation
                   JOIN messages AS message ON message.id = citation.message_id
                  WHERE message.chat_id = ?1
                  ORDER BY message.ordinal, citation.ordinal",
            )
            .map_err(storage_failed)?;
        let rows = statement
            .query_map([chat.to_storage()], |row| {
                Ok((
                    required_id(row, 0, MessageId::parse, "MessageId")?,
                    SourceLink {
                        url: row.get(1)?,
                        title: row.get(2)?,
                    },
                ))
            })
            .map_err(storage_failed)?;
        collect_rows(rows)
    }

    fn append_feedback(&mut self, event: &FeedbackEvent) -> Result<FeedbackEvent, AiError> {
        if event.message_id.is_none() && event.attempt_id.is_none() {
            return Err(invariant_failed());
        }
        self.connection
            .execute(
                "INSERT INTO feedback_events
                (id, chat_id, message_id, attempt_id, value, surface, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO NOTHING",
                params![
                    event.id.to_storage(),
                    event.chat_id.map(ChatId::to_storage),
                    event.message_id.map(MessageId::to_storage),
                    event.attempt_id.map(InferenceAttemptId::to_storage),
                    feedback_token(event.value),
                    surface_token(event.surface),
                    event.created_at.millis(),
                ],
            )
            .map_err(storage_failed)?;
        self.connection
            .query_row(
                "SELECT id, chat_id, message_id, attempt_id, value, surface, created_at_ms
               FROM feedback_events WHERE id = ?1",
                [event.id.to_storage()],
                feedback_from_row,
            )
            .map_err(storage_failed)
    }

    fn attempt_for_message(&self, message: MessageId) -> Result<Option<InferenceAttempt>, AiError> {
        self.connection
            .query_row(
                &format!(
                    "SELECT {ATTEMPT_COLUMNS} FROM inference_attempts WHERE assistant_message_id = ?1"
                ),
                [message.to_storage()],
                attempt_from_row,
            )
            .optional()
            .map_err(storage_failed)
    }

    fn usage(&self, chat: Option<ChatId>) -> Result<UsageReport, AiError> {
        let current_chat = chat
            .map(|chat_id| query_usage_totals(&self.connection, chat_id))
            .transpose()?;
        Ok(UsageReport {
            chat_id: chat,
            current_chat,
            groups: query_usage_groups(&self.connection)?,
        })
    }

    fn delete_chat(&mut self, chat: ChatId) -> Result<bool, AiError> {
        self.connection
            .execute("DELETE FROM chats WHERE id = ?1", [chat.to_storage()])
            .map(|deleted| deleted != 0)
            .map_err(storage_failed)
    }

    fn clear_history(&mut self) -> Result<u64, AiError> {
        self.connection
            .execute("DELETE FROM chats", [])
            .map(|deleted| u64::try_from(deleted).unwrap_or(u64::MAX))
            .map_err(storage_failed)
    }

    fn clear_usage(&mut self) -> Result<UsageClearResult, AiError> {
        let transaction = self.connection.transaction().map_err(storage_failed)?;
        let feedback_events = transaction
            .execute("DELETE FROM feedback_events", [])
            .map_err(storage_failed)?;
        let attempts = transaction
            .execute("DELETE FROM inference_attempts", [])
            .map_err(storage_failed)?;
        transaction.commit().map_err(storage_failed)?;
        Ok(UsageClearResult {
            attempts: u64::try_from(attempts).unwrap_or(u64::MAX),
            feedback_events: u64::try_from(feedback_events).unwrap_or(u64::MAX),
        })
    }

    fn message(&self, id: MessageId) -> Result<Option<Message>, AiError> {
        self.connection
            .query_row(
                "SELECT id, chat_id, ordinal, role, state, markdown, origin_message_id,
                    provider, requested_model, resolved_model, error_class,
                    created_at_ms, completed_at_ms
               FROM messages WHERE id = ?1",
                [id.to_storage()],
                message_from_row,
            )
            .optional()
            .map_err(storage_failed)
    }

    fn message_block_context(
        &self,
        block: MessageBlockId,
    ) -> Result<Option<MessageBlockContext>, AiError> {
        self.connection
            .query_row(
                "SELECT message.chat_id, chat.private, message.id, block.id, message.role,
                        block.markdown, message.created_at_ms
                   FROM message_blocks AS block
                   JOIN messages AS message ON message.id = block.message_id
                   JOIN chats AS chat ON chat.id = message.chat_id
                  WHERE block.id = ?1",
                [block.to_storage()],
                |row| {
                    Ok(MessageBlockContext {
                        chat_id: required_id(row, 0, ChatId::parse, "ChatId")?,
                        chat_private: row.get(1)?,
                        message_id: required_id(row, 2, MessageId::parse, "MessageId")?,
                        block_id: required_id(row, 3, MessageBlockId::parse, "MessageBlockId")?,
                        role: parse_role(&row.get::<_, String>(4)?)
                            .ok_or_else(|| decode_failed(4, "role"))?,
                        markdown: row.get(5)?,
                        message_created_at: Timestamp::from_millis(row.get(6)?),
                    })
                },
            )
            .optional()
            .map_err(storage_failed)
    }

    fn attempt(&self, id: InferenceAttemptId) -> Result<Option<InferenceAttempt>, AiError> {
        query_attempt(&self.connection, "id", &id.to_storage())
    }

    fn attempt_by_operation(&self, id: Uuid) -> Result<Option<InferenceAttempt>, AiError> {
        query_attempt(&self.connection, "operation_id", &id.to_string())
    }

    fn tool_activity(&self, id: ToolInvocationId) -> Result<Option<ToolActivity>, AiError> {
        query_tool_activity(&self.connection, id)
    }

    fn tool_activities(&self, chat: ChatId) -> Result<Vec<ToolActivity>, AiError> {
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT {TOOL_ACTIVITY_COLUMNS}
                   FROM tool_invocations
                  WHERE chat_id = ?1
                  ORDER BY proposed_at_ms, id"
            ))
            .map_err(storage_failed)?;
        let rows = statement
            .query_map([chat.to_storage()], tool_activity_from_row)
            .map_err(storage_failed)?;
        collect_rows(rows)
    }

    fn create_tool_activity(&mut self, activity: &ToolActivity) -> Result<(), AiError> {
        if activity.status != ToolActivityStatus::Proposed
            || activity.started_at.is_some()
            || activity.completed_at.is_some()
            || activity.result_bytes.is_some()
            || activity.error_code.is_some()
            || activity.error_message.is_some()
        {
            return Err(invariant_failed());
        }
        let attempt = self
            .attempt(activity.attempt_id)?
            .ok_or_else(invariant_failed)?;
        if attempt.chat_id != Some(activity.chat_id)
            || attempt.assistant_message_id != Some(activity.assistant_message_id)
        {
            return Err(invariant_failed());
        }
        self.connection
            .execute(
                "INSERT INTO tool_invocations (
                    id, attempt_id, chat_id, tool_name, read_only, status, approval,
                    proposed_at_ms, started_at_ms, completed_at_ms, result_bytes, error_code,
                    target_workspace_id, target_document_id, error_message,
                    agent_label, agent_summary, agent_input, agent_output, text_offset
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, NULL, NULL, ?9, ?10, NULL,
                           ?11, ?12, ?13, ?14,
                           CASE WHEN ?15 THEN (
                               SELECT length(CAST(markdown AS BLOB)) FROM messages
                                WHERE id = ?16
                           ) END)",
                params![
                    activity.id.to_storage(),
                    activity.attempt_id.to_storage(),
                    activity.chat_id.to_storage(),
                    activity.tool_name,
                    activity.read_only,
                    tool_activity_status_token(activity.status),
                    tool_approval_token(activity.approval),
                    activity.proposed_at.millis(),
                    activity.target_workspace_id.map(|id| id.to_storage()),
                    activity.target_document_id.map(|id| id.to_storage()),
                    activity.agent_label,
                    activity.agent_summary,
                    activity.agent_input,
                    activity.agent_output,
                    // Where the reply stood when a local agent's call began
                    // (schema 10). Text deltas are written before the next
                    // event is applied, so the length is exact.
                    AgentToolKind::is_agent_token(&activity.tool_name),
                    activity.assistant_message_id.to_storage(),
                ],
            )
            .map_err(storage_failed)?;
        Ok(())
    }

    fn update_tool_activity(&mut self, activity: &ToolActivity) -> Result<bool, AiError> {
        let start_required = matches!(
            activity.status,
            ToolActivityStatus::Running | ToolActivityStatus::Complete | ToolActivityStatus::Failed
        );
        let start_forbidden = matches!(
            activity.status,
            ToolActivityStatus::Proposed
                | ToolActivityStatus::ApprovalRequired
                | ToolActivityStatus::Denied
        );
        if activity.completed_at.is_some() != activity.status.is_terminal()
            || (start_required && activity.started_at.is_none())
            || (start_forbidden && activity.started_at.is_some())
            || activity
                .completed_at
                .is_some_and(|completed| completed < activity.proposed_at)
            || (activity.error_message.is_some()
                && !matches!(
                    activity.status,
                    ToolActivityStatus::Denied
                        | ToolActivityStatus::Failed
                        | ToolActivityStatus::Cancelled
                ))
        {
            return Err(invariant_failed());
        }
        self.connection
            .execute(
                "UPDATE tool_invocations
                    SET status = ?6, approval = ?7, started_at_ms = ?8,
                        completed_at_ms = ?9, result_bytes = ?10, error_code = ?11,
                        error_message = ?12
                  WHERE id = ?1 AND attempt_id = ?2 AND chat_id = ?3
                    AND tool_name = ?4 AND read_only = ?5
                    AND target_workspace_id IS ?13 AND target_document_id IS ?14",
                params![
                    activity.id.to_storage(),
                    activity.attempt_id.to_storage(),
                    activity.chat_id.to_storage(),
                    activity.tool_name,
                    activity.read_only,
                    tool_activity_status_token(activity.status),
                    tool_approval_token(activity.approval),
                    activity.started_at.map(Timestamp::millis),
                    activity.completed_at.map(Timestamp::millis),
                    optional_u64(activity.result_bytes)?,
                    activity.error_code.map(tool_error_code_token),
                    activity.error_message,
                    activity.target_workspace_id.map(|id| id.to_storage()),
                    activity.target_document_id.map(|id| id.to_storage()),
                ],
            )
            .map(|updated| updated != 0)
            .map_err(storage_failed)
    }

    fn advance_agent_activity(&mut self, activity: &ToolActivity) -> Result<(), AiError> {
        if !AgentToolKind::is_agent_token(&activity.tool_name) {
            return Err(invariant_failed());
        }
        // The same timestamp invariants as a workspace tool's row: a running
        // or finished tool has started; a proposed, pending or denied one has
        // not; a terminal row has completed; prose sits only on a failure.
        let start_required = matches!(
            activity.status,
            ToolActivityStatus::Running | ToolActivityStatus::Complete | ToolActivityStatus::Failed
        );
        let start_forbidden = matches!(
            activity.status,
            ToolActivityStatus::Proposed
                | ToolActivityStatus::ApprovalRequired
                | ToolActivityStatus::Denied
        );
        if activity.completed_at.is_some() != activity.status.is_terminal()
            || (start_required && activity.started_at.is_none())
            || (start_forbidden && activity.started_at.is_some())
            || activity
                .completed_at
                .is_some_and(|completed| completed < activity.proposed_at)
            || (activity.error_message.is_some()
                && !matches!(
                    activity.status,
                    ToolActivityStatus::Denied
                        | ToolActivityStatus::Failed
                        | ToolActivityStatus::Cancelled
                ))
        {
            return Err(invariant_failed());
        }
        // Identity is the row, not its kind: an agent names a tool's kind
        // only when it completes, so `tool_name` is what this call may
        // change. A row that is already terminal stays terminal, so a stale
        // or duplicate event changes nothing — and that is an error here.
        let updated = self
            .connection
            .execute(
                "UPDATE tool_invocations
                    SET tool_name = ?4, status = ?5, approval = ?6, started_at_ms = ?7,
                        completed_at_ms = ?8, error_code = ?9, error_message = ?10,
                        agent_label = ?11, agent_summary = ?12, agent_input = ?13,
                        agent_output = ?14
                  WHERE id = ?1 AND attempt_id = ?2 AND chat_id = ?3
                    AND status NOT IN ('complete', 'denied', 'failed', 'cancelled')",
                params![
                    activity.id.to_storage(),
                    activity.attempt_id.to_storage(),
                    activity.chat_id.to_storage(),
                    activity.tool_name,
                    tool_activity_status_token(activity.status),
                    tool_approval_token(activity.approval),
                    activity.started_at.map(Timestamp::millis),
                    activity.completed_at.map(Timestamp::millis),
                    activity.error_code.map(tool_error_code_token),
                    activity.error_message,
                    activity.agent_label,
                    activity.agent_summary,
                    activity.agent_input,
                    activity.agent_output,
                ],
            )
            .map_err(storage_failed)?;
        if updated == 0 {
            return Err(invariant_failed());
        }
        Ok(())
    }

    fn delete_chats(&mut self, chats: &[ChatId]) -> Result<u64, AiError> {
        let transaction = self.connection.transaction().map_err(storage_failed)?;
        let mut deleted = 0u64;
        for chat in chats {
            deleted = deleted.saturating_add(
                transaction
                    .execute("DELETE FROM chats WHERE id = ?1", [chat.to_storage()])
                    .map(|count| u64::try_from(count).unwrap_or(u64::MAX))
                    .map_err(storage_failed)?,
            );
        }
        transaction.commit().map_err(storage_failed)?;
        Ok(deleted)
    }

    fn begin_turn(&mut self, turn: &PendingTurn) -> Result<InferenceAttempt, AiError> {
        validate_turn(turn)?;
        let started = Instant::now();
        let transaction = self.connection.transaction().map_err(storage_failed)?;
        if let Some(existing) = query_attempt(
            &transaction,
            "operation_id",
            &turn.attempt.operation_id.to_string(),
        )? {
            trace_attempt_transaction("begin_turn", existing.id, "replayed", started);
            return Ok(existing);
        }

        insert_message(&transaction, &turn.user_message)?;
        insert_message(&transaction, &turn.assistant_message)?;
        insert_attempt(&transaction, &turn.attempt)?;
        attachments::bind_attachments(
            &transaction,
            turn.user_message.chat_id,
            turn.user_message.id,
            &turn.attachment_ids,
            turn.user_message.created_at,
        )?;
        transaction
            .execute(
                "UPDATE chats
                    SET updated_at_ms = max(updated_at_ms, ?2)
                  WHERE id = ?1",
                params![
                    turn.user_message.chat_id.to_storage(),
                    turn.assistant_message.created_at.millis()
                ],
            )
            .map_err(storage_failed)?;
        transaction.commit().map_err(storage_failed)?;
        trace_attempt_transaction("begin_turn", turn.attempt.id, "committed", started);
        Ok(turn.attempt.clone())
    }

    fn append_user_message(&mut self, message: &Message) -> Result<Message, AiError> {
        if message.role != ChatRole::User || message.state != MessageState::Complete {
            return Err(invariant_failed());
        }
        let transaction = self.connection.transaction().map_err(storage_failed)?;
        let existing = transaction
            .query_row(
                "SELECT id, chat_id, ordinal, role, state, markdown, origin_message_id,
                    provider, requested_model, resolved_model, error_class,
                    created_at_ms, completed_at_ms
               FROM messages WHERE id = ?1",
                [message.id.to_storage()],
                message_from_row,
            )
            .optional()
            .map_err(storage_failed)?;
        if let Some(existing) = existing {
            return if existing.chat_id == message.chat_id {
                Ok(existing)
            } else {
                Err(invariant_failed())
            };
        }
        // After every message the chat has, the running reply's included.
        let next: i64 = transaction
            .query_row(
                "SELECT COALESCE(MAX(ordinal) + 1, 0) FROM messages WHERE chat_id = ?1",
                [message.chat_id.to_storage()],
                |row| row.get(0),
            )
            .map_err(storage_failed)?;
        let mut stored = message.clone();
        stored.ordinal = u32::try_from(next).map_err(|_| invariant_failed())?;
        insert_message(&transaction, &stored)?;
        transaction
            .execute(
                "UPDATE chats SET updated_at_ms = max(updated_at_ms, ?2) WHERE id = ?1",
                params![stored.chat_id.to_storage(), stored.created_at.millis()],
            )
            .map_err(storage_failed)?;
        transaction.commit().map_err(storage_failed)?;
        Ok(stored)
    }

    fn begin_retry(&mut self, retry: &PendingRetry) -> Result<InferenceAttempt, AiError> {
        if retry.assistant_message.role != ChatRole::Assistant
            || retry.assistant_message.state != MessageState::Pending
            || retry.attempt.assistant_message_id != Some(retry.assistant_message.id)
            || retry.attempt.chat_id != Some(retry.assistant_message.chat_id)
        {
            return Err(invariant_failed());
        }
        let started = Instant::now();
        let transaction = self.connection.transaction().map_err(storage_failed)?;
        if let Some(existing) = query_attempt(
            &transaction,
            "operation_id",
            &retry.attempt.operation_id.to_string(),
        )? {
            trace_attempt_transaction("begin_retry", existing.id, "replayed", started);
            return Ok(existing);
        }
        insert_message(&transaction, &retry.assistant_message)?;
        insert_attempt(&transaction, &retry.attempt)?;
        transaction
            .execute(
                "UPDATE chats SET updated_at_ms = max(updated_at_ms, ?2) WHERE id = ?1",
                params![
                    retry.assistant_message.chat_id.to_storage(),
                    retry.assistant_message.created_at.millis()
                ],
            )
            .map_err(storage_failed)?;
        transaction.commit().map_err(storage_failed)?;
        trace_attempt_transaction("begin_retry", retry.attempt.id, "committed", started);
        Ok(retry.attempt.clone())
    }

    fn create_branch(&mut self, branch: &BranchCopy) -> Result<(), AiError> {
        if branch.chat.branched_from.is_none()
            || branch.tool_policy.chat_id != branch.chat.id
            || branch
                .messages
                .iter()
                .any(|message| message.chat_id != branch.chat.id)
            || branch.blocks.iter().any(|block| {
                !branch
                    .messages
                    .iter()
                    .any(|message| message.id == block.message_id)
            })
            || branch.citations.iter().any(|(message, _)| {
                !branch
                    .messages
                    .iter()
                    .any(|candidate| candidate.id == *message)
            })
            || branch
                .pinned_documents
                .iter()
                .any(|pin| pin.chat_id != branch.chat.id)
            || branch.attachments.iter().any(|record| {
                record.chat_id != branch.chat.id
                    || !record.attachment.message_id.is_some_and(|message| {
                        branch
                            .messages
                            .iter()
                            .any(|candidate| candidate.id == message)
                    })
            })
        {
            return Err(invariant_failed());
        }
        let transaction = self.connection.transaction().map_err(storage_failed)?;
        if query_chat(&transaction, branch.chat.id)?.is_some() {
            return Ok(());
        }
        insert_chat(&transaction, &branch.chat)?;
        transaction
            .execute(
                "UPDATE chats SET tool_mode = ?2 WHERE id = ?1",
                params![
                    branch.chat.id.to_storage(),
                    tool_mode_token(branch.tool_policy.mode)
                ],
            )
            .map_err(storage_failed)?;
        for workspace in &branch.tool_policy.workspace_ids {
            transaction
                .execute(
                    "INSERT INTO chat_workspace_grants
                        (chat_id, workspace_id, granted_at_ms)
                     VALUES (?1, ?2, ?3)",
                    params![
                        branch.chat.id.to_storage(),
                        workspace.to_storage(),
                        branch.chat.created_at.millis()
                    ],
                )
                .map_err(storage_failed)?;
        }
        for message in &branch.messages {
            insert_message(&transaction, message)?;
        }
        for block in &branch.blocks {
            insert_block(&transaction, block)?;
        }
        for (message, source) in &branch.citations {
            transaction
                .execute(
                    "INSERT INTO message_citations (message_id, ordinal, url, title)
                     SELECT ?1, COALESCE(MAX(ordinal) + 1, 0), ?2, ?3
                       FROM message_citations WHERE message_id = ?1",
                    params![message.to_storage(), source.url, source.title],
                )
                .map_err(storage_failed)?;
        }
        for pin in &branch.pinned_documents {
            transaction
                .execute(
                    "INSERT INTO chat_documents
                    (chat_id, document_id, display_title, pinned, first_used_at_ms, last_used_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        pin.chat_id.to_storage(),
                        pin.document_id.to_storage(),
                        pin.display_title,
                        pin.pinned,
                        pin.first_used_at.millis(),
                        pin.last_used_at.millis()
                    ],
                )
                .map_err(storage_failed)?;
        }
        for record in &branch.attachments {
            attachments::insert_attachment(&transaction, record, Some(branch.chat.created_at))?;
        }
        transaction.commit().map_err(storage_failed)?;
        Ok(())
    }

    fn begin_export(&mut self, export: &DocumentExport) -> Result<DocumentExport, AiError> {
        if export.status != koru_ai_core::ExportStatus::Pending
            || export.document_id.is_some()
            || export.completed_at.is_some()
        {
            return Err(invariant_failed());
        }
        self.connection
            .execute(
                "INSERT INTO document_exports (
                id, idempotency_key, chat_id, through_message_id,
                destination_profile_id, destination_workspace_id, destination_parent_id,
                destination_index, destination_title, status, document_id, error_class,
                created_at_ms, completed_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', NULL, NULL, ?10, NULL)
             ON CONFLICT(idempotency_key) DO NOTHING",
                params![
                    export.id.to_storage(),
                    export.idempotency_key.to_string(),
                    export.chat_id.to_storage(),
                    export.through_message_id.to_storage(),
                    export.destination.profile_id.to_storage(),
                    export.destination.workspace_id.to_storage(),
                    export
                        .destination
                        .parent_document_id
                        .map(|id| id.to_storage()),
                    export
                        .destination
                        .index
                        .and_then(|index| i64::try_from(index).ok()),
                    export.destination.title,
                    export.created_at.millis()
                ],
            )
            .map_err(storage_failed)?;
        query_export(
            &self.connection,
            "idempotency_key",
            &export.idempotency_key.to_string(),
        )?
        .ok_or_else(storage_missing)
    }

    fn export(&self, id: DocumentExportId) -> Result<Option<DocumentExport>, AiError> {
        query_export(&self.connection, "id", &id.to_storage())
    }

    fn export_by_key(&self, key: Uuid) -> Result<Option<DocumentExport>, AiError> {
        query_export(&self.connection, "idempotency_key", &key.to_string())
    }

    fn finish_export(
        &mut self,
        id: DocumentExportId,
        document: Option<koru_ai_core::DocumentId>,
        error: Option<AiErrorClass>,
        completed_at: Timestamp,
    ) -> Result<DocumentExport, AiError> {
        if document.is_some() == error.is_some() {
            return Err(invariant_failed());
        }
        let transaction = self.connection.transaction().map_err(storage_failed)?;
        let current =
            query_export(&transaction, "id", &id.to_storage())?.ok_or_else(storage_missing)?;
        if current.status != koru_ai_core::ExportStatus::Pending {
            return Ok(current);
        }
        let status = if document.is_some() {
            "complete"
        } else {
            "failed"
        };
        transaction
            .execute(
                "UPDATE document_exports SET status = ?2, document_id = ?3, error_class = ?4,
                    completed_at_ms = ?5 WHERE id = ?1 AND status = 'pending'",
                params![
                    id.to_storage(),
                    status,
                    document.map(|id| id.to_storage()),
                    error.map(error_class_token),
                    completed_at.millis()
                ],
            )
            .map_err(storage_failed)?;
        if let Some(document) = document {
            transaction
                .execute(
                    "INSERT INTO message_document_refs
                    (message_id, document_id, relation, display_title, source_revision, used_at_ms)
                 VALUES (?1, ?2, 'exported_copy', ?3, NULL, ?4)",
                    params![
                        current.through_message_id.to_storage(),
                        document.to_storage(),
                        current.destination.title,
                        completed_at.millis()
                    ],
                )
                .map_err(storage_failed)?;
        }
        let updated =
            query_export(&transaction, "id", &id.to_storage())?.ok_or_else(storage_missing)?;
        transaction.commit().map_err(storage_failed)?;
        Ok(updated)
    }

    fn commit_context(&mut self, context: &ContextCommit) -> Result<InferenceAttempt, AiError> {
        if context.completed_at < context.started_at
            || usize::try_from(context.measurements.context_document_count).ok()
                != Some(context.document_refs.len())
            || usize::try_from(context.measurements.context_message_block_count).ok()
                != Some(context.message_block_refs.len())
            || context
                .document_refs
                .iter()
                .any(|reference| reference.relation != DocumentRelation::Context)
        {
            return Err(invariant_failed());
        }

        let started = Instant::now();
        let transaction = self.connection.transaction().map_err(storage_failed)?;
        let current = query_attempt(&transaction, "id", &context.attempt_id.to_storage())?
            .ok_or_else(storage_missing)?;
        if current.status.is_terminal() {
            trace_attempt_transaction("commit_context", current.id, "terminal_noop", started);
            return Ok(current);
        }
        if context.started_at < current.queued_at
            || context
                .pinned_documents
                .iter()
                .any(|document| Some(document.chat_id) != current.chat_id)
        {
            return Err(invariant_failed());
        }

        transaction
            .execute(
                "UPDATE inference_attempts
                    SET context_started_at_ms = ?2, context_completed_at_ms = ?3,
                        context_payload_bytes = ?4, message_count = ?5,
                        context_document_count = ?6,
                        context_message_block_count = ?7
                  WHERE id = ?1 AND status = 'pending'",
                params![
                    context.attempt_id.to_storage(),
                    context.started_at.millis(),
                    context.completed_at.millis(),
                    required_u64(context.measurements.context_payload_bytes)?,
                    i64::from(context.measurements.message_count),
                    i64::from(context.measurements.context_document_count),
                    i64::from(context.measurements.context_message_block_count),
                ],
            )
            .map_err(storage_failed)?;

        if context.replace_pinned_documents {
            let chat_id = current.chat_id.as_ref().ok_or_else(invariant_failed)?;
            transaction
                .execute(
                    "DELETE FROM chat_documents WHERE chat_id = ?1 AND pinned = 1",
                    params![chat_id.to_storage()],
                )
                .map_err(storage_failed)?;
        }
        for document in &context.pinned_documents {
            transaction
                .execute(
                    "INSERT INTO chat_documents (
                        chat_id, document_id, display_title, pinned,
                        first_used_at_ms, last_used_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(chat_id, document_id) DO UPDATE SET
                        display_title = excluded.display_title,
                        pinned = excluded.pinned,
                        last_used_at_ms = max(last_used_at_ms, excluded.last_used_at_ms)",
                    params![
                        document.chat_id.to_storage(),
                        document.document_id.to_storage(),
                        document.display_title,
                        document.pinned,
                        document.first_used_at.millis(),
                        document.last_used_at.millis(),
                    ],
                )
                .map_err(storage_failed)?;
        }
        for reference in &context.document_refs {
            transaction
                .execute(
                    "INSERT INTO message_document_refs (
                        message_id, document_id, relation, display_title,
                        source_revision, used_at_ms
                     ) VALUES (?1, ?2, 'context', ?3, ?4, ?5)",
                    params![
                        reference.message_id.to_storage(),
                        reference.document_id.to_storage(),
                        reference.display_title,
                        reference
                            .source_revision
                            .as_ref()
                            .map(|revision| &revision.0),
                        reference.used_at.millis(),
                    ],
                )
                .map_err(storage_failed)?;
        }
        for reference in &context.message_block_refs {
            transaction
                .execute(
                    "INSERT INTO message_block_refs (message_id, source_block_id, used_at_ms)
                     VALUES (?1, ?2, ?3)",
                    params![
                        reference.message_id.to_storage(),
                        reference.source_block_id.to_storage(),
                        reference.used_at.millis(),
                    ],
                )
                .map_err(storage_failed)?;
        }

        let updated = query_attempt(&transaction, "id", &context.attempt_id.to_storage())?
            .ok_or_else(storage_missing)?;
        transaction.commit().map_err(storage_failed)?;
        trace_attempt_transaction("commit_context", updated.id, "committed", started);
        Ok(updated)
    }

    fn apply_event(
        &mut self,
        event: &AiEvent,
        finalized_blocks: &[MessageBlock],
    ) -> Result<InferenceAttempt, AiError> {
        if !matches!(event, AiEvent::Completed { .. }) && !finalized_blocks.is_empty() {
            return Err(invariant_failed());
        }
        let started = Instant::now();
        let transaction = self.connection.transaction().map_err(storage_failed)?;
        let attempt_id = event.attempt_id();
        let current = query_attempt(&transaction, "id", &attempt_id.to_storage())?
            .ok_or_else(storage_missing)?;
        if current.status.is_terminal() {
            trace_attempt_transaction("apply_event", current.id, "terminal_noop", started);
            return Ok(current);
        }
        let assistant = current.assistant_message_id.map(MessageId::to_storage);

        match event {
            AiEvent::Started {
                provider_response_id,
                request_payload_bytes,
                resolved_model,
                effective_reasoning,
                at,
                ..
            } => {
                transaction
                    .execute(
                        "UPDATE inference_attempts
                            SET status = 'streaming', provider_started_at_ms =
                                    coalesce(provider_started_at_ms, ?2),
                                provider_response_id = ?3, resolved_model = ?4,
                                effective_reasoning = ?5,
                                request_payload_bytes = coalesce(?6, request_payload_bytes)
                          WHERE id = ?1",
                        params![
                            attempt_id.to_storage(),
                            at.millis(),
                            provider_response_id,
                            resolved_model,
                            effective_reasoning.map(ReasoningChoice::as_token),
                            optional_u64(*request_payload_bytes)?,
                        ],
                    )
                    .map_err(storage_failed)?;
                update_message_resolved_model(&transaction, assistant.as_deref(), resolved_model)?;
            }
            AiEvent::TextDelta { text, at, .. } => {
                transaction
                    .execute(
                        "UPDATE inference_attempts
                            SET status = 'streaming',
                                provider_started_at_ms = coalesce(provider_started_at_ms, ?2),
                                first_token_at_ms = coalesce(first_token_at_ms, ?2)
                          WHERE id = ?1",
                        params![attempt_id.to_storage(), at.millis()],
                    )
                    .map_err(storage_failed)?;
                if let Some(message) = assistant.as_deref() {
                    transaction
                        .execute(
                            "UPDATE messages SET markdown = markdown || ?2
                              WHERE id = ?1 AND state = 'pending'",
                            params![message, text],
                        )
                        .map_err(storage_failed)?;
                }
            }
            AiEvent::ReasoningDelta { at, .. } => {
                transaction
                    .execute(
                        "UPDATE inference_attempts
                            SET status = 'streaming',
                                provider_started_at_ms = coalesce(provider_started_at_ms, ?2)
                          WHERE id = ?1",
                        params![attempt_id.to_storage(), at.millis()],
                    )
                    .map_err(storage_failed)?;
            }
            AiEvent::Usage { usage, .. } => {
                transaction
                    .execute(
                        "UPDATE inference_attempts
                            SET prompt_tokens = ?2, cached_prompt_tokens = ?3,
                                completion_tokens = ?4, reasoning_tokens = ?5,
                                total_tokens = ?6, usage_source = ?7
                          WHERE id = ?1",
                        params![
                            attempt_id.to_storage(),
                            optional_u64(usage.prompt_tokens)?,
                            optional_u64(usage.cached_prompt_tokens)?,
                            optional_u64(usage.completion_tokens)?,
                            optional_u64(usage.reasoning_tokens)?,
                            optional_u64(usage.total_tokens)?,
                            usage_source_token(usage.source),
                        ],
                    )
                    .map_err(storage_failed)?;
            }
            AiEvent::ToolCalls { at, .. } => {
                // Tool-call details become their own sanitized durable records
                // in the workspace-tool migration. Until then this event keeps
                // the parent attempt active without writing arguments or
                // provider reasoning into ai.db.
                transaction
                    .execute(
                        "UPDATE inference_attempts
                            SET status = 'streaming',
                                provider_started_at_ms = coalesce(provider_started_at_ms, ?2)
                          WHERE id = ?1",
                        params![attempt_id.to_storage(), at.millis()],
                    )
                    .map_err(storage_failed)?;
            }
            AiEvent::ToolActivity { .. }
            | AiEvent::AgentActivity { .. }
            | AiEvent::Notice { .. } => {}
            AiEvent::WebSearch { phase, at, .. } => {
                transaction
                    .execute(
                        "UPDATE inference_attempts
                            SET status = 'streaming',
                                provider_started_at_ms = coalesce(provider_started_at_ms, ?2),
                                web_search_calls = web_search_calls + ?3
                          WHERE id = ?1",
                        params![
                            attempt_id.to_storage(),
                            at.millis(),
                            i64::from(matches!(phase, WebSearchPhase::Completed)),
                        ],
                    )
                    .map_err(storage_failed)?;
            }
            AiEvent::Citation { source, .. } => {
                if let Some(message) = assistant.as_deref() {
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO message_citations (message_id, ordinal, url, title)
                             SELECT ?1, COALESCE(MAX(ordinal) + 1, 0), ?2, ?3
                               FROM message_citations WHERE message_id = ?1",
                            params![message, source.url, source.title],
                        )
                        .map_err(storage_failed)?;
                }
            }
            AiEvent::Completed {
                stop_reason, at, ..
            } => {
                transaction
                    .execute(
                        "UPDATE inference_attempts
                            SET status = 'complete', completed_at_ms = ?2, stop_reason = ?3
                          WHERE id = ?1",
                        params![attempt_id.to_storage(), at.millis(), stop_reason],
                    )
                    .map_err(storage_failed)?;
                complete_message(
                    &transaction,
                    assistant.as_deref(),
                    MessageState::Complete,
                    None,
                    *at,
                )?;
                let Some(assistant) = current.assistant_message_id else {
                    if !finalized_blocks.is_empty() {
                        return Err(invariant_failed());
                    }
                    let updated = query_attempt(&transaction, "id", &attempt_id.to_storage())?
                        .ok_or_else(storage_missing)?;
                    transaction.commit().map_err(storage_failed)?;
                    trace_attempt_transaction(
                        ai_event_transaction_name(event),
                        updated.id,
                        "committed",
                        started,
                    );
                    return Ok(updated);
                };
                for block in finalized_blocks {
                    if block.message_id != assistant {
                        return Err(invariant_failed());
                    }
                    insert_block(&transaction, block)?;
                }
            }
            AiEvent::Failed { error, at, .. } => {
                transaction
                    .execute(
                        "UPDATE inference_attempts
                            SET status = 'failed', completed_at_ms = ?2, error_class = ?3
                          WHERE id = ?1",
                        params![
                            attempt_id.to_storage(),
                            at.millis(),
                            error_class_token(error.class)
                        ],
                    )
                    .map_err(storage_failed)?;
                complete_message(
                    &transaction,
                    assistant.as_deref(),
                    MessageState::Failed,
                    Some(error.class),
                    *at,
                )?;
            }
            AiEvent::Cancelled { at, .. } => {
                transaction
                    .execute(
                        "UPDATE inference_attempts
                            SET status = 'cancelled', completed_at_ms = ?2,
                                error_class = 'cancelled'
                          WHERE id = ?1",
                        params![attempt_id.to_storage(), at.millis()],
                    )
                    .map_err(storage_failed)?;
                complete_message(
                    &transaction,
                    assistant.as_deref(),
                    MessageState::Interrupted,
                    Some(AiErrorClass::Cancelled),
                    *at,
                )?;
            }
        }

        let updated = query_attempt(&transaction, "id", &attempt_id.to_storage())?
            .ok_or_else(storage_missing)?;
        transaction.commit().map_err(storage_failed)?;
        trace_attempt_transaction(
            ai_event_transaction_name(event),
            updated.id,
            "committed",
            started,
        );
        Ok(updated)
    }

    fn recover_interrupted(&mut self, now: Timestamp) -> Result<RecoveryCounts, AiError> {
        let started = Instant::now();
        let transaction = self.connection.transaction().map_err(storage_failed)?;
        let attempts = transaction
            .execute(
                "UPDATE inference_attempts
                    SET status = 'interrupted',
                        completed_at_ms = max(queued_at_ms, ?1)
                  WHERE status IN ('pending', 'streaming')",
                [now.millis()],
            )
            .map_err(storage_failed)?;
        let messages = transaction
            .execute(
                "UPDATE messages
                    SET state = 'interrupted',
                        completed_at_ms = max(created_at_ms, ?1)
                  WHERE role = 'assistant' AND state = 'pending'",
                [now.millis()],
            )
            .map_err(storage_failed)?;
        transaction
            .execute(
                "UPDATE tool_invocations
                    SET status = 'cancelled', approval = CASE
                            WHEN approval = 'pending' THEN 'denied' ELSE approval END,
                        completed_at_ms = max(proposed_at_ms, ?1),
                        error_code = 'cancelled',
                        error_message = 'The workspace tool was cancelled.'
                  WHERE status IN ('proposed', 'approval_required', 'running')",
                [now.millis()],
            )
            .map_err(storage_failed)?;
        transaction.commit().map_err(storage_failed)?;
        tracing::debug!(
            target: "koru_ai::sqlite",
            transaction = "recover_interrupted",
            attempts,
            messages,
            elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
            "AI SQLite transaction committed"
        );
        Ok(RecoveryCounts {
            attempts: attempts as u64,
            messages: messages as u64,
        })
    }
}

fn trace_attempt_transaction(
    operation: &'static str,
    attempt: InferenceAttemptId,
    outcome: &'static str,
    started: Instant,
) {
    tracing::debug!(
        target: "koru_ai::sqlite",
        transaction = operation,
        attempt = %attempt,
        outcome,
        elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
        "AI SQLite transaction completed"
    );
}

fn ai_event_transaction_name(event: &AiEvent) -> &'static str {
    match event {
        AiEvent::Started { .. } => "apply_started",
        AiEvent::TextDelta { .. } => "apply_coalesced_text",
        AiEvent::ReasoningDelta { .. } => "apply_coalesced_reasoning",
        AiEvent::Usage { .. } => "apply_usage",
        AiEvent::ToolCalls { .. } => "apply_tool_calls",
        AiEvent::ToolActivity { .. } => "apply_tool_activity",
        AiEvent::AgentActivity { .. } => "apply_agent_activity",
        AiEvent::Notice { .. } => "apply_notice",
        AiEvent::WebSearch { .. } => "apply_web_search",
        AiEvent::Citation { .. } => "apply_citation",
        AiEvent::Completed { .. } => "apply_completed",
        AiEvent::Failed { .. } => "apply_failed",
        AiEvent::Cancelled { .. } => "apply_cancelled",
    }
}

fn settings_from_row(row: &Row<'_>) -> rusqlite::Result<AiSettings> {
    let selected_provider = row
        .get::<_, Option<String>>(0)?
        .map(|value| AiProviderId::parse(value).map_err(|_| decode_failed(0, "provider id")))
        .transpose()?;
    let openai_reasoning = row.get::<_, String>(3)?;
    let ollama_endpoint = row.get::<_, String>(4)?;
    let ollama_reasoning = row.get::<_, String>(6)?;
    Ok(AiSettings {
        selected_provider,
        openai: OpenAiSettings {
            model: row.get(1)?,
            key_account: row.get(2)?,
            reasoning: ReasoningChoice::parse(&openai_reasoning)
                .ok_or_else(|| decode_failed(3, "OpenAI reasoning"))?,
        },
        ollama: OllamaSettings {
            endpoint: url::Url::parse(&ollama_endpoint)
                .map_err(|_| decode_failed(4, "Ollama endpoint"))?,
            model: row.get(5)?,
            reasoning: ReasoningChoice::parse(&ollama_reasoning)
                .ok_or_else(|| decode_failed(6, "Ollama reasoning"))?,
        },
        created_at: Timestamp::from_millis(row.get(7)?),
        updated_at: Timestamp::from_millis(row.get(8)?),
    })
}

fn validate_settings(settings: &AiSettings) -> Result<(), AiError> {
    if settings.updated_at < settings.created_at
        || settings.selected_provider.as_ref().is_some_and(|provider| {
            // The three built-in providers, or a local agent (`agent-<kind>`,
            // AGENTUX): the worker admits only the agents it registered, so
            // the row's job is the shape, not the catalogue.
            !matches!(
                provider.as_str(),
                AiProviderId::OPENAI | AiProviderId::OLLAMA | AiProviderId::KORU
            ) && !provider
                .as_str()
                .strip_prefix("agent-")
                .is_some_and(|kind| !kind.is_empty())
        })
        || settings
            .openai
            .model
            .as_deref()
            .is_some_and(invalid_setting_text)
        || settings
            .openai
            .key_account
            .as_deref()
            .is_some_and(invalid_setting_text)
        || settings
            .ollama
            .model
            .as_deref()
            .is_some_and(invalid_setting_text)
        || !matches!(settings.ollama.endpoint.scheme(), "http" | "https")
        || settings.ollama.endpoint.host_str().is_none()
        || settings.ollama.endpoint.username() != ""
        || settings.ollama.endpoint.password().is_some()
        || settings.ollama.endpoint.query().is_some()
        || settings.ollama.endpoint.fragment().is_some()
    {
        return Err(invariant_failed());
    }
    Ok(())
}

fn invalid_setting_text(value: &str) -> bool {
    value.trim().is_empty() || value.len() > 500 || value.chars().any(char::is_control)
}

const ATTEMPT_COLUMNS: &str = "id, operation_id, chat_id, assistant_message_id,
    surface, intent, provider, requested_model, resolved_model, requested_reasoning,
    effective_reasoning, prompt_template_version, status, queued_at_ms,
    context_started_at_ms, context_completed_at_ms, provider_started_at_ms,
    first_token_at_ms, completed_at_ms, request_payload_bytes, context_payload_bytes,
    message_count, context_document_count, context_message_block_count, prompt_tokens,
    cached_prompt_tokens, completion_tokens, reasoning_tokens, total_tokens, usage_source,
    stop_reason, error_class, provider_response_id, web_search_requested, web_search_calls";

fn query_chat(connection: &Connection, id: ChatId) -> Result<Option<Chat>, AiError> {
    connection
        .query_row(
            "SELECT id, title, current_provider, current_model, current_reasoning,
                    branched_from_chat_id, branched_from_message_id, created_at_ms, updated_at_ms,
                    private, agent_working_folder, agent_mode, agent_fast
               FROM chats WHERE id = ?1",
            [id.to_storage()],
            chat_from_row,
        )
        .optional()
        .map_err(storage_failed)
}

fn query_chat_tool_policy(
    connection: &Connection,
    chat: ChatId,
) -> Result<Option<ChatToolPolicy>, AiError> {
    let mode = connection
        .query_row(
            "SELECT tool_mode FROM chats WHERE id = ?1",
            [chat.to_storage()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(storage_failed)?;
    let Some(mode) = mode else {
        return Ok(None);
    };
    let mode = parse_tool_mode(&mode).ok_or_else(invariant_failed)?;
    let mut statement = connection
        .prepare(
            "SELECT workspace_id
               FROM chat_workspace_grants
              WHERE chat_id = ?1
              ORDER BY workspace_id",
        )
        .map_err(storage_failed)?;
    let rows = statement
        .query_map([chat.to_storage()], |row| {
            required_id(row, 0, WorkspaceId::parse, "WorkspaceId")
        })
        .map_err(storage_failed)?;
    Ok(Some(ChatToolPolicy {
        chat_id: chat,
        mode,
        workspace_ids: collect_rows(rows)?,
    }))
}

const TOOL_ACTIVITY_COLUMNS: &str = "id, attempt_id, chat_id, tool_name, read_only,
    status, approval, proposed_at_ms, started_at_ms, completed_at_ms,
    result_bytes, error_code, target_workspace_id, target_document_id, error_message,
    agent_label, agent_summary, agent_input, agent_output,
    (SELECT assistant_message_id FROM inference_attempts
      WHERE id = tool_invocations.attempt_id),
    text_offset";

fn query_tool_activity(
    connection: &Connection,
    id: ToolInvocationId,
) -> Result<Option<ToolActivity>, AiError> {
    connection
        .query_row(
            &format!("SELECT {TOOL_ACTIVITY_COLUMNS} FROM tool_invocations WHERE id = ?1"),
            [id.to_storage()],
            tool_activity_from_row,
        )
        .optional()
        .map_err(storage_failed)
}

fn insert_chat(connection: &Connection, chat: &Chat) -> Result<(), AiError> {
    let (branch_chat, branch_message) =
        chat.branched_from.as_ref().map_or((None, None), |origin| {
            (
                Some(origin.chat_id.to_storage()),
                Some(origin.through_message_id.to_storage()),
            )
        });
    connection
        .execute(
            "INSERT INTO chats (
            id, title, current_provider, current_model, current_reasoning,
            branched_from_chat_id, branched_from_message_id, created_at_ms, updated_at_ms, private,
            agent_working_folder, agent_mode, agent_fast
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                chat.id.to_storage(),
                chat.title,
                chat.current_model.provider.as_str(),
                chat.current_model.requested_model,
                chat.current_model.default_reasoning.as_token(),
                branch_chat,
                branch_message,
                chat.created_at.millis(),
                chat.updated_at.millis(),
                chat.private,
                chat.agent_working_folder,
                chat.agent_mode,
                chat.agent_fast,
            ],
        )
        .map_err(storage_failed)?;
    Ok(())
}

const EXPORT_COLUMNS: &str = "id, idempotency_key, chat_id, through_message_id,
    destination_profile_id, destination_workspace_id, destination_parent_id,
    destination_index, destination_title, status, document_id, error_class,
    created_at_ms, completed_at_ms";

fn query_export(
    connection: &Connection,
    column: &'static str,
    value: &str,
) -> Result<Option<DocumentExport>, AiError> {
    debug_assert!(matches!(column, "id" | "idempotency_key"));
    let sql = format!("SELECT {EXPORT_COLUMNS} FROM document_exports WHERE {column} = ?1");
    connection
        .query_row(&sql, [value], export_from_row)
        .optional()
        .map_err(storage_failed)
}

fn query_attempt(
    connection: &Connection,
    column: &'static str,
    value: &str,
) -> Result<Option<InferenceAttempt>, AiError> {
    debug_assert!(matches!(column, "id" | "operation_id"));
    let sql = format!("SELECT {ATTEMPT_COLUMNS} FROM inference_attempts WHERE {column} = ?1");
    connection
        .query_row(&sql, [value], attempt_from_row)
        .optional()
        .map_err(storage_failed)
}

const USAGE_TOTAL_COLUMNS: &str = "
    COUNT(*),
    COALESCE(SUM(CASE WHEN attempt.usage_source = 'provider_reported' THEN 1 ELSE 0 END), 0),
    SUM(attempt.request_payload_bytes),
    COALESCE(SUM(attempt.context_payload_bytes), 0),
    COALESCE(SUM(attempt.message_count), 0),
    COALESCE(SUM(attempt.context_document_count), 0),
    COALESCE(SUM(attempt.context_message_block_count), 0),
    SUM(attempt.prompt_tokens),
    SUM(attempt.cached_prompt_tokens),
    SUM(attempt.completion_tokens),
    SUM(attempt.reasoning_tokens),
    SUM(attempt.total_tokens),
    CAST(AVG(attempt.context_assembly_ms) AS INTEGER),
    CAST(AVG(attempt.provider_start_ms) AS INTEGER),
    CAST(AVG(attempt.time_to_first_token_ms) AS INTEGER),
    CAST(AVG(attempt.generation_ms) AS INTEGER),
    CAST(AVG(attempt.total_ms) AS INTEGER),
    COALESCE(SUM(CASE WHEN feedback.value = 'good' THEN 1 ELSE 0 END), 0),
    COALESCE(SUM(CASE WHEN feedback.value = 'bad' THEN 1 ELSE 0 END), 0),
    COALESCE(SUM(raw.web_search_calls), 0)";

fn query_usage_totals(connection: &Connection, chat: ChatId) -> Result<UsageTotals, AiError> {
    let sql = format!(
        "SELECT {USAGE_TOTAL_COLUMNS}
           FROM inference_attempt_durations AS attempt
           JOIN inference_attempts AS raw ON raw.id = attempt.id
           LEFT JOIN current_message_feedback AS feedback
             ON feedback.attempt_id = attempt.id
          WHERE attempt.chat_id = ?1"
    );
    connection
        .query_row(&sql, [chat.to_storage()], |row| {
            usage_totals_from_row(row, 0)
        })
        .map_err(storage_failed)
}

fn query_usage_groups(connection: &Connection) -> Result<Vec<UsageGroup>, AiError> {
    let sql = format!(
        "SELECT date(attempt.completed_at_ms / 1000, 'unixepoch'),
                attempt.surface,
                attempt.provider,
                COALESCE(attempt.resolved_model, attempt.requested_model),
                COALESCE(attempt.effective_reasoning, attempt.requested_reasoning),
                attempt.status,
                attempt.prompt_template_version,
                {USAGE_TOTAL_COLUMNS}
           FROM inference_attempt_durations AS attempt
           JOIN inference_attempts AS raw ON raw.id = attempt.id
           LEFT JOIN current_message_feedback AS feedback
             ON feedback.attempt_id = attempt.id
          WHERE attempt.completed_at_ms IS NOT NULL
          GROUP BY 1, 2, 3, 4, 5, 6, 7
          ORDER BY 1 DESC, 2, 3, 4, 5, 6, 7"
    );
    let mut statement = connection.prepare(&sql).map_err(storage_failed)?;
    let rows = statement
        .query_map([], |row| {
            let surface = row.get::<_, String>(1)?;
            let status = row.get::<_, String>(5)?;
            Ok(UsageGroup {
                utc_day: row.get(0)?,
                surface: parse_surface(&surface).ok_or_else(|| decode_failed(1, "AI surface"))?,
                provider: provider(row, 2)?,
                model: row.get(3)?,
                reasoning: reasoning(row, 4)?,
                status: parse_attempt_status(&status)
                    .ok_or_else(|| decode_failed(5, "attempt status"))?,
                prompt_template_version: row.get(6)?,
                totals: usage_totals_from_row(row, 7)?,
            })
        })
        .map_err(storage_failed)?;
    collect_rows(rows)
}

fn usage_totals_from_row(row: &Row<'_>, base: usize) -> rusqlite::Result<UsageTotals> {
    Ok(UsageTotals {
        attempts: nonnegative_u64(row, base)?,
        attempts_with_reported_usage: nonnegative_u64(row, base + 1)?,
        request_payload_bytes: optional_nonnegative_u64(row, base + 2)?,
        context_payload_bytes: nonnegative_u64(row, base + 3)?,
        message_count: nonnegative_u64(row, base + 4)?,
        context_document_count: nonnegative_u64(row, base + 5)?,
        context_message_block_count: nonnegative_u64(row, base + 6)?,
        prompt_tokens: optional_nonnegative_u64(row, base + 7)?,
        cached_prompt_tokens: optional_nonnegative_u64(row, base + 8)?,
        completion_tokens: optional_nonnegative_u64(row, base + 9)?,
        reasoning_tokens: optional_nonnegative_u64(row, base + 10)?,
        total_tokens: optional_nonnegative_u64(row, base + 11)?,
        average_context_assembly_ms: optional_nonnegative_u64(row, base + 12)?,
        average_provider_start_ms: optional_nonnegative_u64(row, base + 13)?,
        average_time_to_first_token_ms: optional_nonnegative_u64(row, base + 14)?,
        average_generation_ms: optional_nonnegative_u64(row, base + 15)?,
        average_total_ms: optional_nonnegative_u64(row, base + 16)?,
        good_feedback: nonnegative_u64(row, base + 17)?,
        bad_feedback: nonnegative_u64(row, base + 18)?,
        web_search_calls: nonnegative_u64(row, base + 19)?,
    })
}

fn insert_message(connection: &Connection, message: &Message) -> Result<(), AiError> {
    connection
        .execute(
            "INSERT INTO messages (
                id, chat_id, ordinal, role, state, markdown, origin_message_id,
                provider, requested_model, resolved_model, error_class,
                created_at_ms, completed_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                message.id.to_storage(),
                message.chat_id.to_storage(),
                i64::from(message.ordinal),
                role_token(message.role),
                message_state_token(message.state),
                message.markdown,
                message.origin_message_id.map(MessageId::to_storage),
                message.provider.as_ref().map(AiProviderId::as_str),
                message.requested_model,
                message.resolved_model,
                message.error_class.map(error_class_token),
                message.created_at.millis(),
                message.completed_at.map(Timestamp::millis),
            ],
        )
        .map_err(storage_failed)?;
    Ok(())
}

fn insert_attempt(connection: &Connection, attempt: &InferenceAttempt) -> Result<(), AiError> {
    connection
        .execute(
            "INSERT INTO inference_attempts (
                id, operation_id, chat_id, assistant_message_id, surface, intent, provider,
                requested_model, resolved_model, requested_reasoning, effective_reasoning,
                prompt_template_version, status, queued_at_ms, context_started_at_ms,
                context_completed_at_ms, provider_started_at_ms, first_token_at_ms,
                completed_at_ms, request_payload_bytes, context_payload_bytes, message_count,
                context_document_count, context_message_block_count, prompt_tokens,
                cached_prompt_tokens, completion_tokens, reasoning_tokens, total_tokens,
                usage_source, stop_reason, error_class, provider_response_id,
                web_search_requested, web_search_calls
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26,
                ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35
             )",
            params![
                attempt.id.to_storage(),
                attempt.operation_id.to_string(),
                attempt.chat_id.map(ChatId::to_storage),
                attempt.assistant_message_id.map(MessageId::to_storage),
                surface_token(attempt.surface),
                intent_token(attempt.intent),
                attempt.provider.as_str(),
                attempt.requested_model,
                attempt.resolved_model,
                attempt.requested_reasoning.as_token(),
                attempt.effective_reasoning.map(ReasoningChoice::as_token),
                attempt.prompt_template_version,
                attempt_status_token(attempt.status),
                attempt.queued_at.millis(),
                attempt.context_started_at.map(Timestamp::millis),
                attempt.context_completed_at.map(Timestamp::millis),
                attempt.provider_started_at.map(Timestamp::millis),
                attempt.first_token_at.map(Timestamp::millis),
                attempt.completed_at.map(Timestamp::millis),
                optional_u64(attempt.request_payload_bytes)?,
                required_u64(attempt.context_payload_bytes)?,
                i64::from(attempt.message_count),
                i64::from(attempt.context_document_count),
                i64::from(attempt.context_message_block_count),
                optional_u64(attempt.usage.prompt_tokens)?,
                optional_u64(attempt.usage.cached_prompt_tokens)?,
                optional_u64(attempt.usage.completion_tokens)?,
                optional_u64(attempt.usage.reasoning_tokens)?,
                optional_u64(attempt.usage.total_tokens)?,
                usage_source_token(attempt.usage.source),
                attempt.stop_reason,
                attempt.error_class.map(error_class_token),
                attempt.provider_response_id,
                attempt.web_search_requested,
                i64::from(attempt.web_search_calls),
            ],
        )
        .map_err(storage_failed)?;
    Ok(())
}

fn insert_block(connection: &Connection, block: &MessageBlock) -> Result<(), AiError> {
    connection
        .execute(
            "INSERT INTO message_blocks
                (id, message_id, ordinal, kind, markdown, plain_text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                block.id.to_storage(),
                block.message_id.to_storage(),
                i64::from(block.ordinal),
                block_kind_token(block.kind),
                block.markdown,
                block.plain_text,
            ],
        )
        .map_err(storage_failed)?;
    Ok(())
}

fn validate_turn(turn: &PendingTurn) -> Result<(), AiError> {
    let user = &turn.user_message;
    let assistant = &turn.assistant_message;
    let attempt = &turn.attempt;
    if user.chat_id != assistant.chat_id
        || attempt.chat_id != Some(user.chat_id)
        || attempt.assistant_message_id != Some(assistant.id)
        || user.role != ChatRole::User
        || user.state != MessageState::Complete
        || assistant.role != ChatRole::Assistant
        || assistant.state != MessageState::Pending
        || assistant.ordinal != user.ordinal.saturating_add(1)
        || attempt.status != AttemptStatus::Pending
        || attempt.requested_model.is_empty()
        || attempt.prompt_template_version.is_empty()
    {
        return Err(invariant_failed());
    }
    Ok(())
}

fn complete_message(
    connection: &Connection,
    message: Option<&str>,
    state: MessageState,
    error_class: Option<AiErrorClass>,
    at: Timestamp,
) -> Result<(), AiError> {
    if let Some(message) = message {
        connection
            .execute(
                "UPDATE messages
                    SET state = ?2, completed_at_ms = ?3, error_class = ?4
                  WHERE id = ?1 AND state = 'pending'",
                params![
                    message,
                    message_state_token(state),
                    at.millis(),
                    error_class.map(error_class_token)
                ],
            )
            .map_err(storage_failed)?;
    }
    Ok(())
}

fn update_message_resolved_model(
    connection: &Connection,
    message: Option<&str>,
    resolved_model: &Option<String>,
) -> Result<(), AiError> {
    if let Some(message) = message {
        connection
            .execute(
                "UPDATE messages SET resolved_model = ?2
                  WHERE id = ?1 AND state = 'pending'",
                params![message, resolved_model],
            )
            .map_err(storage_failed)?;
    }
    Ok(())
}

fn chat_from_row(row: &Row<'_>) -> rusqlite::Result<Chat> {
    let branch_chat = optional_id(row, 5, ChatId::parse, "ChatId")?;
    let branch_message = optional_id(row, 6, MessageId::parse, "MessageId")?;
    let branched_from = match (branch_chat, branch_message) {
        (None, None) => None,
        (Some(chat_id), Some(through_message_id)) => Some(BranchOrigin {
            chat_id,
            through_message_id,
        }),
        _ => return Err(decode_failed(5, "incomplete branch origin")),
    };
    Ok(Chat {
        id: required_id(row, 0, ChatId::parse, "ChatId")?,
        title: row.get(1)?,
        private: row.get(9)?,
        current_model: ModelSelection {
            provider: provider(row, 2)?,
            requested_model: row.get(3)?,
            default_reasoning: reasoning(row, 4)?,
        },
        branched_from,
        agent_working_folder: row.get(10)?,
        agent_mode: row.get(11)?,
        agent_fast: row.get(12)?,
        created_at: Timestamp::from_millis(row.get(7)?),
        updated_at: Timestamp::from_millis(row.get(8)?),
    })
}

fn message_from_row(row: &Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        id: required_id(row, 0, MessageId::parse, "MessageId")?,
        chat_id: required_id(row, 1, ChatId::parse, "ChatId")?,
        ordinal: nonnegative_u32(row, 2)?,
        role: parse_role(&row.get::<_, String>(3)?).ok_or_else(|| decode_failed(3, "role"))?,
        state: parse_message_state(&row.get::<_, String>(4)?)
            .ok_or_else(|| decode_failed(4, "message state"))?,
        markdown: row.get(5)?,
        origin_message_id: optional_id(row, 6, MessageId::parse, "MessageId")?,
        provider: optional_provider(row, 7)?,
        requested_model: row.get(8)?,
        resolved_model: row.get(9)?,
        error_class: optional_error_class(row, 10)?,
        created_at: Timestamp::from_millis(row.get(11)?),
        completed_at: row.get::<_, Option<i64>>(12)?.map(Timestamp::from_millis),
    })
}

fn block_from_row(row: &Row<'_>) -> rusqlite::Result<MessageBlock> {
    let kind = row.get::<_, String>(3)?;
    Ok(MessageBlock {
        id: required_id(row, 0, MessageBlockId::parse, "MessageBlockId")?,
        message_id: required_id(row, 1, MessageId::parse, "MessageId")?,
        ordinal: nonnegative_u32(row, 2)?,
        kind: parse_block_kind(&kind).ok_or_else(|| decode_failed(3, "message block kind"))?,
        markdown: row.get(4)?,
        plain_text: row.get(5)?,
    })
}

fn feedback_from_row(row: &Row<'_>) -> rusqlite::Result<FeedbackEvent> {
    Ok(FeedbackEvent {
        id: required_id(row, 0, FeedbackEventId::parse, "FeedbackEventId")?,
        chat_id: optional_id(row, 1, ChatId::parse, "ChatId")?,
        message_id: optional_id(row, 2, MessageId::parse, "MessageId")?,
        attempt_id: optional_id(row, 3, InferenceAttemptId::parse, "InferenceAttemptId")?,
        value: parse_feedback(&row.get::<_, String>(4)?)
            .ok_or_else(|| decode_failed(4, "feedback value"))?,
        surface: parse_surface(&row.get::<_, String>(5)?)
            .ok_or_else(|| decode_failed(5, "AI surface"))?,
        created_at: Timestamp::from_millis(row.get(6)?),
    })
}

fn export_from_row(row: &Row<'_>) -> rusqlite::Result<DocumentExport> {
    let key = row.get::<_, String>(1)?;
    let index = row
        .get::<_, Option<i64>>(7)?
        .map(|value| usize::try_from(value).map_err(|_| decode_failed(7, "destination index")))
        .transpose()?;
    Ok(DocumentExport {
        id: required_id(row, 0, DocumentExportId::parse, "DocumentExportId")?,
        idempotency_key: Uuid::parse_str(&key).map_err(|_| decode_failed(1, "idempotency UUID"))?,
        chat_id: required_id(row, 2, ChatId::parse, "ChatId")?,
        through_message_id: required_id(row, 3, MessageId::parse, "MessageId")?,
        destination: DocumentDestination {
            profile_id: required_id(row, 4, koru_ai_core::ProfileId::parse, "ProfileId")?,
            workspace_id: required_id(row, 5, koru_ai_core::WorkspaceId::parse, "WorkspaceId")?,
            parent_document_id: optional_id(row, 6, koru_ai_core::DocumentId::parse, "DocumentId")?,
            index,
            title: row.get(8)?,
        },
        status: parse_export_status(&row.get::<_, String>(9)?)
            .ok_or_else(|| decode_failed(9, "export status"))?,
        document_id: optional_id(row, 10, koru_ai_core::DocumentId::parse, "DocumentId")?,
        error_class: optional_error_class(row, 11)?,
        created_at: Timestamp::from_millis(row.get(12)?),
        completed_at: optional_timestamp(row, 13)?,
    })
}

fn attempt_from_row(row: &Row<'_>) -> rusqlite::Result<InferenceAttempt> {
    let operation = row.get::<_, String>(1)?;
    Ok(InferenceAttempt {
        id: required_id(row, 0, InferenceAttemptId::parse, "InferenceAttemptId")?,
        operation_id: Uuid::parse_str(&operation)
            .map_err(|_| decode_failed(1, "operation UUID"))?,
        chat_id: optional_id(row, 2, ChatId::parse, "ChatId")?,
        assistant_message_id: optional_id(row, 3, MessageId::parse, "MessageId")?,
        surface: parse_surface(&row.get::<_, String>(4)?)
            .ok_or_else(|| decode_failed(4, "AI surface"))?,
        intent: parse_intent(&row.get::<_, String>(5)?)
            .ok_or_else(|| decode_failed(5, "AI intent"))?,
        provider: provider(row, 6)?,
        requested_model: row.get(7)?,
        resolved_model: row.get(8)?,
        requested_reasoning: reasoning(row, 9)?,
        effective_reasoning: optional_reasoning(row, 10)?,
        prompt_template_version: row.get(11)?,
        status: parse_attempt_status(&row.get::<_, String>(12)?)
            .ok_or_else(|| decode_failed(12, "attempt status"))?,
        queued_at: Timestamp::from_millis(row.get(13)?),
        context_started_at: optional_timestamp(row, 14)?,
        context_completed_at: optional_timestamp(row, 15)?,
        provider_started_at: optional_timestamp(row, 16)?,
        first_token_at: optional_timestamp(row, 17)?,
        completed_at: optional_timestamp(row, 18)?,
        request_payload_bytes: optional_nonnegative_u64(row, 19)?,
        web_search_requested: row.get(33)?,
        web_search_calls: nonnegative_u32(row, 34)?,
        context_payload_bytes: nonnegative_u64(row, 20)?,
        message_count: nonnegative_u32(row, 21)?,
        context_document_count: nonnegative_u32(row, 22)?,
        context_message_block_count: nonnegative_u32(row, 23)?,
        usage: AiUsage {
            prompt_tokens: optional_nonnegative_u64(row, 24)?,
            cached_prompt_tokens: optional_nonnegative_u64(row, 25)?,
            completion_tokens: optional_nonnegative_u64(row, 26)?,
            reasoning_tokens: optional_nonnegative_u64(row, 27)?,
            total_tokens: optional_nonnegative_u64(row, 28)?,
            source: optional_usage_source(row, 29)?.unwrap_or(UsageSource::Unavailable),
        },
        stop_reason: row.get(30)?,
        error_class: optional_error_class(row, 31)?,
        provider_response_id: row.get(32)?,
    })
}

fn tool_activity_from_row(row: &Row<'_>) -> rusqlite::Result<ToolActivity> {
    Ok(ToolActivity {
        id: required_id(row, 0, ToolInvocationId::parse, "ToolInvocationId")?,
        attempt_id: required_id(row, 1, InferenceAttemptId::parse, "InferenceAttemptId")?,
        chat_id: required_id(row, 2, ChatId::parse, "ChatId")?,
        assistant_message_id: required_id(row, 19, MessageId::parse, "MessageId")?,
        tool_name: row.get(3)?,
        read_only: row.get(4)?,
        status: parse_tool_activity_status(&row.get::<_, String>(5)?)
            .ok_or_else(|| decode_failed(5, "tool activity status"))?,
        approval: parse_tool_approval(&row.get::<_, String>(6)?)
            .ok_or_else(|| decode_failed(6, "tool approval"))?,
        proposed_at: Timestamp::from_millis(row.get(7)?),
        started_at: optional_timestamp(row, 8)?,
        completed_at: optional_timestamp(row, 9)?,
        result_bytes: optional_nonnegative_u64(row, 10)?,
        error_code: row
            .get::<_, Option<String>>(11)?
            .map(|value| {
                parse_tool_error_code(&value).ok_or_else(|| decode_failed(11, "tool error code"))
            })
            .transpose()?,
        target_workspace_id: optional_id(row, 12, WorkspaceId::parse, "WorkspaceId")?,
        target_document_id: optional_id(row, 13, DocumentId::parse, "DocumentId")?,
        error_message: row.get(14)?,
        agent_label: row.get(15)?,
        agent_summary: row.get(16)?,
        agent_input: row.get(17)?,
        agent_output: row.get(18)?,
        text_offset: row
            .get::<_, Option<i64>>(20)?
            .map(|offset| u32::try_from(offset).map_err(|_| decode_failed(20, "text offset")))
            .transpose()?,
        display_target: None,
        live: None,
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>, AiError> {
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage_failed)
}

fn required_id<T>(
    row: &Row<'_>,
    index: usize,
    parse: fn(&str) -> Option<T>,
    name: &'static str,
) -> rusqlite::Result<T> {
    let value = row.get::<_, String>(index)?;
    parse(&value).ok_or_else(|| decode_failed(index, name))
}

fn optional_id<T>(
    row: &Row<'_>,
    index: usize,
    parse: fn(&str) -> Option<T>,
    name: &'static str,
) -> rusqlite::Result<Option<T>> {
    row.get::<_, Option<String>>(index)?
        .map(|value| parse(&value).ok_or_else(|| decode_failed(index, name)))
        .transpose()
}

fn provider(row: &Row<'_>, index: usize) -> rusqlite::Result<AiProviderId> {
    let value = row.get::<_, String>(index)?;
    AiProviderId::parse(value).map_err(|_| decode_failed(index, "provider id"))
}

fn optional_provider(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<AiProviderId>> {
    row.get::<_, Option<String>>(index)?
        .map(|value| AiProviderId::parse(value).map_err(|_| decode_failed(index, "provider id")))
        .transpose()
}

fn reasoning(row: &Row<'_>, index: usize) -> rusqlite::Result<ReasoningChoice> {
    let value = row.get::<_, String>(index)?;
    ReasoningChoice::parse(&value).ok_or_else(|| decode_failed(index, "reasoning choice"))
}

fn optional_reasoning(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<ReasoningChoice>> {
    row.get::<_, Option<String>>(index)?
        .map(|value| {
            ReasoningChoice::parse(&value).ok_or_else(|| decode_failed(index, "reasoning choice"))
        })
        .transpose()
}

fn optional_error_class(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<AiErrorClass>> {
    row.get::<_, Option<String>>(index)?
        .map(|value| {
            parse_error_class(&value).ok_or_else(|| decode_failed(index, "AI error class"))
        })
        .transpose()
}

fn optional_usage_source(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<UsageSource>> {
    row.get::<_, Option<String>>(index)?
        .map(|value| parse_usage_source(&value).ok_or_else(|| decode_failed(index, "usage source")))
        .transpose()
}

fn optional_timestamp(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<Timestamp>> {
    Ok(row
        .get::<_, Option<i64>>(index)?
        .map(Timestamp::from_millis))
}

fn nonnegative_u32(row: &Row<'_>, index: usize) -> rusqlite::Result<u32> {
    let value = row.get::<_, i64>(index)?;
    u32::try_from(value).map_err(|_| decode_failed(index, "non-negative u32"))
}

fn nonnegative_u64(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value = row.get::<_, i64>(index)?;
    u64::try_from(value).map_err(|_| decode_failed(index, "non-negative u64"))
}

fn optional_nonnegative_u64(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)?
        .map(|value| u64::try_from(value).map_err(|_| decode_failed(index, "non-negative u64")))
        .transpose()
}

fn required_u64(value: u64) -> Result<i64, AiError> {
    i64::try_from(value).map_err(|_| invariant_failed())
}

fn optional_u64(value: Option<u64>) -> Result<Option<i64>, AiError> {
    value
        .map(|value| i64::try_from(value).map_err(|_| invariant_failed()))
        .transpose()
}

#[derive(Debug)]
struct DecodeError(&'static str);

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid persisted {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

fn decode_failed(index: usize, field: &'static str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(DecodeError(field)))
}

fn storage_failed(_: rusqlite::Error) -> AiError {
    AiError::new(
        AiErrorClass::Storage,
        "AI history could not be updated.",
        true,
    )
}

fn storage_missing() -> AiError {
    AiError::new(
        AiErrorClass::Storage,
        "The AI attempt is no longer available.",
        false,
    )
}

fn invariant_failed() -> AiError {
    AiError::new(
        AiErrorClass::Storage,
        "The AI operation could not be saved.",
        false,
    )
}

fn fts_query(text: &str) -> String {
    text.split_whitespace()
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn highlighted_snippet(marked: &str) -> (String, Vec<AiSearchSpan>) {
    let mut snippet = String::with_capacity(marked.len());
    let mut spans = Vec::new();
    let mut start = None;
    for character in marked.chars() {
        match character {
            '\u{1}' => start = Some(snippet.len()),
            '\u{2}' => {
                if let Some(start) = start.take()
                    && start < snippet.len()
                {
                    spans.push(AiSearchSpan {
                        start,
                        end: snippet.len(),
                    });
                }
            }
            _ => snippet.push(character),
        }
    }
    (snippet, spans)
}

fn tool_mode_token(value: ToolMode) -> &'static str {
    match value {
        ToolMode::Manual => "manual",
        ToolMode::Auto => "auto",
    }
}

fn parse_tool_mode(value: &str) -> Option<ToolMode> {
    match value {
        "manual" => Some(ToolMode::Manual),
        "auto" => Some(ToolMode::Auto),
        _ => None,
    }
}

fn tool_activity_status_token(value: ToolActivityStatus) -> &'static str {
    match value {
        ToolActivityStatus::Proposed => "proposed",
        ToolActivityStatus::ApprovalRequired => "approval_required",
        ToolActivityStatus::Running => "running",
        ToolActivityStatus::Complete => "complete",
        ToolActivityStatus::Denied => "denied",
        ToolActivityStatus::Failed => "failed",
        ToolActivityStatus::Cancelled => "cancelled",
    }
}

fn parse_tool_activity_status(value: &str) -> Option<ToolActivityStatus> {
    match value {
        "proposed" => Some(ToolActivityStatus::Proposed),
        "approval_required" => Some(ToolActivityStatus::ApprovalRequired),
        "running" => Some(ToolActivityStatus::Running),
        "complete" => Some(ToolActivityStatus::Complete),
        "denied" => Some(ToolActivityStatus::Denied),
        "failed" => Some(ToolActivityStatus::Failed),
        "cancelled" => Some(ToolActivityStatus::Cancelled),
        _ => None,
    }
}

fn tool_approval_token(value: ToolApproval) -> &'static str {
    match value {
        ToolApproval::NotRequired => "not_required",
        ToolApproval::Pending => "pending",
        ToolApproval::Approved => "approved",
        ToolApproval::Denied => "denied",
    }
}

fn parse_tool_approval(value: &str) -> Option<ToolApproval> {
    match value {
        "not_required" => Some(ToolApproval::NotRequired),
        "pending" => Some(ToolApproval::Pending),
        "approved" => Some(ToolApproval::Approved),
        "denied" => Some(ToolApproval::Denied),
        _ => None,
    }
}

fn tool_error_code_token(value: ToolErrorCode) -> &'static str {
    match value {
        ToolErrorCode::PermissionDenied => "permission_denied",
        ToolErrorCode::ExecutionFailed => "execution_failed",
        ToolErrorCode::Cancelled => "cancelled",
    }
}

fn parse_tool_error_code(value: &str) -> Option<ToolErrorCode> {
    match value {
        "permission_denied" => Some(ToolErrorCode::PermissionDenied),
        "execution_failed" => Some(ToolErrorCode::ExecutionFailed),
        "cancelled" => Some(ToolErrorCode::Cancelled),
        _ => None,
    }
}

fn role_token(value: ChatRole) -> &'static str {
    match value {
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
    }
}

fn parse_role(value: &str) -> Option<ChatRole> {
    match value {
        "user" => Some(ChatRole::User),
        "assistant" => Some(ChatRole::Assistant),
        _ => None,
    }
}

fn message_state_token(value: MessageState) -> &'static str {
    match value {
        MessageState::Pending => "pending",
        MessageState::Complete => "complete",
        MessageState::Interrupted => "interrupted",
        MessageState::Failed => "failed",
    }
}

fn parse_message_state(value: &str) -> Option<MessageState> {
    match value {
        "pending" => Some(MessageState::Pending),
        "complete" => Some(MessageState::Complete),
        "interrupted" => Some(MessageState::Interrupted),
        "failed" => Some(MessageState::Failed),
        _ => None,
    }
}

fn attempt_status_token(value: AttemptStatus) -> &'static str {
    match value {
        AttemptStatus::Pending => "pending",
        AttemptStatus::Streaming => "streaming",
        AttemptStatus::Complete => "complete",
        AttemptStatus::Interrupted => "interrupted",
        AttemptStatus::Failed => "failed",
        AttemptStatus::Cancelled => "cancelled",
    }
}

fn parse_attempt_status(value: &str) -> Option<AttemptStatus> {
    match value {
        "pending" => Some(AttemptStatus::Pending),
        "streaming" => Some(AttemptStatus::Streaming),
        "complete" => Some(AttemptStatus::Complete),
        "interrupted" => Some(AttemptStatus::Interrupted),
        "failed" => Some(AttemptStatus::Failed),
        "cancelled" => Some(AttemptStatus::Cancelled),
        _ => None,
    }
}

fn surface_token(value: AiSurface) -> &'static str {
    match value {
        AiSurface::Standalone => "standalone",
        AiSurface::ContextPane => "context_pane",
    }
}

fn parse_surface(value: &str) -> Option<AiSurface> {
    match value {
        "standalone" => Some(AiSurface::Standalone),
        "context_pane" => Some(AiSurface::ContextPane),
        _ => None,
    }
}

fn feedback_token(value: FeedbackValue) -> &'static str {
    match value {
        FeedbackValue::Good => "good",
        FeedbackValue::Bad => "bad",
        FeedbackValue::Cleared => "cleared",
    }
}

fn parse_feedback(value: &str) -> Option<FeedbackValue> {
    match value {
        "good" => Some(FeedbackValue::Good),
        "bad" => Some(FeedbackValue::Bad),
        "cleared" => Some(FeedbackValue::Cleared),
        _ => None,
    }
}

fn parse_export_status(value: &str) -> Option<koru_ai_core::ExportStatus> {
    match value {
        "pending" => Some(koru_ai_core::ExportStatus::Pending),
        "complete" => Some(koru_ai_core::ExportStatus::Complete),
        "failed" => Some(koru_ai_core::ExportStatus::Failed),
        _ => None,
    }
}

fn intent_token(value: AiIntent) -> &'static str {
    match value {
        AiIntent::Chat => "chat",
    }
}

fn parse_intent(value: &str) -> Option<AiIntent> {
    match value {
        "chat" => Some(AiIntent::Chat),
        _ => None,
    }
}

fn usage_source_token(value: UsageSource) -> &'static str {
    match value {
        UsageSource::ProviderReported => "provider_reported",
        UsageSource::Unavailable => "unavailable",
    }
}

fn parse_usage_source(value: &str) -> Option<UsageSource> {
    match value {
        "provider_reported" => Some(UsageSource::ProviderReported),
        "unavailable" => Some(UsageSource::Unavailable),
        _ => None,
    }
}

fn error_class_token(value: AiErrorClass) -> &'static str {
    match value {
        AiErrorClass::Configuration => "configuration",
        AiErrorClass::Authentication => "authentication",
        AiErrorClass::RateLimit => "rate_limit",
        AiErrorClass::ContextTooLarge => "context_too_large",
        AiErrorClass::ModelUnavailable => "model_unavailable",
        AiErrorClass::Network => "network",
        AiErrorClass::Provider => "provider",
        AiErrorClass::Cancelled => "cancelled",
        AiErrorClass::Storage => "storage",
    }
}

fn parse_error_class(value: &str) -> Option<AiErrorClass> {
    match value {
        "configuration" => Some(AiErrorClass::Configuration),
        "authentication" => Some(AiErrorClass::Authentication),
        "rate_limit" => Some(AiErrorClass::RateLimit),
        "context_too_large" => Some(AiErrorClass::ContextTooLarge),
        "model_unavailable" => Some(AiErrorClass::ModelUnavailable),
        "network" => Some(AiErrorClass::Network),
        "provider" => Some(AiErrorClass::Provider),
        "cancelled" => Some(AiErrorClass::Cancelled),
        "storage" => Some(AiErrorClass::Storage),
        _ => None,
    }
}

fn parse_block_kind(value: &str) -> Option<MessageBlockKind> {
    match value {
        "paragraph" => Some(MessageBlockKind::Paragraph),
        "heading" => Some(MessageBlockKind::Heading),
        "list_item" => Some(MessageBlockKind::ListItem),
        "quote" => Some(MessageBlockKind::Quote),
        "code" => Some(MessageBlockKind::Code),
        "rule" => Some(MessageBlockKind::Rule),
        "other" => Some(MessageBlockKind::Other),
        _ => None,
    }
}

fn block_kind_token(value: MessageBlockKind) -> &'static str {
    match value {
        MessageBlockKind::Paragraph => "paragraph",
        MessageBlockKind::Heading => "heading",
        MessageBlockKind::ListItem => "list_item",
        MessageBlockKind::Quote => "quote",
        MessageBlockKind::Code => "code",
        MessageBlockKind::Rule => "rule",
        MessageBlockKind::Other => "other",
    }
}

#[cfg(test)]
mod tests;
