// SPDX-License-Identifier: MPL-2.0

//! **The hosted root catalog's rules**, shared by the private Koru Cloud
//! service and the clients that talk to it — root-catalog
//! `docs/features/root-catalog/TECHNICAL.final.md` §4.1 (one account catalog
//! binding), §4.3 (the derived-export rule, applied on hosted admission and
//! forwarding too), §4.4 (independent Profile-unit eligibility) and §7.3
//! (account attach, admission of single subject units, the replayable
//! discovery projection with per-unit watermarks, and the decoupling of
//! ownership/membership from placement). The contract the service
//! implements against is `docs/cloud-platform/contracts/root-catalog-cloud-v2.md`.
//!
//! Everything here is pure and memory-only: the service owns storage,
//! transactions and clocks and imports this crate for the policy, so a row
//! it writes was decided by the same function a client test runs. Nothing
//! here opens a database, verifies a credential or issues a permit.
//!
//! - [`binding`]: get-or-create of the account's one catalog binding —
//!   idempotent, refusing a device already bound to another account, and
//!   indifferent to which verified identity of the account is asking.
//! - [`admission`]: one admitted subject unit at a time, validated by the
//!   Store's own validators, checked against the session's scope and the
//!   account's Profiles, and passed through `root_catalog::export::derive`
//!   with the Cloud-eligible Profiles as the visible set — redacted or held,
//!   never stored raw.
//! - [`projection`]: the discovery projection as a reducer over admitted
//!   units with a per-unit watermark that never regresses, and the coverage
//!   a receipt reports.
//! - [`placement`]: content admission on Workspace id, membership,
//!   incarnation and the client's policy, with no former-Profile
//!   prerequisite; Identity/Private eligibility from current placement; a
//!   move that cannot touch replica, incarnation or membership.
//! - [`ports`]: the storage seams the service implements.

pub mod admission;
pub mod binding;
pub mod placement;
pub mod ports;
pub mod projection;

pub use admission::*;
pub use binding::*;
pub use placement::*;
pub use ports::*;
pub use projection::*;

use koru_store_core::root_catalog::{CatalogUnitRef, ExportSubject};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ProfileId, WorkspaceId};

/// The peer id the hosted side opens a unit under. Cloud never authors an
/// operation into a catalog or personal unit — it admits, stores, projects
/// and forwards — so the writer stream a `LoroDoc` wants is irrelevant and
/// is pinned to one value rather than minted, so that a derived snapshot
/// never depends on a random number.
pub const HOSTED_READER_PEER: u64 = 0x4b6f_7275_436c_6f75;

/// Which unit a hosted row, frame, receipt, watermark or hold is about: the
/// three unit families the account path exchanges (§4.2), addressed by kind
/// and subject under the session's catalog scope. The scope itself is never
/// in the address: it is the row key the session resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CloudUnitRef {
    ProfileCatalog { subject_id: ProfileId },
    WorkspaceCatalog { subject_id: WorkspaceId },
    PersonalV2 { subject_id: WorkspaceId },
}

/// The `kind` column of a hosted unit row and the discriminator on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CloudUnitKind {
    ProfileCatalog,
    WorkspaceCatalog,
    PersonalV2,
}

impl CloudUnitKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProfileCatalog => "profile_catalog",
            Self::WorkspaceCatalog => "workspace_catalog",
            Self::PersonalV2 => "personal_v2",
        }
    }
}

impl CloudUnitRef {
    pub fn catalog(unit: CatalogUnitRef) -> Self {
        match unit {
            CatalogUnitRef::Profile { profile } => Self::ProfileCatalog {
                subject_id: profile,
            },
            CatalogUnitRef::Workspace { workspace } => Self::WorkspaceCatalog {
                subject_id: workspace,
            },
        }
    }

    pub fn personal(workspace: WorkspaceId) -> Self {
        Self::PersonalV2 {
            subject_id: workspace,
        }
    }

    /// The Store's address for a catalog unit; `None` for a personal unit,
    /// whose address is the Workspace alone (§5.1).
    pub fn as_catalog(self) -> Option<CatalogUnitRef> {
        match self {
            Self::ProfileCatalog { subject_id } => Some(CatalogUnitRef::Profile {
                profile: subject_id,
            }),
            Self::WorkspaceCatalog { subject_id } => Some(CatalogUnitRef::Workspace {
                workspace: subject_id,
            }),
            Self::PersonalV2 { .. } => None,
        }
    }

    pub fn kind(self) -> CloudUnitKind {
        match self {
            Self::ProfileCatalog { .. } => CloudUnitKind::ProfileCatalog,
            Self::WorkspaceCatalog { .. } => CloudUnitKind::WorkspaceCatalog,
            Self::PersonalV2 { .. } => CloudUnitKind::PersonalV2,
        }
    }

    /// The subject UUID as the row's `subject_id` column holds it.
    pub fn subject_uuid(self) -> Uuid {
        match self {
            Self::ProfileCatalog { subject_id } => subject_id.as_uuid(),
            Self::WorkspaceCatalog { subject_id } | Self::PersonalV2 { subject_id } => {
                subject_id.as_uuid()
            }
        }
    }

    /// The export boundary's name for this unit.
    pub fn export_subject(self) -> ExportSubject {
        match self {
            Self::ProfileCatalog { subject_id } => ExportSubject::ProfileCatalog(subject_id),
            Self::WorkspaceCatalog { subject_id } => ExportSubject::WorkspaceCatalog(subject_id),
            Self::PersonalV2 { subject_id } => ExportSubject::PersonalV2(subject_id),
        }
    }

    /// Whether the subject id is spelled canonically — lowercase, hyphenated,
    /// a v4 UUID — as every id in a catalog unit must be (§4.2). A row is
    /// never filed under another spelling of the same subject.
    pub fn is_canonical(self) -> bool {
        let id = self.subject_uuid();
        id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122
    }
}

#[cfg(test)]
mod tests;
