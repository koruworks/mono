// SPDX-License-Identifier: MPL-2.0

//! Reading the feed: the markers above a cursor, and for each the subject as
//! it is now, in the shapes the local public API already answers with.
//!
//! **What is never built.** A record of a Bot that is not this computer's
//! (another device, another person), of a Bot whose home workspace the caller
//! did not name as copied, of a private conversation, of a conversation one
//! Bot opened in another that its owner has not admitted, or of a message of
//! such a conversation. Each is answered as removed, whatever its marker
//! says. A tool call's arguments and results are not read into a record at
//! all.
//!
//! **Every table a record is built from is named in
//! [`FEED_RECORD_TABLES`].** The test audit refuses to start when one of them
//! has no audit trigger, so a builder cannot grow a read of a table whose
//! writes nothing watches.

use std::collections::{BTreeSet, HashMap};

use super::*;

/// The tables a record reads. `bot_child_chats` and `bot_command_receipts`
/// decide whether a conversation is shown, and `bot_command_receipts` whether
/// a reply names its Run.
pub const FEED_RECORD_TABLES: &[&str] = &[
    "bots",
    "bot_selected_pages",
    "bot_connection_grants",
    "bot_chats",
    "bot_chat_trash",
    "bot_chat_runs",
    "bot_child_chats",
    "bot_command_receipts",
    "bot_historical_run_refs",
    "bot_team_pause",
    "chats",
    "messages",
    "message_blocks",
    "message_citations",
    "message_attachments",
    "inference_attempts",
    "tool_invocations",
    "agent_notices",
    "feedback_events",
];

/// The kind of a feed entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FeedKind {
    Bot,
    Chat,
    Message,
    TeamPause,
}

impl FeedKind {
    /// The kind as a feed entry spells it.
    pub const fn token(self) -> &'static str {
        match self {
            Self::Bot => "bot",
            Self::Chat => "chat",
            Self::Message => "message",
            Self::TeamPause => "team_pause",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "bot" => Some(Self::Bot),
            "chat" => Some(Self::Chat),
            "message" => Some(Self::Message),
            "team_pause" => Some(Self::TeamPause),
            _ => None,
        }
    }
}

/// Markers that are neither removed nor withheld, by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeedCounts {
    pub bots: u32,
    pub chats: u32,
    pub messages: u32,
}

/// One marker: identities, two flags and its place in the feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedMarker {
    pub seq: u64,
    pub kind: FeedKind,
    pub subject_id: String,
    pub bot_id: Option<BotId>,
    pub chat_id: Option<ChatId>,
    pub removed: bool,
    pub withheld: bool,
}

/// A Bot's home workspace, whether the Bot is this computer's, and what its
/// marker says: what a reconcile compares with the Store's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedBotHome {
    pub bot: BotId,
    pub home: WorkspaceId,
    /// The Bot's owner and device are the ones asked about.
    pub local: bool,
    /// The marker's `withheld`.
    pub withheld: bool,
}

/// One conversation as a record shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct FeedChat {
    /// The conversation, without its Bot label: the Bot's name is on the
    /// Bot's own record, so renaming a Bot moves one marker, not one per
    /// conversation.
    pub chat: Chat,
    pub bot_id: BotId,
    pub purpose: BotChatPurpose,
    pub trashed: bool,
    pub running: bool,
    pub needs_you: bool,
    /// The Run that is writing the conversation's running reply, and the
    /// workspace its record is in. Absent when no reply is running.
    pub run: Option<(koru_delegate_core::RunId, WorkspaceId)>,
}

/// One message with what the conversation shows beside it.
#[derive(Clone, PartialEq)]
pub struct FeedMessage {
    /// A pending reply's text is not read: `markdown` is empty.
    pub message: Message,
    /// Empty for a pending reply.
    pub blocks: Vec<MessageBlock>,
    /// Without `agent_input` and `agent_output`: the columns are not read.
    pub tool_activities: Vec<ToolActivity>,
    pub notices: Vec<AgentNotice>,
    pub feedback: Vec<FeedbackEvent>,
    /// The attempt that wrote a reply that has ended; `None` for a pending
    /// reply and for a message no attempt wrote.
    pub attempt: Option<InferenceAttempt>,
    /// The Run `read_run` accepts for a reply that has ended.
    pub run: Option<koru_delegate_core::RunId>,
    /// Empty for a pending reply.
    pub sources: Vec<SourceLink>,
    pub attachment_count: u32,
}

/// What an entry carries.
#[derive(Clone, PartialEq)]
pub enum FeedBody {
    /// The subject is gone, is not shown, or is withheld.
    Removed,
    Bot(Box<Bot>),
    Chat(Box<FeedChat>),
    Message(Box<FeedMessage>),
    Team(Box<TeamPauseView>),
}

/// One marker and its subject.
#[derive(Clone, PartialEq)]
pub struct FeedItem {
    pub marker: FeedMarker,
    pub body: FeedBody,
}

/// Markers above a cursor, in `seq` order, each with its subject.
#[derive(Clone, PartialEq)]
pub struct FeedScan {
    pub items: Vec<FeedItem>,
    /// The feed's newest `seq` when the scan was made.
    pub head: u64,
    /// No marker lies above the last item: the reader is at the head.
    pub exhausted: bool,
    /// A marker and the caller's answer disagreed about a Bot being
    /// withheld. The caller reconciles; the entry was answered as removed
    /// either way.
    pub stale_withheld: bool,
}

macro_rules! content_free_debug {
    ($name:ident) => {
        impl std::fmt::Debug for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter
                    .debug_struct(stringify!($name))
                    .finish_non_exhaustive()
            }
        }
    };
}
content_free_debug!(FeedChat);
content_free_debug!(FeedMessage);

impl std::fmt::Debug for FeedBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Removed => "Removed",
            Self::Bot(_) => "Bot",
            Self::Chat(_) => "Chat",
            Self::Message(_) => "Message",
            Self::Team(_) => "Team",
        })
    }
}

impl std::fmt::Debug for FeedItem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FeedItem")
            .field("seq", &self.marker.seq)
            .field("kind", &self.marker.kind)
            .field("body", &self.body)
            .finish()
    }
}

impl std::fmt::Debug for FeedScan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FeedScan")
            .field("items", &self.items.len())
            .field("head", &self.head)
            .field("exhausted", &self.exhausted)
            .field("stale_withheld", &self.stale_withheld)
            .finish()
    }
}

const MARKERS_SQL: &str = "SELECT seq, kind, subject_id, bot_id, chat_id, removed, withheld
   FROM bot_client_feed WHERE seq > ?1 ORDER BY seq LIMIT ?2";

/// One conversation's record. `running` and `needs_you` are the two values
/// `BOT_ACTIVITY_SQL` computes, for this conversation alone.
const CHAT_RECORD_SQL: &str = "SELECT c.id, c.title, c.current_provider, c.current_model, c.current_reasoning,
        c.branched_from_chat_id, c.branched_from_message_id, c.created_at_ms, c.updated_at_ms,
        c.private, c.agent_working_folder, c.agent_mode, c.agent_fast, c.pinned_at_ms, c.profile_id,
        bc.bot_id, bc.purpose,
        EXISTS(SELECT 1 FROM bot_chat_trash AS t WHERE t.chat_id = bc.chat_id),
        EXISTS(SELECT 1 FROM messages AS m INDEXED BY one_pending_assistant_per_chat
                WHERE m.chat_id = bc.chat_id AND m.state = 'pending' AND m.role = 'assistant'),
        (EXISTS(SELECT 1 FROM tool_invocations AS t INDEXED BY tool_invocations_by_attempt
                 WHERE t.attempt_id = (SELECT a.id FROM inference_attempts AS a INDEXED BY inference_attempts_by_chat
                                        WHERE a.chat_id = bc.chat_id ORDER BY a.queued_at_ms DESC LIMIT 1)
                   AND t.status = 'approval_required')
         OR EXISTS(SELECT 1 FROM messages AS p INDEXED BY one_pending_assistant_per_chat
                    CROSS JOIN bot_chat_runs AS r ON r.assistant_message_id = p.id
                    CROSS JOIN tool_invocations AS pt INDEXED BY tool_invocations_by_attempt
                            ON pt.attempt_id = r.ai_attempt_id
                   WHERE p.chat_id = bc.chat_id AND p.state = 'pending' AND p.role = 'assistant'
                     AND pt.status = 'approval_required')),
        (c.private = 1 OR EXISTS(SELECT 1 FROM bot_child_chats AS cc
            CROSS JOIN bot_command_receipts AS cr
                    ON cr.user_scope_id = cc.user_scope_id AND cr.request_id = cc.child_request_id
           WHERE cc.chat_id = bc.chat_id AND cr.stage NOT IN ('accepted','completed'))),
        (SELECT r.run_id FROM messages AS p INDEXED BY one_pending_assistant_per_chat
           CROSS JOIN bot_chat_runs AS r ON r.assistant_message_id = p.id
          WHERE p.chat_id = bc.chat_id AND p.state = 'pending' AND p.role = 'assistant'),
        (SELECT r.record_workspace_id FROM messages AS p INDEXED BY one_pending_assistant_per_chat
           CROSS JOIN bot_chat_runs AS r ON r.assistant_message_id = p.id
          WHERE p.chat_id = bc.chat_id AND p.state = 'pending' AND p.role = 'assistant')
   FROM bot_chats AS bc
  CROSS JOIN chats AS c ON c.id = bc.chat_id
  WHERE bc.chat_id = ?1";

/// A message and whether its conversation is shown. The text is not read for
/// a pending reply.
const MESSAGE_RECORD_SQL: &str = "SELECT m.id, m.chat_id, m.ordinal, m.role, m.state,
        CASE WHEN m.state = 'pending' THEN '' ELSE m.markdown END, m.origin_message_id,
        m.provider, m.requested_model, m.resolved_model, m.error_class,
        m.created_at_ms, m.completed_at_ms,
        bc.bot_id,
        (c.private = 1 OR EXISTS(SELECT 1 FROM bot_child_chats AS cc
            CROSS JOIN bot_command_receipts AS cr
                    ON cr.user_scope_id = cc.user_scope_id AND cr.request_id = cc.child_request_id
           WHERE cc.chat_id = bc.chat_id AND cr.stage NOT IN ('accepted','completed')))
   FROM messages AS m
  CROSS JOIN bot_chats AS bc ON bc.chat_id = m.chat_id
  CROSS JOIN chats AS c ON c.id = bc.chat_id
  WHERE m.id = ?1";

/// The tool calls of one attempt, as [`tool_activity_from_row`] reads them,
/// with NULL where a call's arguments and results are stored.
const TOOL_RECORD_SQL: &str = "SELECT id, attempt_id, chat_id, tool_name, read_only,
    status, approval, proposed_at_ms, started_at_ms, completed_at_ms,
    result_bytes, error_code, target_workspace_id, target_document_id, error_message,
    agent_label, agent_summary, NULL, NULL, ?2, text_offset
   FROM tool_invocations INDEXED BY tool_invocations_by_attempt
  WHERE attempt_id = ?1 ORDER BY proposed_at_ms, id";

/// What a scan knows of a Bot: whether it is this computer's, and whether
/// its home workspace is copied.
#[derive(Clone, Copy)]
struct Served {
    local: bool,
    copied: bool,
}

impl Repository {
    /// The home workspaces of the Bots whose markers are among the next
    /// `limit` above `cursor`: what a page asks the Store about before it is
    /// built.
    pub fn feed_page_homes(&self, cursor: u64, limit: u16) -> Result<Vec<WorkspaceId>, BotError> {
        let mut statement = self
            .connection
            .prepare_cached(
                "SELECT DISTINCT b.home_workspace_id FROM
                   (SELECT bot_id FROM bot_client_feed WHERE seq > ?1 ORDER BY seq LIMIT ?2) AS f
                   CROSS JOIN bots AS b ON b.bot_id = f.bot_id",
            )
            .map_err(bad)?;
        let rows = statement
            .query_map(params![i64::try_from(cursor).map_err(bad)?, limit], |row| {
                row.get::<_, String>(0)
            })
            .map_err(bad)?;
        rows.map(|row| WorkspaceId::parse(&row.map_err(bad)?).ok_or(BotError::Storage))
            .collect()
    }

    /// Every Bot that has a marker, with its home workspace and what its
    /// marker says of it: one row per Bot, through the marker's unique index
    /// and the Bot's primary key. What a reconcile starts from.
    pub fn feed_bot_homes(&self, owner: BotOwner) -> Result<Vec<FeedBotHome>, BotError> {
        let mut statement = self
            .connection
            .prepare_cached(
                "SELECT b.bot_id, b.home_workspace_id, b.user_scope_id = ?1 AND b.owner_device_id = ?2, f.withheld
                   FROM bot_client_feed AS f CROSS JOIN bots AS b ON b.bot_id = f.subject_id
                  WHERE f.kind = 'bot' ORDER BY f.seq",
            )
            .map_err(bad)?;
        let rows = statement
            .query_map(
                params![owner.user_scope.to_storage(), owner.device.to_storage()],
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
        rows.map(|row| {
            let (bot, home, local, withheld) = row.map_err(bad)?;
            Ok(FeedBotHome {
                bot: bot.parse().map_err(bad)?,
                home: WorkspaceId::parse(&home).ok_or(BotError::Storage)?,
                local,
                withheld,
            })
        })
        .collect()
    }

    /// At most `limit` markers above `cursor`, in `seq` order, each with its
    /// subject as it is now. The scan also stops once the text it has read
    /// passes `text_budget` bytes, so one call never holds a long history in
    /// memory; it always answers at least one item when one exists.
    ///
    /// `owner` is the person and computer whose Bots these are. `copied` are
    /// the home workspaces the Store said are not kept on this computer
    /// only. A Bot of anybody else, or whose home is not in `copied`, is
    /// answered as removed, and so is every conversation and message of it.
    ///
    /// One seek of the markers by `seq`. Per message: its row, its
    /// conversation, its blocks, its feedback, its citations and, for a
    /// reply, its attempt and that attempt's tool calls, each through an
    /// index. A conversation's notices are read once per scan.
    pub fn feed_scan(
        &self,
        owner: BotOwner,
        cursor: u64,
        limit: u16,
        text_budget: usize,
        copied: &BTreeSet<WorkspaceId>,
    ) -> Result<FeedScan, BotError> {
        if limit == 0 {
            return Err(BotError::Invalid);
        }
        let head = self.feed_head()?;
        let markers = self.feed_markers(cursor, limit)?;
        let mut scan = FeedScan {
            items: Vec::with_capacity(markers.len()),
            head,
            exhausted: markers.last().is_none_or(|last| last.seq >= head),
            stale_withheld: false,
        };
        let mut served: HashMap<BotId, Option<Served>> = HashMap::new();
        let mut notices: HashMap<ChatId, Vec<AgentNotice>> = HashMap::new();
        let mut text = 0usize;
        let count = markers.len();
        for (index, marker) in markers.into_iter().enumerate() {
            let shown = match marker.bot_id {
                None => true,
                Some(bot) => {
                    let known = match served.get(&bot) {
                        Some(known) => *known,
                        None => {
                            let known = self.feed_served(owner, bot, copied)?;
                            served.insert(bot, known);
                            known
                        }
                    };
                    let shown = known.is_some_and(|bot| bot.local && bot.copied);
                    if marker.withheld == shown {
                        scan.stale_withheld = true;
                    }
                    shown && !marker.withheld
                }
            };
            let body = if marker.removed || !shown {
                FeedBody::Removed
            } else {
                self.feed_body(owner, &marker, &mut notices)?
            };
            if let FeedBody::Message(message) = &body {
                text = text
                    .saturating_add(message.message.markdown.len())
                    .saturating_add(
                        message
                            .blocks
                            .iter()
                            .map(|block| block.markdown.len() + block.plain_text.len())
                            .sum::<usize>(),
                    );
            }
            scan.items.push(FeedItem { marker, body });
            if text > text_budget && index + 1 < count {
                scan.exhausted = false;
                break;
            }
        }
        Ok(scan)
    }

    fn feed_markers(&self, cursor: u64, limit: u16) -> Result<Vec<FeedMarker>, BotError> {
        let mut statement = self.connection.prepare_cached(MARKERS_SQL).map_err(bad)?;
        let rows = statement
            .query_map(params![i64::try_from(cursor).map_err(bad)?, limit], |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, bool>(5)?,
                    row.get::<_, bool>(6)?,
                ))
            })
            .map_err(bad)?;
        rows.map(|row| {
            let (seq, kind, subject_id, bot, chat, removed, withheld) = row.map_err(bad)?;
            Ok(FeedMarker {
                seq,
                kind: FeedKind::parse(&kind).ok_or(BotError::Storage)?,
                subject_id,
                bot_id: bot.map(|bot| bot.parse().map_err(bad)).transpose()?,
                chat_id: chat
                    .map(|chat| ChatId::parse(&chat).ok_or(BotError::Storage))
                    .transpose()?,
                removed,
                withheld,
            })
        })
        .collect()
    }

    fn feed_served(
        &self,
        owner: BotOwner,
        bot: BotId,
        copied: &BTreeSet<WorkspaceId>,
    ) -> Result<Option<Served>, BotError> {
        let row: Option<(String, bool)> = self
            .connection
            .prepare_cached(
                "SELECT home_workspace_id, user_scope_id = ?2 AND owner_device_id = ?3
                   FROM bots WHERE bot_id = ?1",
            )
            .map_err(bad)?
            .query_row(
                params![
                    bot.to_storage(),
                    owner.user_scope.to_storage(),
                    owner.device.to_storage()
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(bad)?;
        row.map(|(home, local)| {
            let home = WorkspaceId::parse(&home).ok_or(BotError::Storage)?;
            Ok(Served {
                local,
                copied: copied.contains(&home),
            })
        })
        .transpose()
    }

    fn feed_body(
        &self,
        owner: BotOwner,
        marker: &FeedMarker,
        notices: &mut HashMap<ChatId, Vec<AgentNotice>>,
    ) -> Result<FeedBody, BotError> {
        match marker.kind {
            FeedKind::Bot => {
                let id: BotId = marker.subject_id.parse().map_err(bad)?;
                Ok(match self.bot_any_lifecycle(id)? {
                    // Asked again on the row itself: a marker is not proof of
                    // whose Bot this is.
                    Some((bot, _))
                        if bot.user_scope == owner.user_scope
                            && bot.owner_device == owner.device =>
                    {
                        FeedBody::Bot(Box::new(bot))
                    }
                    _ => FeedBody::Removed,
                })
            }
            FeedKind::Chat => Ok(self
                .feed_chat(&marker.subject_id)?
                .map_or(FeedBody::Removed, |chat| FeedBody::Chat(Box::new(chat)))),
            FeedKind::Message => Ok(self
                .feed_message(&marker.subject_id, notices)?
                .map_or(FeedBody::Removed, |message| {
                    FeedBody::Message(Box::new(message))
                })),
            FeedKind::TeamPause => {
                if marker.subject_id != owner.device.to_storage() {
                    return Ok(FeedBody::Removed);
                }
                Ok(FeedBody::Team(Box::new(self.team_pause_view(owner)?)))
            }
        }
    }

    fn feed_chat(&self, chat: &str) -> Result<Option<FeedChat>, BotError> {
        let row = self
            .connection
            .prepare_cached(CHAT_RECORD_SQL)
            .map_err(bad)?
            .query_row([chat], |row| {
                Ok((
                    chat_from_row(row)?,
                    row.get::<_, String>(15)?,
                    row.get::<_, String>(16)?,
                    row.get::<_, bool>(17)?,
                    row.get::<_, bool>(18)?,
                    row.get::<_, bool>(19)?,
                    row.get::<_, bool>(20)?,
                    row.get::<_, Option<String>>(21)?,
                    row.get::<_, Option<String>>(22)?,
                ))
            })
            .optional()
            .map_err(bad)?;
        let Some((chat, bot, purpose, trashed, running, needs_you, hidden, run, workspace)) = row
        else {
            return Ok(None);
        };
        if hidden {
            return Ok(None);
        }
        let run = match (run, workspace) {
            (Some(run), Some(workspace)) => Some((
                run.parse().map_err(bad)?,
                WorkspaceId::parse(&workspace).ok_or(BotError::Storage)?,
            )),
            _ => None,
        };
        Ok(Some(FeedChat {
            chat,
            bot_id: bot.parse().map_err(bad)?,
            purpose: BotChatPurpose::parse(&purpose).ok_or(BotError::Storage)?,
            trashed,
            running,
            needs_you,
            run,
        }))
    }

    fn feed_message(
        &self,
        message: &str,
        notices: &mut HashMap<ChatId, Vec<AgentNotice>>,
    ) -> Result<Option<FeedMessage>, BotError> {
        let row = self
            .connection
            .prepare_cached(MESSAGE_RECORD_SQL)
            .map_err(bad)?
            .query_row([message], |row| {
                Ok((
                    message_from_row(row)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, bool>(14)?,
                ))
            })
            .optional()
            .map_err(bad)?;
        let Some((message, bot, hidden)) = row else {
            return Ok(None);
        };
        if hidden {
            return Ok(None);
        }
        let bot: BotId = bot.parse().map_err(bad)?;
        let pending = message.state == MessageState::Pending;
        let reply = message.role == ChatRole::Assistant;
        let id = message.id.to_storage();
        let chat = message.chat_id.to_storage();

        let attempt = if reply {
            self.feed_attempt(&chat, &id)?
        } else {
            None
        };
        let (tool_activities, attempt_notices) = match &attempt {
            Some(attempt) => {
                let tools = self.feed_tools(attempt.id, &id)?;
                if !notices.contains_key(&message.chat_id) {
                    let loaded = self
                        .agent_notices(message.chat_id)
                        .map_err(|_| BotError::Storage)?;
                    notices.insert(message.chat_id, loaded);
                }
                let own = notices
                    .get(&message.chat_id)
                    .map(|all| {
                        all.iter()
                            .filter(|notice| notice.attempt_id == attempt.id)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                (tools, own)
            }
            None => (Vec::new(), Vec::new()),
        };
        let blocks = if pending {
            Vec::new()
        } else {
            self.feed_rows(
                "SELECT id, message_id, ordinal, kind, markdown, plain_text
                   FROM message_blocks INDEXED BY message_blocks_by_message
                  WHERE message_id = ?1 ORDER BY ordinal",
                &id,
                block_from_row,
            )?
        };
        // The newest feedback on the message, unless it was cleared: what
        // `current_message_feedback` answers for one message.
        let feedback: Vec<FeedbackEvent> = self
            .feed_rows(
                "SELECT id, chat_id, message_id, attempt_id, value, surface, created_at_ms
                   FROM feedback_events INDEXED BY feedback_events_by_message
                  WHERE message_id = ?1 ORDER BY created_at_ms DESC, id DESC LIMIT 1",
                &id,
                feedback_from_row,
            )?
            .into_iter()
            .filter(|event| event.value != FeedbackValue::Cleared)
            .collect();
        let sources = if pending {
            Vec::new()
        } else {
            self.feed_rows(
                "SELECT url, title FROM message_citations WHERE message_id = ?1 ORDER BY ordinal",
                &id,
                |row| {
                    Ok(SourceLink {
                        url: row.get(0)?,
                        title: row.get(1)?,
                    })
                },
            )?
        };
        let run = if reply && !pending {
            self.bot_reply_runs(bot, message.chat_id, &[message.id])?
                .into_iter()
                .next()
                .map(|(_, run)| run)
        } else {
            None
        };
        let attachment_count: u32 = if reply {
            0
        } else {
            self.connection
                .prepare_cached(
                    "SELECT count(*) FROM message_attachments INDEXED BY message_attachments_by_chat
                      WHERE chat_id = ?1 AND message_id = ?2",
                )
                .map_err(bad)?
                .query_row([&chat, &id], |row| row.get(0))
                .map_err(bad)?
        };
        Ok(Some(FeedMessage {
            message,
            blocks,
            tool_activities,
            notices: attempt_notices,
            feedback,
            // A pending reply's attempt changes with every flush; its usage
            // is sent when the reply ends.
            attempt: attempt.filter(|_| !pending),
            run,
            sources,
            attachment_count,
        }))
    }

    /// The attempt that wrote a reply: through the Run's own row when a Bot
    /// Run wrote it (a unique index), otherwise among the conversation's
    /// attempts.
    fn feed_attempt(
        &self,
        chat: &str,
        message: &str,
    ) -> Result<Option<InferenceAttempt>, BotError> {
        let by_run = self
            .connection
            .prepare_cached(&format!(
                "SELECT {ATTEMPT_COLUMNS} FROM inference_attempts
                  WHERE id = (SELECT ai_attempt_id FROM bot_chat_runs WHERE assistant_message_id = ?1)"
            ))
            .map_err(bad)?
            .query_row([message], attempt_from_row)
            .optional()
            .map_err(bad)?;
        if by_run.is_some() {
            return Ok(by_run);
        }
        self.connection
            .prepare_cached(&format!(
                "SELECT {ATTEMPT_COLUMNS} FROM inference_attempts INDEXED BY inference_attempts_by_chat
                  WHERE chat_id = ?1 AND assistant_message_id = ?2
                  ORDER BY queued_at_ms DESC LIMIT 1"
            ))
            .map_err(bad)?
            .query_row([chat, message], attempt_from_row)
            .optional()
            .map_err(bad)
    }

    fn feed_tools(
        &self,
        attempt: InferenceAttemptId,
        message: &str,
    ) -> Result<Vec<ToolActivity>, BotError> {
        let mut statement = self
            .connection
            .prepare_cached(TOOL_RECORD_SQL)
            .map_err(bad)?;
        let rows = statement
            .query_map(
                params![attempt.to_storage(), message],
                tool_activity_from_row,
            )
            .map_err(bad)?;
        rows.map(|row| row.map_err(bad)).collect()
    }

    fn feed_rows<T>(
        &self,
        sql: &str,
        key: &str,
        read: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>, BotError> {
        let mut statement = self.connection.prepare_cached(sql).map_err(bad)?;
        let rows = statement.query_map([key], read).map_err(bad)?;
        rows.map(|row| row.map_err(bad)).collect()
    }
}
