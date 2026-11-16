// SPDX-License-Identifier: MPL-2.0

//! Every closed enum a `delegate.*` row carries, mapped to its registry
//! token by exhaustive `match`.
//!
//! The registry declares each vocabulary once (`registry.rs`'s `DELEGATE_*`
//! constants); the enums here are the run vocabulary's own where it has one
//! (`koru-delegate-core`, the session protocol) and this crate's where the
//! row's question has no enum yet (which agent, which scope kind, how the
//! tools went). A variant added to any of them is a build error here until
//! it is mapped, and `tests::tokens` proves every mapped token is inside its
//! row's vocabulary — so a token outside a vocabulary cannot be built, and
//! a vocabulary the registry widened without a builder change is caught the
//! same way.
//!
//! [`ScopeOverlap`] is the run vocabulary's own (`koru-delegate-core`): the
//! scope crate answers the overlap question, a `RunFact` carries the answer
//! across a restart, and this row spells it — one word, one definition.

use koru_delegate_core::{
    ApprovalOutcome, CancelReason, Completeness, FailureStage, IncompleteReason, PriorAttempt,
    Rating, ResumeSeam, RunStatus, TurnOriginator, WorkspaceScope,
};
use koru_session_protocol::{CachedInputRelationship, ErrorCode, ProviderKind};

/// The run vocabulary's own, re-exported so a row's tokens are all named
/// from one module.
pub use koru_delegate_core::ScopeOverlap;

/// A closed value with one registry spelling.
pub trait RowToken: Copy {
    fn token(self) -> &'static str;
}

impl RowToken for koru_delegate_core::PrivateOperationKind {
    fn token(self) -> &'static str {
        match self {
            Self::ReadItem => "read_item",
        }
    }
}

impl RowToken for koru_delegate_core::PrivateEffectRefusal {
    fn token(self) -> &'static str {
        match self {
            Self::NotDispatched => "not_dispatched",
            Self::AccessDenied => "access_denied",
            Self::Revoked => "revoked",
            Self::WrongAccount => "wrong_account",
            Self::CredentialExpired => "credential_expired",
            Self::CredentialLocked => "credential_locked",
            Self::CredentialMissing => "credential_missing",
            Self::ItemUnavailable => "item_unavailable",
            Self::DestinationChanged => "destination_changed",
            Self::VendorRejected => "vendor_rejected",
            Self::Unavailable => "unavailable",
        }
    }
}

/// The final stage `delegate.private_operation.settled` reports. An open
/// stage has no variant, so it has no token.
impl RowToken for crate::private::PrivateOutcome {
    fn token(self) -> &'static str {
        match self {
            Self::Confirmed { .. } => "confirmed",
            Self::Refused { .. } => "refused",
            Self::Unknown => "unknown",
        }
    }
}

impl RowToken for TurnOriginator {
    fn token(self) -> &'static str {
        match self {
            Self::Orchestrator => "orchestrator",
            Self::Human => "human",
        }
    }
}

impl RowToken for CancelReason {
    fn token(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::TakeoverBoundary => "takeover_boundary",
        }
    }
}

impl RowToken for FailureStage {
    fn token(self) -> &'static str {
        match self {
            Self::ScopeAdmission => "scope_admission",
            Self::ContextCapture => "context_capture",
            Self::ContextMaterialization => "context_materialization",
            Self::SessionCreation => "session_creation",
            Self::ProviderStart => "provider_start",
            Self::Authority => "authority",
            Self::Turn => "turn",
        }
    }
}

impl RowToken for PriorAttempt {
    fn token(self) -> &'static str {
        match self {
            Self::Ended => "ended",
            Self::Unknown => "unknown",
        }
    }
}

impl RowToken for ResumeSeam {
    fn token(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::RecordedContext => "recorded_context",
        }
    }
}

impl RowToken for ApprovalOutcome {
    fn token(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
        }
    }
}

impl RowToken for Rating {
    fn token(self) -> &'static str {
        match self {
            Self::Good => "good",
            Self::NotGood => "not_good",
        }
    }
}

impl RowToken for CachedInputRelationship {
    fn token(self) -> &'static str {
        match self {
            Self::Included => "included",
            Self::Separate => "separate",
            Self::Undeclared => "undeclared",
        }
    }
}

/// `Completeness` and its reason, folded into one token: a usage row says in
/// one word whether a price over it would be a guess and why.
impl RowToken for Completeness {
    fn token(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Incomplete { reason } => match reason {
                IncompleteReason::CachedInputRelationshipUndeclared => {
                    "cached_relationship_undeclared"
                }
                IncompleteReason::MultipleModels => "multiple_models",
                IncompleteReason::NoUsageReported => "no_usage_reported",
            },
        }
    }
}

/// The provider category a run was delegated to (`delegate.run.created`'s
/// `agent`). The four P1.1 evaluated, plus `other` — the local-mapping rule:
/// a `ProviderKind` this crate does not recognise becomes `Other` **on this
/// machine**, so a provider token never rides an attribute.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DelegatedAgent {
    Codex,
    Claude,
    Gemini,
    Opencode,
    Other,
}

impl DelegatedAgent {
    /// Every variant, for the tests that walk the vocabulary.
    pub const ALL: &'static [Self] = &[
        Self::Codex,
        Self::Claude,
        Self::Gemini,
        Self::Opencode,
        Self::Other,
    ];

    /// The provider category's token, as the session protocol spells it
    /// (`codex`, `claude`, `gemini`, `opencode`), collapsed to `Other`
    /// otherwise.
    pub fn from_provider_kind(kind: &ProviderKind) -> Self {
        match kind.as_str() {
            "codex" => Self::Codex,
            "claude" => Self::Claude,
            "gemini" => Self::Gemini,
            "opencode" => Self::Opencode,
            _ => Self::Other,
        }
    }
}

impl RowToken for DelegatedAgent {
    fn token(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::Opencode => "opencode",
            Self::Other => "other",
        }
    }
}

/// Which kind of working folder a run has — `WorkspaceScope` with its
/// payload (the project, the folder) left behind, because the folder is
/// content and the event says only what *kind* of scope it was.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ScopeKind {
    None,
    ProjectRoot,
    ProjectFolder,
    Temporary,
    BotFolder,
}

impl ScopeKind {
    pub const ALL: &'static [Self] = &[
        Self::None,
        Self::ProjectRoot,
        Self::ProjectFolder,
        Self::Temporary,
        Self::BotFolder,
    ];

    pub fn of(scope: &WorkspaceScope) -> Self {
        match scope {
            WorkspaceScope::None => Self::None,
            WorkspaceScope::ProjectRoot { .. } => Self::ProjectRoot,
            WorkspaceScope::ProjectFolder { .. } => Self::ProjectFolder,
            WorkspaceScope::Temporary => Self::Temporary,
            WorkspaceScope::BotFolder { .. } => Self::BotFolder,
        }
    }
}

impl RowToken for ScopeKind {
    fn token(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ProjectRoot => "project_root",
            Self::ProjectFolder => "project_folder",
            Self::Temporary => "temporary",
            Self::BotFolder => "bot_folder",
        }
    }
}

/// D29, as a token: a completed run either delivered a result or finished
/// needing attention because it registered none.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum CompletionOutcome {
    Delivered,
    NeedsAttention,
}

impl CompletionOutcome {
    pub const ALL: &'static [Self] = &[Self::Delivered, Self::NeedsAttention];

    /// D29's rule: no artifact, no delivered result.
    pub fn of(artifacts_registered: u32) -> Self {
        if artifacts_registered == 0 {
            Self::NeedsAttention
        } else {
            Self::Delivered
        }
    }
}

impl RowToken for CompletionOutcome {
    fn token(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::NeedsAttention => "needs_attention",
        }
    }
}

/// How a turn's coalesced tool calls went.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ToolOutcome {
    Succeeded,
    Partial,
    Failed,
}

impl ToolOutcome {
    pub const ALL: &'static [Self] = &[Self::Succeeded, Self::Partial, Self::Failed];

    /// Derived from the two counts, so a surface never has to. Zero calls
    /// is `Succeeded`: nothing failed.
    pub fn of(calls: u32, failed: u32) -> Self {
        if failed == 0 {
            Self::Succeeded
        } else if failed >= calls {
            Self::Failed
        } else {
            Self::Partial
        }
    }
}

impl RowToken for ToolOutcome {
    fn token(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Partial => "partial",
            Self::Failed => "failed",
        }
    }
}

/// Why an attachment was refused: the session protocol's two attachment
/// error classes, or `Other`. Never the code's text.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AttachmentRejection {
    NotFound,
    Limit,
    Other,
}

impl AttachmentRejection {
    pub const ALL: &'static [Self] = &[Self::NotFound, Self::Limit, Self::Other];

    pub fn of(code: ErrorCode) -> Self {
        match code {
            ErrorCode::AttachmentNotFound => Self::NotFound,
            ErrorCode::AttachmentLimit => Self::Limit,
            _ => Self::Other,
        }
    }
}

impl RowToken for AttachmentRejection {
    fn token(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Limit => "limit",
            Self::Other => "other",
        }
    }
}

/// The terminal status a run's usage fold was published under.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RunOutcome {
    Completed,
    Failed,
    Cancelled,
}

impl RunOutcome {
    pub const ALL: &'static [Self] = &[Self::Completed, Self::Failed, Self::Cancelled];

    /// `Some` for a terminal status, `None` for a run still in flight —
    /// the fold is published when the run ends (§17.1), so a caller with a
    /// non-terminal status has nothing to publish yet. Exhaustive, so a
    /// status added later is decided here rather than defaulted.
    pub fn of(status: &RunStatus) -> Option<Self> {
        match status {
            RunStatus::Completed => Some(Self::Completed),
            RunStatus::Failed { .. } => Some(Self::Failed),
            RunStatus::Cancelled => Some(Self::Cancelled),
            RunStatus::Created
            | RunStatus::Starting
            | RunStatus::Running { .. }
            | RunStatus::Interrupted { .. }
            | RunStatus::AwaitingReview
            | RunStatus::WaitingForParticipant => None,
        }
    }
}

impl RowToken for RunOutcome {
    fn token(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl RowToken for ScopeOverlap {
    fn token(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Inside => "inside",
            Self::Around => "around",
        }
    }
}
