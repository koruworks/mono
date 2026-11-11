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

/// One page of an owner's Bot conversations in Trash, for one workspace,
/// whatever the Bot's lifecycle. It is driven by `bot_chat_trash` in its
/// primary-key order (`sqlite_autoindex_bot_chat_trash_1`), a range seek past
/// the cursor: that table holds only what is in Trash now (a Restore deletes
/// its rows), never lifecycle history. Each row reaches its association,
/// Bot, Chat and operation by primary key. `?4` is the cursor's Chat ID or
/// NULL for the first page; `?6` the last Trash row this page may examine
/// ([`TRASHED_BOT_CHAT_WINDOW_SQL`]), or NULL when fewer remain.
pub(crate) const TRASHED_BOT_CHAT_PAGE_SQL: &str =
    "SELECT c.id, c.title, c.current_provider, c.current_model, c.current_reasoning,
        c.branched_from_chat_id, c.branched_from_message_id, c.created_at_ms, c.updated_at_ms,
        c.private, c.agent_working_folder, c.agent_mode, c.agent_fast, c.pinned_at_ms, c.profile_id,
        bc.purpose, b.name, b.bot_id, b.lifecycle, t.operation_id, i.kind, t.trashed_at_ms
   FROM bot_chat_trash AS t INDEXED BY sqlite_autoindex_bot_chat_trash_1
  CROSS JOIN bot_chats AS bc ON bc.chat_id = t.chat_id
  CROSS JOIN bots AS b ON b.bot_id = bc.bot_id
  CROSS JOIN chats AS c ON c.id = t.chat_id
  CROSS JOIN bot_lifecycle_intents AS i ON i.user_scope_id = t.user_scope_id AND i.operation_id = t.operation_id
  WHERE (?4 IS NULL OR t.chat_id > ?4) AND (?6 IS NULL OR t.chat_id <= ?6)
    AND b.user_scope_id = ?1 AND b.owner_device_id = ?2 AND b.home_workspace_id = ?3
  ORDER BY t.chat_id
  LIMIT ?5";

/// How many Trash rows one page may examine. `bot_chat_trash` holds every
/// owner's and workspace's conversations in Trash on this device and has no
/// index by owner (ai.db 18), so a page filtered by owner and workspace
/// would otherwise walk all of Trash past the cursor to fill itself. Each
/// row costs four primary-key reads.
pub(crate) const TRASHED_BOT_CHAT_SCAN: i64 = 512;

/// The last Trash row of a page's window: the `?2`-th row (counted from 0)
/// past the cursor `?1`, read from the primary key alone, or none when fewer
/// remain, which makes the window the rest of Trash.
pub(crate) const TRASHED_BOT_CHAT_WINDOW_SQL: &str =
    "SELECT chat_id FROM bot_chat_trash INDEXED BY sqlite_autoindex_bot_chat_trash_1
  WHERE (?1 IS NULL OR chat_id > ?1)
  ORDER BY chat_id
  LIMIT 1 OFFSET ?2";

/// The owner's completed whole-Bot Archive and Trash operations of the named
/// Bots that no completed Restore names as its source: the candidates for
/// what holds each archived or trashed Bot. There is no index from a Bot to
/// its finished operations in ai.db 18 (QUESTIONS O44), so this walks the
/// owner's own lifecycle journal through its primary key, which leads with
/// the owner's scope (`sqlite_autoindex_bot_lifecycle_intents_1`): it reads
/// every lifecycle operation that owner has ever made, across all their Bots,
/// never another owner's, and checks each candidate's reversal through
/// `bot_lifecycle_source`. `?3` is a JSON array of the Bot IDs wanted.
pub(crate) const BOT_LIFECYCLE_HOLDERS_SQL: &str =
    "SELECT i.bot_id, i.operation_id, i.kind, i.completed_at_ms
   FROM bot_lifecycle_intents AS i INDEXED BY sqlite_autoindex_bot_lifecycle_intents_1
  WHERE i.user_scope_id = ?1 AND i.owner_device_id = ?2
    AND i.chat_id IS NULL AND i.kind IN ('archive', 'trash') AND i.stage = 'completed'
    AND i.bot_id IN (SELECT value FROM json_each(?3))
    AND NOT EXISTS(SELECT 1 FROM bot_lifecycle_intents AS r INDEXED BY bot_lifecycle_source
                    WHERE r.user_scope_id = i.user_scope_id AND r.source_operation_id = i.operation_id
                      AND r.stage = 'completed')";

/// The workspace a Bot Run's record lives in, when that Run wrote this reply
/// (or the reply a branch copied here, through `historical_bot_run`). Keyed
/// by `bot_chat_runs`' unique `run_id`, checked against the Bot and the reply.
const BOT_RUN_RECORD_SQL: &str = "SELECT record_workspace_id FROM bot_chat_runs
  WHERE run_id = ?1 AND bot_id = ?2 AND assistant_message_id = ?3";

/// The same for a copy whose source was purged: the inert reference the
/// copy keeps, by its primary key `(chat_id, message_id, run_id)`.
const BOT_HISTORICAL_RUN_SQL: &str = "SELECT record_workspace_id FROM bot_historical_run_refs
  WHERE chat_id = ?1 AND message_id = ?2 AND run_id = ?3 AND bot_id = ?4";

impl Repository {
    /// Whether `run` is the Run of reply `message` in `chat`, a conversation
    /// of `bot`, and if so the workspace its Run record is in. The reply is
    /// the Run's own (`bot_chat_runs`), a copy a branch made of it
    /// (resolved through [`Self::historical_bot_run`], up to its hop bound),
    /// or a copy whose source was purged (`bot_historical_run_refs`). Any
    /// mismatch — another Bot, another conversation, a message not in
    /// `chat`, another Run — is `None`, never "it exists elsewhere". Every
    /// step is a primary-key or unique-index seek. History only: the answer
    /// grants no control, approval or result authority, and the caller has
    /// checked the owner first.
    pub fn bound_bot_run(
        &self,
        bot: BotId,
        chat: ChatId,
        message: MessageId,
        run: koru_delegate_core::RunId,
    ) -> Result<Option<WorkspaceId>, BotError> {
        if bot_for_chat_on(&self.connection, chat)? != Some(bot) {
            return Ok(None);
        }
        let in_chat: Option<String> = self
            .connection
            .query_row(
                "SELECT chat_id FROM messages WHERE id = ?1",
                [message.to_storage()],
                |row| row.get(0),
            )
            .optional()
            .map_err(bad)?;
        if in_chat.as_deref() != Some(chat.to_storage().as_str()) {
            return Ok(None);
        }
        let workspace: Option<String> = match self.historical_bot_run(message)? {
            Some(found) if found.bot == bot && found.run == run => self
                .connection
                .query_row(
                    BOT_RUN_RECORD_SQL,
                    params![
                        run.to_storage(),
                        bot.to_storage(),
                        found.assistant_message.to_storage()
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(bad)?,
            _ => None,
        };
        let workspace = match workspace {
            Some(workspace) => Some(workspace),
            None => self
                .connection
                .query_row(
                    BOT_HISTORICAL_RUN_SQL,
                    params![
                        chat.to_storage(),
                        message.to_storage(),
                        run.to_storage(),
                        bot.to_storage()
                    ],
                    |row| row.get(0),
                )
                .optional()
                .map_err(bad)?,
        };
        workspace
            .map(|w| WorkspaceId::parse(&w).ok_or(BotError::Storage))
            .transpose()
    }

    /// Record that `run` wrote reply `message`, as a Bot conversation's
    /// replay preparation does, with the least the read-only resolvers need
    /// (a completed `start` receipt, so no recovery picks it up). The
    /// message's attempt must exist.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn record_bot_chat_run_for_fixture(
        &self,
        bot: &Bot,
        attempt: InferenceAttemptId,
        chat: ChatId,
        message: MessageId,
        run: koru_delegate_core::RunId,
        record_workspace: WorkspaceId,
    ) -> Result<(), BotError> {
        let request = BotRequestId::mint();
        self.connection
            .execute(
                "INSERT INTO bot_command_receipts(user_scope_id,request_id,bot_id,fingerprint,command_kind,stage,planned_run_id,captured_config_revision,captured_policy_revision,submission_version,submission_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,'fixture','start','completed',?4,1,1,1,'{}',1,1)",
                params![
                    bot.user_scope.to_storage(),
                    request.to_storage(),
                    bot.id.to_storage(),
                    run.to_storage()
                ],
            )
            .map_err(bad)?;
        self.connection
            .execute(
                "INSERT INTO bot_chat_runs(ai_attempt_id,user_scope_id,owner_device_id,submission_id,bot_id,chat_id,assistant_message_id,run_id,record_workspace_id,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1)",
                params![
                    attempt.to_storage(),
                    bot.user_scope.to_storage(),
                    bot.owner_device.to_storage(),
                    request.to_storage(),
                    bot.id.to_storage(),
                    chat.to_storage(),
                    message.to_storage(),
                    run.to_storage(),
                    record_workspace.to_storage()
                ],
            )
            .map_err(bad)?;
        Ok(())
    }

    /// What a purge of a copied reply's source leaves (no purge writes it
    /// yet): the source's Run binding is gone, and the copy keeps an inert
    /// reference to the Run in `bot_historical_run_refs`.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn purge_bot_chat_run_source_for_fixture(
        &self,
        run: koru_delegate_core::RunId,
        copy_chat: ChatId,
        copy_message: MessageId,
    ) -> Result<(), BotError> {
        let tx = self.connection.unchecked_transaction().map_err(bad)?;
        let changed = tx
            .execute(
                "INSERT INTO bot_historical_run_refs(chat_id,message_id,source_chat_id,source_message_id,bot_id,run_id,record_workspace_id,recorded_at_ms)
                 SELECT ?1, ?2, chat_id, assistant_message_id, bot_id, run_id, record_workspace_id, 1
                   FROM bot_chat_runs WHERE run_id = ?3",
                params![
                    copy_chat.to_storage(),
                    copy_message.to_storage(),
                    run.to_storage()
                ],
            )
            .map_err(bad)?;
        if changed != 1 {
            return Err(BotError::NotFound);
        }
        tx.execute(
            "UPDATE messages SET origin_message_id = NULL WHERE id = ?1",
            [copy_message.to_storage()],
        )
        .map_err(bad)?;
        tx.execute(
            "DELETE FROM bot_chat_runs WHERE run_id = ?1",
            [run.to_storage()],
        )
        .map_err(bad)?;
        tx.commit().map_err(bad)
    }

    /// What holds each archived or trashed Bot of `bots` where it is: the
    /// completed, unreversed Trash of a trashed Bot, the completed,
    /// unreversed Archive of an archived one. An active Bot, or one with no
    /// such operation, is absent from the answer. One read serves a whole
    /// listing. The caller has checked the owner's authority over the Bots.
    ///
    /// Which operation holds is decided by the Bot's lifecycle, not by
    /// comparing completion times: Archive needs an active Bot and Trash a
    /// Bot that is not trashed, and only a Restore leaves either state, so a
    /// trashed Bot has one unreversed Trash and an archived Bot one
    /// unreversed Archive (a Bot archived then trashed has both, and the
    /// Trash holds it).
    pub fn bot_lifecycle_holders(
        &self,
        owner: BotOwner,
        bots: &[(BotId, BotLifecycle)],
    ) -> Result<std::collections::HashMap<BotId, BotLifecycleHolder>, BotError> {
        let wanted: Vec<(BotId, BotLifecycleKind)> = bots
            .iter()
            .filter_map(|(bot, lifecycle)| match lifecycle {
                BotLifecycle::Archived => Some((*bot, BotLifecycleKind::Archive)),
                BotLifecycle::Trashed => Some((*bot, BotLifecycleKind::Trash)),
                BotLifecycle::Active => None,
            })
            .collect();
        let mut holders = std::collections::HashMap::new();
        if wanted.is_empty() {
            return Ok(holders);
        }
        let ids: Vec<BotId> = wanted.iter().map(|(bot, _)| *bot).collect();
        for (bot, holder) in self.unreversed_whole_bot_operations(owner, &ids)? {
            let Some((_, kind)) = wanted.iter().find(|(id, _)| *id == bot) else {
                continue;
            };
            if holder.kind != *kind {
                continue;
            }
            // Only one can hold; should the journal ever hold two, the
            // answer is still one of them, never a mixture.
            let newer = holders.get(&bot).is_none_or(|known: &BotLifecycleHolder| {
                (holder.completed_at_ms, holder.request_id.to_storage())
                    > (known.completed_at_ms, known.request_id.to_storage())
            });
            if newer {
                holders.insert(bot, holder);
            }
        }
        Ok(holders)
    }

    /// The candidate scan behind [`Self::bot_lifecycle_holders`]. An indexed
    /// lookup from a Bot to its finished operations replaces only this.
    fn unreversed_whole_bot_operations(
        &self,
        owner: BotOwner,
        bots: &[BotId],
    ) -> Result<Vec<(BotId, BotLifecycleHolder)>, BotError> {
        let ids =
            serde_json::to_string(&bots.iter().map(|bot| bot.to_storage()).collect::<Vec<_>>())
                .map_err(bad)?;
        let mut statement = self
            .connection
            .prepare(BOT_LIFECYCLE_HOLDERS_SQL)
            .map_err(bad)?;
        let rows = statement
            .query_map(
                params![
                    owner.user_scope.to_storage(),
                    owner.device.to_storage(),
                    ids
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .map_err(bad)?;
        rows.map(|row| {
            let (bot, operation, kind, completed_at_ms) = row.map_err(bad)?;
            Ok((
                bot.parse().map_err(bad)?,
                BotLifecycleHolder {
                    request_id: operation.parse().map_err(bad)?,
                    kind: BotLifecycleKind::parse(&kind).ok_or(BotError::Storage)?,
                    completed_at_ms,
                },
            ))
        })
        .collect()
    }

    /// One page of the owner's Bot conversations in Trash whose Bot lives in
    /// `workspace`, active, archived or trashed. The caller has checked the
    /// owner's current authority over the workspace.
    pub fn trashed_bot_chat_page(
        &self,
        owner: BotOwner,
        workspace: WorkspaceId,
        cursor: Option<&BotTrashedChatCursor>,
        limit: u16,
    ) -> Result<BotTrashedChatPage, BotError> {
        if limit == 0 || limit > BOT_CHAT_PAGE_LIMIT {
            return Err(BotError::Invalid);
        }
        let after = cursor.map(|c| c.chat_id.to_storage());
        // The page examines at most TRASHED_BOT_CHAT_SCAN rows past the
        // cursor, whoever they belong to.
        let window_end: Option<String> = self
            .connection
            .query_row(
                TRASHED_BOT_CHAT_WINDOW_SQL,
                params![after, TRASHED_BOT_CHAT_SCAN - 1],
                |row| row.get(0),
            )
            .optional()
            .map_err(bad)?;
        let mut statement = self
            .connection
            .prepare(TRASHED_BOT_CHAT_PAGE_SQL)
            .map_err(bad)?;
        let rows = statement
            .query_map(
                params![
                    owner.user_scope.to_storage(),
                    owner.device.to_storage(),
                    workspace.to_storage(),
                    after,
                    i64::from(limit) + 1,
                    window_end,
                ],
                |row| {
                    Ok((
                        chat_from_row(row)?,
                        row.get::<_, String>(15)?,
                        row.get::<_, String>(16)?,
                        row.get::<_, String>(17)?,
                        row.get::<_, String>(18)?,
                        row.get::<_, String>(19)?,
                        row.get::<_, String>(20)?,
                        row.get::<_, i64>(21)?,
                    ))
                },
            )
            .map_err(bad)?;
        let mut chats = Vec::new();
        let mut held = Vec::new();
        for row in rows {
            let (mut chat, purpose, bot_name, bot, lifecycle, operation, kind, at) =
                row.map_err(bad)?;
            let label = BotChatLabel {
                bot_id: bot.parse().map_err(bad)?,
                bot_name,
                purpose: BotChatPurpose::parse(&purpose).ok_or(BotError::Storage)?,
            };
            chat.bot = Some(label.clone());
            let kind = BotLifecycleKind::parse(&kind).ok_or(BotError::Storage)?;
            let lifecycle = BotLifecycle::parse(&lifecycle).ok_or(BotError::Storage)?;
            // Only a conversation trashed on its own, of an active Bot, is
            // restored on its own now (`restore_chat` refuses on an archived
            // or trashed Bot).
            let restore =
                if kind == BotLifecycleKind::TrashChat && lifecycle == BotLifecycle::Active {
                    BotChatRestore::Conversation
                } else {
                    BotChatRestore::Bot
                };
            if lifecycle != BotLifecycle::Active {
                held.push((label.bot_id, lifecycle));
            }
            chats.push(BotTrashedChat {
                conversation: BotConversation { chat, label },
                trashed_by: BotTrashedBy {
                    request_id: operation.parse().map_err(bad)?,
                    kind,
                },
                trashed_at_ms: at,
                restore,
                bot_held_by: None,
            });
        }
        let next_cursor = if chats.len() > usize::from(limit) {
            chats.truncate(usize::from(limit));
            chats.last().map(|last| BotTrashedChatCursor {
                chat_id: last.conversation.chat.id,
            })
        } else {
            // The window ended before the page filled: the next page goes on
            // from its last row, which may be in another workspace's Trash.
            // It grants nothing; every entry is read again under authority.
            window_end
                .map(|end| ChatId::parse(&end).ok_or(BotError::Storage))
                .transpose()?
                .map(|chat_id| BotTrashedChatCursor { chat_id })
        };
        // What holds each archived or trashed Bot of the page, so an entry
        // whose `restore` is `bot` names the Restore to send.
        let inactive: Vec<(BotId, BotLifecycle)> = held
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>()
            .into_iter()
            .collect();
        let holders = self.bot_lifecycle_holders(owner, &inactive)?;
        for entry in &mut chats {
            entry.bot_held_by = holders.get(&entry.conversation.label.bot_id).cloned();
        }
        Ok(BotTrashedChatPage { chats, next_cursor })
    }

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
            let (mut chat, purpose, bot_name) = row.map_err(bad)?;
            let label = BotChatLabel {
                bot_id: bot,
                bot_name,
                purpose: BotChatPurpose::parse(&purpose).ok_or(BotError::Storage)?,
            };
            // The page is the owner's: its Chats carry the same label every
            // other authorized Chat read gives them.
            chat.bot = Some(label.clone());
            chats.push(BotConversation { chat, label });
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
