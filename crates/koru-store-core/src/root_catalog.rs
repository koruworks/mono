// SPDX-License-Identifier: MPL-2.0

//! The per-user root catalog's units: one Loro document per Profile and one
//! per Workspace, addressed by [`CatalogUnitRef`] under a
//! [`UserScopeId`](crate::ids::UserScopeId) that lives in SQL and never in
//! the document. This is `docs/features/root-catalog/TECHNICAL.final.md`
//! §4.2 made concrete; the keys, types and rules here are the signed-off
//! contract, not a design.
//!
//! ```text
//! Profile catalog unit            Workspace catalog unit
//!   meta                            meta
//!     schema_version  i64 = 1         schema_version  i64 = 1
//!     unit_kind       "profile_catalog"   unit_kind   "workspace_catalog"
//!     profile_id      UUID            workspace_id    UUID
//!   deletions[DeleteId]             placement
//!     {at_ms, retention_ms,           current  {profile_id: UUID|null, move_id: UUID}
//!      batch_id|null}               deletions[DeleteId]   as the Profile unit
//!   restores[RestoreId]             restores[RestoreId]   as the Profile unit
//!     {at_ms, observed_deletions:   purged[DeleteId]      as the Profile unit
//!      [UUID], batch_id|null}
//!   purged[DeleteId]
//!     {at_ms}
//! ```
//!
//! Three rules shape everything in this module:
//!
//! - **Lifecycle is facts, not flags.** `deletions`, `restores` and `purged`
//!   are append-only maps of immutable atomic records. A subject is deleted
//!   while a deletion id is not named by any restore's `observed_deletions`;
//!   an unseen concurrent delete therefore stays active, and an empty
//!   observation list clears nothing. There is no LWW boolean to lose a race.
//! - **Placement is one atomic register.** `placement.current` is written as
//!   a whole `{profile_id, move_id}` value, so concurrent moves resolve by
//!   Loro's own map ordering — Lamport, then peer id — with no application
//!   clock, second election or receiver prompt. A null `profile_id` is
//!   *explicitly unassigned*; an absent register is *pending* and acceptable
//!   only on a unit that has not been committed yet.
//! - **A candidate is validated before it is published.** Unknown roots or
//!   keys, non-canonical UUIDs, negative durations, a wrong `unit_kind`, a
//!   header that does not match the address and a live Workspace unit
//!   without a placement are all refused, with a typed
//!   [`CatalogUnitRefusal`], before any byte reaches a row or a peer.
//!
//! The export boundary of §4.3 — native history redaction of hidden Profile
//! references before any unit leaves the device — is [`export`]. The SQLite
//! rows (§4.2 "Approved SQLite shapes") are the
//! [`Catalog`](crate::ports::Catalog) port's business.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::errors::{Code, Error};
use crate::ids::{ProfileId, RestoreId, WorkspaceId};
use crate::unit_doc::UnitFailure;

pub mod export;
mod lifecycle;
pub mod seed;
mod unit;
mod validation;

pub use crate::unit_doc::Staged;
pub use export::{
    Derived, DerivedKey, ExportRefusal, ExportScope, ExportSubject, HoldReason, PayloadKind,
    PolicyCapture, Publication, derive,
};
pub use lifecycle::{DeletionRecord, Placement, PurgeRecord, RestoreRecord};
pub use seed::{ROOT_SEED_NAMESPACE, RootSeedReceipt, seed_profile_unit, seed_workspace_unit};
pub use unit::{ProfileCatalogUnit, WorkspaceCatalogUnit};

/// The `store_state` key under which the device-local
/// [`UserScopeId`](crate::ids::UserScopeId) is kept before sign-in. Written
/// once by the catalog migration that creates the root-catalog tables;
/// read by [`Catalog::local_user_scope`](crate::ports::Catalog::local_user_scope).
/// A `store_state` row because, like the peer id beside it, it must never
/// replicate.
pub const K_LOCAL_USER_SCOPE: &str = "local_user_scope";

pub(crate) const R_META: &str = "meta";
pub(crate) const R_PLACEMENT: &str = "placement";
pub(crate) const R_DELETIONS: &str = "deletions";
pub(crate) const R_RESTORES: &str = "restores";
pub(crate) const R_PURGED: &str = "purged";
pub(crate) const K_SCHEMA_VERSION: &str = "schema_version";
pub(crate) const K_UNIT_KIND: &str = "unit_kind";
pub(crate) const K_PROFILE_ID: &str = "profile_id";
pub(crate) const K_WORKSPACE_ID: &str = "workspace_id";
pub(crate) const K_CURRENT: &str = "current";
pub(crate) const F_PROFILE_ID: &str = "profile_id";
pub(crate) const F_MOVE_ID: &str = "move_id";
pub(crate) const F_AT_MS: &str = "at_ms";
pub(crate) const F_RETENTION_MS: &str = "retention_ms";
pub(crate) const F_BATCH_ID: &str = "batch_id";
pub(crate) const F_OBSERVED_DELETIONS: &str = "observed_deletions";

/// Which of the two unit families a document belongs to. Its storage
/// spelling is `meta.unit_kind` and the `unit_kind` column of every root
/// catalog row.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CatalogUnitKind {
    Profile,
    Workspace,
}

impl CatalogUnitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Profile => "profile_catalog",
            Self::Workspace => "workspace_catalog",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "profile_catalog" => Some(Self::Profile),
            "workspace_catalog" => Some(Self::Workspace),
            _ => None,
        }
    }
}

/// The address of one root-catalog unit. Separate from
/// [`ProfileUnitRef`](crate::profile_units::ProfileUnitRef), whose
/// `profile()` assumes every unit has a Profile: a Workspace catalog unit has
/// none, and its placement may name no Profile at all.
///
/// Rows are keyed `(user_scope_id, unit_kind, subject_id)`; [`Self::kind`]
/// and [`Self::subject_id`] are those two columns and [`Self::parse`] is the
/// way back.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum CatalogUnitRef {
    #[serde(rename = "profile_catalog")]
    Profile { profile: ProfileId },
    #[serde(rename = "workspace_catalog")]
    Workspace { workspace: WorkspaceId },
}

impl CatalogUnitRef {
    pub fn kind(self) -> CatalogUnitKind {
        match self {
            Self::Profile { .. } => CatalogUnitKind::Profile,
            Self::Workspace { .. } => CatalogUnitKind::Workspace,
        }
    }

    /// The canonical lowercase UUID of the subject: the row's `subject_id`.
    pub fn subject_id(self) -> String {
        match self {
            Self::Profile { profile } => profile.to_storage(),
            Self::Workspace { workspace } => workspace.to_storage(),
        }
    }

    /// Rebuild an address from its two row columns. `None` for an unknown
    /// kind or a subject that is not a canonical UUID; a row this build does
    /// not understand is never filed under a guessed address.
    pub fn parse(kind: &str, subject_id: &str) -> Option<Self> {
        match CatalogUnitKind::parse(kind)? {
            CatalogUnitKind::Profile => {
                canonical_id(subject_id, ProfileId::parse, |id| id.to_storage())
                    .map(|profile| Self::Profile { profile })
            }
            CatalogUnitKind::Workspace => {
                canonical_id(subject_id, WorkspaceId::parse, |id| id.to_storage())
                    .map(|workspace| Self::Workspace { workspace })
            }
        }
    }

    pub fn profile(self) -> Option<ProfileId> {
        match self {
            Self::Profile { profile } => Some(profile),
            Self::Workspace { .. } => None,
        }
    }

    pub fn workspace(self) -> Option<WorkspaceId> {
        match self {
            Self::Workspace { workspace } => Some(workspace),
            Self::Profile { .. } => None,
        }
    }
}

/// Parse an id and insist on the canonical spelling: lowercase, hyphenated,
/// exactly what `to_storage` would write. Every UUID in a catalog unit is
/// held to this, so one subject can never appear under two spellings.
pub(crate) fn canonical_id<T: Copy>(
    value: &str,
    parse: impl FnOnce(&str) -> Option<T>,
    storage: impl FnOnce(T) -> String,
) -> Option<T> {
    parse(value).filter(|id| storage(*id) == value)
}

/// A bare operation UUID (`move_id`, `batch_id`) in its canonical spelling.
pub(crate) fn canonical_uuid(value: &str) -> Option<Uuid> {
    canonical_id(value, |s| Uuid::parse_str(s).ok(), |u| u.to_string())
}

/// Why a catalog unit, a frame or a write was refused. Typed so a caller
/// branches on the reason and a test asserts it; the `Display` text is the
/// developer detail of the [`Error`] it converts into.
///
/// Paths name the root and key that failed (`deletions.<id>`,
/// `placement.current.move_id`); ids are allowed in detail text, content is
/// not, and a catalog unit carries none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogUnitRefusal {
    /// Durable bytes did not decode as a Loro snapshot.
    UndecodableSnapshot,
    /// Durable bytes decoded but their history has gaps.
    SnapshotMissingAncestors,
    /// An incoming frame did not decode.
    UndecodableFrame,
    /// An incoming frame needs ancestors the unit does not have. Staged
    /// imports report this as [`Staged::Pending`] instead; it survives here
    /// only as the mechanism's vocabulary.
    MissingAncestors,
    /// `meta.schema_version` is absent or not a positive integer.
    SchemaAbsent,
    /// The held unit was written by a newer build and is open read-only.
    ReadOnly { found: i64, understood: i64 },
    /// An incoming frame would move a writable unit to a newer format.
    IncomingTooNew { found: i64, understood: i64 },
    /// A top-level container this format does not define.
    UnknownRoot { root: String },
    /// A top-level container that is not a map.
    RootNotMap { root: String },
    /// A key this format does not define under a known root.
    UnknownKey { root: &'static str, key: String },
    /// `meta.unit_kind` is absent or names the other family.
    WrongUnitKind { found: Option<String> },
    /// `meta.profile_id` / `meta.workspace_id` is absent, non-canonical or
    /// names another subject than the address.
    HeaderMismatch { key: &'static str },
    /// A UUID (map key, id field or list element) that is not canonical.
    MalformedUuid { path: String },
    /// A time or duration below zero.
    NegativeMilliseconds { path: String },
    /// A record that must be one atomic value is a child container.
    NotAtomic { path: String },
    /// A record with a missing or extra field, or a field of the wrong type.
    RecordShape { path: String },
    /// A restore that observed nothing and belongs to no batch action; it
    /// could clear nothing and names no operation, so it is noise at best.
    RestoreWithoutObservations { restore: RestoreId },
    /// A live Workspace unit about to be committed with no placement.
    PlacementRequired,
    /// A lifecycle fact already exists under this id; facts are immutable.
    DuplicateLifecycleId { id: Uuid },
    /// A Loro operation failed. Not a validation outcome.
    Crdt,
}

impl fmt::Display for CatalogUnitRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UndecodableSnapshot => write!(f, "invalid catalog unit snapshot"),
            Self::SnapshotMissingAncestors => {
                write!(f, "catalog unit snapshot has missing causal ancestors")
            }
            Self::UndecodableFrame => write!(f, "catalog unit frame rejected"),
            Self::MissingAncestors => write!(f, "catalog unit frame has missing causal ancestors"),
            Self::SchemaAbsent => write!(f, "catalog unit format is absent or invalid"),
            Self::ReadOnly { found, understood } => write!(
                f,
                "catalog unit format {found} is newer than this build's {understood}"
            ),
            Self::IncomingTooNew { found, understood } => write!(
                f,
                "incoming catalog unit format {found} is newer than this build's {understood}"
            ),
            Self::UnknownRoot { root } => write!(f, "unknown catalog unit root {root}"),
            Self::RootNotMap { root } => write!(f, "catalog unit root {root} is not a map"),
            Self::UnknownKey { root, key } => write!(f, "unknown catalog unit key {root}.{key}"),
            Self::WrongUnitKind { found: Some(kind) } => {
                write!(f, "catalog unit kind {kind} differs from its address")
            }
            Self::WrongUnitKind { found: None } => write!(f, "catalog unit kind is absent"),
            Self::HeaderMismatch { key } => {
                write!(f, "catalog unit meta.{key} differs from its address")
            }
            Self::MalformedUuid { path } => write!(f, "{path} is not a canonical UUID"),
            Self::NegativeMilliseconds { path } => write!(f, "{path} is negative"),
            Self::NotAtomic { path } => write!(f, "{path} is not an atomic value"),
            Self::RecordShape { path } => {
                write!(f, "{path} has a missing, extra or mistyped field")
            }
            Self::RestoreWithoutObservations { restore } => write!(
                f,
                "restore {restore} observed no deletions and belongs to no batch"
            ),
            Self::PlacementRequired => {
                write!(
                    f,
                    "a live workspace catalog unit needs a placement before commit"
                )
            }
            Self::DuplicateLifecycleId { id } => {
                write!(f, "lifecycle fact {id} already exists and is immutable")
            }
            Self::Crdt => write!(f, "catalog unit CRDT operation failed"),
        }
    }
}

impl From<UnitFailure> for CatalogUnitRefusal {
    fn from(failure: UnitFailure) -> Self {
        match failure {
            UnitFailure::UndecodableSnapshot => Self::UndecodableSnapshot,
            UnitFailure::SnapshotMissingAncestors => Self::SnapshotMissingAncestors,
            UnitFailure::UndecodableFrame => Self::UndecodableFrame,
            UnitFailure::FrameMissingAncestors => Self::MissingAncestors,
            UnitFailure::ReadOnly { found, understood } => Self::ReadOnly { found, understood },
            UnitFailure::IncomingTooNew { found, understood } => {
                Self::IncomingTooNew { found, understood }
            }
            UnitFailure::Crdt => Self::Crdt,
        }
    }
}

impl From<CatalogUnitRefusal> for Error {
    fn from(refusal: CatalogUnitRefusal) -> Self {
        use CatalogUnitRefusal as R;
        let code = match &refusal {
            R::UndecodableFrame | R::MissingAncestors => Code::CrdtRejected,
            R::ReadOnly { .. } | R::IncomingTooNew { .. } => Code::SchemaTooNew,
            R::UnknownRoot { .. }
            | R::UnknownKey { .. }
            | R::WrongUnitKind { .. }
            | R::HeaderMismatch { .. } => Code::ProfileUnitScopeMismatch,
            R::DuplicateLifecycleId { .. } => Code::IdempotencyConflict,
            R::Crdt => Code::StorageFailed,
            R::UndecodableSnapshot
            | R::SnapshotMissingAncestors
            | R::SchemaAbsent
            | R::RootNotMap { .. }
            | R::MalformedUuid { .. }
            | R::NegativeMilliseconds { .. }
            | R::NotAtomic { .. }
            | R::RecordShape { .. }
            | R::RestoreWithoutObservations { .. }
            | R::PlacementRequired => Code::CorruptState,
        };
        Error::new(code, refusal.to_string())
    }
}

#[cfg(test)]
mod tests;
