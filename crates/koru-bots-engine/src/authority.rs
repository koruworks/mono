// SPDX-License-Identifier: MPL-2.0

//! Engine-owned session credentials and immutable call identity. Only trusted
//! carrier adapters construct Worker; no provider request chooses that identity.
pub(crate) mod calls;
pub mod input;
pub use calls::{Binding, Call, Descriptor, Resolution};

use crate::{
    Database, Error,
    messages::{self, receipts},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Worker {
    pub bot: Uuid,
    pub incarnation: Uuid,
}

/// Debug is redacted; only the private response and provider launch expose it.
#[derive(Clone, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Secret(String);
impl Secret {
    pub fn parse(value: String) -> Result<Self, Error> {
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Unauthenticated);
        }
        Ok(Self(value))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    fn digest(&self) -> [u8; 32] {
        Sha256::digest(self.0.as_bytes()).into()
    }
}
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionSecret(<redacted>)")
    }
}
impl TryFrom<String> for Secret {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        Self::parse(value)
    }
}
impl From<Secret> for String {
    fn from(secret: Secret) -> Self {
        secret.0
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mint {
    pub command: Uuid,
    pub thread: Uuid,
    pub predecessor: Option<Uuid>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Issued {
    pub session: Uuid,
    pub secret: Option<Secret>,
    pub replayed: bool,
}

struct Identity {
    scope: String,
    bot: Uuid,
    thread: Uuid,
    session: Uuid,
    worker: Uuid,
}

fn encode(value: &impl Serialize) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(value).map_err(|_| Error::InvalidRequest)
}
fn decode<T: serde::de::DeserializeOwned>(value: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(value).map_err(|_| Error::StorageUnavailable)
}
fn id(value: String) -> Result<Uuid, Error> {
    Uuid::parse_str(&value).map_err(|_| Error::StorageUnavailable)
}

pub(crate) fn worker(connection: &Connection, who: Worker) -> Result<(), Error> {
    if who.bot.is_nil() || who.incarnation.is_nil() {
        return Err(Error::Unauthenticated);
    }
    let live: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM workers WHERE bot=?1 AND incarnation=?2)",
        params![who.bot.to_string(), who.incarnation.to_string()],
        |r| r.get(0),
    )?;
    if live {
        Ok(())
    } else {
        Err(Error::WorkerReplaced)
    }
}
pub(crate) fn membership(
    connection: &Connection,
    who: Worker,
    thread: Uuid,
) -> Result<String, Error> {
    worker(connection, who)?;
    let owner:Option<String>=connection.query_row("SELECT b.owner FROM bots b JOIN participants p ON p.participant=b.id AND p.kind='bot' JOIN threads t ON t.id=p.thread WHERE b.id=?1 AND t.id=?2 AND b.lifecycle='active' AND t.lifecycle='active'",params![who.bot.to_string(),thread.to_string()],|r|r.get(0)).optional()?;
    owner
        .filter(|scope| !scope.is_empty())
        .ok_or(Error::NotAllowed)
}
fn head(connection: &Connection, who: Worker, thread: Uuid) -> Result<Option<Uuid>, Error> {
    let current:Option<String>=connection.query_row("SELECT session FROM credentials WHERE bot=?1 AND thread=?2 ORDER BY rowid DESC LIMIT 1",params![who.bot.to_string(),thread.to_string()],|r|r.get(0)).optional()?;
    current.map(id).transpose()
}
fn identity(connection: &Connection, secret: &Secret) -> Result<Identity, Error> {
    let row:Option<(String,String,String,String,String,bool)>=connection.query_row("SELECT scope,bot,thread,session,worker_incarnation,revoked FROM credentials WHERE digest=?1",[secret.digest().as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
    let (scope, bot, thread, session, incarnation, revoked) = row.ok_or(Error::Unauthenticated)?;
    if revoked {
        return Err(Error::CredentialRevoked);
    }
    let identity = Identity {
        scope,
        bot: id(bot)?,
        thread: id(thread)?,
        session: id(session)?,
        worker: id(incarnation)?,
    };
    let current_scope = membership(
        connection,
        Worker {
            bot: identity.bot,
            incarnation: identity.worker,
        },
        identity.thread,
    )?;
    if current_scope != identity.scope {
        return Err(Error::NotAllowed);
    }
    Ok(identity)
}

impl Database {
    pub(crate) fn check_session(
        &self,
        who: Worker,
        thread: Uuid,
        session: Uuid,
        secret: &Secret,
    ) -> Result<(), Error> {
        let identity = identity(&self.connection, secret)?;
        if identity.bot != who.bot
            || identity.worker != who.incarnation
            || identity.thread != thread
            || identity.session != session
        {
            return Err(Error::Unauthenticated);
        }
        Ok(())
    }
    pub fn session_head(&self, who: Worker, thread: Uuid) -> Result<Option<Uuid>, Error> {
        membership(&self.connection, who, thread)?;
        head(&self.connection, who, thread)
    }

    /// Static pre-turn discovery uses the thread grant captured at session
    /// start. It exposes no resource and never authorizes execution.
    pub fn session_catalog_grant(
        &mut self,
        who: Worker,
        secret: &Secret,
    ) -> Result<serde_json::Value, Error> {
        let tx = self.connection.transaction()?;
        let identity = identity(&tx, secret)?;
        if identity.bot != who.bot || identity.worker != who.incarnation {
            return Err(Error::Unauthenticated);
        }
        let grant: Vec<u8> = tx.query_row(
            "SELECT grant FROM credentials WHERE session=?1",
            [identity.session.to_string()],
            |r| r.get(0),
        )?;
        decode(&grant)
    }

    /// The Agent-authenticated gateway may discover a session's static grant.
    /// This is not resource authorization and never resolves a tool call.
    pub fn boundary_catalog_grant(&self, secret: &Secret) -> Result<serde_json::Value, Error> {
        let identity = identity(&self.connection, secret)?;
        let grant: Vec<u8> = self.connection.query_row(
            "SELECT grant FROM credentials WHERE session=?1",
            [identity.session.to_string()],
            |row| row.get(0),
        )?;
        decode(&grant)
    }

    /// The only persisted secret material is its digest. A retry returns the
    /// issued incarnation, never the bearer; lost issuance requires replacement.
    pub fn mint_session(&mut self, who: Worker, request: Mint, now: i64) -> Result<Issued, Error> {
        if request.command.is_nil() || request.thread.is_nil() {
            return Err(Error::InvalidRequest);
        }
        let digest = receipts::digest("mint-session", &request)?;
        let subject = format!("session-mint:{}:{}", who.bot, who.incarnation);
        let tx = self.connection.transaction()?;
        let scope = membership(&tx, who, request.thread)?;
        if let Some(session) = receipts::find::<Uuid>(&tx, &subject, request.command, &digest)? {
            return Ok(Issued {
                session,
                secret: None,
                replayed: true,
            });
        }
        if head(&tx, who, request.thread)? != request.predecessor {
            return Err(Error::SessionConflict);
        }
        let running: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM turns WHERE bot=?1 AND thread=?2 AND state IN ('running','needs_you'))",
            params![who.bot.to_string(), request.thread.to_string()],
            |r| r.get(0),
        )?;
        if running {
            return Err(Error::TurnRunning);
        }
        let (grant, kind): (Vec<u8>, String) = tx.query_row(
            "SELECT b.access,t.kind FROM bots b JOIN threads t ON t.id=?2 WHERE b.id=?1",
            params![who.bot.to_string(), request.thread.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        // M3 supplies the Routine's narrower grant. Never use the main grant
        // merely because that composition has not landed yet.
        if kind == "routine" {
            return Err(Error::NotAllowed);
        }
        let session = Uuid::new_v4();
        let secret = Secret(format!(
            "{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        ));
        tx.execute(
            "UPDATE credentials SET revoked=1 WHERE bot=?1 AND thread=?2 AND revoked=0",
            params![who.bot.to_string(), request.thread.to_string()],
        )?;
        tx.execute("INSERT INTO credentials(digest,scope,bot,thread,session,worker_incarnation,grant) VALUES (?1,?2,?3,?4,?5,?6,?7)",params![secret.digest().as_slice(),scope,who.bot.to_string(),request.thread.to_string(),session.to_string(),who.incarnation.to_string(),grant])?;
        receipts::save(&tx, &subject, request.command, &digest, &session, now)?;
        tx.commit()?;
        Ok(Issued {
            session,
            secret: Some(secret),
            replayed: false,
        })
    }

    /// Authenticated production claim. The engine actor exclusively owns this
    /// connection; no command can interleave validation with claim_messages.
    pub fn claim_session_messages(
        &mut self,
        who: Worker,
        session: Uuid,
        resume: messages::Resume,
        budget: usize,
        now: i64,
    ) -> Result<Option<messages::Delivery>, Error> {
        worker(&self.connection, who)?;
        let thread:Option<String>=self.connection.query_row("SELECT thread FROM credentials WHERE bot=?1 AND worker_incarnation=?2 AND session=?3 AND revoked=0",params![who.bot.to_string(),who.incarnation.to_string(),session.to_string()],|r|r.get(0)).optional()?;
        let thread = id(thread.ok_or(Error::CredentialRevoked)?)?;
        membership(&self.connection, who, thread)?;
        // No route has qualified warm reuse: one turn ever per incarnation.
        let used: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM turns WHERE session=?1)",
            [session.to_string()],
            |r| r.get(0),
        )?;
        if used {
            return Err(Error::SessionConflict);
        }
        self.claim_messages(
            who.bot,
            thread,
            who.incarnation,
            session,
            resume,
            budget,
            now,
        )
    }
}

#[cfg(test)]
mod tests;
