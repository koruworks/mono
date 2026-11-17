// SPDX-License-Identifier: MPL-2.0

//! The copy this computer keeps of another computer's Bots (M8, P8.3; the
//! six client tables of ai.db 28).
//!
//! "Owner" is the computer that runs the Bots. This computer is its client:
//! it reads that owner's feed and writes what it is sent here, keyed by the
//! owner's device id.
//!
//! **A copy is read-only and nothing runs from it.** The only writer of the
//! five copy tables is [`Repository::mirror_apply_page`], which writes what
//! one page of the owner's feed holds, and the functions that delete a copy.
//! No function in this module reads or writes a canonical table of a Bot, a
//! conversation, a message, an attempt, a Run or a Routine, except to refuse
//! an id that already belongs to this computer ([`claimed`]). Nothing on the
//! path that runs a Bot reads a table of this module.
//!
//! **A page is applied whole or not at all**: one transaction holds every
//! row of the page and the cursor that says the page was applied.
//!
//! **A reset is staged.** A copy has a generation. The pages of a reset are
//! written under the next generation, which no read sees, and the page that
//! brings the copy up to date switches the generation and deletes the old
//! rows in the same transaction. A reset that was interrupted leaves staged
//! rows; they are deleted when the next one begins
//! ([`Repository::mirror_discard_staging`]).
//!
//! **An owner cannot claim an id that routes elsewhere.** A page that names
//! a Bot, a conversation or a message that exists in this computer's own
//! tables, or in the copy of another owner, is refused.
//!
//! **Withdrawal.** When an owner stops being one this computer may keep a
//! copy of, its row goes to `withdrawing`, which every read here hides at
//! once, and its rows are deleted in bounded batches, the owner row last
//! ([`Repository::mirror_delete_batch`]). The state is durable, so a delete
//! that was interrupted continues at the next start.
//!
//! **Commands this computer sent** (`bot_mirror_commands`) are a record, not
//! a queue: a row is written only when the command is about to be written
//! to an open stream. It holds identities, a kind, a state and a code, and,
//! until its receipt arrives, the exact bytes to send again.
//!
//! A row of this module is what another of the person's computers sent:
//! their Bots, what they wrote and what the Bots answered. Nothing here logs
//! or returns it in an error.

use super::*;
use koru_ai_core::bots::LinkedOwnerState;
use rusqlite::TransactionBehavior;
use uuid::Uuid;

/// A command whose receipt did not arrive is sent again for this long, then
/// it is `expired`.
pub const MIRROR_COMMAND_UNCONFIRMED_MS: i64 = 24 * 60 * 60 * 1000;
/// A command that is settled or expired is kept this long.
pub const MIRROR_COMMAND_KEEP_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// At most this many old command rows are deleted at once.
pub const MIRROR_COMMAND_PRUNE_BATCH: i64 = 200;
/// The largest team record, as the table holds it.
const MAX_TEAM_RECORD_BYTES: usize = 65_536;
/// The largest record of any other kind, as the tables hold it.
const MAX_RECORD_BYTES: usize = 1_048_576;

/// One owner this computer keeps a copy of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirrorOwner {
    pub owner_device: DeviceId,
    pub feed_id: Option<String>,
    /// The owner start whose pages produced `cursor`.
    pub boot_id: Option<String>,
    pub generation: u64,
    pub cursor: u64,
    /// The generation and cursor of a reset that is being received.
    pub staging: Option<(u64, u64)>,
    pub withheld_bots: u32,
    pub state: LinkedOwnerState,
    pub last_confirmed_at_ms: Option<i64>,
}

/// One change of one subject, as a page of the feed carries it. The
/// repository does not know the wire: `record_json` is the record as it was
/// sent, and the few members beside it are the ones the tables index.
#[derive(Clone, PartialEq, Eq)]
pub enum MirrorChange {
    Bot {
        seq: u64,
        bot_id: BotId,
        home_workspace: WorkspaceId,
        /// `active`, `archived` or `trashed`.
        lifecycle: String,
        revision: u64,
        record_json: String,
    },
    Chat {
        seq: u64,
        chat_id: ChatId,
        bot_id: BotId,
        /// `main` or `additional`.
        purpose: String,
        trashed: bool,
        updated_at_ms: i64,
        record_json: String,
    },
    Message {
        seq: u64,
        message_id: MessageId,
        chat_id: ChatId,
        /// The Bot the entry names, when it names one.
        bot_id: Option<BotId>,
        ordinal: u32,
        record_json: String,
    },
    Team {
        seq: u64,
        record_json: String,
    },
    /// The Bot is gone, or is no longer copied: it, its conversations and
    /// their messages are deleted.
    RemoveBot {
        bot_id: BotId,
    },
    /// The conversation and its messages are deleted.
    RemoveChat {
        chat_id: ChatId,
    },
    RemoveMessage {
        message_id: MessageId,
    },
    RemoveTeam,
}

impl std::fmt::Debug for MirrorChange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Bot { .. } => "MirrorChange(bot)",
            Self::Chat { .. } => "MirrorChange(chat)",
            Self::Message { .. } => "MirrorChange(message)",
            Self::Team { .. } => "MirrorChange(team)",
            Self::RemoveBot { .. } => "MirrorChange(remove bot)",
            Self::RemoveChat { .. } => "MirrorChange(remove chat)",
            Self::RemoveMessage { .. } => "MirrorChange(remove message)",
            Self::RemoveTeam => "MirrorChange(remove team)",
        })
    }
}

/// One page of an owner's feed, checked against the stream it came on by
/// the caller, to be applied in one transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirrorPage {
    pub owner_device: DeviceId,
    pub feed_id: String,
    pub boot_id: String,
    pub protocol_version: u32,
    pub replica_format: u32,
    pub from_cursor: u64,
    pub to_cursor: u64,
    /// True on every page of a reset.
    pub reset: bool,
    /// On the page that brings the copy up to date: how many Bots that
    /// owner withholds.
    pub caught_up: Option<u32>,
    pub changes: Vec<MirrorChange>,
}

/// How many rows a copy holds, by kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MirrorCounts {
    pub bots: i64,
    pub chats: i64,
    pub messages: i64,
}

/// What applying a page did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MirrorApplied {
    /// The cursor the page left: the visible copy's, or the staged one's
    /// while a reset is being received.
    pub cursor: u64,
    /// The generation the page's rows were written under.
    pub generation: u64,
    /// The page ended a reset: the staged generation is now the copy.
    pub switched: bool,
    /// How the row counts of that generation moved.
    pub delta: MirrorCounts,
}

/// Why a page was not applied. Nothing of the page was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorRefusal {
    /// No row for that owner, or it is being withdrawn.
    NoOwner,
    /// The page is not the next one of the copy or of its staged reset.
    OutOfOrder,
    /// The page names an id that belongs to this computer or to the copy of
    /// another owner.
    Claimed,
    /// A member the tables refuse: a record that is not JSON or is too
    /// large, a token of the wrong shape, a conversation that changed Bot.
    Malformed,
    Storage,
}

impl From<BotError> for MirrorRefusal {
    fn from(_: BotError) -> Self {
        Self::Storage
    }
}

/// A command this computer sent to an owner.
#[derive(Clone, PartialEq, Eq)]
pub struct MirrorCommand {
    pub owner_device: DeviceId,
    pub request_id: Uuid,
    pub fingerprint: String,
    pub kind: String,
    pub bot_id: Option<BotId>,
    pub state: MirrorCommandState,
    /// The exact bytes that were sent, kept until the receipt arrives.
    pub command_json: Option<String>,
    pub outcome: Option<ClientOutcome>,
    pub failure_code: Option<String>,
    pub feed_cursor: Option<u64>,
    pub created_at_ms: i64,
}

impl std::fmt::Debug for MirrorCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MirrorCommand")
            .field("kind", &self.kind)
            .field("state", &self.state)
            .field("outcome", &self.outcome)
            .field("failure_code", &self.failure_code)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorCommandState {
    /// Written to a stream; no receipt yet.
    Sent,
    /// The owner answered. Final.
    Receipted,
    /// The stream ended before the receipt. Sent again on the next
    /// connection.
    Unconfirmed,
    /// Never confirmed, and no longer sent.
    Expired,
}

impl MirrorCommandState {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Sent => "sent",
            Self::Receipted => "receipted",
            Self::Unconfirmed => "unconfirmed",
            Self::Expired => "expired",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "sent" => Self::Sent,
            "receipted" => Self::Receipted,
            "unconfirmed" => Self::Unconfirmed,
            "expired" => Self::Expired,
            _ => return None,
        })
    }
}

/// What recording a command to send found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MirrorCommandBegin {
    /// A row was written as `sent`: write the command to the stream now.
    Send,
    /// The request was recorded before, with the same content.
    Known(MirrorCommand),
    /// The same request id with other content.
    Conflict,
}

/// What one batch of a withdrawal left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorDeleted {
    /// Rows of the copy remain; call again.
    More,
    /// The copy and its owner row are gone.
    Done,
}

/// What a command, a read or a decision of this computer names, for the
/// lookup that says whose it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorSubject {
    Bot(BotId),
    Chat(ChatId),
    Message(MessageId),
}

/// Whose a subject is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorPlace {
    /// It is in this computer's own tables.
    Local,
    /// It is in the visible copy of this owner. For a conversation or a
    /// message, the conversation it is or belongs to.
    Linked {
        owner_device: DeviceId,
        chat_id: Option<ChatId>,
    },
    /// Neither.
    Nowhere,
}

fn token32(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn kind_valid(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 64
        && kind
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
}

fn signed(value: u64) -> Result<i64, MirrorRefusal> {
    i64::try_from(value).map_err(|_| MirrorRefusal::Malformed)
}

fn unsigned(value: i64) -> Result<u64, BotError> {
    u64::try_from(value).map_err(bad)
}

fn refused(_: impl std::fmt::Display) -> MirrorRefusal {
    MirrorRefusal::Storage
}

const OWNER_COLUMNS: &str = "owner_device_id, feed_id, boot_id, generation, cursor,
    staging_generation, staging_cursor, withheld_bots, state, last_confirmed_at_ms";

fn owner_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<MirrorOwner, BotError>> {
    let device: String = row.get(0)?;
    let generation: i64 = row.get(3)?;
    let cursor: i64 = row.get(4)?;
    let staging_generation: Option<i64> = row.get(5)?;
    let staging_cursor: Option<i64> = row.get(6)?;
    let withheld: i64 = row.get(7)?;
    let state: String = row.get(8)?;
    let feed_id = row.get(1)?;
    let boot_id = row.get(2)?;
    let last_confirmed_at_ms = row.get(9)?;
    Ok((|| {
        Ok(MirrorOwner {
            owner_device: DeviceId::parse(&device).ok_or(BotError::Storage)?,
            feed_id,
            boot_id,
            generation: unsigned(generation)?,
            cursor: unsigned(cursor)?,
            staging: match (staging_generation, staging_cursor) {
                (Some(generation), Some(cursor)) => {
                    Some((unsigned(generation)?, unsigned(cursor)?))
                }
                _ => None,
            },
            withheld_bots: u32::try_from(withheld).map_err(bad)?,
            state: LinkedOwnerState::parse(&state).ok_or(BotError::Storage)?,
            last_confirmed_at_ms,
        })
    })())
}

fn owner_on(conn: &Connection, owner: DeviceId) -> Result<Option<MirrorOwner>, BotError> {
    conn.query_row(
        &format!("SELECT {OWNER_COLUMNS} FROM bot_mirror_owners WHERE owner_device_id = ?1"),
        [owner.to_storage()],
        owner_row,
    )
    .optional()
    .map_err(bad)?
    .transpose()
}

fn exists(
    conn: &Connection,
    sql: &str,
    values: impl rusqlite::Params,
) -> Result<bool, MirrorRefusal> {
    conn.query_row(sql, values, |_| Ok(()))
        .optional()
        .map(|found| found.is_some())
        .map_err(refused)
}

/// Whether an id a page names already belongs to this computer's own tables
/// or to the copy of another owner. An owner that could write such a row
/// would decide where this computer sends the commands that name it.
fn claimed(conn: &Connection, owner: &str, subject: MirrorSubject) -> Result<bool, MirrorRefusal> {
    Ok(match subject {
        MirrorSubject::Bot(bot) => {
            let bot = bot.to_storage();
            exists(conn, "SELECT 1 FROM bots WHERE bot_id = ?1", [&bot])?
                || exists(
                    conn,
                    "SELECT 1 FROM bot_mirror_bots WHERE bot_id = ?1 AND owner_device_id <> ?2",
                    params![bot, owner],
                )?
        }
        MirrorSubject::Chat(chat) => {
            let chat = chat.to_storage();
            exists(conn, "SELECT 1 FROM chats WHERE id = ?1", [&chat])?
                || exists(
                    conn,
                    "SELECT 1 FROM bot_mirror_owners AS o
                       JOIN bot_mirror_chats AS c ON c.owner_device_id = o.owner_device_id
                        AND c.generation IN (o.generation, o.staging_generation)
                        AND c.chat_id = ?1
                      WHERE o.owner_device_id <> ?2",
                    params![chat, owner],
                )?
        }
        MirrorSubject::Message(message) => {
            let message = message.to_storage();
            exists(conn, "SELECT 1 FROM messages WHERE id = ?1", [&message])?
                || exists(
                    conn,
                    "SELECT 1 FROM bot_mirror_owners AS o
                       JOIN bot_mirror_messages AS m ON m.owner_device_id = o.owner_device_id
                        AND m.generation IN (o.generation, o.staging_generation)
                        AND m.message_id = ?1
                      WHERE o.owner_device_id <> ?2",
                    params![message, owner],
                )?
        }
    })
}

fn record_fits(record: &str, limit: usize) -> Result<(), MirrorRefusal> {
    if record.len() > limit {
        Err(MirrorRefusal::Malformed)
    } else {
        Ok(())
    }
}

/// A write of a record. A row the table's own checks refuse (a record that
/// is not JSON, a token outside its set) is a malformed page, not a storage
/// failure.
fn write(
    conn: &Connection,
    sql: &str,
    values: impl rusqlite::Params,
) -> Result<usize, MirrorRefusal> {
    conn.execute(sql, values).map_err(|error| match error {
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            MirrorRefusal::Malformed
        }
        _ => MirrorRefusal::Storage,
    })
}

/// Delete one conversation's messages and the conversation, under one
/// generation. Answers how many messages and conversations went.
fn delete_chat(
    conn: &Connection,
    owner: &str,
    generation: i64,
    chat: &str,
) -> Result<(usize, usize), MirrorRefusal> {
    let messages = write(
        conn,
        "DELETE FROM bot_mirror_messages
          WHERE owner_device_id = ?1 AND generation = ?2 AND chat_id = ?3",
        params![owner, generation, chat],
    )?;
    let chats = write(
        conn,
        "DELETE FROM bot_mirror_chats
          WHERE owner_device_id = ?1 AND generation = ?2 AND chat_id = ?3",
        params![owner, generation, chat],
    )?;
    Ok((messages, chats))
}

/// Write one change under `generation` and add what it did to `delta`.
fn apply_change(
    conn: &Connection,
    owner: &str,
    generation: i64,
    change: &MirrorChange,
    delta: &mut MirrorCounts,
) -> Result<(), MirrorRefusal> {
    let as_count = |rows: usize| i64::try_from(rows).unwrap_or(i64::MAX);
    match change {
        MirrorChange::Bot {
            seq,
            bot_id,
            home_workspace,
            lifecycle,
            revision,
            record_json,
        } => {
            record_fits(record_json, MAX_RECORD_BYTES)?;
            if claimed(conn, owner, MirrorSubject::Bot(*bot_id))? {
                return Err(MirrorRefusal::Claimed);
            }
            let bot = bot_id.to_storage();
            let known = exists(
                conn,
                "SELECT 1 FROM bot_mirror_bots
                  WHERE owner_device_id = ?1 AND generation = ?2 AND bot_id = ?3",
                params![owner, generation, bot],
            )?;
            write(
                conn,
                "INSERT INTO bot_mirror_bots(owner_device_id, generation, bot_id, seq,
                   home_workspace_id, lifecycle, revision, record_json)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(owner_device_id, generation, bot_id) DO UPDATE SET
                   seq = excluded.seq, home_workspace_id = excluded.home_workspace_id,
                   lifecycle = excluded.lifecycle, revision = excluded.revision,
                   record_json = excluded.record_json",
                params![
                    owner,
                    generation,
                    bot,
                    signed(*seq)?,
                    home_workspace.to_storage(),
                    lifecycle,
                    signed(*revision)?,
                    record_json
                ],
            )?;
            if !known {
                delta.bots += 1;
            }
        }
        MirrorChange::Chat {
            seq,
            chat_id,
            bot_id,
            purpose,
            trashed,
            updated_at_ms,
            record_json,
        } => {
            record_fits(record_json, MAX_RECORD_BYTES)?;
            if claimed(conn, owner, MirrorSubject::Bot(*bot_id))?
                || claimed(conn, owner, MirrorSubject::Chat(*chat_id))?
            {
                return Err(MirrorRefusal::Claimed);
            }
            let chat = chat_id.to_storage();
            let bot = bot_id.to_storage();
            let stored: Option<String> = conn
                .query_row(
                    "SELECT bot_id FROM bot_mirror_chats
                      WHERE owner_device_id = ?1 AND generation = ?2 AND chat_id = ?3",
                    params![owner, generation, chat],
                    |row| row.get(0),
                )
                .optional()
                .map_err(refused)?;
            // A conversation never changes its Bot.
            if stored.as_ref().is_some_and(|stored| *stored != bot) {
                return Err(MirrorRefusal::Malformed);
            }
            write(
                conn,
                "INSERT INTO bot_mirror_chats(owner_device_id, generation, chat_id, bot_id, seq,
                   purpose, trashed, updated_at_ms, record_json)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(owner_device_id, generation, chat_id) DO UPDATE SET
                   seq = excluded.seq, purpose = excluded.purpose, trashed = excluded.trashed,
                   updated_at_ms = excluded.updated_at_ms, record_json = excluded.record_json",
                params![
                    owner,
                    generation,
                    chat,
                    bot,
                    signed(*seq)?,
                    purpose,
                    trashed,
                    updated_at_ms,
                    record_json
                ],
            )?;
            if stored.is_none() {
                delta.chats += 1;
            }
        }
        MirrorChange::Message {
            seq,
            message_id,
            chat_id,
            bot_id,
            ordinal,
            record_json,
        } => {
            record_fits(record_json, MAX_RECORD_BYTES)?;
            if claimed(conn, owner, MirrorSubject::Message(*message_id))?
                || claimed(conn, owner, MirrorSubject::Chat(*chat_id))?
                || match bot_id {
                    Some(bot) => claimed(conn, owner, MirrorSubject::Bot(*bot))?,
                    None => false,
                }
            {
                return Err(MirrorRefusal::Claimed);
            }
            let message = message_id.to_storage();
            let chat = chat_id.to_storage();
            let stored: Option<String> = conn
                .query_row(
                    "SELECT chat_id FROM bot_mirror_messages
                      WHERE owner_device_id = ?1 AND generation = ?2 AND message_id = ?3",
                    params![owner, generation, message],
                    |row| row.get(0),
                )
                .optional()
                .map_err(refused)?;
            // A message never changes its conversation.
            if stored.as_ref().is_some_and(|stored| *stored != chat) {
                return Err(MirrorRefusal::Malformed);
            }
            write(
                conn,
                "INSERT INTO bot_mirror_messages(owner_device_id, generation, message_id, chat_id,
                   ordinal, seq, record_json)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(owner_device_id, generation, message_id) DO UPDATE SET
                   ordinal = excluded.ordinal, seq = excluded.seq,
                   record_json = excluded.record_json",
                params![
                    owner,
                    generation,
                    message,
                    chat,
                    ordinal,
                    signed(*seq)?,
                    record_json
                ],
            )?;
            if stored.is_none() {
                delta.messages += 1;
            }
        }
        MirrorChange::Team { seq, record_json } => {
            record_fits(record_json, MAX_TEAM_RECORD_BYTES)?;
            write(
                conn,
                "INSERT INTO bot_mirror_team(owner_device_id, generation, seq, record_json)
                 VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(owner_device_id, generation) DO UPDATE SET
                   seq = excluded.seq, record_json = excluded.record_json",
                params![owner, generation, signed(*seq)?, record_json],
            )?;
        }
        MirrorChange::RemoveBot { bot_id } => {
            let bot = bot_id.to_storage();
            delta.messages -= as_count(write(
                conn,
                "DELETE FROM bot_mirror_messages
                  WHERE owner_device_id = ?1 AND generation = ?2 AND chat_id IN (
                    SELECT chat_id FROM bot_mirror_chats
                     WHERE owner_device_id = ?1 AND generation = ?2 AND bot_id = ?3)",
                params![owner, generation, bot],
            )?);
            delta.chats -= as_count(write(
                conn,
                "DELETE FROM bot_mirror_chats
                  WHERE owner_device_id = ?1 AND generation = ?2 AND bot_id = ?3",
                params![owner, generation, bot],
            )?);
            delta.bots -= as_count(write(
                conn,
                "DELETE FROM bot_mirror_bots
                  WHERE owner_device_id = ?1 AND generation = ?2 AND bot_id = ?3",
                params![owner, generation, bot],
            )?);
        }
        MirrorChange::RemoveChat { chat_id } => {
            let (messages, chats) = delete_chat(conn, owner, generation, &chat_id.to_storage())?;
            delta.messages -= as_count(messages);
            delta.chats -= as_count(chats);
        }
        MirrorChange::RemoveMessage { message_id } => {
            delta.messages -= as_count(write(
                conn,
                "DELETE FROM bot_mirror_messages
                  WHERE owner_device_id = ?1 AND generation = ?2 AND message_id = ?3",
                params![owner, generation, message_id.to_storage()],
            )?);
        }
        MirrorChange::RemoveTeam => {
            write(
                conn,
                "DELETE FROM bot_mirror_team WHERE owner_device_id = ?1 AND generation = ?2",
                params![owner, generation],
            )?;
        }
    }
    Ok(())
}

/// Delete every row of `owner` that is not of `keep`.
fn delete_other_generations(
    conn: &Connection,
    owner: &str,
    keep: i64,
) -> Result<(), MirrorRefusal> {
    for table in [
        "bot_mirror_messages",
        "bot_mirror_chats",
        "bot_mirror_bots",
        "bot_mirror_team",
    ] {
        write(
            conn,
            &format!("DELETE FROM {table} WHERE owner_device_id = ?1 AND generation <> ?2"),
            params![owner, keep],
        )?;
    }
    Ok(())
}

const COMMAND_COLUMNS: &str = "owner_device_id, request_id, fingerprint, command_kind, bot_id,
    state, command_json, outcome, failure_code, feed_cursor, created_at_ms";

fn command_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<MirrorCommand, BotError>> {
    let owner: String = row.get(0)?;
    let request: String = row.get(1)?;
    let fingerprint = row.get(2)?;
    let kind = row.get(3)?;
    let bot: Option<String> = row.get(4)?;
    let state: String = row.get(5)?;
    let command_json = row.get(6)?;
    let outcome: Option<String> = row.get(7)?;
    let failure_code = row.get(8)?;
    let feed_cursor: Option<i64> = row.get(9)?;
    let created_at_ms = row.get(10)?;
    Ok((|| {
        Ok(MirrorCommand {
            owner_device: DeviceId::parse(&owner).ok_or(BotError::Storage)?,
            request_id: Uuid::parse_str(&request).map_err(bad)?,
            fingerprint,
            kind,
            bot_id: bot.map(|bot| bot.parse().map_err(bad)).transpose()?,
            state: MirrorCommandState::parse(&state).ok_or(BotError::Storage)?,
            command_json,
            outcome: match outcome.as_deref() {
                None => None,
                Some("completed") => Some(ClientOutcome::Completed),
                Some("accepted") => Some(ClientOutcome::Accepted),
                Some("refused") => Some(ClientOutcome::Refused),
                Some("unknown") => Some(ClientOutcome::Unknown),
                Some(_) => return Err(BotError::Storage),
            },
            failure_code,
            feed_cursor: feed_cursor.map(unsigned).transpose()?,
            created_at_ms,
        })
    })())
}

fn command_on(
    conn: &Connection,
    owner: DeviceId,
    request: Uuid,
) -> Result<Option<MirrorCommand>, BotError> {
    conn.query_row(
        &format!(
            "SELECT {COMMAND_COLUMNS} FROM bot_mirror_commands
              WHERE owner_device_id = ?1 AND request_id = ?2"
        ),
        params![owner.to_storage(), request.to_string()],
        command_row,
    )
    .optional()
    .map_err(bad)?
    .transpose()
}

impl Repository {
    /// Every owner this computer keeps a copy of that is not being
    /// withdrawn, in device id order.
    pub fn mirror_owners(&self) -> Result<Vec<MirrorOwner>, BotError> {
        let mut query = self
            .connection
            .prepare(&format!(
                "SELECT {OWNER_COLUMNS} FROM bot_mirror_owners
                  WHERE state <> 'withdrawing' ORDER BY owner_device_id"
            ))
            .map_err(bad)?;
        let rows = query
            .query_map([], owner_row)
            .map_err(bad)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(bad)?;
        rows.into_iter().collect()
    }

    /// One owner's row, in whatever state it is.
    pub fn mirror_owner(&self, owner: DeviceId) -> Result<Option<MirrorOwner>, BotError> {
        owner_on(&self.connection, owner)
    }

    /// How many owner rows there are, a withdrawing one included.
    pub fn mirror_owner_count(&self) -> Result<u64, BotError> {
        let count: i64 = self
            .connection
            .query_row("SELECT count(*) FROM bot_mirror_owners", [], |row| {
                row.get(0)
            })
            .map_err(bad)?;
        unsigned(count)
    }

    /// The owners whose copy is being deleted.
    pub fn mirror_withdrawing(&self) -> Result<Vec<DeviceId>, BotError> {
        let mut query = self
            .connection
            .prepare(
                "SELECT owner_device_id FROM bot_mirror_owners
                  WHERE state = 'withdrawing' ORDER BY owner_device_id",
            )
            .map_err(bad)?;
        let rows = query
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(bad)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(bad)?;
        rows.into_iter()
            .map(|device| DeviceId::parse(&device).ok_or(BotError::Storage))
            .collect()
    }

    /// Make the row of an owner this computer may keep a copy of, in the
    /// state `never`, when there is none. An existing row is left as it is
    /// and answered, a withdrawing one included: its copy is deleted first.
    pub fn mirror_ensure_owner(
        &mut self,
        owner: DeviceId,
        now_ms: i64,
    ) -> Result<MirrorOwner, BotError> {
        self.connection
            .execute(
                "INSERT INTO bot_mirror_owners(owner_device_id, state, created_at_ms, updated_at_ms)
                 VALUES(?1, 'never', ?2, ?2) ON CONFLICT(owner_device_id) DO NOTHING",
                params![owner.to_storage(), now_ms],
            )
            .map_err(bad)?;
        owner_on(&self.connection, owner)?.ok_or(BotError::Storage)
    }

    /// What this run of the worker finds when it starts: no subscription is
    /// open, so every copy that was `current` is `stale`; a reset that was
    /// being received is dropped; and a command that was written to a
    /// stream and never answered is `unconfirmed`. One transaction.
    pub fn mirror_recover(&mut self, now_ms: i64) -> Result<(), BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        tx.execute(
            "UPDATE bot_mirror_owners SET state = 'stale', updated_at_ms = ?1
              WHERE state = 'current'",
            [now_ms],
        )
        .map_err(bad)?;
        let staged: Vec<(String, i64)> = {
            let mut query = tx
                .prepare(
                    "SELECT owner_device_id, generation FROM bot_mirror_owners
                      WHERE staging_generation IS NOT NULL AND state <> 'withdrawing'",
                )
                .map_err(bad)?;
            let rows = query
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(bad)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(bad)?;
            rows
        };
        for (owner, generation) in staged {
            delete_other_generations(&tx, &owner, generation).map_err(|_| BotError::Storage)?;
        }
        tx.execute(
            "UPDATE bot_mirror_owners SET staging_generation = NULL, staging_cursor = NULL,
               updated_at_ms = ?1 WHERE staging_generation IS NOT NULL",
            [now_ms],
        )
        .map_err(bad)?;
        tx.execute(
            "UPDATE bot_mirror_commands SET state = 'unconfirmed', updated_at_ms = ?1
              WHERE state = 'sent'",
            [now_ms],
        )
        .map_err(bad)?;
        tx.commit().map_err(bad)
    }

    /// Drop a reset that was being received from `owner`: its staged rows
    /// and its staged cursor. The visible copy is untouched. Called before
    /// every subscription, so an interrupted reset never mixes with the
    /// next one.
    pub fn mirror_discard_staging(&mut self, owner: DeviceId, now_ms: i64) -> Result<(), BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let Some(row) = owner_on(&tx, owner)? else {
            return Ok(());
        };
        if row.staging.is_none() {
            return Ok(());
        }
        let device = owner.to_storage();
        delete_other_generations(&tx, &device, i64::try_from(row.generation).map_err(bad)?)
            .map_err(|_| BotError::Storage)?;
        tx.execute(
            "UPDATE bot_mirror_owners SET staging_generation = NULL, staging_cursor = NULL,
               updated_at_ms = ?2 WHERE owner_device_id = ?1",
            params![device, now_ms],
        )
        .map_err(bad)?;
        tx.commit().map_err(bad)
    }

    /// Set what a copy is shown as. A withdrawing copy stays withdrawing,
    /// and nothing puts a copy into `withdrawing` here
    /// ([`Repository::mirror_withdraw`] does).
    pub fn mirror_set_state(
        &mut self,
        owner: DeviceId,
        state: LinkedOwnerState,
        now_ms: i64,
    ) -> Result<(), BotError> {
        if state == LinkedOwnerState::Withdrawing {
            return Err(BotError::Invalid);
        }
        self.connection
            .execute(
                "UPDATE bot_mirror_owners SET state = ?2, updated_at_ms = ?3
                  WHERE owner_device_id = ?1 AND state NOT IN ('withdrawing', ?2)",
                params![owner.to_storage(), state.as_str(), now_ms],
            )
            .map_err(bad)?;
        Ok(())
    }

    /// Apply one page of an owner's feed, whole or not at all (contract
    /// 5.2). One transaction: the page's place is checked, each record is
    /// written, each removed subject is deleted with what belongs to it, and
    /// the cursor moves.
    ///
    /// The caller has checked the page against the stream it came on (the
    /// feed and start of the hello, the ascending `seq` values, the records
    /// against their entries). This checks what only the database knows:
    ///
    /// - the page is the next one: `from_cursor` is the copy's cursor, or,
    ///   for a page of a reset, 0 (which begins the reset again) or the
    ///   staged cursor;
    /// - no id it names belongs to this computer or to another owner's copy.
    ///
    /// A page of a reset is written under the staged generation, which no
    /// read sees. The page that brings a reset up to date (`caught_up` and
    /// `reset`) switches the generation and deletes every other row of the
    /// owner in this same transaction. A page that brings the copy up to
    /// date makes it `current` and stamps the time.
    pub fn mirror_apply_page(
        &mut self,
        page: &MirrorPage,
        now_ms: i64,
    ) -> Result<MirrorApplied, MirrorRefusal> {
        if !token32(&page.feed_id) || !token32(&page.boot_id) || page.to_cursor < page.from_cursor {
            return Err(MirrorRefusal::Malformed);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(refused)?;
        let row = owner_on(&tx, page.owner_device)?.ok_or(MirrorRefusal::NoOwner)?;
        if row.state == LinkedOwnerState::Withdrawing {
            return Err(MirrorRefusal::NoOwner);
        }
        let owner = page.owner_device.to_storage();
        let visible = signed(row.generation)?;
        let generation = if page.reset {
            match row.staging {
                Some((staged, cursor)) if page.from_cursor == cursor && cursor > 0 => {
                    signed(staged)?
                }
                // The first page of a reset, or of one that begins again.
                _ if page.from_cursor == 0 => {
                    delete_other_generations(&tx, &owner, visible)?;
                    visible + 1
                }
                _ => return Err(MirrorRefusal::OutOfOrder),
            }
        } else {
            if page.from_cursor != row.cursor
                || row
                    .feed_id
                    .as_ref()
                    .is_some_and(|feed| *feed != page.feed_id)
            {
                return Err(MirrorRefusal::OutOfOrder);
            }
            // A page that continues the copy ends any reset left half way.
            if row.staging.is_some() {
                delete_other_generations(&tx, &owner, visible)?;
            }
            visible
        };
        let mut delta = MirrorCounts::default();
        for change in &page.changes {
            apply_change(&tx, &owner, generation, change, &mut delta)?;
        }
        let cursor = signed(page.to_cursor)?;
        let switched = page.reset && page.caught_up.is_some();
        if page.reset && !switched {
            write(
                &tx,
                "UPDATE bot_mirror_owners SET staging_generation = ?2, staging_cursor = ?3,
                   updated_at_ms = ?4 WHERE owner_device_id = ?1",
                params![owner, generation, cursor, now_ms],
            )?;
        } else {
            if switched {
                delete_other_generations(&tx, &owner, generation)?;
            }
            write(
                &tx,
                "UPDATE bot_mirror_owners SET generation = ?2, cursor = ?3, feed_id = ?4,
                   boot_id = ?5, protocol_version = ?6, replica_format = ?7,
                   staging_generation = NULL, staging_cursor = NULL, updated_at_ms = ?8
                 WHERE owner_device_id = ?1",
                params![
                    owner,
                    generation,
                    cursor,
                    page.feed_id,
                    page.boot_id,
                    page.protocol_version,
                    page.replica_format,
                    now_ms
                ],
            )?;
        }
        if let Some(withheld) = page.caught_up {
            write(
                &tx,
                "UPDATE bot_mirror_owners SET withheld_bots = ?2, last_confirmed_at_ms = ?3,
                   state = 'current' WHERE owner_device_id = ?1",
                params![owner, withheld, now_ms],
            )?;
        }
        tx.commit().map_err(refused)?;
        Ok(MirrorApplied {
            cursor: page.to_cursor,
            generation: u64::try_from(generation).map_err(refused)?,
            switched,
            delta,
        })
    }

    /// How many Bots, conversations and messages the visible copy of
    /// `owner` holds. Reads every key of the copy: the caller keeps the
    /// counts and asks again only after a reset.
    pub fn mirror_counts(&self, owner: DeviceId) -> Result<MirrorCounts, BotError> {
        let device = owner.to_storage();
        let count = |table: &str| -> Result<i64, BotError> {
            self.connection
                .query_row(
                    &format!(
                        "SELECT count(*) FROM {table} AS t
                           JOIN bot_mirror_owners AS o ON o.owner_device_id = t.owner_device_id
                            AND o.generation = t.generation
                          WHERE t.owner_device_id = ?1"
                    ),
                    [&device],
                    |row| row.get(0),
                )
                .map_err(bad)
        };
        Ok(MirrorCounts {
            bots: count("bot_mirror_bots")?,
            chats: count("bot_mirror_chats")?,
            messages: count("bot_mirror_messages")?,
        })
    }

    /// Whose a Bot, a conversation or a message is: this computer's own,
    /// the visible copy of an owner that is not being withdrawn, or
    /// neither. This computer's own tables are looked at first, so a copy
    /// can never take a command meant for a Bot that runs here.
    pub fn mirror_place(&self, subject: MirrorSubject) -> Result<MirrorPlace, BotError> {
        let conn: &Connection = &self.connection;
        let local = |sql: &str, id: String| -> Result<bool, BotError> {
            conn.query_row(sql, [id], |_| Ok(()))
                .optional()
                .map(|found| found.is_some())
                .map_err(bad)
        };
        let linked = |sql: &str, id: String| -> Result<MirrorPlace, BotError> {
            let found: Option<(String, Option<String>)> = conn
                .query_row(sql, [id], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()
                .map_err(bad)?;
            Ok(match found {
                None => MirrorPlace::Nowhere,
                Some((owner, chat)) => MirrorPlace::Linked {
                    owner_device: DeviceId::parse(&owner).ok_or(BotError::Storage)?,
                    chat_id: chat
                        .map(|chat| ChatId::parse(&chat).ok_or(BotError::Storage))
                        .transpose()?,
                },
            })
        };
        match subject {
            MirrorSubject::Bot(bot) => {
                if local("SELECT 1 FROM bots WHERE bot_id = ?1", bot.to_storage())? {
                    return Ok(MirrorPlace::Local);
                }
                linked(
                    "SELECT b.owner_device_id, NULL FROM bot_mirror_bots AS b
                       JOIN bot_mirror_owners AS o ON o.owner_device_id = b.owner_device_id
                        AND o.generation = b.generation
                      WHERE b.bot_id = ?1 AND o.state <> 'withdrawing'",
                    bot.to_storage(),
                )
            }
            MirrorSubject::Chat(chat) => {
                if local("SELECT 1 FROM chats WHERE id = ?1", chat.to_storage())? {
                    return Ok(MirrorPlace::Local);
                }
                linked(
                    "SELECT c.owner_device_id, c.chat_id FROM bot_mirror_owners AS o
                       JOIN bot_mirror_chats AS c ON c.owner_device_id = o.owner_device_id
                        AND c.generation = o.generation AND c.chat_id = ?1
                      WHERE o.state <> 'withdrawing'",
                    chat.to_storage(),
                )
            }
            MirrorSubject::Message(message) => {
                if local("SELECT 1 FROM messages WHERE id = ?1", message.to_storage())? {
                    return Ok(MirrorPlace::Local);
                }
                linked(
                    "SELECT m.owner_device_id, m.chat_id FROM bot_mirror_owners AS o
                       JOIN bot_mirror_messages AS m ON m.owner_device_id = o.owner_device_id
                        AND m.generation = o.generation AND m.message_id = ?1
                      WHERE o.state <> 'withdrawing'",
                    message.to_storage(),
                )
            }
        }
    }

    fn mirror_records(
        &self,
        sql: &str,
        values: impl rusqlite::Params,
    ) -> Result<Vec<String>, BotError> {
        let mut query = self.connection.prepare(sql).map_err(bad)?;
        let rows = query
            .query_map(values, |row| row.get::<_, String>(0))
            .map_err(bad)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(bad)?;
        Ok(rows)
    }

    /// The records of an owner's Bots in the visible copy, oldest marker
    /// first; with `workspace`, only the Bots whose home it is.
    pub fn mirror_bots(
        &self,
        owner: DeviceId,
        workspace: Option<WorkspaceId>,
    ) -> Result<Vec<String>, BotError> {
        self.mirror_records(
            "SELECT b.record_json FROM bot_mirror_bots AS b
               JOIN bot_mirror_owners AS o ON o.owner_device_id = b.owner_device_id
                AND o.generation = b.generation
              WHERE b.owner_device_id = ?1 AND o.state <> 'withdrawing'
                AND (?2 IS NULL OR b.home_workspace_id = ?2)
              ORDER BY b.bot_id",
            params![
                owner.to_storage(),
                workspace.map(|workspace| workspace.to_storage())
            ],
        )
    }

    /// One Bot's record in the visible copy.
    pub fn mirror_bot(&self, owner: DeviceId, bot: BotId) -> Result<Option<String>, BotError> {
        Ok(self
            .mirror_records(
                "SELECT b.record_json FROM bot_mirror_bots AS b
                   JOIN bot_mirror_owners AS o ON o.owner_device_id = b.owner_device_id
                    AND o.generation = b.generation
                  WHERE b.owner_device_id = ?1 AND b.bot_id = ?2 AND o.state <> 'withdrawing'",
                params![owner.to_storage(), bot.to_storage()],
            )?
            .pop())
    }

    /// The records of one Bot's conversations in the visible copy: the main
    /// conversation first, then the newest first, then by id. `trashed`
    /// picks the conversations in Trash, or the ones outside it.
    pub fn mirror_chats(
        &self,
        owner: DeviceId,
        bot: BotId,
        trashed: bool,
    ) -> Result<Vec<String>, BotError> {
        self.mirror_records(
            "SELECT c.record_json FROM bot_mirror_chats AS c
               JOIN bot_mirror_owners AS o ON o.owner_device_id = c.owner_device_id
                AND o.generation = c.generation
              WHERE c.owner_device_id = ?1 AND c.bot_id = ?2 AND c.trashed = ?3
                AND o.state <> 'withdrawing'
              ORDER BY c.purpose = 'main' DESC, c.updated_at_ms DESC, c.chat_id",
            params![owner.to_storage(), bot.to_storage(), trashed],
        )
    }

    /// One conversation's record in the visible copy.
    pub fn mirror_chat(&self, owner: DeviceId, chat: ChatId) -> Result<Option<String>, BotError> {
        Ok(self
            .mirror_records(
                "SELECT c.record_json FROM bot_mirror_chats AS c
                   JOIN bot_mirror_owners AS o ON o.owner_device_id = c.owner_device_id
                    AND o.generation = c.generation
                  WHERE c.owner_device_id = ?1 AND c.chat_id = ?2 AND o.state <> 'withdrawing'",
                params![owner.to_storage(), chat.to_storage()],
            )?
            .pop())
    }

    /// The records of one conversation's messages in the visible copy, in
    /// the order the conversation reads.
    pub fn mirror_messages(&self, owner: DeviceId, chat: ChatId) -> Result<Vec<String>, BotError> {
        self.mirror_records(
            "SELECT m.record_json FROM bot_mirror_messages AS m
               JOIN bot_mirror_owners AS o ON o.owner_device_id = m.owner_device_id
                AND o.generation = m.generation
              WHERE m.owner_device_id = ?1 AND m.chat_id = ?2 AND o.state <> 'withdrawing'
              ORDER BY m.ordinal, m.message_id",
            params![owner.to_storage(), chat.to_storage()],
        )
    }

    /// One message's record in the visible copy.
    pub fn mirror_message(
        &self,
        owner: DeviceId,
        message: MessageId,
    ) -> Result<Option<String>, BotError> {
        Ok(self
            .mirror_records(
                "SELECT m.record_json FROM bot_mirror_messages AS m
                   JOIN bot_mirror_owners AS o ON o.owner_device_id = m.owner_device_id
                    AND o.generation = m.generation
                  WHERE m.owner_device_id = ?1 AND m.message_id = ?2
                    AND o.state <> 'withdrawing'",
                params![owner.to_storage(), message.to_storage()],
            )?
            .pop())
    }

    /// The newest `per_chat` message records of every conversation of the
    /// visible copies that the owner last said is running or needs the
    /// person, with the owner each belongs to. It is how the replies and
    /// pending calls a command may name are found again after a restart.
    pub fn mirror_active_messages(
        &self,
        per_chat: u32,
    ) -> Result<Vec<(DeviceId, String)>, BotError> {
        let mut query = self
            .connection
            .prepare(
                "SELECT m.owner_device_id, m.record_json
                   FROM bot_mirror_owners AS o
                   JOIN bot_mirror_chats AS c ON c.owner_device_id = o.owner_device_id
                    AND c.generation = o.generation
                   JOIN bot_mirror_messages AS m ON m.owner_device_id = c.owner_device_id
                    AND m.generation = c.generation AND m.chat_id = c.chat_id
                  WHERE o.state <> 'withdrawing'
                    AND (json_extract(c.record_json, '$.activity.running')
                      OR json_extract(c.record_json, '$.activity.needs_you'))
                    AND m.ordinal + ?1 > (
                      SELECT max(ordinal) FROM bot_mirror_messages AS last
                       WHERE last.owner_device_id = m.owner_device_id
                         AND last.generation = m.generation AND last.chat_id = m.chat_id)",
            )
            .map_err(bad)?;
        let rows = query
            .query_map([per_chat], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(bad)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(bad)?;
        rows.into_iter()
            .map(|(owner, record)| Ok((DeviceId::parse(&owner).ok_or(BotError::Storage)?, record)))
            .collect()
    }

    /// An owner's team pause record in the visible copy.
    pub fn mirror_team(&self, owner: DeviceId) -> Result<Option<String>, BotError> {
        Ok(self
            .mirror_records(
                "SELECT t.record_json FROM bot_mirror_team AS t
                   JOIN bot_mirror_owners AS o ON o.owner_device_id = t.owner_device_id
                    AND o.generation = t.generation
                  WHERE t.owner_device_id = ?1 AND o.state <> 'withdrawing'",
                [owner.to_storage()],
            )?
            .pop())
    }

    /// Begin withdrawing every copy whose owner is not in `named` (contract
    /// 5.3): its row goes to `withdrawing`, which hides it from every read
    /// at once, and its commands that were not confirmed are `expired`. One
    /// transaction. Answers every owner that is being withdrawn, the ones
    /// an earlier run began included.
    ///
    /// **The caller passes `named` only from an admission port that has
    /// been told something.** An empty list here deletes every copy.
    pub fn mirror_withdraw(
        &mut self,
        named: &[DeviceId],
        now_ms: i64,
    ) -> Result<Vec<DeviceId>, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let owners: Vec<String> = {
            let mut query = tx
                .prepare(
                    "SELECT owner_device_id FROM bot_mirror_owners WHERE state <> 'withdrawing'",
                )
                .map_err(bad)?;
            let rows = query
                .query_map([], |row| row.get(0))
                .map_err(bad)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(bad)?;
            rows
        };
        let named: Vec<String> = named.iter().map(|device| device.to_storage()).collect();
        for owner in owners.iter().filter(|owner| !named.contains(owner)) {
            tx.execute(
                "UPDATE bot_mirror_owners SET state = 'withdrawing', staging_generation = NULL,
                   staging_cursor = NULL, updated_at_ms = ?2 WHERE owner_device_id = ?1",
                params![owner, now_ms],
            )
            .map_err(bad)?;
            tx.execute(
                "UPDATE bot_mirror_commands SET state = 'expired', command_json = NULL,
                   updated_at_ms = ?2
                  WHERE owner_device_id = ?1 AND state IN ('sent', 'unconfirmed')",
                params![owner, now_ms],
            )
            .map_err(bad)?;
        }
        tx.commit().map_err(bad)?;
        self.mirror_withdrawing()
    }

    /// Delete up to `limit` rows of a copy that is being withdrawn: its
    /// messages first, then its conversations, its Bots and its team
    /// record, and, once none of those is left, the owner row itself. One
    /// short transaction, so the writer is never held for a whole copy.
    /// A copy that is not being withdrawn is left alone and answers `Done`.
    pub fn mirror_delete_batch(
        &mut self,
        owner: DeviceId,
        limit: u32,
    ) -> Result<MirrorDeleted, BotError> {
        let device = owner.to_storage();
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let withdrawing: Option<String> = tx
            .query_row(
                "SELECT state FROM bot_mirror_owners WHERE owner_device_id = ?1",
                [&device],
                |row| row.get(0),
            )
            .optional()
            .map_err(bad)?;
        if withdrawing.as_deref() != Some("withdrawing") {
            return Ok(MirrorDeleted::Done);
        }
        let mut left = i64::from(limit.max(1));
        for table in [
            "bot_mirror_messages",
            "bot_mirror_chats",
            "bot_mirror_bots",
            "bot_mirror_team",
        ] {
            let deleted = tx
                .execute(
                    &format!(
                        "DELETE FROM {table} WHERE rowid IN (
                           SELECT rowid FROM {table} WHERE owner_device_id = ?1 LIMIT ?2)"
                    ),
                    params![device, left],
                )
                .map_err(bad)?;
            left -= i64::try_from(deleted).map_err(bad)?;
            if left <= 0 {
                tx.commit().map_err(bad)?;
                return Ok(MirrorDeleted::More);
            }
        }
        // Nothing of the copy is left: the owner row goes last.
        tx.execute(
            "DELETE FROM bot_mirror_owners WHERE owner_device_id = ?1",
            [&device],
        )
        .map_err(bad)?;
        tx.commit().map_err(bad)?;
        Ok(MirrorDeleted::Done)
    }

    /// Record a command this computer is about to write to an open stream
    /// (contract 4.4), as `sent`, or find the row an earlier call with the
    /// same request id left. `command_json` is the exact bytes that will be
    /// written, kept so a re-send is identical.
    pub fn mirror_command_begin(
        &mut self,
        owner: DeviceId,
        request: Uuid,
        fingerprint: &str,
        kind: &str,
        bot: Option<BotId>,
        command_json: &str,
        now_ms: i64,
    ) -> Result<MirrorCommandBegin, BotError> {
        if fingerprint.len() != 64
            || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !kind_valid(kind)
            || command_json.len() > MAX_RECORD_BYTES
        {
            return Err(BotError::Invalid);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        if let Some(known) = command_on(&tx, owner, request)? {
            return Ok(if known.fingerprint == fingerprint {
                MirrorCommandBegin::Known(known)
            } else {
                MirrorCommandBegin::Conflict
            });
        }
        tx.execute(
            "INSERT INTO bot_mirror_commands(owner_device_id, request_id, fingerprint,
               command_kind, bot_id, state, command_json, created_at_ms, updated_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, 'sent', ?6, ?7, ?7)",
            params![
                owner.to_storage(),
                request.to_string(),
                fingerprint,
                kind,
                bot.map(|bot| bot.to_storage()),
                command_json,
                now_ms
            ],
        )
        .map_err(bad)?;
        tx.commit().map_err(bad)?;
        Ok(MirrorCommandBegin::Send)
    }

    /// One command this computer sent.
    pub fn mirror_command(
        &self,
        owner: DeviceId,
        request: Uuid,
    ) -> Result<Option<MirrorCommand>, BotError> {
        command_on(&self.connection, owner, request)
    }

    /// The owner answered a command: the row is `receipted` with the
    /// outcome, the failure code and the feed cursor, and the bytes to send
    /// again are dropped. A row that is already settled, or expired, keeps
    /// what it has: the first answer stands.
    pub fn mirror_command_receipt(
        &mut self,
        owner: DeviceId,
        request: Uuid,
        outcome: ClientOutcome,
        failure_code: Option<&str>,
        feed_cursor: Option<u64>,
        now_ms: i64,
    ) -> Result<(), BotError> {
        // A failure code is a short stable token; anything else is not
        // stored, so the row never holds text somebody typed.
        let failure_code = failure_code.filter(|code| {
            !code.is_empty() && code.len() <= 64 && code.bytes().all(|byte| byte.is_ascii_graphic())
        });
        self.connection
            .execute(
                "UPDATE bot_mirror_commands SET state = 'receipted', outcome = ?3,
                   failure_code = ?4, feed_cursor = ?5, command_json = NULL, updated_at_ms = ?6
                 WHERE owner_device_id = ?1 AND request_id = ?2
                   AND state IN ('sent', 'unconfirmed')",
                params![
                    owner.to_storage(),
                    request.to_string(),
                    outcome.token(),
                    failure_code,
                    feed_cursor
                        .map(|cursor| i64::try_from(cursor).map_err(bad))
                        .transpose()?,
                    now_ms
                ],
            )
            .map_err(bad)?;
        Ok(())
    }

    /// The stream ended after a command was written and before its receipt
    /// arrived (contract 4.5): the row is `unconfirmed`, to be sent again
    /// on the next connection.
    pub fn mirror_command_unconfirmed(
        &mut self,
        owner: DeviceId,
        request: Uuid,
        now_ms: i64,
    ) -> Result<(), BotError> {
        self.connection
            .execute(
                "UPDATE bot_mirror_commands SET state = 'unconfirmed', updated_at_ms = ?3
                  WHERE owner_device_id = ?1 AND request_id = ?2 AND state = 'sent'",
                params![owner.to_storage(), request.to_string(), now_ms],
            )
            .map_err(bad)?;
        Ok(())
    }

    /// The commands to `owner` that are not confirmed, oldest first, each
    /// with the exact bytes to send again.
    pub fn mirror_commands_unconfirmed(
        &self,
        owner: DeviceId,
    ) -> Result<Vec<MirrorCommand>, BotError> {
        let mut query = self
            .connection
            .prepare(&format!(
                "SELECT {COMMAND_COLUMNS} FROM bot_mirror_commands
                  WHERE owner_device_id = ?1 AND state = 'unconfirmed'
                    AND command_json IS NOT NULL
                  ORDER BY created_at_ms, request_id"
            ))
            .map_err(bad)?;
        let rows = query
            .query_map([owner.to_storage()], command_row)
            .map_err(bad)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(bad)?;
        rows.into_iter().collect()
    }

    /// Housekeeping of the commands this computer sent, by `now_ms`: one
    /// that has not been confirmed for [`MIRROR_COMMAND_UNCONFIRMED_MS`]
    /// since it was first sent is `expired` and is never sent again; and up
    /// to [`MIRROR_COMMAND_PRUNE_BATCH`] settled or expired rows older than
    /// [`MIRROR_COMMAND_KEEP_MS`] are deleted. Answers how many expired.
    pub fn mirror_commands_sweep(&mut self, now_ms: i64) -> Result<usize, BotError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(bad)?;
        let expired = tx
            .execute(
                "UPDATE bot_mirror_commands SET state = 'expired', command_json = NULL,
                   updated_at_ms = ?1
                  WHERE state = 'unconfirmed' AND created_at_ms <= ?2",
                params![now_ms, now_ms.saturating_sub(MIRROR_COMMAND_UNCONFIRMED_MS)],
            )
            .map_err(bad)?;
        tx.execute(
            "DELETE FROM bot_mirror_commands WHERE rowid IN (
               SELECT rowid FROM bot_mirror_commands
                WHERE state IN ('receipted', 'expired') AND updated_at_ms < ?1
                ORDER BY updated_at_ms LIMIT ?2)",
            params![
                now_ms.saturating_sub(MIRROR_COMMAND_KEEP_MS),
                MIRROR_COMMAND_PRUNE_BATCH
            ],
        )
        .map_err(bad)?;
        tx.commit().map_err(bad)?;
        Ok(expired)
    }

    /// Fixture only: every table that is not one of the six client tables,
    /// with every row of it as text. Two equal answers mean no canonical
    /// row was written between them.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn canonical_rows_for_fixture(&self) -> Vec<(String, Vec<String>)> {
        let conn = self.connection.fixture();
        let tables: Vec<String> = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table'
                   AND name NOT LIKE 'bot_mirror_%' AND name NOT LIKE 'sqlite_%'
                   AND sql NOT LIKE 'CREATE VIRTUAL%' ORDER BY name",
            )
            .and_then(|mut query| {
                let names = query
                    .query_map([], |row| row.get(0))?
                    .collect::<Result<Vec<String>, _>>();
                names
            })
            .unwrap_or_default();
        tables
            .into_iter()
            .filter_map(|table| {
                let mut query = conn.prepare(&format!("SELECT * FROM \"{table}\"")).ok()?;
                let columns = query.column_count();
                let rows = query
                    .query_map([], |row| {
                        Ok((0..columns)
                            .map(|column| match row.get_ref(column) {
                                Ok(value) => format!("{value:?}"),
                                Err(_) => "?".to_owned(),
                            })
                            .collect::<Vec<_>>()
                            .join("|"))
                    })
                    .ok()?
                    .collect::<Result<Vec<_>, _>>()
                    .ok()?;
                Some((table, rows))
            })
            .collect()
    }

    /// Fixture only: how many rows each of the six client tables holds, in
    /// the order owners, Bots, conversations, messages, team, commands.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn mirror_row_counts_for_fixture(&self) -> [i64; 6] {
        [
            "bot_mirror_owners",
            "bot_mirror_bots",
            "bot_mirror_chats",
            "bot_mirror_messages",
            "bot_mirror_team",
            "bot_mirror_commands",
        ]
        .map(|table| {
            self.connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap_or(-1)
        })
    }
}

#[cfg(test)]
mod tests;
