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

// Drive from the ordered main-chat index, not bots_home followed by a sort
// of the whole roster. Unrelated owners/workspaces are filtered before LIMIT.
const BOT_SUMMARY_PAGE_SQL: &str = "SELECT b.bot_id,b.name,bc.chat_id,b.revision,b.paused
    FROM bot_chats AS bc INDEXED BY bot_main_chat
   CROSS JOIN bots AS b ON b.bot_id=bc.bot_id
   WHERE bc.purpose='main' AND bc.bot_id>?4
     AND b.user_scope_id=?1 AND b.owner_device_id=?2 AND b.home_workspace_id=?3
     AND b.lifecycle='active'
     AND NOT EXISTS(SELECT 1 FROM bot_chat_trash AS t WHERE t.chat_id=bc.chat_id)
   ORDER BY bc.bot_id LIMIT ?5";

impl Repository {
    /// Keyset roster over narrow scalar columns. Neither configuration nor
    /// page grants/history are loaded, and preparation proves only workspace
    /// authority, so a caller's page does not first materialize every Bot.
    /// The main-chat index is global: this may scan unrelated associations
    /// before filling a page. Materialized rows, not SQL work, are bounded.
    pub fn bot_summary_page(
        &self,
        owner: BotOwner,
        workspace: WorkspaceId,
        after: Option<BotId>,
        limit: u16,
    ) -> Result<BotSummaryPage, BotError> {
        if !(1..=100).contains(&limit) {
            return Err(BotError::Invalid);
        }
        let mut statement = self.connection.prepare(BOT_SUMMARY_PAGE_SQL).map_err(bad)?;
        let rows = statement
            .query_map(
                params![
                    owner.user_scope.to_storage(),
                    owner.device.to_storage(),
                    workspace.to_storage(),
                    after.map(|id| id.to_storage()).unwrap_or_default(),
                    u32::from(limit) + 1
                ],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, u64>(3)?,
                        r.get::<_, bool>(4)?,
                    ))
                },
            )
            .map_err(bad)?;
        let mut bots = rows
            .map(|row| {
                let (id, name, chat, revision, paused) = row.map_err(bad)?;
                Ok(BotSummary {
                    bot_id: id.parse().map_err(bad)?,
                    name,
                    main_chat: ChatId::parse(&chat).ok_or(BotError::Storage)?,
                    revision,
                    paused,
                    lifecycle: BotLifecycle::Active,
                })
            })
            .collect::<Result<Vec<_>, BotError>>()?;
        let more = bots.len() > usize::from(limit);
        bots.truncate(usize::from(limit));
        let next = if more {
            bots.last().map(|b| b.bot_id)
        } else {
            None
        };
        Ok(BotSummaryPage { bots, next })
    }
}

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
    AND NOT EXISTS(SELECT 1 FROM bot_child_chats AS cc
                     CROSS JOIN bot_command_receipts AS cr ON cr.user_scope_id = cc.user_scope_id AND cr.request_id = cc.child_request_id
                     WHERE cc.chat_id = bc.chat_id AND cr.stage NOT IN ('accepted','completed'))
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
                 AND pt.status = 'approval_required')) AS needs_you,
    EXISTS(SELECT 1 FROM messages AS w INDEXED BY one_pending_assistant_per_chat
            CROSS JOIN bot_chat_runs AS wl ON wl.assistant_message_id = w.id
            CROSS JOIN bot_child_wait_enrollments AS we INDEXED BY bot_child_wait_enrollments_run
                    ON we.run_id = wl.run_id AND we.ai_attempt_id = wl.ai_attempt_id
           WHERE w.chat_id = bc.chat_id AND w.state = 'pending' AND w.role = 'assistant'
             AND NOT EXISTS(SELECT 1 FROM bot_child_replay_rollovers AS wo WHERE wo.wait_id = we.wait_id)) AS waiting
    FROM bot_chats AS bc
   CROSS JOIN bots AS b ON b.bot_id = bc.bot_id
   WHERE b.user_scope_id = ?1 AND b.owner_device_id = ?2 AND b.home_workspace_id = ?3
     AND b.lifecycle = 'active'
     AND NOT EXISTS(SELECT 1 FROM bot_chat_trash AS tr WHERE tr.chat_id = bc.chat_id)
     AND NOT EXISTS(SELECT 1 FROM bot_child_chats AS cc
                     CROSS JOIN bot_command_receipts AS cr ON cr.user_scope_id = cc.user_scope_id AND cr.request_id = cc.child_request_id
                     WHERE cc.chat_id = bc.chat_id AND cr.stage NOT IN ('accepted','completed'))
   ORDER BY bc.bot_id, bc.chat_id";

/// One page of an owner's Bot conversations in Trash, for one workspace,
/// whatever the Bot's lifecycle. It is a range of the caller's own rows of
/// `bot_chat_trash` past the cursor, in Chat ID order, through
/// `bot_chat_trash_owner (user_scope_id, chat_id)`: that table holds only
/// what is in Trash now (a Restore deletes its rows), never lifecycle
/// history, and another owner's rows are never read. Each row reaches its
/// association, Bot, Chat and operation by primary key. The device and
/// workspace filters apply to the owner's own rows, so a page stops as soon
/// as it holds `?5` rows: at worst it reads the owner's Trash of other
/// workspaces past the cursor. `?4` is the cursor's Chat ID, or the empty
/// string for the first page (every Chat ID sorts after it), so the range
/// always starts at a seek.
pub(crate) const TRASHED_BOT_CHAT_PAGE_SQL: &str =
    "SELECT c.id, c.title, c.current_provider, c.current_model, c.current_reasoning,
        c.branched_from_chat_id, c.branched_from_message_id, c.created_at_ms, c.updated_at_ms,
        c.private, c.agent_working_folder, c.agent_mode, c.agent_fast, c.pinned_at_ms, c.profile_id,
        bc.purpose, b.name, b.bot_id, b.lifecycle, t.operation_id, i.kind, t.trashed_at_ms
   FROM bot_chat_trash AS t INDEXED BY bot_chat_trash_owner
  CROSS JOIN bot_chats AS bc ON bc.chat_id = t.chat_id
  CROSS JOIN bots AS b ON b.bot_id = bc.bot_id
  CROSS JOIN chats AS c ON c.id = t.chat_id
  CROSS JOIN bot_lifecycle_intents AS i ON i.user_scope_id = t.user_scope_id AND i.operation_id = t.operation_id
  WHERE t.user_scope_id = ?1 AND t.chat_id > ?4
    AND b.user_scope_id = ?1 AND b.owner_device_id = ?2 AND b.home_workspace_id = ?3
  ORDER BY t.chat_id
  LIMIT ?5";

/// The owner's completed whole-Bot Archive and Trash operations of the named
/// Bots: what holds each archived or trashed Bot. One seek of
/// `bot_lifecycle_held` per Bot (`?3` is a JSON array of the Bot IDs
/// wanted), whose predicate this repeats word for word. A Restore marks the
/// operation it reverses `reversed`, so a completed one is unreversed by its
/// stage alone and no Restore is looked up. Another owner's or device's
/// operation naming the same Bot ID is filtered out on its row.
pub(crate) const BOT_LIFECYCLE_HOLDERS_SQL: &str =
    "SELECT i.bot_id, i.operation_id, i.kind, i.completed_at_ms
   FROM bot_lifecycle_intents AS i INDEXED BY bot_lifecycle_held
  WHERE i.bot_id IN (SELECT value FROM json_each(?3))
    AND i.stage = 'completed' AND i.chat_id IS NULL AND i.kind IN ('archive','trash')
    AND i.user_scope_id = ?1 AND i.owner_device_id = ?2";

/// The workspace a Bot Run's record lives in, when that Run wrote this reply
/// (or the reply a branch copied here, through `historical_bot_run`). Keyed
/// by `bot_chat_runs`' unique `run_id`, checked against the Bot and the reply.
const BOT_RUN_RECORD_SQL: &str = "SELECT record_workspace_id FROM bot_chat_runs
  WHERE run_id = ?1 AND bot_id = ?2 AND assistant_message_id = ?3";

/// The same for a copy whose source was purged: the inert reference the
/// copy keeps, by its primary key `(chat_id, message_id, run_id)`.
const BOT_HISTORICAL_RUN_SQL: &str = "SELECT record_workspace_id FROM bot_historical_run_refs
  WHERE chat_id = ?1 AND message_id = ?2 AND run_id = ?3 AND bot_id = ?4";

/// The receipt of a task started through the API, by its unique planned
/// Run (the `planned_run_id` unique index), narrowed to the Bot: what
/// `read_run` without a message binds through.
pub(crate) const BOT_TASK_RUN_SQL: &str =
    "SELECT user_scope_id, request_id FROM bot_command_receipts
  WHERE planned_run_id = ?1 AND bot_id = ?2";

/// Whether a Run wrote a reply: its row by the unique `run_id`.
pub(crate) const BOT_RUN_HAS_REPLY_SQL: &str =
    "SELECT EXISTS(SELECT 1 FROM bot_chat_runs WHERE run_id = ?1)";

/// Whether the execution owner admitted a Run's submission: its receipt,
/// by primary key from the Run's unique `run_id`, is `completed` (a refused
/// one is `rejected`, one still in flight `prepared`).
const BOT_RUN_ADMITTED_SQL: &str = "SELECT EXISTS(SELECT 1 FROM bot_chat_runs AS r
  JOIN bot_command_receipts AS c ON c.user_scope_id = r.user_scope_id AND c.request_id = r.submission_id
  WHERE r.run_id = ?1 AND c.stage = 'completed')";

/// The Run a copy whose source was purged keeps a reference to, by the
/// leading `(chat_id, message_id)` of the primary key.
const BOT_REPLY_HISTORICAL_RUN_SQL: &str = "SELECT run_id FROM bot_historical_run_refs
  WHERE chat_id = ?1 AND message_id = ?2 AND bot_id = ?3 ORDER BY run_id LIMIT 1";

impl Repository {
    /// For each of `replies` in `chat`, a conversation of `bot`, the Run
    /// [`Self::bound_bot_run`] accepts for it, when the execution owner
    /// holds one: the reply's own Run or the one a branch copied it from,
    /// once its submission was admitted (a Run still being submitted, or
    /// refused before admission, is left out), or the Run a copy keeps a
    /// reference to after its source was purged. A reply with neither is
    /// absent. Bounded by `replies`: per reply at most 32 primary-key hops
    /// and three index seeks. The caller passes only messages of `chat` and
    /// has checked the owner's authority over the conversation.
    pub fn bot_reply_runs(
        &self,
        bot: BotId,
        chat: ChatId,
        replies: &[MessageId],
    ) -> Result<Vec<(MessageId, koru_delegate_core::RunId)>, BotError> {
        if replies.is_empty() || bot_for_chat_on(&self.connection, chat)? != Some(bot) {
            return Ok(Vec::new());
        }
        let mut admitted = self
            .connection
            .prepare_cached(BOT_RUN_ADMITTED_SQL)
            .map_err(bad)?;
        let mut referenced = self
            .connection
            .prepare_cached(BOT_REPLY_HISTORICAL_RUN_SQL)
            .map_err(bad)?;
        let mut runs = Vec::new();
        for reply in replies {
            let own = match self.historical_bot_run(*reply)? {
                Some(found) if found.bot == bot => Some(found.run),
                _ => None,
            };
            let run = match own {
                Some(run)
                    if admitted
                        .query_row([run.to_storage()], |row| row.get::<_, bool>(0))
                        .map_err(bad)? =>
                {
                    Some(run)
                }
                // An unadmitted Run of its own wins over no reference; as
                // in `bound_bot_run`, a reference is asked for only when
                // the chain names no Run of this Bot.
                Some(_) => None,
                None => referenced
                    .query_row(
                        params![chat.to_storage(), reply.to_storage(), bot.to_storage()],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(bad)?
                    .map(|run| run.parse().map_err(bad))
                    .transpose()?,
            };
            if let Some(run) = run {
                runs.push((*reply, run));
            }
        }
        Ok(runs)
    }

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
        // The chain decides when it names a Run of this Bot: that Run, and
        // only it. A purged source's reference is asked for only when the
        // chain names none, exactly as `bot_reply_runs` reports.
        let workspace: Option<String> = match self.historical_bot_run(message)? {
            Some(found) if found.bot == bot => {
                if found.run != run {
                    return Ok(None);
                }
                self.connection
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
                    .map_err(bad)?
            }
            _ => self
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

    /// The record workspace of `run` when `read_run` may bind it without a
    /// message: one of `bot`'s tasks started through the API. Its receipt,
    /// found by its unique planned Run, is `owner`'s and this Bot's,
    /// `completed` (admitted by its execution owner), started from no
    /// conversation attempt (a reply's Run binds only through its reply, by
    /// [`Self::bound_bot_run`]), and its submission names `chat`, a
    /// conversation of this Bot. The workspace is the submission's, where
    /// the Run's record is. Anything else is `None`, never "it exists
    /// elsewhere"; the caller has checked the owner first. Three key seeks.
    pub fn bound_bot_task_run(
        &self,
        owner: BotOwner,
        bot: BotId,
        chat: ChatId,
        run: koru_delegate_core::RunId,
    ) -> Result<Option<WorkspaceId>, BotError> {
        if bot_for_chat_on(&self.connection, chat)? != Some(bot) {
            return Ok(None);
        }
        let replied: bool = self
            .connection
            .query_row(BOT_RUN_HAS_REPLY_SQL, [run.to_storage()], |row| row.get(0))
            .map_err(bad)?;
        if replied {
            return Ok(None);
        }
        let found: Option<(String, String)> = self
            .connection
            .query_row(
                BOT_TASK_RUN_SQL,
                params![run.to_storage(), bot.to_storage()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(bad)?;
        let Some((scope, request)) = found else {
            return Ok(None);
        };
        if scope != owner.user_scope.to_storage() {
            return Ok(None);
        }
        let request: BotRequestId = request.parse().map_err(bad)?;
        let Some(row) = super::admission::raw(&self.connection, owner.user_scope, request)? else {
            return Ok(None);
        };
        if !matches!(row.kind.as_str(), "start" | "continue") {
            return Ok(None);
        }
        let receipt = super::admission::decode_submission(row, owner.user_scope, request)?;
        let submission = &receipt.intent.submission;
        let p = &submission.provenance;
        Ok((receipt.stage == BotReceiptStage::Completed
            && p.ai_attempt_id.is_none()
            && p.owner.device == owner.device
            && p.bot == bot
            && p.chat_id == chat.as_uuid()
            && submission.request.run == run)
            .then_some(submission.workspace))
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
        // One transaction, with the markers a real preparation moves: the
        // conversation names the Run, and a reply that has ended names it.
        let tx = self.connection.unchecked_transaction().map_err(bad)?;
        tx
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
        tx
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
        super::client_feed::touch_turn(&tx, chat, &[message], true).map_err(bad)?;
        tx.commit().map_err(bad)
    }

    /// Set the stage of the receipt behind `run`'s submission: `prepared`
    /// (still being submitted), `rejected` (refused before admission) or
    /// `completed` (admitted), as the admission path leaves it.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn set_bot_run_receipt_stage_for_fixture(
        &self,
        run: koru_delegate_core::RunId,
        stage: &str,
    ) -> Result<(), BotError> {
        if !matches!(stage, "prepared" | "rejected" | "completed") {
            return Err(BotError::Invalid);
        }
        let tx = self.connection.unchecked_transaction().map_err(bad)?;
        let changed = tx
            .execute(
                "UPDATE bot_command_receipts SET stage = ?2
                  WHERE (user_scope_id, request_id) =
                        (SELECT user_scope_id, submission_id FROM bot_chat_runs WHERE run_id = ?1)",
                params![run.to_storage(), stage],
            )
            .map_err(bad)?;
        if changed != 1 {
            return Err(BotError::NotFound);
        }
        // Whether the reply names its Run follows the receipt's stage.
        let reply: String = tx
            .query_row(
                "SELECT assistant_message_id FROM bot_chat_runs WHERE run_id = ?1",
                [run.to_storage()],
                |row| row.get(0),
            )
            .map_err(bad)?;
        super::client_feed::touch_message_by_id(&tx, &reply, false).map_err(bad)?;
        tx.commit().map_err(bad)
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
        let source: Option<String> = tx
            .query_row(
                "SELECT assistant_message_id FROM bot_chat_runs WHERE run_id = ?1",
                [run.to_storage()],
                |row| row.get(0),
            )
            .optional()
            .map_err(bad)?;
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
        // The source reply no longer names a Run, and the copy names the
        // one it keeps a reference to.
        if let Some(source) = source {
            super::client_feed::touch_message_by_id(&tx, &source, true).map_err(bad)?;
        }
        super::client_feed::touch_message_by_id(&tx, &copy_message.to_storage(), true)
            .map_err(bad)?;
        tx.commit().map_err(bad)
    }

    /// What holds each archived or trashed Bot of `bots` where it is: the
    /// completed (so unreversed) Trash of a trashed Bot, the completed
    /// Archive of an archived one. An active Bot, or one with no such
    /// operation, is absent from the answer. One statement, one index seek
    /// per Bot, serves a whole listing. The caller has checked the owner's
    /// authority over the Bots.
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
        for (bot, holder) in self.completed_whole_bot_operations(owner, &ids)? {
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

    /// The candidates behind [`Self::bot_lifecycle_holders`]: the owner's
    /// completed whole-Bot Archive and Trash of each named Bot, through
    /// `bot_lifecycle_held`.
    fn completed_whole_bot_operations(
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
        // Every Chat ID sorts after the empty string.
        let after = cursor.map_or_else(String::new, |c| c.chat_id.to_storage());
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
        // A page that is not the last is full, and its cursor is its own
        // last entry: one of the caller's conversations.
        let next_cursor = if chats.len() > usize::from(limit) {
            chats.truncate(usize::from(limit));
            chats.last().map(|last| BotTrashedChatCursor {
                chat_id: last.conversation.chat.id,
            })
        } else {
            None
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
                        row.get::<_, bool>(4)?,
                    ))
                },
            )
            .map_err(bad)?;
        let mut entries = Vec::new();
        for row in rows {
            let (bot, chat, running, needs_you, waiting) = row.map_err(bad)?;
            // Needs you comes first: a card open for the person outranks a
            // wait. A wait outranks plain running: the reply is not working,
            // it is held until the Bots it asked have answered.
            let state = match (needs_you, waiting, running) {
                (true, _, _) => BotActivityState::NeedsYou,
                (false, true, _) => BotActivityState::Waiting,
                (false, false, true) => BotActivityState::Running,
                (false, false, false) => continue,
            };
            if entries.len() == limit {
                return Ok(BotActivityPage {
                    entries,
                    truncated: true,
                });
            }
            let bot_id: BotId = bot.parse().map_err(bad)?;
            let chat_id = ChatId::parse(&chat).ok_or(BotError::Storage)?;
            let waiting = if state == BotActivityState::Waiting {
                // The row said a wait is enrolled; if it settled between the
                // two reads the entry names nobody, which is still true.
                let detail = self.bot_child_waiting(owner, bot_id, chat_id)?;
                Some(BotActivityWaiting {
                    paused: detail.as_ref().is_some_and(|d| d.paused),
                    on: detail.map(|d| d.recipients).unwrap_or_default(),
                })
            } else {
                None
            };
            entries.push(BotActivity {
                bot_id,
                chat_id,
                state,
                waiting,
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
