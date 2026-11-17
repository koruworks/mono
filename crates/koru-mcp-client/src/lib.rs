// SPDX-License-Identifier: MPL-2.0

//! A client of a remote Koru MCP server.
//!
//! One tool call is one `POST` carrying one JSON-RPC message, under a bearer.
//! This crate builds that message, reads the answer, and says in types what
//! the answer means: a read is an answer or a [`Refusal`]; a write is a
//! [`WriteOutcome`], which is exactly one of saved, not saved, partially saved
//! or unknown.
//!
//! # Nothing constructs this yet
//!
//! **No product code builds a [`Client`].** This crate is the client and its
//! fake, and nothing else: no composition, no backend that forwards a Bot's
//! calls, no credential storage, no consent flow, no UI. The two ports below
//! have no implementation outside [`fake`]. The forwarding backend that will
//! sit on this client is a later plan (Bots M9, plan M9-C2).
//!
//! # The two ports
//!
//! - [`Transport`] sends one request and returns one response. The client
//!   never opens a socket and holds no address: where a request goes is the
//!   adapter's, which is why there is no URL, host name or origin anywhere in
//!   this crate. (The one protected resource the hosted server serves is
//!   already a constant, `koru_account_wire::app_connections::MCP_RESOURCE`;
//!   an adapter reuses it.)
//! - [`AccessTokens`] hands over the bearer for one request. The client keeps
//!   no token between calls and never refreshes one.
//!
//! # What is taken from a document, and what was observed
//!
//! **Nothing here has spoken to the hosted server.** Its deployment is
//! unverified, and its source is not in this repository. Every behavior is
//! the author's reading of a document, and each place says which:
//!
//! - `contracts/hosted-tools-v1.md`: effects, the three states, the four
//!   answers of a mutating call, the idempotency key, claims, the other hosted
//!   codes.
//! - `crates/koru-mcp-server/MCP.md`: result shapes, the error shape
//!   (`{ code, message, details? }` under `isError`).
//! - The request framing (the protocol revision, its headers, the `401`, the
//!   `403`, the revision refusal) is in neither of those two. It is taken from
//!   `docs/cloud-platform/mcp/HOST.md`, a page of the team's internal
//!   documentation that describes the hosted server as built. Those places
//!   are marked "HOST.md" and are the least certain part of this crate.
//!
//! What *was* observed: the tool schemas and the tool layer's answers, because
//! [`fake`] runs the real shared executor from `koru-mcp-server` over an
//! in-memory Store. What the hosted server adds on top (claims, coverage, the
//! front door, admission) is scripted in the fake by hand.
//!
//! # Outcomes are conservative
//!
//! - A write is `saved` only when the answer is a success that carries what a
//!   success must carry. Anything unread, unparsed or unanswered after the
//!   request left is `unknown`, never `not saved`.
//! - `not saved` is only said when it is proven: the request never left, the
//!   front door refused it, or the tool answered a refusal of its own.
//! - **The client never retries.** After `unknown` or `partially saved` the
//!   only repeat it offers is [`Client::finish_create`], for
//!   `create_document` with the same idempotency key. An edit has no key and
//!   no tool looks an operation up, so an unknown edit is the caller's to
//!   resolve by reading the document.
//!
//! # Nothing secret or authored is printed
//!
//! No error text and no `Debug` output of any type here carries a token, a
//! claim handle, a document's content or a title. Answers are held in
//! [`Sealed`], which prints as its size, and a caller that needs one asks for
//! it by name. This crate logs nothing.

mod client;
mod outcome;
mod protocol;
mod sealed;
mod tools;
mod transport;

/// A hosted MCP server in memory, scripted from the published documents.
/// In no normal build: compiled for this crate's own tests, and for another
/// crate's only behind `test-support`.
#[cfg(any(test, feature = "test-support"))]
pub mod fake;

pub use client::{Client, ListedTool, ListedWorkspace, NameStatus};
pub use outcome::{
    ClaimGrant, ClaimHandle, ClaimedDocument, Coverage, Effect, EffectKind, EffectState,
    InvalidCall, NotSaved, PartiallySaved, PendingCreate, Read, Recovery, Refusal, Revision, Saved,
    SearchCoverage, ToolRefusal, Unknown, UnknownCause, WriteOutcome,
};
pub use protocol::PROTOCOL_REVISION;
pub use sealed::Sealed;
pub use tools::{HostedTool, ReadTool, WriteTool};
pub use transport::{
    AccessToken, AccessTokens, InvalidAccessToken, PostRequest, PostResponse, Transport,
    TransportFailure,
};
