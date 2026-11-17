// SPDX-License-Identifier: MPL-2.0
//! `bot_child_chats`: the asks this computer's AI worker prepared.
//!
//! An ask is prepared in one transaction, before the delegate hears of it:
//! one new conversation, one opening turn and one ordinary `start` receipt
//! per recipient, and one row per request that names who asked. The delegate
//! then admits the whole ask or refuses it, and the rows here are settled to
//! match: accepted together with the wait's enrollment, or rejected with the
//! code the refusal was answered with.
use super::*;
use rusqlite::TransactionBehavior;

const COLUMNS: &str = "cc.child_request_id,cc.parent_bot_id,cc.parent_chat_id,cc.parent_run_id,cc.parent_assistant_message_id,cc.ask_key,cc.ordinal,cc.wait_id,cc.chat_id,cc.created_at_ms,cc.refusal,cc.refusal_ordinal,cc.ask_fingerprint,r.bot_id,r.planned_run_id,r.stage";
const JOINED: &str = "bot_child_chats AS cc CROSS JOIN bot_command_receipts AS r ON r.user_scope_id=cc.user_scope_id AND r.request_id=cc.child_request_id";
/// A request whose recipient's receipt the delegate accepted. Every reader a
/// person sees goes through this: a prepared or refused ask is listed nowhere.
const ADMITTED: &str = "r.stage IN ('accepted','completed')";

fn decode(row: &rusqlite::Row<'_>) -> rusqlite::Result<(BotChildChat, String)> {
    fn id<T: std::str::FromStr>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<T> {
        row.get::<_, String>(index)?.parse().map_err(|_| {
            rusqlite::Error::InvalidColumnType(
                index,
                "identity".into(),
                rusqlite::types::Type::Text,
            )
        })
    }
    fn parse<T>(index: usize, value: Option<T>) -> rusqlite::Result<T> {
        value.ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(
                index,
                "identity".into(),
                rusqlite::types::Type::Text,
            )
        })
    }
    Ok((
        BotChildChat {
            child_request: id(row, 0)?,
            parent_bot: id(row, 1)?,
            parent_chat: parse(2, ChatId::parse(&row.get::<_, String>(2)?))?,
            parent_run: id(row, 3)?,
            parent_assistant_message: parse(4, MessageId::parse(&row.get::<_, String>(4)?))?,
            ask_key: id(row, 5)?,
            ordinal: row.get(6)?,
            wait: id(row, 7)?,
            chat: parse(8, ChatId::parse(&row.get::<_, String>(8)?))?,
            created_at_ms: row.get(9)?,
            refusal: row.get(10)?,
            refusal_ordinal: row.get(11)?,
            recipient_bot: id(row, 13)?,
            child_run: id(row, 14)?,
            stage: parse(15, BotReceiptStage::parse(&row.get::<_, String>(15)?))?,
        },
        row.get(12)?,
    ))
}

/// The rows of one ask, in ordinal order, with the fingerprint they share.
fn ask_on(
    conn: &Connection,
    owner: BotOwner,
    parent_run: koru_delegate_core::RunId,
    ask_key: Uuid,
) -> Result<Vec<(BotChildChat, String)>, BotError> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM {JOINED} WHERE cc.user_scope_id=?1 AND cc.parent_run_id=?2 AND cc.ask_key=?3 ORDER BY cc.ordinal"
        ))
        .map_err(bad)?;
    let rows = statement
        .query_map(
            params![
                owner.user_scope.to_storage(),
                parent_run.to_storage(),
                ask_key.to_string()
            ],
            decode,
        )
        .map_err(bad)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(bad)?;
    // Ordinals are 0..n without a gap, and the rows of one ask share its wait.
    if rows
        .iter()
        .enumerate()
        .any(|(index, (row, _))| usize::from(row.ordinal) != index || row.wait != rows[0].0.wait)
        || rows.len() > BOT_CHILD_ASK_LIMIT
    {
        return Err(BotError::Storage);
    }
    Ok(rows)
}

fn chats_of(rows: Vec<(BotChildChat, String)>) -> Vec<BotChildChat> {
    rows.into_iter().map(|(row, _)| row).collect()
}

/// Whether the recipient is one the asking Bot may name at all: the same
/// person, home workspace and computer, and active.
fn recipient_on(
    conn: &Connection,
    owner: BotOwner,
    home: WorkspaceId,
    recipient: BotId,
) -> Result<bool, BotError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bots WHERE bot_id=?1 AND user_scope_id=?2 AND owner_device_id=?3 AND home_workspace_id=?4 AND lifecycle='active')",
        params![
            recipient.to_storage(),
            owner.user_scope.to_storage(),
            owner.device.to_storage(),
            home.to_storage()
        ],
        |r| r.get(0),
    )
    .map_err(bad)
}

impl Repository {
    /// Prepare a whole ask, or refuse it whole. One writer transaction, in the
    /// order of the contract: replay before every check; the asking Bot's
    /// switch; every recipient against current rows; then the conversations,
    /// the opening turns, the `start` receipts and the request rows.
    ///
    /// The page checks are the caller's, made before this call: this owner
    /// reads no page. An unsettled access edit of a recipient answers
    /// `bot_operation_pending` (retry), as it does for any submission.
    pub fn prepare_bot_child_ask(
        &mut self,
        ask: &BotChildAsk,
        now: Timestamp,
    ) -> Result<BotChildAskPrepared, BotError> {
        // A Bot never asks itself. Named before the shape check, which would
        // refuse the same thing without saying which item it was.
        if let Some(index) = ask
            .items
            .iter()
            .position(|item| item.intent.submission.provenance.bot == ask.parent_bot)
        {
            return Ok(BotChildAskPrepared::Refused(BotChildAskRefusal::SelfAsk {
                index: index.min(usize::from(u8::MAX)) as u8,
            }));
        }
        ask.validate()?;
        let owner = ask.owner;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        // 1. Replay: the same key answers its stored ids, whatever changed since.
        let prior = ask_on(&tx, owner, ask.parent_run, ask.ask_key)?;
        if let Some((_, fingerprint)) = prior.first() {
            return Ok(if *fingerprint == ask.ask_fingerprint {
                BotChildAskPrepared::Retained(chats_of(prior))
            } else {
                BotChildAskPrepared::Refused(BotChildAskRefusal::AskKeyConflict)
            });
        }
        // The asking Run is this Bot's reply in this conversation.
        let parent = admission::guard(&tx, owner, ask.parent_bot)?;
        let asking: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM bot_chat_runs WHERE run_id=?1 AND user_scope_id=?2 AND owner_device_id=?3 AND bot_id=?4 AND chat_id=?5 AND assistant_message_id=?6)",
                params![
                    ask.parent_run.to_storage(),
                    owner.user_scope.to_storage(),
                    owner.device.to_storage(),
                    ask.parent_bot.to_storage(),
                    ask.parent_chat.to_storage(),
                    ask.parent_assistant_message.to_storage()
                ],
                |r| r.get(0),
            )
            .map_err(bad)?;
        if !asking {
            return Err(BotError::NotFound);
        }
        // 2. The switch, read current.
        let may_ask: bool = tx
            .query_row(
                "SELECT may_ask FROM bots WHERE bot_id=?1",
                [ask.parent_bot.to_storage()],
                |r| r.get(0),
            )
            .map_err(bad)?;
        if !may_ask {
            return Ok(BotChildAskPrepared::Refused(
                BotChildAskRefusal::AskNotAllowed,
            ));
        }
        let mut named = std::collections::BTreeSet::new();
        for (index, item) in ask.items.iter().enumerate() {
            let index = index as u8;
            let recipient = item.intent.submission.provenance.bot;
            // No Bot is named twice in one call: a malformed request.
            if !named.insert(recipient) {
                return Err(BotError::Invalid);
            }
            if !recipient_on(&tx, owner, parent.home, recipient)? {
                return Ok(BotChildAskPrepared::Refused(
                    BotChildAskRefusal::RecipientUnavailable { index },
                ));
            }
            if !admission::bot_admitting_on(&tx, recipient)? {
                return Ok(BotChildAskPrepared::Refused(
                    BotChildAskRefusal::RecipientPaused { index },
                ));
            }
        }
        // An asked Bot runs as itself (O160): a new ask is written only with
        // each recipient's own Library capture. Replay was answered above.
        if ask.items.iter().any(|item| item.capture.is_none()) {
            return Err(BotError::Invalid);
        }
        // 3 and 4. Write. Any failure below drops the transaction whole.
        for (index, item) in ask.items.iter().enumerate() {
            let submission = &item.intent.submission;
            let recipient = submission.provenance.bot;
            let capture = item.capture.as_ref().ok_or(BotError::Invalid)?;
            insert_chat(&tx, &item.chat).map_err(|_| BotError::Storage)?;
            insert_additional_chat(&tx, item.chat.id, recipient, now)?;
            // The same capture a task in the recipient's own conversation
            // commits with its admission, in this one transaction.
            super::super::library::prepare_library_submission_tx(
                &tx,
                &item.intent,
                capture,
                &item.fingerprint,
                item.expected_revision,
                now.millis(),
            )?;
            crate::repository::bot_replay::prepare_bot_association_on(
                &tx,
                &koru_ai_core::bots::replay::BotChatPreparation::Turn(item.turn.clone()),
                submission,
                now,
            )
            .map_err(|_| BotError::Conflict)?;
            tx.execute(
                "INSERT INTO bot_child_chats(user_scope_id,child_request_id,parent_bot_id,parent_chat_id,parent_run_id,parent_assistant_message_id,ask_key,ordinal,ask_fingerprint,wait_id,chat_id,created_at_ms)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    owner.user_scope.to_storage(),
                    item.child_request.to_storage(),
                    ask.parent_bot.to_storage(),
                    ask.parent_chat.to_storage(),
                    ask.parent_run.to_storage(),
                    ask.parent_assistant_message.to_storage(),
                    ask.ask_key.to_string(),
                    index as i64,
                    ask.ask_fingerprint,
                    ask.wait.to_string(),
                    item.chat.id.to_storage(),
                    now.millis()
                ],
            )
            .map_err(bad)?;
        }
        let rows = chats_of(ask_on(&tx, owner, ask.parent_run, ask.ask_key)?);
        if rows.len() != ask.items.len() {
            return Err(BotError::Storage);
        }
        tx.commit().map_err(bad)?;
        Ok(BotChildAskPrepared::Prepared(rows))
    }

    /// The stored rows of one ask in ordinal order, or none. For the restart
    /// scan that resends a prepared ask and for a replayed call.
    pub fn bot_child_ask(
        &self,
        owner: BotOwner,
        parent_run: koru_delegate_core::RunId,
        ask_key: Uuid,
    ) -> Result<Vec<BotChildChat>, BotError> {
        Ok(chats_of(ask_on(
            &self.connection,
            owner,
            parent_run,
            ask_key,
        )?))
    }

    /// One request by its id, in any stage.
    pub fn bot_child_chat(
        &self,
        owner: BotOwner,
        child_request: ChildRequestId,
    ) -> Result<Option<BotChildChat>, BotError> {
        self.connection
            .query_row(
                &format!("SELECT {COLUMNS} FROM {JOINED} WHERE cc.user_scope_id=?1 AND cc.child_request_id=?2"),
                params![owner.user_scope.to_storage(), child_request.to_storage()],
                decode,
            )
            .optional()
            .map_err(bad)
            .map(|row| row.map(|(row, _)| row))
    }

    /// The admitted requests made from one conversation of a Bot, oldest
    /// first: the newest `limit` of them, through the parent index. A
    /// prepared or refused ask is not among them.
    pub fn bot_child_requests_of_chat(
        &self,
        owner: BotOwner,
        bot: BotId,
        chat: ChatId,
        limit: usize,
    ) -> Result<Vec<BotChildChat>, BotError> {
        let mut statement = self
            .connection
            .prepare(&format!(
                "SELECT {COLUMNS} FROM bot_child_chats AS cc INDEXED BY bot_child_chats_parent
                 CROSS JOIN bot_command_receipts AS r ON r.user_scope_id=cc.user_scope_id AND r.request_id=cc.child_request_id
                 WHERE cc.parent_chat_id=?1 AND cc.user_scope_id=?2 AND cc.parent_bot_id=?3 AND {ADMITTED}
                 ORDER BY cc.parent_chat_id DESC,cc.created_at_ms DESC,cc.child_request_id DESC LIMIT ?4"
            ))
            .map_err(bad)?;
        let mut rows = statement
            .query_map(
                params![
                    chat.to_storage(),
                    owner.user_scope.to_storage(),
                    bot.to_storage(),
                    limit.clamp(1, BOT_CHILD_CARD_LIMIT) as i64
                ],
                decode,
            )
            .map_err(bad)?
            .map(|row| row.map(|(row, _)| row))
            .collect::<Result<Vec<_>, _>>()
            .map_err(bad)?;
        // Oldest first, and the items of one ask in the order they were named.
        rows.reverse();
        rows.sort_by_key(|row| (row.created_at_ms, row.parent_run, row.ask_key, row.ordinal));
        Ok(rows)
    }

    /// Who asked for this conversation, when an admitted request started it.
    pub fn bot_child_origin_of_chat(
        &self,
        owner: BotOwner,
        bot: BotId,
        chat: ChatId,
    ) -> Result<Option<BotChildChat>, BotError> {
        self.connection
            .query_row(
                &format!("SELECT {COLUMNS} FROM {JOINED} WHERE cc.chat_id=?1 AND cc.user_scope_id=?2 AND r.bot_id=?3 AND {ADMITTED}"),
                params![
                    chat.to_storage(),
                    owner.user_scope.to_storage(),
                    bot.to_storage()
                ],
                decode,
            )
            .optional()
            .map_err(bad)
            .map(|row| row.map(|(row, _)| row))
    }

    /// The delegate admitted the ask (`created` or `already_created`): in one
    /// transaction, store the wait's enrollment from the intent it returned
    /// and mark every recipient's receipt accepted with its admission. Only
    /// after this commits may the tool's answer be produced. Exact replay
    /// changes nothing and answers the same.
    pub fn accept_bot_child_ask(
        &mut self,
        owner: BotOwner,
        parent_run: koru_delegate_core::RunId,
        ask_key: Uuid,
        enrollment: &BotChildWaitCleanupIntentV1,
        admissions: &[koru_delegate_core::BotAdmissionView],
        now: Timestamp,
    ) -> Result<(BotChildWaitEnrollmentReply, Vec<BotChildChat>), BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let rows = chats_of(ask_on(&tx, owner, parent_run, ask_key)?);
        let first = rows.first().ok_or(BotError::NotFound)?;
        if enrollment.wait_id != first.wait
            || enrollment.run != parent_run
            || enrollment.bot != first.parent_bot
            || admissions.len() != rows.len()
        {
            return Err(BotError::Conflict);
        }
        let reply = super::enrollment::enroll_on(&tx, owner, enrollment, now)?;
        for row in &rows {
            let request = BotRequestId::from_uuid(row.child_request.as_uuid());
            let admission = admissions
                .iter()
                .find(|view| view.key.request == request)
                .ok_or(BotError::Conflict)?;
            match row.stage {
                BotReceiptStage::Prepared => {
                    admission::acknowledge_submission_on(
                        &tx,
                        owner,
                        row.recipient_bot,
                        request,
                        admission,
                        now.millis(),
                    )?;
                }
                // Already settled by an earlier delivery of the same reply.
                BotReceiptStage::Accepted | BotReceiptStage::Completed => {
                    if admission.run != row.child_run {
                        return Err(BotError::Conflict);
                    }
                }
                BotReceiptStage::Rejected => return Err(BotError::Conflict),
            }
        }
        let rows = chats_of(ask_on(&tx, owner, parent_run, ask_key)?);
        tx.commit().map_err(bad)?;
        Ok((reply, rows))
    }

    /// The delegate refused the ask and wrote nothing: mark every recipient's
    /// receipt rejected and keep the code the refusal is answered with, so
    /// the same key answers the same refusal later. `code` is a stable code,
    /// never a message. The conversations stay in the database and are
    /// listed nowhere. Exact replay changes nothing.
    pub fn reject_bot_child_ask(
        &mut self,
        owner: BotOwner,
        parent_run: koru_delegate_core::RunId,
        ask_key: Uuid,
        code: &str,
        ordinal: Option<u8>,
        reason: BotAdmissionRefusal,
        now: Timestamp,
    ) -> Result<Vec<BotChildChat>, BotError> {
        if code.is_empty()
            || code.len() > 64
            || !code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        {
            return Err(BotError::Invalid);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let rows = chats_of(ask_on(&tx, owner, parent_run, ask_key)?);
        if rows.is_empty() {
            return Err(BotError::NotFound);
        }
        if ordinal.is_some_and(|ordinal| usize::from(ordinal) >= rows.len()) {
            return Err(BotError::Invalid);
        }
        if rows
            .iter()
            .all(|row| row.stage == BotReceiptStage::Rejected)
        {
            return if rows
                .iter()
                .all(|row| row.refusal.as_deref() == Some(code) && row.refusal_ordinal == ordinal)
            {
                Ok(rows)
            } else {
                Err(BotError::Conflict)
            };
        }
        // An ask is admitted or refused whole: a receipt the delegate already
        // accepted cannot be refused here.
        if rows
            .iter()
            .any(|row| row.stage != BotReceiptStage::Prepared)
        {
            return Err(BotError::Conflict);
        }
        let outcome = serde_json::to_string(&BotExecutionOutcome::Rejected { version: 1, reason })
            .map_err(bad)?;
        for row in &rows {
            let request = BotRequestId::from_uuid(row.child_request.as_uuid());
            // Release a first-task folder reservation this request took.
            tx.execute(
                "UPDATE bots SET folder_lock_request_id=NULL WHERE bot_id=?1 AND folder_lock_request_id=?2 AND folder_frozen_at_ms IS NULL",
                params![row.recipient_bot.to_storage(), request.to_storage()],
            )
            .map_err(bad)?;
            let changed = tx
                .execute(
                    "UPDATE bot_command_receipts SET stage='rejected',outcome_json=?3,updated_at_ms=?4 WHERE user_scope_id=?1 AND request_id=?2 AND stage='prepared' AND command_kind='start'",
                    params![
                        owner.user_scope.to_storage(),
                        request.to_storage(),
                        outcome,
                        now.millis()
                    ],
                )
                .map_err(bad)?;
            if changed != 1 {
                return Err(BotError::Conflict);
            }
        }
        tx.execute(
            "UPDATE bot_child_chats SET refusal=?4,refusal_ordinal=?5 WHERE user_scope_id=?1 AND parent_run_id=?2 AND ask_key=?3",
            params![
                owner.user_scope.to_storage(),
                parent_run.to_storage(),
                ask_key.to_string(),
                code,
                ordinal
            ],
        )
        .map_err(bad)?;
        let rows = chats_of(ask_on(&tx, owner, parent_run, ask_key)?);
        tx.commit().map_err(bad)?;
        Ok(rows)
    }

    /// Whether this conversation's reply waits on other Bots, as ai.db alone
    /// knows it: a child wait is enrolled for the conversation's pending
    /// reply and no successor has been installed for it. `recipients` are the Bots asked whose own conversation still has a
    /// reply in progress. Bounded per conversation: one pending reply, the
    /// waits of its Run, and at most three requests of a wait.
    pub fn bot_child_waiting(
        &self,
        owner: BotOwner,
        bot: BotId,
        chat: ChatId,
    ) -> Result<Option<BotChildWaitingChat>, BotError> {
        let found: Option<(String, String, bool)> = self
            .connection
            .query_row(
                "SELECT e.wait_id, l.run_id, b.paused
                   FROM messages AS p INDEXED BY one_pending_assistant_per_chat
                  CROSS JOIN bot_chat_runs AS l ON l.assistant_message_id = p.id
                  CROSS JOIN bots AS b ON b.bot_id = l.bot_id
                  CROSS JOIN bot_child_wait_enrollments AS e INDEXED BY bot_child_wait_enrollments_run
                          ON e.run_id = l.run_id AND e.ai_attempt_id = l.ai_attempt_id
                  WHERE p.chat_id = ?1 AND p.state = 'pending' AND p.role = 'assistant'
                    AND l.bot_id = ?2 AND l.user_scope_id = ?3 AND l.owner_device_id = ?4
                    AND NOT EXISTS(SELECT 1 FROM bot_child_replay_rollovers AS o WHERE o.wait_id = e.wait_id)
                  ORDER BY e.created_at_ms DESC, e.wait_id LIMIT 1",
                params![
                    chat.to_storage(),
                    bot.to_storage(),
                    owner.user_scope.to_storage(),
                    owner.device.to_storage()
                ],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(bad)?;
        let Some((wait, run, paused)) = found else {
            return Ok(None);
        };
        let mut statement = self
            .connection
            .prepare(
                "SELECT cc.child_request_id, r.bot_id,
                        EXISTS(SELECT 1 FROM messages AS m INDEXED BY one_pending_assistant_per_chat
                                WHERE m.chat_id = cc.chat_id AND m.state = 'pending' AND m.role = 'assistant')
                   FROM bot_child_chats AS cc
                  CROSS JOIN bot_command_receipts AS r ON r.user_scope_id = cc.user_scope_id AND r.request_id = cc.child_request_id
                  WHERE cc.user_scope_id = ?1 AND cc.parent_run_id = ?2 AND cc.wait_id = ?3
                    AND r.stage IN ('accepted','completed')
                  ORDER BY cc.ordinal LIMIT 3",
            )
            .map_err(bad)?;
        let rows = statement
            .query_map(params![owner.user_scope.to_storage(), run, wait], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            })
            .map_err(bad)?;
        let mut waiting = BotChildWaitingChat {
            bot,
            chat,
            parent_run: run.parse().map_err(bad)?,
            paused,
            recipients: Vec::new(),
            requests: Vec::new(),
        };
        for row in rows {
            let (request, recipient, working) = row.map_err(bad)?;
            if !working {
                continue;
            }
            let recipient: BotId = recipient.parse().map_err(bad)?;
            if !waiting.recipients.contains(&recipient) {
                waiting.recipients.push(recipient);
            }
            waiting.requests.push(request.parse().map_err(bad)?);
        }
        Ok(Some(waiting))
    }
}
