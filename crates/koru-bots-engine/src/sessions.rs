// SPDX-License-Identifier: MPL-2.0

//! One provider owner per conversation. Blocking provider, journal and port
//! calls run on a dedicated session thread, never on IPC/heartbeat dispatch.
//! The workspace/credential port is supplied by the reviewed tool boundary;
//! there is deliberately no direct Store reader or credential fallback here.

mod pool;
mod run;
pub use pool::{Finished, Pool};

use crate::{
    Error,
    bot_database::{AgentState, BotDatabase, JournalEntry},
    messages::{Delivery, Resume, Turn},
};
use koru_session_protocol::{AgentEvent, DelegationLaunch, SessionId};
use koru_session_runtime::providers::{
    ManagedProviderFactory, ManagedProviderRequest, ProviderFailure,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, atomic::AtomicBool, mpsc};
use uuid::Uuid;

pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_SAVED_INPUTS: usize = 1024;

/// Current page content, fetched on every fresh or resumed incarnation.
/// Absence is explicit; an unreadable page is a port error, not an empty page.
#[derive(Clone, Serialize)]
pub struct Context {
    pub role: Option<Page>,
    pub knowledge: Vec<Page>,
    pub memory: Option<Page>,
}
#[derive(Clone, Serialize)]
pub struct Page {
    pub id: Uuid,
    pub text: String,
}

/// Private trusted carrier. Implementations authenticate the worker and
/// fence every turn. `claim` must finish operation reconciliation first;
/// it must not return work while a barrier or unresolved write blocks it.
/// None of these methods is exposed directly to provider JSON.
pub trait Port: Send + Sync + 'static {
    fn context(&self, thread: Uuid) -> Result<Context, Error>;
    /// A new command and session for each attempt, including failed-resume
    /// fallback. Secrets stay in this return value and the provider launch.
    fn credential(&self, thread: Uuid, session: Uuid) -> Result<DelegationLaunch, Error>;
    fn claim(
        &self,
        thread: Uuid,
        session: Uuid,
        resume: Resume,
        budget: usize,
    ) -> Result<Option<Delivery>, Error>;
    fn accepted(&self, turn: Turn, delivery: Uuid) -> Result<(), Error>;
    fn unknown(&self, turn: Turn, delivery: Uuid) -> Result<(), Error>;
    /// Observations, not Publish. A provider's final text cannot complete an
    /// obligation: the tool boundary's explicit Publish transaction does that.
    fn event(&self, turn: Turn, event: &AgentEvent) -> Result<(), Error>;
    /// Called only after confirmed provider shutdown. Preserve an already
    /// published answer; an unfinished turn remains recoverable.
    fn retired(&self, turn: Turn) -> Result<(), Error>;
}

pub struct Start {
    pub thread: Uuid,
    pub worker: Uuid,
    pub request: ManagedProviderRequest,
    pub factory: Arc<dyn ManagedProviderFactory>,
    pub port: Arc<dyn Port>,
}

pub enum Action {
    Approval {
        id: koru_session_protocol::ApprovalId,
        decision: koru_session_protocol::ApprovalDecision,
        choice: Option<koru_session_protocol::ApprovalChoiceId>,
    },
}
struct Control {
    turn: Turn,
    action: Action,
}

#[derive(Clone, Debug)]
pub enum Failure {
    Engine(Error),
    Provider(ProviderFailure),
    /// Process death was not confirmed; retain the capacity reservation
    /// until the owning Bot worker is confirmed dead.
    RetirementUnconfirmed,
}
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Engine(error)
    }
}
impl From<ProviderFailure> for Failure {
    fn from(error: ProviderFailure) -> Self {
        Self::Provider(error)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    version: u8,
    token: Option<koru_session_protocol::ProviderResumeToken>,
    context_hash: [u8; 32],
    #[serde(default)]
    context_delivery: Option<Uuid>,
    inputs: Vec<InputProof>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputProof {
    delivery: Uuid,
    hash: [u8; 32],
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(value).map_err(|_| Error::InvalidRequest)
}
fn journal(
    database: &Mutex<BotDatabase>,
    turn: Uuid,
    entry: JournalEntry<'_>,
) -> Result<(), Error> {
    database
        .lock()
        .map_err(|_| Error::StorageUnavailable)?
        .journal(turn, entry, now())
}
fn save(
    database: &Mutex<BotDatabase>,
    start: &Start,
    session: SessionId,
    state: &Saved,
) -> Result<(), Error> {
    database
        .lock()
        .map_err(|_| Error::StorageUnavailable)?
        .save(&AgentState {
            thread: start.thread,
            session: session.as_uuid(),
            provider: start.request.provider.as_str().into(),
            resume_state: encode(state)?,
            last_delivery: state.inputs.last().map(|p| p.delivery),
            saved_at_ms: now(),
        })
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests;
