// SPDX-License-Identifier: MPL-2.0

//! `actor/1`: who made a change, written as the change's Loro commit message.
//!
//! The format is [`contracts/connector-attribution-v1.md`]: one compact JSON
//! object on every commit an app connection's call makes — an AI assistant
//! editing a synced workspace through Koru's hosted MCP server. It travels
//! with the operations to every device and through every Cloud checkpoint,
//! which keep history.
//!
//! **Who writes it.** The host puts a [`ChangeAttribution`] in the
//! [`RequestContext`](crate::RequestContext); local hosts — the desktop bridge,
//! Koru AI — put none, and their commits carry no message, as before. The tool
//! layer labels its own commits by subscribing on each document it writes
//! before the first commit ([`attribute_commits`]) — the id stamps, the body,
//! the task changes, a created document's seed, the tags an edit creates — and
//! hands the message to the Store port for the commits the Store makes itself:
//! the manifest entry of a created or extracted document, and a system
//! document's registration.
//!
//! **Why a subscription and not `set_next_commit_message`.** The document
//! model commits after every operation, so "the next commit" is one of many;
//! a pre-commit subscription labels each of them, including the implicit
//! commit an export makes, and ends when it is dropped, so nothing outlives
//! the call that set it.
//!
//! [`contracts/connector-attribution-v1.md`]: ../../../contracts/connector-attribution-v1.md

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The format and its version, the value of `koru`.
pub const ACTOR_V1: &str = "actor/1";

/// The one v1 `kind`.
const KIND_ASSISTANT: &str = "assistant";

/// Which app connection a call came through, as the host knows it when the
/// call is admitted. Everything `actor/1` says except the call's own
/// operation id, which the tool layer mints per mutating call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeAttribution {
    /// The app connection.
    pub connection: Uuid,
    /// The connection's name now, 1–64 characters. A later rename does not
    /// rewrite history.
    pub name: String,
    /// The OAuth client id: a metadata document's https URL, or `dcr:<uuid>`.
    pub client: String,
    /// `true` only for a metadata-document client.
    pub verified: bool,
    /// The Koru account the connection belongs to.
    pub account: Uuid,
}

/// The wire spelling, in the contract's property order. serde writes a
/// struct's fields in declaration order with no whitespace, which is the
/// encoding the contract fixes.
#[derive(Serialize)]
struct Wire<'a> {
    koru: &'static str,
    kind: &'static str,
    connection: String,
    name: &'a str,
    client: &'a str,
    verified: bool,
    account: String,
    op: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    create: Option<WireCreate<'a>>,
}

#[derive(Serialize)]
struct WireCreate<'a> {
    request_hash: &'a str,
}

impl ChangeAttribution {
    /// The commit message for every commit of operation `op`, except a
    /// created document's seed.
    pub fn message(&self, op: Uuid) -> String {
        self.encode(op, None)
    }

    /// The commit message for a created document's seed commits: the same,
    /// with the creation's request hash, so a retried creation can recognise
    /// its first attempt from Cloud's history alone.
    pub fn seed_message(&self, op: Uuid, request_hash: &str) -> String {
        self.encode(op, Some(request_hash))
    }

    fn encode(&self, op: Uuid, request_hash: Option<&str>) -> String {
        serde_json::to_string(&Wire {
            koru: ACTOR_V1,
            kind: KIND_ASSISTANT,
            connection: self.connection.to_string(),
            name: &self.name,
            client: &self.client,
            verified: self.verified,
            account: self.account.to_string(),
            op: op.to_string(),
            create: request_hash.map(|request_hash| WireCreate { request_hash }),
        })
        .expect("strings and booleans always serialize")
    }
}

/// An `actor/1` message as a reader sees it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ActorMessage {
    pub kind: String,
    pub connection: Uuid,
    pub name: String,
    pub client: String,
    pub verified: bool,
    pub account: Uuid,
    pub op: Uuid,
    #[serde(default)]
    pub create: Option<ActorCreate>,
}

/// `create`, on a created document's seed commit.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ActorCreate {
    pub request_hash: String,
}

impl ActorMessage {
    /// Read a commit message, by the contract's reader rules: anything whose
    /// `koru` is not exactly `actor/1` — including a message that is not JSON
    /// at all, since commit messages are free text to Loro — is not one, and
    /// unknown properties are ignored.
    ///
    /// **A self-description, not provenance.** Any Loro peer can write these
    /// bytes; nothing may present what this returns as verified authorship.
    pub fn read(message: &str) -> Option<Self> {
        #[derive(Deserialize)]
        struct Tagged {
            koru: String,
            #[serde(flatten)]
            rest: serde_json::Value,
        }
        let tagged: Tagged = serde_json::from_str(message).ok()?;
        if tagged.koru != ACTOR_V1 {
            return None;
        }
        serde_json::from_value(tagged.rest).ok()
    }
}

/// Label every commit `doc` makes, while the returned subscription lives, with
/// `message`.
///
/// Subscribe **before the first commit** the call makes on that document; a
/// commit made earlier is already in the oplog and cannot be relabelled.
/// Every Koru commit path ends in an operation then a commit, and an export
/// commits implicitly, so a subscription held across a call's writes and its
/// persist labels all of them. A fork is a new document and carries no
/// subscription, which is right: the tools' validation forks are never
/// persisted.
pub(crate) fn attribute_commits(doc: &loro::LoroDoc, message: &str) -> loro::Subscription {
    let message: Arc<str> = message.into();
    doc.subscribe_pre_commit(Box::new(move |commit| {
        commit.modifier.set_message(&message);
        true
    }))
}

#[cfg(test)]
mod tests;
