// SPDX-License-Identifier: MPL-2.0

//! What an answer means: refusals, and the four outcomes of a write.
//!
//! The vocabulary is `contracts/hosted-tools-v1.md` sections 1 and 2. A
//! mutating call makes its effects in a fixed order and stops at the first
//! that is not saved, and answers by this table (section 2):
//!
//! | Effects | Answer | Here |
//! | --- | --- | --- |
//! | all `saved` | success, with the tool's result | [`WriteOutcome::Saved`] |
//! | all `not_saved` | the refusal's own code, or `edit_not_saved` | [`WriteOutcome::NotSaved`] |
//! | a `saved` prefix, the rest `not_saved` | `partially_saved` | [`WriteOutcome::PartiallySaved`] |
//! | any `unknown` | `save_status_unknown` | [`WriteOutcome::Unknown`] |
//!
//! The client adds one thing the table cannot say, because the table is the
//! server's: **the answer may never arrive, or may not be readable.** Both are
//! [`WriteOutcome::Unknown`], with a [`UnknownCause`] that says which.
//!
//! **Unverified against the hosted server.** The table, the envelope and the
//! codes are read from the contract. The shared tool layer in this repository
//! builds the same envelope (`koru_mcp_server::effects`), and the tests run
//! against that; what the deployed service sends has not been seen.

use std::fmt;

use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::sealed::Sealed;
use crate::tools::{WriteTool, is_tool_name};

// ---------------------------------------------------------------- refusals

/// Why a call has no answer to use. Each has a stable [`code`](Refusal::code).
///
/// For a write, a `Refusal` inside [`NotSaved`] is a proof that nothing was
/// saved. The two that are not proofs, [`Refusal::NoAnswer`] and
/// [`Refusal::AnswerNotUnderstood`], are only ever returned by a read; a write
/// in the same position is [`WriteOutcome::Unknown`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The hosted server would not accept the bearer: `401`. HOST.md
    /// ("Routes"): "A missing or invalid token is 401". An access token lives
    /// 15 minutes (`docs/developers/APP-CONNECTIONS.md`, "The MCP access
    /// token"), so an expired one is the ordinary case, and the server
    /// answers every token it will not accept the same way. What to do is the
    /// same for all of them: the credential's owner gets a new one.
    TokenExpired,
    /// The connection was revoked: `authorization_revoked`
    /// (`contracts/hosted-tools-v1.md` section 2). A new token will not help;
    /// the person connects again.
    ConnectionRevoked,
    /// The account's Cloud subscription has ended:
    /// `cloud_subscription_ended` (same section). The connection and its
    /// credential survive a lapse (`APP-CONNECTIONS.md`: "checked before
    /// rotation so the refresh token survives a lapse").
    SubscriptionEnded,
    /// The server does not speak the protocol revision this client sent:
    /// `400` with JSON-RPC `-32022` (HOST.md, "Routes"). `supported` is what
    /// the server said it speaks. **Never a retry**: the same build sends the
    /// same revision.
    RevisionRefused { supported: Vec<String> },
    /// The connection is Read and the tool edits: `403 insufficient_scope`
    /// (HOST.md, "Routes").
    InsufficientScope,
    /// A `403` that names no scope. HOST.md gives one more cause, an `Origin`
    /// the server does not allow, which this client never sends.
    Forbidden,
    /// The server could not read the connection's state:
    /// `authorization_unavailable` (`contracts/hosted-tools-v1.md` section 2).
    /// A later call may succeed.
    AuthorizationUnavailable,
    /// The server does not serve a tool of that name: `tool_not_available`
    /// (`contracts/hosted-tools-v1.md` section 3: any tool outside the
    /// catalog, "with no backend work").
    ToolNotAvailable,
    /// The tool answered a refusal of its own, by its code.
    Tool(ToolRefusal),
    /// The server refused the request itself with a JSON-RPC error the front
    /// door is documented to give before it runs anything: `-32020` (a header
    /// that differs from the body) or `-32601` (a method it does not
    /// implement). Either is a fault in this client, not in the call.
    RequestRefused { rpc_code: i64 },
    /// The request never left: the transport proved it
    /// ([`TransportFailure::NotSent`](crate::TransportFailure::NotSent)).
    NotSent,
    /// Read only. The request left and no response came back.
    NoAnswer,
    /// Read only. A response came back and this client could not read it as
    /// the answer to its request.
    AnswerNotUnderstood,
    /// The client refused the call before asking for a token: the arguments
    /// or the tool's name could not be sent as they were.
    InvalidCall(InvalidCall),
}

/// What was wrong with a call the client refused to send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidCall {
    /// A tool's arguments are a JSON object.
    ArgumentsNotAnObject,
    /// `create_document`'s `idempotency_key` was present and not a UUID.
    IdempotencyKeyNotAUuid,
    /// A name that is not a short lower-case token, or, for
    /// [`Client::call_unlisted`](crate::Client::call_unlisted), a name the
    /// hosted catalog does have.
    ToolName,
}

impl Refusal {
    /// The stable code. A caller may store it and branch on it; it never
    /// carries anything from the answer.
    pub const fn code(&self) -> &'static str {
        match self {
            Refusal::TokenExpired => "token_expired",
            Refusal::ConnectionRevoked => "connection_revoked",
            Refusal::SubscriptionEnded => "subscription_ended",
            Refusal::RevisionRefused { .. } => "revision_refused",
            Refusal::InsufficientScope => "insufficient_scope",
            Refusal::Forbidden => "forbidden",
            Refusal::AuthorizationUnavailable => "authorization_unavailable",
            Refusal::ToolNotAvailable => "tool_not_available",
            Refusal::Tool(_) => "tool_refused",
            Refusal::RequestRefused { .. } => "request_refused",
            Refusal::NotSent => "not_sent",
            Refusal::NoAnswer => "no_answer",
            Refusal::AnswerNotUnderstood => "answer_not_understood",
            Refusal::InvalidCall(_) => "invalid_call",
        }
    }

    /// Whether this is a proof that the server ran nothing. False only for
    /// the two a read can return when its answer is missing or unreadable.
    pub const fn proves_nothing_ran(&self) -> bool {
        !matches!(self, Refusal::NoAnswer | Refusal::AnswerNotUnderstood)
    }
}

impl fmt::Display for Refusal {
    /// The code, and for a tool's refusal the tool's own code beside it.
    /// Never a message from the server: a message can quote a document.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Tool(refusal) => write!(f, "{}: {}", self.code(), refusal.code()),
            other => f.write_str(other.code()),
        }
    }
}

impl std::error::Error for Refusal {}

/// A tool's own refusal: `{ code, message, details? }` under `isError`
/// (`crates/koru-mcp-server/MCP.md`, "Results").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRefusal {
    code: String,
    /// Set for `document_claimed`.
    pub claimed: Option<ClaimedDocument>,
    /// Set for `task_index_incomplete`, when the refusal carried its counts.
    pub coverage: Option<SearchCoverage>,
    answer: Sealed<Value>,
}

impl ToolRefusal {
    /// The hosted server's code: `revision_conflict`, `document_claimed`,
    /// `edit_not_saved` and so on. A code that is not a short lower-case
    /// token is reported as `unrecognised`, so this is always safe to print.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The whole error object as the server sent it, for a caller that hands
    /// it on unchanged. It can quote a document.
    pub fn answer(&self) -> &Sealed<Value> {
        &self.answer
    }
}

/// What `document_claimed` says about the claim in the way
/// (`contracts/hosted-tools-v1.md` section 4, "Refusals"). The holder's name,
/// label and purpose are self-declared text and stay in the sealed answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClaimedDocument {
    pub document_id: Option<Uuid>,
    /// Unix milliseconds.
    pub expires_at: Option<i64>,
    pub retry_after_seconds: Option<u64>,
}

/// Turn a tool's error object into a refusal. `raw` is the whole object.
pub(crate) fn refusal_of_tool_error(raw: Value) -> Refusal {
    let code = raw.get("code").and_then(Value::as_str).unwrap_or_default();
    match code {
        // Admission's three (`contracts/hosted-tools-v1.md` section 2).
        "authorization_revoked" => Refusal::ConnectionRevoked,
        "cloud_subscription_ended" => Refusal::SubscriptionEnded,
        "authorization_unavailable" => Refusal::AuthorizationUnavailable,
        "tool_not_available" => Refusal::ToolNotAvailable,
        // HOST.md gives this as a `403`. The shared executor has no such
        // code, but a server that answered it as a tool error would mean the
        // same thing.
        "insufficient_scope" => Refusal::InsufficientScope,
        _ => {
            let details = raw.get("details");
            let claimed = (code == "document_claimed").then(|| ClaimedDocument {
                document_id: details
                    .and_then(|details| details.get("document_id"))
                    .and_then(Value::as_str)
                    .and_then(|id| Uuid::parse_str(id).ok()),
                expires_at: details
                    .and_then(|details| details.get("expires_at"))
                    .and_then(Value::as_i64),
                retry_after_seconds: details
                    .and_then(|details| details.get("retry_after_seconds"))
                    .and_then(Value::as_u64),
            });
            let coverage = (code == "task_index_incomplete")
                .then(|| details.and_then(SearchCoverage::parse))
                .flatten();
            let code = if is_tool_name(code) {
                code.to_owned()
            } else {
                "unrecognised".to_owned()
            };
            Refusal::Tool(ToolRefusal {
                code,
                claimed,
                coverage,
                answer: Sealed::new(raw),
            })
        }
    }
}

// ------------------------------------------------------------------- reads

/// A read's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    /// How much of the workspace the answer covers.
    pub coverage: Coverage,
    answer: Sealed<Value>,
}

impl Read {
    pub(crate) fn new(coverage: Coverage, answer: Value) -> Self {
        Self {
            coverage,
            answer: Sealed::new(answer),
        }
    }

    /// The tool's result object. It can hold a document.
    pub fn answer(&self) -> &Sealed<Value> {
        &self.answer
    }
}

/// How much a read's answer covers.
///
/// Only `search_documents` reports coverage. The hosted server searches
/// titles of every listed document, and content only in the documents that
/// were opened through this connection; `search_coverage` reports the counts
/// (the tool's own published description, `koru-mcp-server/src/lib.rs`
/// `search_documents`; the field names are HOST.md's, "Metadata, document
/// bodies and search").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    /// The tool does not search.
    NotApplicable,
    /// A search answered without `search_coverage`, or with one this client
    /// could not read. **Not the same as complete**: an empty answer here
    /// says nothing about content.
    NotReported,
    /// Content was searched in some documents only. No hit means nothing
    /// about the rest.
    Partial(SearchCoverage),
    /// Content was searched in every eligible document.
    Complete(SearchCoverage),
}

/// The counts of a `search_coverage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchCoverage {
    pub content_documents_searched: u64,
    pub eligible_documents: u64,
}

impl SearchCoverage {
    /// The two counts out of an object that has them.
    fn parse(value: &Value) -> Option<Self> {
        Some(Self {
            content_documents_searched: value.get("content_documents_searched")?.as_u64()?,
            eligible_documents: value.get("eligible_documents")?.as_u64()?,
        })
    }
}

impl Coverage {
    /// Read `search_coverage` out of a `search_documents` answer.
    ///
    /// Complete only when the server says so **and** its own counts agree; a
    /// `partial: false` beside fewer documents searched than eligible is
    /// reported as partial.
    pub(crate) fn of_search(answer: &Value) -> Self {
        let Some(reported) = answer.get("search_coverage") else {
            return Coverage::NotReported;
        };
        let (Some(counts), Some(partial)) = (
            SearchCoverage::parse(reported),
            reported.get("partial").and_then(Value::as_bool),
        ) else {
            return Coverage::NotReported;
        };
        if partial || counts.content_documents_searched < counts.eligible_documents {
            Coverage::Partial(counts)
        } else {
            Coverage::Complete(counts)
        }
    }
}

// ------------------------------------------------------------------ writes

/// What became of a write. Exactly one of four.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOutcome {
    /// The server acknowledged every effect, and the answer carries what a
    /// success of this tool must carry.
    Saved(Saved),
    /// Proven to have changed nothing.
    NotSaved(NotSaved),
    /// Some effects are saved and the rest are not. A rollback is never
    /// claimed.
    PartiallySaved(PartiallySaved),
    /// Nobody knows. **Never shown as saved, and never repeated except
    /// through [`Recovery::FinishCreate`].**
    Unknown(Unknown),
}

impl WriteOutcome {
    /// The contract's own word for the outcome.
    pub const fn code(&self) -> &'static str {
        match self {
            WriteOutcome::Saved(_) => "saved",
            WriteOutcome::NotSaved(_) => "not_saved",
            WriteOutcome::PartiallySaved(_) => "partially_saved",
            WriteOutcome::Unknown(_) => "unknown",
        }
    }
}

/// A saved write.
///
/// A hosted success carries the tool's ordinary result: ids and a revision.
/// It carries no operation id and no `history`
/// (`contracts/hosted-tools-v1.md` sections 2 and 3). So there is no version
/// to pin here, only a revision to record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    /// `create_document`: the new document. `extract_to_document`: the
    /// destination.
    pub document_id: Option<Uuid>,
    /// The written document's revision. `extract_to_document`: the
    /// destination's (`document_revision`). Always set for the five tools
    /// that write a document; a success of one of them without it is
    /// reported as [`WriteOutcome::Unknown`] instead.
    pub revision: Option<Revision>,
    /// `extract_to_document` only: the source's revision after the move.
    pub source_revision: Option<Revision>,
    /// `create_document` only: the key the creation was made under.
    pub idempotency_key: Option<Uuid>,
    /// `claim_document` only.
    pub claim: Option<ClaimGrant>,
    answer: Sealed<Value>,
}

impl Saved {
    /// The tool's result object. It can hold a document.
    pub fn answer(&self) -> &Sealed<Value> {
        &self.answer
    }
}

/// A document revision: opaque, and compared only for equality. A caller
/// sends it back as `base_revision`.
#[derive(Clone, PartialEq, Eq)]
pub struct Revision(String);

impl Revision {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Revision {
    /// Its length only. It is the server's text, and nothing the server
    /// sends is printed.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Revision({} bytes)", self.0.len())
    }
}

/// A claim's handle. **It is the bearer of the claim**
/// (`contracts/hosted-tools-v1.md` section 4), so it does not print.
#[derive(Clone, PartialEq, Eq)]
pub struct ClaimHandle(String);

impl ClaimHandle {
    /// The handle, to pass as `claim_id` or in `claim_ids`.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ClaimHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClaimHandle(sealed)")
    }
}

/// What `claim_document` granted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimGrant {
    pub handle: ClaimHandle,
    /// Unix milliseconds.
    pub expires_at: i64,
}

/// A write that changed nothing, and the proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotSaved {
    /// Why. Always a refusal that [proves nothing
    /// ran](Refusal::proves_nothing_ran).
    pub reason: Refusal,
    /// `create_document` only: the key the attempt carried. Sending the same
    /// request again with it is safe.
    pub idempotency_key: Option<Uuid>,
}

/// One submission of a mutating call, as the server reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct Effect {
    pub effect: EffectKind,
    /// The unit's document. Absent only for a creation refused before the
    /// server named it.
    pub document_id: Option<Uuid>,
    pub state: EffectState,
}

/// The submission's name (`contracts/hosted-tools-v1.md` section 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    System,
    Document,
    Manifest,
    DestinationSeed,
    Destination,
    Source,
}

/// How one effect ended (`contracts/hosted-tools-v1.md` section 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    Saved,
    NotSaved,
    Unknown,
}

/// A saved prefix: the first effects landed and the rest did not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartiallySaved {
    /// Minted by the server for the call. No tool looks one up; it is for a
    /// person reading Cloud's history.
    pub operation_id: Uuid,
    /// Every effect the call planned, the saved ones included.
    pub effects: Vec<Effect>,
    pub recovery: Recovery,
    answer: Sealed<Value>,
}

impl PartiallySaved {
    /// The whole error object, with the tool's recovery sentence and fields.
    /// It can quote a document.
    pub fn answer(&self) -> &Sealed<Value> {
        &self.answer
    }
}

/// A write nobody can vouch for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unknown {
    pub cause: UnknownCause,
    /// Set when the server's answer carried one.
    pub operation_id: Option<Uuid>,
    /// What the server said about each effect; empty when it said nothing
    /// this client could read. The documents named here are the ones to read.
    pub effects: Vec<Effect>,
    pub recovery: Recovery,
    answer: Option<Sealed<Value>>,
}

impl Unknown {
    /// The server's error object, when there was one to keep.
    pub fn answer(&self) -> Option<&Sealed<Value>> {
        self.answer.as_ref()
    }
}

/// Why a write is unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnknownCause {
    /// The server answered `save_status_unknown`: it submitted a write and
    /// Koru Cloud did not acknowledge it in time.
    ServerCouldNotConfirm,
    /// The request left and no response came back.
    NoAnswer,
    /// A response came back that this client could not read as an answer to
    /// its request, or a success that lacks what a success must carry.
    AnswerNotUnderstood,
}

impl UnknownCause {
    pub const fn code(self) -> &'static str {
        match self {
            UnknownCause::ServerCouldNotConfirm => "server_could_not_confirm",
            UnknownCause::NoAnswer => "no_answer",
            UnknownCause::AnswerNotUnderstood => "answer_not_understood",
        }
    }
}

/// The only two things that may follow a write that is not plainly saved or
/// plainly not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recovery {
    /// `create_document` only. Sending the same request with the same
    /// `idempotency_key` is always safe and finishes it
    /// (`contracts/hosted-tools-v1.md` section 2, `save_status_unknown`'s
    /// message, and section 3). Pass this to
    /// [`Client::finish_create`](crate::Client::finish_create).
    FinishCreate(PendingCreate),
    /// Everything else. **Do not send the call again.** No other tool takes a
    /// key, and no tool looks an operation up
    /// (`contracts/hosted-tools-v1.md` section 2, `operation_id`). Read the
    /// documents the effects name and compare; for a claim, a read shows
    /// whether the document is claimed.
    ReadFirst,
}

/// A creation that may be sent again, exactly as it was.
///
/// Only the client makes one, and only for `create_document`, so holding one
/// is the permission to repeat. It keeps the arguments that were sent,
/// because the same key with a different request is refused
/// `idempotency_conflict` (`contracts/hosted-tools-v1.md` section 5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingCreate {
    pub idempotency_key: Uuid,
    arguments: Sealed<Value>,
}

impl PendingCreate {
    pub(crate) fn new(idempotency_key: Uuid, arguments: Value) -> Self {
        Self {
            idempotency_key,
            arguments: Sealed::new(arguments),
        }
    }

    pub(crate) fn arguments(&self) -> &Value {
        self.arguments.expose()
    }
}

// ---------------------------------------------------------- classification

/// What came back for a tool call, before it is given a meaning.
pub(crate) enum ToolAnswer {
    /// `isError` absent or false, with the result object.
    Success(Value),
    /// `isError`, with the error object.
    Failure(Value),
    /// The front door or admission refused the request before any tool ran.
    Refused(Refusal),
    /// The transport proved the request never left.
    NotSent,
    /// The request left and nothing came back.
    NoAnswer,
    /// Something came back and it was not an answer to this request.
    NotUnderstood,
}

/// What the client knows about the write it sent.
pub(crate) struct Sent {
    /// `None` for a name outside the catalog.
    pub tool: Option<WriteTool>,
    /// `create_document`: the creation as it was sent.
    pub create: Option<PendingCreate>,
}

impl Sent {
    fn key(&self) -> Option<Uuid> {
        self.create.as_ref().map(|create| create.idempotency_key)
    }

    fn recovery(&self) -> Recovery {
        match &self.create {
            Some(create) => Recovery::FinishCreate(create.clone()),
            None => Recovery::ReadFirst,
        }
    }

    fn unknown(&self, cause: UnknownCause, answer: Option<Value>) -> WriteOutcome {
        let envelope = answer.as_ref().and_then(Envelope::parse);
        WriteOutcome::Unknown(Unknown {
            cause,
            operation_id: envelope.as_ref().map(|envelope| envelope.operation_id),
            effects: envelope
                .map(|envelope| envelope.effects)
                .unwrap_or_default(),
            recovery: self.recovery(),
            answer: answer.map(Sealed::new),
        })
    }
}

/// `details` of the three envelope codes: `{ operation_id, effects }`
/// (`contracts/hosted-tools-v1.md` section 2).
struct Envelope {
    operation_id: Uuid,
    effects: Vec<Effect>,
}

impl Envelope {
    fn parse(error: &Value) -> Option<Self> {
        let details = error.get("details")?;
        Some(Self {
            operation_id: Uuid::parse_str(details.get("operation_id")?.as_str()?).ok()?,
            effects: effects_of(details)?,
        })
    }
}

/// The `effects` list of an error's `details`, when it has one that reads.
fn effects_of(details: &Value) -> Option<Vec<Effect>> {
    Vec::<Effect>::deserialize(details.get("effects")?).ok()
}

/// Give a write's answer its meaning.
///
/// The rule, in order: an answer that proves nothing ran is `not saved`; an
/// answer that says saved and carries what a success must is `saved`; the
/// server's two envelopes are what they say when they read as the contract
/// writes them; **everything else is `unknown`.**
pub(crate) fn classify_write(sent: Sent, answer: ToolAnswer) -> WriteOutcome {
    match answer {
        ToolAnswer::NotSent => not_saved(&sent, Refusal::NotSent),
        ToolAnswer::Refused(refusal) => not_saved(&sent, refusal),
        ToolAnswer::NoAnswer => sent.unknown(UnknownCause::NoAnswer, None),
        ToolAnswer::NotUnderstood => sent.unknown(UnknownCause::AnswerNotUnderstood, None),
        ToolAnswer::Success(result) => match saved(&sent, result) {
            Some(saved) => WriteOutcome::Saved(saved),
            None => sent.unknown(UnknownCause::AnswerNotUnderstood, None),
        },
        ToolAnswer::Failure(error) => failure(sent, error),
    }
}

fn not_saved(sent: &Sent, reason: Refusal) -> WriteOutcome {
    debug_assert!(reason.proves_nothing_ran());
    WriteOutcome::NotSaved(NotSaved {
        reason,
        idempotency_key: sent.key(),
    })
}

fn failure(sent: Sent, error: Value) -> WriteOutcome {
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    match code.as_str() {
        "save_status_unknown" => sent.unknown(UnknownCause::ServerCouldNotConfirm, Some(error)),
        "partially_saved" => match Envelope::parse(&error) {
            // A refusal stops the sequence, so a mixed outcome is always a
            // saved prefix (section 2). Anything else under this code is not
            // the contract's answer, and is not trusted.
            Some(envelope) if is_saved_prefix(&envelope.effects) => {
                WriteOutcome::PartiallySaved(PartiallySaved {
                    operation_id: envelope.operation_id,
                    effects: envelope.effects,
                    recovery: sent.recovery(),
                    answer: Sealed::new(error),
                })
            }
            _ => sent.unknown(UnknownCause::AnswerNotUnderstood, Some(error)),
        },
        // No code at all is not an error object this client knows.
        "" => sent.unknown(UnknownCause::AnswerNotUnderstood, Some(error)),
        // Every other code is a refusal of its own, and by section 2's table
        // that means every effect is `not_saved`. An answer that says so
        // itself must agree: `edit_not_saved` lists its effects, and one that
        // names a saved or unknown effect under a not-saved code is not
        // trusted either way.
        _ => match error.get("details").and_then(effects_of) {
            Some(effects)
                if effects
                    .iter()
                    .any(|effect| effect.state != EffectState::NotSaved) =>
            {
                sent.unknown(UnknownCause::AnswerNotUnderstood, Some(error))
            }
            _ => not_saved(&sent, refusal_of_tool_error(error)),
        },
    }
}

/// At least one saved effect, then only not-saved ones.
fn is_saved_prefix(effects: &[Effect]) -> bool {
    let saved = effects
        .iter()
        .take_while(|effect| effect.state == EffectState::Saved)
        .count();
    saved > 0
        && saved < effects.len()
        && effects[saved..]
            .iter()
            .all(|effect| effect.state == EffectState::NotSaved)
}

/// A success, if it carries what a success of this tool must carry.
fn saved(sent: &Sent, result: Value) -> Option<Saved> {
    let text = |key: &str| {
        result
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
    };
    let revision = |key: &str| text(key).map(|revision| Revision(revision.to_owned()));
    let document = |key: &str| text(key).and_then(|id| Uuid::parse_str(id).ok());

    let mut saved = Saved {
        document_id: None,
        revision: None,
        source_revision: None,
        idempotency_key: None,
        claim: None,
        answer: Sealed::new(Value::Null),
    };
    match sent.tool {
        Some(WriteTool::CreateDocument) => {
            saved.document_id = Some(document("document_id")?);
            saved.revision = Some(revision("revision")?);
            // A hosted creation answers its key in every answer (section 3).
            // One that answers a different key answered a different request.
            let sent_key = sent.key()?;
            if let Some(answered) = result.get("idempotency_key") {
                let answered = answered
                    .as_str()
                    .and_then(|key| Uuid::parse_str(key).ok())?;
                if answered != sent_key {
                    return None;
                }
            }
            saved.idempotency_key = Some(sent_key);
        }
        Some(WriteTool::ExtractToDocument) => {
            saved.document_id = Some(document("document_id")?);
            saved.revision = Some(revision("document_revision")?);
            saved.source_revision = Some(revision("source_revision")?);
        }
        Some(tool) if tool.writes_a_document() => {
            saved.revision = Some(revision("revision")?);
        }
        Some(WriteTool::ClaimDocument) => {
            saved.claim = Some(ClaimGrant {
                handle: ClaimHandle(text("claim_id")?.to_owned()),
                expires_at: result.get("expires_at")?.as_i64()?,
            });
        }
        // `create_image_upload`, `release_document`, and a name outside the
        // catalog that the server nevertheless ran: an object is all a
        // success has to be.
        _ => {
            result.as_object()?;
        }
    }
    saved.answer = Sealed::new(result);
    Some(saved)
}

#[cfg(test)]
mod tests;
