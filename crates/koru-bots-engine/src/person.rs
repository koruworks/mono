// SPDX-License-Identifier: MPL-2.0
//! Person commands after gateway authentication. This core owns no Store,
//! process or network. The gateway vouches current owner and page placement;
//! neither identity nor workspace authority is read from a public body.
pub(crate) mod change;
mod lifecycle_ops;
mod lists;
mod read;

use crate::{
    Database, Error,
    configuration::Configuration,
    messages::{self, Person, Receipt},
};
use koru_delegate_core::CapturedBotPolicy;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub const MAX_COMMAND_BYTES: usize = koru_bot_client_wire::MAX_FRAME_BYTES / 2;

#[derive(Clone, Serialize, Deserialize)]
pub struct Request {
    pub owner: String,
    pub sender: String,
    pub command: Command,
}
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PersonRequest(<redacted>)")
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Reply(pub Value);
impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PersonReply(<redacted>)")
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Command {
    Read {
        query: Query,
    },
    Change {
        id: Uuid,
        action: Change,
    },
    Send {
        envelope: messages::Envelope,
    },
    Answer {
        id: Uuid,
        question: Uuid,
        answer: crate::questions::Answer,
    },
    Stop {
        id: Uuid,
        turn: messages::Turn,
    },
    Continue {
        id: Uuid,
        thread: Uuid,
        operations: Vec<crate::operations::Key>,
    },
    Observe {
        thread: Uuid,
        boot: Option<Uuid>,
        after: u64,
    },
}
impl Command {
    pub fn changes(&self) -> bool {
        !matches!(self, Self::Read { .. } | Self::Observe { .. })
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Query {
    ConversationSettings {
        bot: Uuid,
        thread: Uuid,
    },
    ChangeReceipt {
        id: Uuid,
        action: Change,
    },
    ControlReceipt {
        bot: Uuid,
        request: Uuid,
    },
    Bot {
        bot: Uuid,
    },
    Bots {
        workspace: Uuid,
        after: Option<Uuid>,
        lifecycle: Lifecycle,
    },
    Threads {
        bot: Uuid,
        after: Option<Uuid>,
        lifecycle: Lifecycle,
    },
    Conversations {
        bot: Uuid,
        after: Option<ConversationCursor>,
        limit: u16,
    },
    TrashedConversations {
        workspace: Uuid,
        after: Option<Uuid>,
        limit: u16,
    },
    Activity {
        workspace: Uuid,
    },
    Thread {
        thread: Uuid,
    },
    History {
        thread: Uuid,
        after: u64,
    },
    Questions {
        thread: Uuid,
    },
    Outputs {
        thread: Uuid,
        after: Option<Uuid>,
    },
    Team,
    LifecycleOperation {
        bot: Uuid,
        request: Uuid,
    },
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Lifecycle {
    Active,
    Archived,
    Trashed,
}
impl Lifecycle {
    fn token(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
            Self::Trashed => "trashed",
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Definition {
    pub name: String,
    #[serde(default)]
    pub avatar: Option<u8>,
    pub configuration: Configuration,
    pub access: CapturedBotPolicy,
    pub role: Option<Uuid>,
    pub knowledge: Vec<Uuid>,
    pub memory: Option<Uuid>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Change {
    ConversationSettings {
        bot: Uuid,
        thread: Uuid,
        revision: u64,
        provider: koru_session_protocol::ProviderKind,
        selection: koru_session_protocol::LaunchSelection,
    },
    Create {
        bot: Uuid,
        definition: Definition,
    },
    Configure {
        bot: Uuid,
        revision: u64,
        name: String,
        #[serde(default)]
        avatar: Option<u8>,
        configuration: Configuration,
        /// When present, save the Role with the other settings atomically.
        #[serde(default)]
        role: Option<Uuid>,
    },
    Knowledge {
        bot: Uuid,
        revision: u64,
        knowledge: Vec<Uuid>,
    },
    Library {
        bot: Uuid,
        revision: u64,
        role: Option<Uuid>,
        knowledge: Vec<Uuid>,
        memory: Option<Uuid>,
    },
    Access {
        bot: Uuid,
        revision: u64,
        access: CapturedBotPolicy,
    },
    Permissions {
        bot: Uuid,
        revision: u64,
        policy_revision: u64,
        access: koru_delegate_core::BotAccess,
        selected_pages: Vec<Uuid>,
        connections: Option<std::collections::BTreeSet<koru_delegate_core::ConnectionGrantRef>>,
        may_ask: Option<bool>,
    },
    Pause {
        bot: Uuid,
        revision: u64,
        paused: bool,
    },
    Team {
        revision: u64,
        paused: bool,
    },
    BotLifecycle {
        bot: Uuid,
        revision: u64,
        lifecycle: Lifecycle,
        #[serde(default)]
        source: Option<Uuid>,
    },
    Conversation {
        bot: Uuid,
        title: String,
    },
    ThreadLifecycle {
        thread: Uuid,
        revision: u64,
        lifecycle: Lifecycle,
        #[serde(default)]
        source: Option<Uuid>,
    },
    /// Existing window controls are checked against the Bot revision they read.
    ConversationLifecycle {
        bot: Uuid,
        revision: u64,
        thread: Uuid,
        lifecycle: Lifecycle,
        source: Option<Uuid>,
    },
    RenameThread {
        thread: Uuid,
        revision: u64,
        title: String,
    },
    RetrySetup {
        thread: Uuid,
        turn: Uuid,
    },
    Reask {
        thread: Uuid,
        question: Uuid,
    },
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Bot {
    pub id: Uuid,
    pub revision: u64,
    pub policy_revision: u64,
    pub definition: Definition,
    pub paused: bool,
    pub may_ask: bool,
    pub lifecycle: Lifecycle,
    pub held_by: Option<LifecycleHolder>,
    pub main_thread: Uuid,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub folder_frozen_at_ms: Option<i64>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: Uuid,
    pub bot: Uuid,
    pub title: String,
    pub kind: String,
    pub lifecycle: Lifecycle,
    pub held_by: Option<LifecycleHolder>,
    pub revision: u64,
    pub origin_thread: Option<Uuid>,
    pub origin_message: Option<Uuid>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub settings: crate::configuration::ConversationSettings,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ConversationCursor {
    pub main: bool,
    pub updated_at_ms: i64,
    pub thread: Uuid,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct LifecycleHolder {
    pub request: Uuid,
    pub lifecycle: Lifecycle,
    pub completed_at_ms: i64,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct LifecycleOperation {
    pub request: Uuid,
    pub bot: Uuid,
    pub thread: Option<Uuid>,
    pub prior: Lifecycle,
    pub lifecycle: Lifecycle,
    pub prior_holder: Option<LifecycleHolder>,
    pub source: Option<Uuid>,
    pub reversed_by: Option<Uuid>,
    pub completed_at_ms: i64,
}
#[derive(Serialize, Deserialize)]
pub struct Team {
    pub paused: bool,
    pub revision: u64,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    Pause,
    Resume,
    Access,
}
impl ControlKind {
    fn token(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Access => "access",
        }
    }
}
#[derive(Serialize, Deserialize)]
pub struct ControlReceipt {
    pub request: Uuid,
    pub bot: Uuid,
    pub kind: ControlKind,
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(value).map_err(|_| Error::InvalidRequest)
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(bytes).map_err(|_| Error::StorageUnavailable)
}
fn value(value: impl Serialize) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|_| Error::InvalidRequest)
}
fn id(value: String) -> Result<Uuid, Error> {
    Uuid::parse_str(&value).map_err(|_| Error::StorageUnavailable)
}
fn lifecycle(value: String) -> Result<Lifecycle, Error> {
    match value.as_str() {
        "active" => Ok(Lifecycle::Active),
        "archived" => Ok(Lifecycle::Archived),
        "trashed" => Ok(Lifecycle::Trashed),
        _ => Err(Error::StorageUnavailable),
    }
}
fn owner(connection: &rusqlite::Connection, who: &Person, bot: Uuid) -> Result<(), Error> {
    let allowed: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM bots WHERE id=?1 AND owner=?2)",
        rusqlite::params![bot.to_string(), who.owner],
        |r| r.get(0),
    )?;
    if allowed {
        Ok(())
    } else {
        Err(Error::NotAllowed)
    }
}
/// Execute a request already vouched by the gateway/Agent carrier. Embedders
/// must authenticate before constructing its identity, as the IPC owner does.
pub fn handle(
    db: &mut Database,
    events: &crate::observations::LiveEvents,
    answers: &mut crate::questions::Transient,
    request: Request,
    now: i64,
) -> Result<Reply, Error> {
    if request.owner.is_empty()
        || request.sender.is_empty()
        || request.owner.len() > 256
        || request.sender.len() > 256
        || encode(&request)?.len() > koru_bot_client_wire::MAX_FRAME_BYTES / 2
    {
        return Err(Error::InvalidRequest);
    }
    let who = Person {
        owner: request.owner,
        sender: request.sender,
    };
    let result = match request.command {
        Command::Read { query } => read::query(db, &who, query),
        Command::Change { id, action } => value(change::apply(db, &who, id, &action, now)?),
        Command::Send { envelope } => value(db.accept_message(&who, &envelope, now)?),
        Command::Answer {
            id,
            question,
            answer,
        } => value(db.answer_question(&who, id, question, &answer, answers, now)?),
        Command::Stop { id, turn } => value(db.stop_turn(&who, id, turn, now)?),
        Command::Continue {
            id,
            thread,
            operations,
        } => value(db.continue_operations(&who, id, thread, &operations, now)?),
        Command::Observe {
            thread,
            boot,
            after,
        } => value(events.read(db, &who, thread, boot, after)?),
    }?;
    if encode(&result)?.len() > koru_bot_client_wire::MAX_FRAME_BYTES {
        return Err(Error::InputTooSmall);
    }
    Ok(Reply(result))
}

#[cfg(test)]
mod tests;
