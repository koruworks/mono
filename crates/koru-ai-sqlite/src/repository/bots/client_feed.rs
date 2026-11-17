// SPDX-License-Identifier: MPL-2.0

//! The owner feed (M8, P8.3): what a linked desktop reads this computer's
//! Bots from.
//!
//! **One marker per subject.** A Bot, a Bot conversation, a message of one
//! and the team pause each have at most one row in `bot_client_feed`. The
//! repository function that changes a subject replaces its marker in the
//! same transaction, which gives it a new, higher `seq`. A linked desktop
//! holds a cursor, a `seq`, and is sent every subject whose marker is above
//! it, as the subject is at that moment. Nothing is logged: a subject that
//! changed twenty times is sent once.
//!
//! **Written by the function that makes the change, not by a trigger.** Only
//! that function knows whether anything a record shows changed. A reply's
//! text arrives in hundreds of small writes that move no marker; the marker
//! moves when the reply ends. An ordinary chat pays one index seek where a
//! Bot conversation pays a marker, and nothing at all while text streams.
//!
//! **What a marker write costs.** Two statements for a subject that already
//! has a marker (`DELETE … RETURNING`, `INSERT … RETURNING`), three for a new
//! one (the counter of its kind). Each rewrites the marker's row and its two
//! index entries.
//!
//! **Nothing here can forget a write silently in a tested path.** The
//! in-memory database every test of this crate and of the AI worker opens
//! carries an audit ([`audit`]): a transaction that changes a row a record is
//! built from, and commits without moving that subject's marker, is refused.
//!
//! No value written here is a message, a title or a credential: a marker is
//! identities, two flags and a counter.

use super::*;

#[cfg(any(test, feature = "fixtures"))]
pub(crate) mod audit;
mod records;

pub use records::{
    FEED_RECORD_TABLES, FeedBody, FeedBotHome, FeedChat, FeedCounts, FeedItem, FeedKind,
    FeedMarker, FeedMessage, FeedScan,
};

/// The newest worker starts kept. A linked desktop that names an older one
/// is sent everything again.
pub const FEED_STARTS_KEPT: i64 = 256;

/// The kind of a marker, as the `kind` column and a feed entry spell it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Bot,
    Chat,
    Message,
    TeamPause,
}

impl Kind {
    pub(crate) const fn token(self) -> &'static str {
        match self {
            Self::Bot => "bot",
            Self::Chat => "chat",
            Self::Message => "message",
            Self::TeamPause => "team_pause",
        }
    }

    /// The counter a marker of this kind is counted in while it is neither
    /// removed nor withheld. The team pause is not counted: a linked desktop
    /// holds one or none.
    const fn count_up(self) -> Option<&'static str> {
        match self {
            Self::Bot => Some("UPDATE bot_client_feed_meta SET live_bots = live_bots + 1"),
            Self::Chat => Some("UPDATE bot_client_feed_meta SET live_chats = live_chats + 1"),
            Self::Message => {
                Some("UPDATE bot_client_feed_meta SET live_messages = live_messages + 1")
            }
            Self::TeamPause => None,
        }
    }

    const fn count_down(self) -> Option<&'static str> {
        match self {
            Self::Bot => Some("UPDATE bot_client_feed_meta SET live_bots = live_bots - 1"),
            Self::Chat => Some("UPDATE bot_client_feed_meta SET live_chats = live_chats - 1"),
            Self::Message => {
                Some("UPDATE bot_client_feed_meta SET live_messages = live_messages - 1")
            }
            Self::TeamPause => None,
        }
    }
}

/// Replace one subject's marker. `withheld` is `None` for a conversation or
/// a message, which take their Bot's, and for a Bot, which keeps its own.
fn write_marker(
    conn: &Connection,
    kind: Kind,
    subject: &str,
    bot: Option<&str>,
    chat: Option<&str>,
    removed: bool,
) -> rusqlite::Result<()> {
    let prior: Option<(bool, bool)> = conn
        .prepare_cached(
            "DELETE FROM bot_client_feed WHERE kind = ?1 AND subject_id = ?2
             RETURNING removed, withheld",
        )?
        .query_row(params![kind.token(), subject], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    // A Bot keeps the state its last reconcile gave it. Its own row is gone
    // by now, so the value is carried rather than read back.
    let own = match kind {
        Kind::Bot => Some(prior.is_some_and(|(_, withheld)| withheld)),
        Kind::Chat | Kind::Message | Kind::TeamPause => None,
    };
    let withheld: bool = conn
        .prepare_cached(
            "INSERT INTO bot_client_feed(kind, subject_id, bot_id, chat_id, removed, withheld)
             VALUES(?1, ?2, ?3, ?4, ?5, COALESCE(?6,
                (SELECT withheld FROM bot_client_feed WHERE kind = 'bot' AND subject_id = ?3), 0))
             RETURNING withheld",
        )?
        .query_row(
            params![kind.token(), subject, bot, chat, removed, own],
            |row| row.get(0),
        )?;
    let was = prior.is_some_and(|(removed, withheld)| !removed && !withheld);
    let is = !removed && !withheld;
    let count = match (was, is) {
        (false, true) => kind.count_up(),
        (true, false) => kind.count_down(),
        _ => None,
    };
    if let Some(count) = count {
        conn.prepare_cached(count)?.execute([])?;
    }
    Ok(())
}

/// Whether a conversation is one a linked desktop is shown: it is not
/// private, and it is not a conversation one Bot opened in another whose
/// owner has not admitted it. The same rule the Bots view lists by
/// (`BOT_CHAT_PAGE_SQL`).
const HIDDEN_SQL: &str = "(c.private = 1 OR EXISTS(SELECT 1 FROM bot_child_chats AS cc
      CROSS JOIN bot_command_receipts AS cr
              ON cr.user_scope_id = cc.user_scope_id AND cr.request_id = cc.child_request_id
     WHERE cc.chat_id = bc.chat_id AND cr.stage NOT IN ('accepted','completed')))";

/// A Bot conversation's Bot and whether it is hidden; `None` for an ordinary
/// chat, which is the one seek an ordinary chat pays.
fn conversation(conn: &Connection, chat: &str) -> rusqlite::Result<Option<(String, bool)>> {
    conn.prepare_cached(&format!(
        "SELECT bc.bot_id, {HIDDEN_SQL} FROM bot_chats AS bc
          CROSS JOIN chats AS c ON c.id = bc.chat_id WHERE bc.chat_id = ?1"
    ))?
    .query_row([chat], |row| Ok((row.get(0)?, row.get(1)?)))
    .optional()
}

/// A Bot was created, or a value its record shows changed.
pub(crate) fn touch_bot(conn: &Connection, bot: BotId) -> rusqlite::Result<()> {
    let bot = bot.to_storage();
    write_marker(conn, Kind::Bot, &bot, Some(&bot), None, false)
}

/// A conversation's record changed. Nothing for an ordinary chat.
pub(crate) fn touch_chat(conn: &Connection, chat: ChatId) -> rusqlite::Result<()> {
    touch_turn(conn, chat, &[], true)
}

/// One message's record changed. Nothing for an ordinary chat.
pub(crate) fn touch_message(
    conn: &Connection,
    chat: ChatId,
    message: MessageId,
) -> rusqlite::Result<()> {
    touch_turn(conn, chat, &[message], false)
}

/// Messages of one conversation, and the conversation itself when `chat` is
/// set, with one lookup of the conversation for all of them.
pub(crate) fn touch_turn(
    conn: &Connection,
    chat_id: ChatId,
    messages: &[MessageId],
    chat: bool,
) -> rusqlite::Result<()> {
    let chat_id = chat_id.to_storage();
    let Some((bot, hidden)) = conversation(conn, &chat_id)? else {
        return Ok(());
    };
    for message in messages {
        write_marker(
            conn,
            Kind::Message,
            &message.to_storage(),
            Some(&bot),
            Some(&chat_id),
            hidden,
        )?;
    }
    if chat {
        write_marker(
            conn,
            Kind::Chat,
            &chat_id,
            Some(&bot),
            Some(&chat_id),
            hidden,
        )?;
    }
    Ok(())
}

/// A message known only by its id, and its conversation when `chat` is set.
/// Nothing when the message is gone or belongs to an ordinary chat.
pub(crate) fn touch_message_by_id(
    conn: &Connection,
    message: &str,
    chat: bool,
) -> rusqlite::Result<()> {
    let found: Option<(String, String, bool)> = conn
        .prepare_cached(&format!(
            "SELECT m.chat_id, bc.bot_id, {HIDDEN_SQL} FROM messages AS m
              CROSS JOIN bot_chats AS bc ON bc.chat_id = m.chat_id
              CROSS JOIN chats AS c ON c.id = bc.chat_id WHERE m.id = ?1"
        ))?
        .query_row([message], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .optional()?;
    let Some((chat_id, bot, hidden)) = found else {
        return Ok(());
    };
    write_marker(
        conn,
        Kind::Message,
        message,
        Some(&bot),
        Some(&chat_id),
        hidden,
    )?;
    if chat {
        write_marker(
            conn,
            Kind::Chat,
            &chat_id,
            Some(&bot),
            Some(&chat_id),
            hidden,
        )?;
    }
    Ok(())
}

/// The reply an attempt writes, and its conversation when `chat` is set.
pub(crate) fn touch_attempt(
    conn: &Connection,
    attempt: InferenceAttemptId,
    chat: bool,
) -> rusqlite::Result<()> {
    let message: Option<Option<String>> = conn
        .prepare_cached("SELECT assistant_message_id FROM inference_attempts WHERE id = ?1")?
        .query_row([attempt.to_storage()], |row| row.get(0))
        .optional()?;
    match message.flatten() {
        Some(message) => touch_message_by_id(conn, &message, chat),
        None => Ok(()),
    }
}

/// A conversation became a Bot's, or started or stopped being shown: its
/// marker and one for every message it already has. A branch inserts its
/// copied messages before it is a Bot's conversation, so none of them has a
/// marker until this runs.
pub(crate) fn touch_conversation(conn: &Connection, chat: ChatId) -> rusqlite::Result<()> {
    let chat_id = chat.to_storage();
    let Some((bot, hidden)) = conversation(conn, &chat_id)? else {
        return Ok(());
    };
    let messages: Vec<String> = conn
        .prepare_cached("SELECT id FROM messages WHERE chat_id = ?1 ORDER BY ordinal")?
        .query_map([&chat_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    write_marker(
        conn,
        Kind::Chat,
        &chat_id,
        Some(&bot),
        Some(&chat_id),
        hidden,
    )?;
    for message in messages {
        write_marker(
            conn,
            Kind::Message,
            &message,
            Some(&bot),
            Some(&chat_id),
            hidden,
        )?;
    }
    Ok(())
}

/// The team pause of the computer `device` changed.
pub(crate) fn touch_team(conn: &Connection, device: DeviceId) -> rusqlite::Result<()> {
    write_marker(
        conn,
        Kind::TeamPause,
        &device.to_storage(),
        None,
        None,
        false,
    )
}

/// A tool call's `status` and `approval` as stored, to tell a change of
/// value from a write that moves neither. One seek by primary key.
pub(crate) fn tool_state(
    conn: &Connection,
    invocation: ToolInvocationId,
) -> rusqlite::Result<Option<(String, String)>> {
    conn.prepare_cached("SELECT status, approval FROM tool_invocations WHERE id = ?1")?
        .query_row([invocation.to_storage()], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()
}

/// A tool call's stored `(status, approval)` before a write to it.
pub(crate) type ToolBefore = (String, String);

/// Before a tool call's row is written: its stored status and approval when
/// the call is a Bot conversation's. `None` for an ordinary chat, which is
/// then one seek; a Bot conversation's call is two.
pub(crate) fn tool_before(
    conn: &Connection,
    activity: &ToolActivity,
) -> rusqlite::Result<Option<ToolBefore>> {
    let bot: Option<String> = conn
        .prepare_cached("SELECT bot_id FROM bot_chats WHERE chat_id = ?1")?
        .query_row([activity.chat_id.to_storage()], |row| row.get(0))
        .optional()?;
    if bot.is_none() {
        return Ok(None);
    }
    tool_state(conn, activity.id)
}

/// After a tool call's row was written with `activity`: the reply's marker
/// when the call's status or approval changed value, and the conversation's
/// when the call started or stopped needing the person. A write that moves
/// neither, which is every progress update of a running call, moves nothing.
pub(crate) fn tool_after(
    conn: &Connection,
    before: Option<&ToolBefore>,
    activity: &ToolActivity,
) -> rusqlite::Result<()> {
    let Some(before) = before else {
        return Ok(());
    };
    let (message, chat) = tool_moved(before, activity);
    if !message {
        return Ok(());
    }
    touch_turn(
        conn,
        activity.chat_id,
        &[activity.assistant_message_id],
        chat,
    )
}

/// Whether a write of `activity` over `before` changes the reply's record,
/// and whether it changes the conversation's.
pub(crate) fn tool_moved(before: &ToolBefore, activity: &ToolActivity) -> (bool, bool) {
    let status = tool_activity_status_token(activity.status);
    let message = before.0 != status || before.1 != tool_approval_token(activity.approval);
    let waiting = "approval_required";
    (message, (before.0 == waiting) != (status == waiting))
}

/// The replies of Bot conversations that are pending or hold a tool call
/// that has not ended, by message id: what a restart is about to end. Two
/// reads of the kind the restart's own statements make.
pub(crate) fn open_replies(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT m.id FROM messages AS m JOIN bot_chats AS bc ON bc.chat_id = m.chat_id
          WHERE m.role = 'assistant' AND m.state = 'pending'
         UNION
         SELECT a.assistant_message_id FROM tool_invocations AS t
           JOIN bot_chats AS bc ON bc.chat_id = t.chat_id
           JOIN inference_attempts AS a ON a.id = t.attempt_id
          WHERE t.status IN ('proposed', 'approval_required', 'running')
            AND a.assistant_message_id IS NOT NULL",
    )?;
    statement.query_map([], |row| row.get(0))?.collect()
}

/// A task's submission was admitted by what runs the Bots. Two things a
/// linked desktop is shown follow from that: a conversation one Bot opened
/// in another is listed from then on, and a reply that had already ended
/// names its Run. Two seeks when neither applies, which is the usual case:
/// the reply is still pending and names its Run when it ends.
pub(crate) fn touch_admitted(
    conn: &Connection,
    scope: UserScopeId,
    request: BotRequestId,
) -> rusqlite::Result<()> {
    let scope = scope.to_storage();
    let request = request.to_storage();
    let child: Option<String> = conn
        .prepare_cached(
            "SELECT chat_id FROM bot_child_chats WHERE user_scope_id = ?1 AND child_request_id = ?2",
        )?
        .query_row([&scope, &request], |row| row.get(0))
        .optional()?;
    if let Some(chat) = child.as_deref().and_then(ChatId::parse) {
        return touch_conversation(conn, chat);
    }
    let ended: Option<String> = conn
        .prepare_cached(
            "SELECT m.id FROM bot_chat_runs AS r CROSS JOIN messages AS m ON m.id = r.assistant_message_id
              WHERE r.user_scope_id = ?1 AND r.submission_id = ?2 AND m.state <> 'pending'",
        )?
        .query_row([&scope, &request], |row| row.get(0))
        .optional()?;
    match ended {
        Some(message) => touch_message_by_id(conn, &message, false),
        None => Ok(()),
    }
}

fn head_on(conn: &Connection) -> Result<u64, BotError> {
    conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM bot_client_feed",
        [],
        |row| row.get(0),
    )
    .map_err(bad)
}

impl Repository {
    /// This database's feed: its identity, which a restore does not change
    /// and a new database does not share.
    pub fn feed_id(&self) -> Result<String, BotError> {
        self.connection
            .query_row(
                "SELECT feed_id FROM bot_client_feed_meta WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .map_err(bad)
    }

    /// The feed's newest `seq`; 0 for a feed nothing was written to. The
    /// rowid's maximum, which is one seek.
    pub fn feed_head(&self) -> Result<u64, BotError> {
        head_on(&self.connection)
    }

    /// How many Bots, conversations and messages a linked desktop that is up
    /// to date holds: the markers that are neither removed nor withheld. Read
    /// from the three counters, never counted.
    pub fn feed_counts(&self) -> Result<FeedCounts, BotError> {
        self.connection
            .query_row(
                "SELECT live_bots, live_chats, live_messages FROM bot_client_feed_meta WHERE singleton = 1",
                [],
                |row| {
                    Ok(FeedCounts {
                        bots: row.get(0)?,
                        chats: row.get(1)?,
                        messages: row.get(2)?,
                    })
                },
            )
            .map_err(bad)
    }

    /// Record one start of the AI worker, with the feed's head at that
    /// moment, and keep the newest [`FEED_STARTS_KEPT`]. Answers that head.
    /// `boot_id` is 32 hexadecimal characters the worker draws once per run.
    pub fn feed_start(&mut self, boot_id: &str) -> Result<u64, BotError> {
        if boot_id.len() != 32 || !boot_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(BotError::Invalid);
        }
        let tx = self.connection.transaction().map_err(bad)?;
        let head = head_on(&tx)?;
        tx.execute(
            "INSERT INTO bot_client_feed_boots(boot_id, start_seq) VALUES(?1, ?2)",
            params![boot_id, i64::try_from(head).map_err(bad)?],
        )
        .map_err(bad)?;
        tx.execute(
            "DELETE FROM bot_client_feed_boots
              WHERE ordinal <= (SELECT MAX(ordinal) FROM bot_client_feed_boots) - ?1",
            [FEED_STARTS_KEPT],
        )
        .map_err(bad)?;
        tx.commit().map_err(bad)?;
        Ok(head)
    }

    /// The newest recorded start: the one this run of the worker serves under.
    pub fn feed_current_start(&self) -> Result<Option<String>, BotError> {
        self.connection
            .query_row(
                "SELECT boot_id FROM bot_client_feed_boots ORDER BY ordinal DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(bad)
    }

    /// Whether a linked desktop's `(feed_id, boot_id, cursor)` names history
    /// this database still has, so that it may continue from `cursor`.
    ///
    /// It does when the feed is this one, the start is recorded, and the
    /// cursor is not past what that start could have served: the head, for
    /// the current start; otherwise the head the following start began at.
    /// After a restore from a backup the first new start begins at the
    /// restored head, so a cursor from the lost history is above it and is
    /// refused, and so is a start the backup never saw. Three seeks.
    pub fn feed_cursor_served(
        &self,
        feed_id: &str,
        boot_id: &str,
        cursor: u64,
    ) -> Result<bool, BotError> {
        if feed_id != self.feed_id()? {
            return Ok(false);
        }
        let ordinal: Option<i64> = self
            .connection
            .query_row(
                "SELECT ordinal FROM bot_client_feed_boots WHERE boot_id = ?1",
                [boot_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(bad)?;
        let Some(ordinal) = ordinal else {
            return Ok(false);
        };
        let following: Option<u64> = self
            .connection
            .query_row(
                "SELECT start_seq FROM bot_client_feed_boots WHERE ordinal > ?1 ORDER BY ordinal LIMIT 1",
                [ordinal],
                |row| row.get(0),
            )
            .optional()
            .map_err(bad)?;
        let limit = match following {
            Some(start) => start,
            None => self.feed_head()?,
        };
        Ok(cursor <= limit)
    }

    /// Set whether a Bot is withheld, and give its marker and every marker
    /// of its conversations and messages a new `seq`, so that each linked
    /// desktop is sent the Bot, or its removal, again. Answers whether
    /// anything changed. One statement over the Bot's own markers, through
    /// `bot_client_feed_bot`; the three counters are then counted afresh,
    /// which reads every marker. A Bot becomes withheld when the person
    /// changes where its workspace is kept, not while it works.
    pub fn feed_set_withheld(&mut self, bot: BotId, withheld: bool) -> Result<bool, BotError> {
        let tx = self.connection.transaction().map_err(bad)?;
        let current: Option<bool> = tx
            .query_row(
                "SELECT withheld FROM bot_client_feed WHERE kind = 'bot' AND subject_id = ?1",
                [bot.to_storage()],
                |row| row.get(0),
            )
            .optional()
            .map_err(bad)?;
        if current.is_none_or(|current| current == withheld) {
            return Ok(false);
        }
        // The Bot's marker first, in the order the markers already have, so
        // a linked desktop meets the Bot before its conversations.
        tx.execute(
            "INSERT OR REPLACE INTO bot_client_feed(kind, subject_id, bot_id, chat_id, removed, withheld)
             SELECT kind, subject_id, bot_id, chat_id, removed, ?2 FROM bot_client_feed
              WHERE bot_id = ?1
              ORDER BY CASE kind WHEN 'bot' THEN 0 WHEN 'chat' THEN 1 ELSE 2 END, seq",
            params![bot.to_storage(), withheld],
        )
        .map_err(bad)?;
        tx.execute(
            "UPDATE bot_client_feed_meta SET
               live_bots = (SELECT count(*) FROM bot_client_feed WHERE kind = 'bot' AND removed = 0 AND withheld = 0),
               live_chats = (SELECT count(*) FROM bot_client_feed WHERE kind = 'chat' AND removed = 0 AND withheld = 0),
               live_messages = (SELECT count(*) FROM bot_client_feed WHERE kind = 'message' AND removed = 0 AND withheld = 0)",
            [],
        )
        .map_err(bad)?;
        tx.commit().map_err(bad)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
