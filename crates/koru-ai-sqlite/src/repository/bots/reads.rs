// SPDX-License-Identifier: MPL-2.0

//! Bounded owner reads for the Bots view (P1.8): a Bot's conversations, page
//! by page, and a workspace's running and Needs-you conversations. Both read
//! only ai.db 18's committed tables and indexes.
//!
//! `bot_chats` has no index on `bot_id` (only the partial `bot_main_chat`), so
//! a page scans the association table — one narrow row per Bot conversation
//! on this root, the same bound the aggregate Chat reads already pay through
//! `visible_bot_chats` — and reaches every Chat, Bot and Trash row by primary
//! key. It never walks `chats_by_updated`, messages or attempts. The order is
//! forced with `CROSS JOIN`, because Koru never runs ANALYZE and the planner
//! would otherwise be free to drive from `chats`.
use super::*;

/// The page query. `?2` is the cursor's main rank (1 or 0) or NULL for the
/// first page; `?3`/`?4` the cursor's `updated_at_ms` and Chat ID.
pub(crate) const BOT_CHAT_PAGE_SQL: &str =
    "SELECT c.id, c.title, c.current_provider, c.current_model, c.current_reasoning,
        c.branched_from_chat_id, c.branched_from_message_id, c.created_at_ms, c.updated_at_ms,
        c.private, c.agent_working_folder, c.agent_mode, c.agent_fast, c.pinned_at_ms, c.profile_id,
        bc.purpose, b.name
   FROM bot_chats AS bc
  CROSS JOIN chats AS c ON c.id = bc.chat_id
  CROSS JOIN bots AS b ON b.bot_id = bc.bot_id
  WHERE bc.bot_id = ?1
    AND NOT EXISTS(SELECT 1 FROM bot_chat_trash AS t WHERE t.chat_id = bc.chat_id)
    AND (?2 IS NULL
         OR (bc.purpose = 'main') < ?2
         OR ((bc.purpose = 'main') = ?2
             AND (c.updated_at_ms < ?3 OR (c.updated_at_ms = ?3 AND c.id > ?4))))
  ORDER BY (bc.purpose = 'main') DESC, c.updated_at_ms DESC, c.id
  LIMIT ?5";

/// A workspace's running and Needs-you Bot conversations. Per conversation it
/// reads at most three bounded index probes, never its history:
/// - **running**: the chat's pending assistant message, through the partial
///   unique `one_pending_assistant_per_chat` (at most one row per chat);
/// - **needs you**: an `approval_required` tool row (an approval, or a
///   question card the person must answer) on the chat's latest attempt —
///   one seek of `inference_attempts_by_chat`, then that attempt's own rows in
///   `tool_invocations_by_attempt` — or on the attempt of its pending reply,
///   reached through `bot_chat_runs`' unique assistant message.
///
/// Only the latest attempt can hold an open row: a chat has at most one
/// pending reply, sending another message closes the cards it leaves open,
/// and an AI restart cancels every open row. The latest attempt is taken by
/// `queued_at_ms`; the pending-reply probe keeps a live approval visible even
/// if the clock stepped back between two attempts of one chat.
///
/// Idle conversations are skipped while reading rather than in SQL: an outer
/// `WHERE running OR needs_you` makes SQLite evaluate each probe twice.
pub(crate) const BOT_ACTIVITY_SQL: &str = "SELECT bc.bot_id, bc.chat_id,
    EXISTS(SELECT 1 FROM messages AS m INDEXED BY one_pending_assistant_per_chat
            WHERE m.chat_id = bc.chat_id AND m.state = 'pending' AND m.role = 'assistant') AS running,
    (EXISTS(SELECT 1 FROM tool_invocations AS t INDEXED BY tool_invocations_by_attempt
             WHERE t.attempt_id = (SELECT a.id FROM inference_attempts AS a INDEXED BY inference_attempts_by_chat
                                    WHERE a.chat_id = bc.chat_id ORDER BY a.queued_at_ms DESC LIMIT 1)
               AND t.status = 'approval_required')
     OR EXISTS(SELECT 1 FROM messages AS p INDEXED BY one_pending_assistant_per_chat
                CROSS JOIN bot_chat_runs AS r ON r.assistant_message_id = p.id
                CROSS JOIN tool_invocations AS pt INDEXED BY tool_invocations_by_attempt
                        ON pt.attempt_id = r.ai_attempt_id
               WHERE p.chat_id = bc.chat_id AND p.state = 'pending' AND p.role = 'assistant'
                 AND pt.status = 'approval_required')) AS needs_you
    FROM bot_chats AS bc
   CROSS JOIN bots AS b ON b.bot_id = bc.bot_id
   WHERE b.user_scope_id = ?1 AND b.owner_device_id = ?2 AND b.home_workspace_id = ?3
     AND b.lifecycle = 'active'
     AND NOT EXISTS(SELECT 1 FROM bot_chat_trash AS tr WHERE tr.chat_id = bc.chat_id)
   ORDER BY bc.bot_id, bc.chat_id";

impl Repository {
    /// The owner's active Bots of `workspace`: each conversation outside Trash
    /// that is running or needs the person, at most `limit`. The caller has
    /// checked the owner's current authority over the workspace.
    pub fn bot_activity(
        &self,
        owner: BotOwner,
        workspace: WorkspaceId,
        limit: usize,
    ) -> Result<BotActivityPage, BotError> {
        let mut statement = self.connection.prepare(BOT_ACTIVITY_SQL).map_err(bad)?;
        let rows = statement
            .query_map(
                params![
                    owner.user_scope.to_storage(),
                    owner.device.to_storage(),
                    workspace.to_storage(),
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, bool>(3)?,
                    ))
                },
            )
            .map_err(bad)?;
        let mut entries = Vec::new();
        for row in rows {
            let (bot, chat, running, needs_you) = row.map_err(bad)?;
            let state = match (needs_you, running) {
                (true, _) => BotActivityState::NeedsYou,
                (false, true) => BotActivityState::Running,
                (false, false) => continue,
            };
            if entries.len() == limit {
                return Ok(BotActivityPage {
                    entries,
                    truncated: true,
                });
            }
            entries.push(BotActivity {
                bot_id: bot.parse().map_err(bad)?,
                chat_id: ChatId::parse(&chat).ok_or(BotError::Storage)?,
                state,
            });
        }
        Ok(BotActivityPage {
            entries,
            truncated: false,
        })
    }

    /// One page of a Bot's conversations outside Trash, main first, then
    /// newest first. The caller has checked the owner's current authority
    /// over this Bot; this reads nothing of another Bot.
    pub fn bot_chat_page(
        &self,
        bot: BotId,
        cursor: Option<&BotChatCursor>,
        limit: u16,
    ) -> Result<BotChatPage, BotError> {
        if limit == 0 || limit > BOT_CHAT_PAGE_LIMIT {
            return Err(BotError::Invalid);
        }
        let mut statement = self.connection.prepare(BOT_CHAT_PAGE_SQL).map_err(bad)?;
        let rows = statement
            .query_map(
                params![
                    bot.to_storage(),
                    cursor.map(|c| i64::from(c.main)),
                    cursor.map(|c| c.updated_at_ms),
                    cursor.map(|c| c.chat_id.to_storage()),
                    i64::from(limit) + 1,
                ],
                |row| {
                    Ok((
                        chat_from_row(row)?,
                        row.get::<_, String>(15)?,
                        row.get::<_, String>(16)?,
                    ))
                },
            )
            .map_err(bad)?;
        let mut chats = Vec::new();
        for row in rows {
            let (chat, purpose, bot_name) = row.map_err(bad)?;
            chats.push(BotConversation {
                chat,
                label: BotChatLabel {
                    bot_id: bot,
                    bot_name,
                    purpose: BotChatPurpose::parse(&purpose).ok_or(BotError::Storage)?,
                },
            });
        }
        let next_cursor = if chats.len() > usize::from(limit) {
            chats.truncate(usize::from(limit));
            chats.last().map(|last| BotChatCursor {
                main: last.label.purpose == BotChatPurpose::Main,
                updated_at_ms: last.chat.updated_at.millis(),
                chat_id: last.chat.id,
            })
        } else {
            None
        };
        Ok(BotChatPage { chats, next_cursor })
    }
}

#[cfg(test)]
mod tests;
