// SPDX-License-Identifier: MPL-2.0

//! Bounded owner reads for the Bots view (P1.8): a Bot's conversations, page
//! by page. Both read only ai.db 18's committed tables and indexes.
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

impl Repository {
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
