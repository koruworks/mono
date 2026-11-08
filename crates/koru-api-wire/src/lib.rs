// SPDX-License-Identifier: MPL-2.0

//! Gateway-owned public wire types. Store bodies remain in `koru-store-wire`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use koru_ai_core::AiSearchHit;
use koru_store_wire::search::{RecentQuery, SearchFacets, SearchHit, SearchQuery};

pub const API_VERSION: u32 = 1;
pub const ASYNC_API_VERSION: &str = "1.0.0";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DomainReadiness {
    Starting,
    Ready,
    Disabled,
    Degraded,
    Unavailable,
    Incompatible,
}

/// `GET /v1/status`. Its first four fields preserve the Store status shape so
/// older readers ignore the additive gateway readiness fields safely.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GatewayStatus {
    pub api_version: u32,
    pub capabilities: Vec<String>,
    pub channel: String,
    pub started_at: i64,
    pub gateway: DomainReadiness,
    pub store: DomainReadiness,
    pub ai: DomainReadiness,
}

// ---------------------------------------------------- the workspace lifecycle

/// What `POST /v1/workspaces/{id}/lifecycle` is being asked to do.
///
/// **Three verbs, and no permanent delete.** There is no purge route anywhere
/// in Koru — the retention sweep is the only thing that removes bytes — so
/// naming one here would be describing an operation that does not exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceLifecycleAction {
    /// Hide the workspace and its projects. Reversible; its terminals close.
    Archive,
    /// Move the workspace to the trash. Recoverable until the retention window
    /// purges it; its terminals close.
    SoftDelete,
    /// Bring a soft-deleted workspace back. Closes nothing — its projects
    /// reappear with no project mutation.
    Restore,
}

impl WorkspaceLifecycleAction {
    pub fn token(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::SoftDelete => "soft_delete",
            Self::Restore => "restore",
        }
    }

    /// Whether this action closes the workspace's terminals first.
    pub fn closes_sessions(self) -> bool {
        matches!(self, Self::Archive | Self::SoftDelete)
    }
}

/// What `POST /v1/workspaces/{id}/lifecycle/confirmation` is being asked
/// about.
///
/// The dialog asks before it draws anything, because what it draws is the
/// answer: which terminals this action would close.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfirmationRequest {
    pub action: WorkspaceLifecycleAction,
}

/// One terminal a confirmation names, in the words the dialog shows.
///
/// **Enough to recognise a terminal and nothing more.** A person deciding
/// whether to archive a workspace needs to know which windows are about to
/// close, which is a name and where it is working; the process state, the
/// controller, the attention counters and the capability set are the session
/// surface's business and would only make this list harder to read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceSessionConsequence {
    pub id: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
}

/// **A consequence the gateway computed, and the token that proves it was
/// shown.**
///
/// The caller presents `sessions` (S13), and if the person accepts it sends
/// `confirmation` straight back in a [`WorkspaceLifecycleRequest`]. The token
/// is bound to this workspace, this action and this list, and it is spent by
/// being used — so an acceptance is provably about the consequence that was on
/// screen rather than one the caller says it displayed. ADR-267 recorded this
/// as the recommendation; SP-8b's dialog is the round trip it was waiting for.
///
/// **It is still not a credential.** `workspace.destroy` is what authorizes the
/// operation; this only says which question was answered. A token nobody can
/// guess is not the point — a caller holding the scope could always mint its
/// own before — the point is that the gateway and the dialog agree about what
/// was accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceConfirmation {
    pub confirmation: String,
    pub workspace: String,
    pub action: WorkspaceLifecycleAction,
    /// The terminals this action would close, in the order the operation
    /// would close them. Empty for `restore`, which closes nothing.
    pub sessions: Vec<WorkspaceSessionConsequence>,
    /// When this token stops being accepted, in epoch milliseconds. A dialog
    /// left open past it asks again rather than sending something stale.
    pub expires_at: i64,
}

/// The one destructive workspace operation's body.
///
/// **The consequence is presented by the caller and accepted before this is
/// sent** (rev 4 §4.2 step 2, S13). What crosses is the acceptance: the token
/// the gateway issued alongside the consequence it computed, round-tripped
/// unchanged — see [`WorkspaceConfirmation`].
///
/// It is still not a credential: the scope is what authorizes the operation.
/// What the token adds is that the acceptance is *about* something. A token
/// the gateway does not recognise, one issued for another workspace or another
/// action, one already spent, or one that has expired is refused before
/// anything is closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceLifecycleRequest {
    pub action: WorkspaceLifecycleAction,
    pub confirmation: String,
    /// Localized default for a required replacement. Older callers use the
    /// protocol's English fallback; this is not a second creation operation.
    #[serde(default = "default_workspace_name")]
    pub replacement_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removal_attempt: Option<koru_store_wire::RemovalAttemptId>,
}

/// English fallback for callers predating localized replacement names.
pub fn default_workspace_name() -> String {
    "My Workspace".into()
}

/// What the operation actually did, in the order it did it.
///
/// **Every field is a fact about this call, and the shape is the same whether
/// it succeeded.** S24 is a partial failure — the terminals are closed and the
/// catalog is unchanged — and a caller that has to report that truth needs the
/// same body it would have read on success, not a bare error string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceLifecycleOutcome {
    pub workspace: String,
    pub action: WorkspaceLifecycleAction,
    /// The sessions this call closed. Empty on a retry, and empty for
    /// `restore`, which closes nothing.
    pub terminated_sessions: Vec<String>,
    /// Whether step 3 completed. `false` means nothing after it was attempted.
    pub sessions_closed: bool,
    /// Whether step 4 committed. `false` with `sessions_closed: true` is
    /// exactly S24, and the retry is the same request again.
    pub mutation_applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_committed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing: Option<koru_store_wire::ListingState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<koru_store_wire::Replacement>,
}

// ---------------------------------------------------- the workspace migration

/// What `POST /v1/workspaces/{id}/migration/confirmation` is being asked about.
///
/// **The destination, and not the origin.** The gateway reads the origin from
/// the catalog; a caller that named it could name a stale one, and a token
/// bound to the wrong pair would refuse a move that is perfectly fine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceMigrationConfirmationRequest {
    /// The profile the workspace would move to.
    pub to: String,
}

/// **A move's consequence, and the token that proves it was shown.**
///
/// The same contract as [`WorkspaceConfirmation`], for a different question.
/// The token is bound to this workspace *and this destination*: a person shown
/// "these four terminals will close, and Notes goes to Home" has not accepted
/// anything about a move to some other profile, and a dialog that could
/// round-trip one token into a different destination would make the acceptance
/// meaningless.
///
/// `from` is here because the dialog names both ends of the move, and reading
/// it off the catalog separately would be a second chance to be out of date in
/// a different way than the operation is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceMigration {
    pub confirmation: String,
    pub workspace: String,
    /// The profile that owns it now.
    pub from: String,
    /// The profile it would move to.
    pub to: String,
    /// The terminals the move would close, in the order it would close them.
    /// Empty when the workspace is already owned by `to` — a move that has
    /// nothing to do closes nothing.
    pub sessions: Vec<WorkspaceSessionConsequence>,
    pub expires_at: i64,
}

/// The migration operation's body.
///
/// `to` travels again rather than being read out of the token: the token is
/// opaque to the caller, and an operation that took the destination *only* from
/// its own ledger would give a caller no way to say what it thought it was
/// doing — so a mismatch would be undetectable instead of a 400.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceMigrationRequest {
    pub to: String,
    pub confirmation: String,
    #[serde(default = "default_workspace_name")]
    pub replacement_name: String,
}

/// What the migration actually did, in the order it did it.
///
/// The same shape whether it succeeded, for [`WorkspaceLifecycleOutcome`]'s
/// reason: a caller reporting a partial failure needs the body it would have
/// read on success.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceMigrationOutcome {
    pub workspace: String,
    /// The owner the operation found. On a retry after the move already
    /// committed this is already the destination.
    pub from: String,
    pub to: String,
    /// The sessions this call closed. Empty on a retry, and empty when the
    /// workspace was already where it was being sent.
    pub terminated_sessions: Vec<String>,
    /// Whether the terminals closed. `false` means nothing after it ran.
    pub sessions_closed: bool,
    /// Whether the Store reassignment committed. `false` with
    /// `sessions_closed: true` is the partial failure, and the retry is the
    /// same request with a fresh confirmation.
    pub mutation_applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement: Option<koru_store_wire::Replacement>,
    /// **The projects whose typed scope now names the new owner**, or `None`
    /// when the session host could not be asked.
    ///
    /// Three-valued on purpose, and the third value is the one that matters.
    /// `Some([])` is "the host was asked and had nothing to repair" — the
    /// ordinary answer for a workspace with no projects, and for a retry.
    /// `None` is "nobody has repaired them yet": the move committed, and until
    /// a repair lands those projects name a profile that no longer owns the
    /// workspace, so the terminal draws them in its missing-workspace area
    /// rather than lying about where they live. Sending the same request again
    /// is what repairs them.
    ///
    /// A repair that could not run is deliberately *not* a failed migration.
    /// The workspace has moved, the fact is published, and every other reader
    /// is right; treating an unreachable session host as a failure would
    /// invite a caller to retry the whole operation as though nothing had
    /// happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projects_rescoped: Option<Vec<String>>,
}

/// The public `POST /v1/search` body. Existing Store-search fields stay at the
/// top level; federation adds only an opt-out and an opaque continuation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FederatedSearchRequest {
    #[serde(flatten)]
    pub query: SearchQuery,
    #[serde(default)]
    pub include_facets: bool,
    #[serde(default)]
    pub recent_queries: Option<usize>,
    #[serde(default = "default_true")]
    pub include_ai: bool,
    #[serde(default)]
    pub cursor: Option<String>,
    /// **Only this profile's chats** among the AI results (AI schema 13):
    /// a window searches its own profile's. Absent searches every
    /// profile's, each hit naming its own. The Store results are scoped by
    /// `workspaces`, as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<koru_ai_core::ProfileId>,
}

impl Default for FederatedSearchRequest {
    fn default() -> Self {
        Self {
            query: SearchQuery::default(),
            include_facets: false,
            recent_queries: None,
            include_ai: true,
            cursor: None,
            profile: None,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum FederatedSearchHit {
    Store { hit: SearchHit },
    Ai { hit: AiSearchHit },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchSource {
    Store,
    Ai,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchSourceState {
    Complete,
    Disabled,
    Failed,
    TimedOut,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchSourceStatus {
    pub source: SearchSource,
    pub state: SearchSourceState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FederatedSearchPage {
    pub hits: Vec<FederatedSearchHit>,
    pub next_cursor: Option<String>,
    pub updating_workspaces: Vec<String>,
    pub request_seq: u64,
    pub corrected_query: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FederatedSearchResponse {
    pub page: FederatedSearchPage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facets: Option<SearchFacets>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_queries: Option<Vec<RecentQuery>>,
    pub sources: Vec<SearchSourceStatus>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamMessage {
    pub name: &'static str,
    pub direction: &'static str,
    pub description: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamChannel {
    pub path: &'static str,
    pub protocol: &'static str,
    pub version: u32,
    pub ordered: bool,
    pub backpressured: bool,
    pub messages: &'static [StreamMessage],
}

pub const LORO_MESSAGES: &[StreamMessage] = &[
    StreamMessage {
        name: "join",
        direction: "bidirectional",
        description: "Join an authorized Loro room and exchange version state",
    },
    StreamMessage {
        name: "update",
        direction: "bidirectional",
        description: "Bounded standard Loro Protocol update or fragment",
    },
    StreamMessage {
        name: "ack",
        direction: "server_to_client",
        description: "Durable Store acknowledgement",
    },
    StreamMessage {
        name: "leave",
        direction: "bidirectional",
        description: "Half-close one logical room",
    },
    StreamMessage {
        name: "error",
        direction: "server_to_client",
        description: "Terminal room or connection error",
    },
];

pub const AI_MESSAGES: &[StreamMessage] = &[
    StreamMessage {
        name: "accepted",
        direction: "server_to_client",
        description: "Durable turn acceptance and retry-safe attempt identities",
    },
    StreamMessage {
        name: "event",
        direction: "server_to_client",
        description: "Ordered normalized Koru AI stream event",
    },
];

pub const MODEL_PULL_MESSAGES: &[StreamMessage] = &[
    StreamMessage {
        name: "accepted",
        direction: "server_to_client",
        description: "Accepted Ollama model pull",
    },
    StreamMessage {
        name: "event",
        direction: "server_to_client",
        description: "Normalized progress followed by one completed, failed, or cancelled event",
    },
];

pub const STREAM_CHANNELS: &[StreamChannel] = &[
    StreamChannel {
        path: "/v1/sync",
        protocol: "loro-protocol",
        version: 1,
        ordered: true,
        backpressured: true,
        messages: LORO_MESSAGES,
    },
    StreamChannel {
        path: "/v1/ai/chats/{chat}/turns",
        protocol: "sse",
        version: koru_ai_wire::AI_STREAM_VERSION,
        ordered: true,
        backpressured: true,
        messages: AI_MESSAGES,
    },
    StreamChannel {
        path: "/v1/ai/providers/ollama/pulls",
        protocol: "sse",
        version: koru_ai_wire::MODEL_PULL_STREAM_VERSION,
        ordered: true,
        backpressured: true,
        messages: MODEL_PULL_MESSAGES,
    },
];
