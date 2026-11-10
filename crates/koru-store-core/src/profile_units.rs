// SPDX-License-Identifier: MPL-2.0

//! Independently addressed Profile documents. Constructors author fresh lineages;
//! no unit is a filtered export of the legacy Profile document.
//!
//! The staging mechanism every unit shares lives in [`crate::unit_doc`]; this
//! module owns the Profile-oriented addresses, their headers and their
//! validation. The personal unit has two addresses: the v1
//! `(owner Profile, Workspace)` pair this contract started with, and the v2
//! Workspace-only address of `docs/features/root-catalog/TECHNICAL.final.md`
//! §5.1, whose owning user scope is a validated local binding that is never
//! written into the CRDT.

use loro::{LoroDoc, LoroValue, ValueOrContainer};
use serde::{Deserialize, Serialize};

use crate::errors::{Code, Error, Result};
use crate::ids::{ProfileId, WorkspaceId};
use crate::unit_doc::{Staged, UnitAddress, UnitDoc, UnitFailure, entries, integer, string};
use crate::version::{
    Compatibility, PERSONAL_CRDT_SCHEMA_VERSION, PERSONAL_V2_CRDT_SCHEMA_VERSION,
    PROFILE_IDENTITY_CRDT_SCHEMA_VERSION, PROFILE_PRIVATE_CRDT_SCHEMA_VERSION,
};

pub(crate) mod content;
mod identity;
mod personal;
pub mod personal_migration;
mod private;
mod validation;
pub use identity::IdentityManifest;
pub use personal::PersonalManifest;
pub use personal_migration::{
    LegacyPersonalSource, MigratedPersonalUnit, PersonalMigrationReceipt, PersonalMigrationRefusal,
    admit_legacy_source, migrate_personal_sources,
};
pub use private::PrivateManifest;

/// Required wire address. Unknown tags/fields never select another unit.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProfileUnitRef {
    Identity {
        profile: ProfileId,
    },
    LegacyPrivate {
        profile: ProfileId,
    },
    Personal {
        owner: ProfileId,
        workspace: WorkspaceId,
    },
    Private {
        profile: ProfileId,
    },
}

impl ProfileUnitRef {
    pub fn profile(self) -> ProfileId {
        match self {
            Self::LegacyPrivate { profile }
            | Self::Identity { profile }
            | Self::Private { profile } => profile,
            Self::Personal { owner, .. } => owner,
        }
    }

    pub fn workspace(self) -> Option<WorkspaceId> {
        match self {
            Self::Personal { workspace, .. } => Some(workspace),
            _ => None,
        }
    }

    fn schema(self) -> i64 {
        match self {
            Self::Identity { .. } => PROFILE_IDENTITY_CRDT_SCHEMA_VERSION,
            Self::Personal { .. } => PERSONAL_CRDT_SCHEMA_VERSION,
            Self::Private { .. } => PROFILE_PRIVATE_CRDT_SCHEMA_VERSION,
            Self::LegacyPrivate { .. } => unreachable!("legacy does not use fresh unit storage"),
        }
    }

    /// The v1 header: format version plus the ids that name this exact unit.
    fn write_header(self, doc: &LoroDoc) -> Result<()> {
        let meta = doc.get_map("meta");
        meta.insert("schema_version", self.schema())
            .map_err(loro_failed)?;
        match self {
            ProfileUnitRef::Personal { owner, workspace } => {
                meta.insert("owner_profile_id", owner.to_storage())
                    .map_err(loro_failed)?;
                meta.insert("workspace_id", workspace.to_storage())
                    .map_err(loro_failed)?;
            }
            _ => meta
                .insert("profile_id", self.profile().to_storage())
                .map_err(loro_failed)?,
        }
        Ok(())
    }
}

impl UnitAddress for ProfileUnitRef {
    type Refusal = Error;

    fn validate(&self, doc: &LoroDoc) -> Result<Compatibility> {
        validation::validate(doc, *self)
    }

    fn merged(&self, before: &LoroDoc, after: &LoroDoc) {
        if matches!(self, ProfileUnitRef::Identity { .. }) {
            // Not a scope mismatch: the frame names this exact unit and
            // every scope check passed. What it disagrees about is a
            // field the contract calls immutable, which can only happen
            // when a second `IdentityManifest::create` lineage exists for
            // this Profile. Saying `scope` here sends the next reader to
            // the unit reference, the export mode and the advertised
            // version — all three of which are correct. See PLAN19 round
            // five.
            //
            // **Reported, not refused.** Refusing is self-sealing: each
            // side keeps its own lineage and rejects the other in both
            // directions forever, and only a device holding no copy can
            // ever escape. Worse, one refused unit ends the whole dataset
            // pass, so a disagreement about a *name* froze the private and
            // personal units beside it on every pass, permanently.
            //
            // Merging is self-healing. Loro takes both histories,
            // last-writer-wins settles one `created_at` and one `kind`,
            // and the merged document then converges everywhere it
            // reaches. Nothing a person cannot retype is lost, and
            // nothing is dropped: a Profile's Workspace list is not in
            // this document at all.
            //
            // The signal is kept because two lineages still mean a defect
            // upstream, and this is the only place that can see it.
            identity_lineage_divergence(self, &before.get_map("meta"), &after.get_map("meta"));
        }
    }
}

/// The v2 personal address: one Workspace, resolved under the caller's
/// validated [`UserScopeId`](crate::ids::UserScopeId). The scope is the SQL
/// row key and the authorization boundary; it is deliberately not a field
/// here because it is never written into the document.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PersonalUnitRef {
    pub workspace: WorkspaceId,
}

/// What a personal document is validated against. The content containers
/// are identical in both; only the header and the scope rule differ.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PersonalScope {
    /// v1: strict against its original `(owner Profile, Workspace)`. Kept so
    /// a legacy source is validated by its own contract before migration.
    Legacy {
        owner: ProfileId,
        workspace: WorkspaceId,
    },
    /// v2: the Workspace only. `meta.owner_profile_id`, if present, is a
    /// legacy string or a redaction null and is ignored for ownership.
    Workspace(PersonalUnitRef),
}

impl PersonalScope {
    pub fn workspace(self) -> WorkspaceId {
        match self {
            Self::Legacy { workspace, .. } => workspace,
            Self::Workspace(unit) => unit.workspace,
        }
    }

    /// The v1 wire address, when this is a legacy unit.
    pub fn legacy_unit(self) -> Option<ProfileUnitRef> {
        match self {
            Self::Legacy { owner, workspace } => {
                Some(ProfileUnitRef::Personal { owner, workspace })
            }
            Self::Workspace(_) => None,
        }
    }
}

impl UnitAddress for PersonalScope {
    type Refusal = Error;

    fn validate(&self, doc: &LoroDoc) -> Result<Compatibility> {
        match *self {
            Self::Legacy { owner, workspace } => {
                validation::validate(doc, ProfileUnitRef::Personal { owner, workspace })
            }
            Self::Workspace(unit) => validation::validate_personal_v2(doc, unit.workspace),
        }
    }
}

/// Load v1 bytes, deriving the Profile-oriented address from the header the
/// way this contract always has: the header names the unit, and the unit is
/// then validated against that same header.
fn load_profile_unit(
    bytes: &[u8],
    kind: fn(ProfileId, Option<WorkspaceId>) -> Result<ProfileUnitRef>,
) -> Result<UnitDoc<ProfileUnitRef>> {
    UnitDoc::load_with(bytes, |doc| {
        let meta = doc.get_map("meta");
        let owner = string(&meta, "owner_profile_id")
            .or_else(|| string(&meta, "profile_id"))
            .and_then(|id| ProfileId::parse(&id))
            .ok_or_else(|| corrupt("unit has no Profile scope"))?;
        let workspace = string(&meta, "workspace_id").and_then(|id| WorkspaceId::parse(&id));
        kind(owner, workspace)
    })
}

// Every public manifest uses the same validated staging boundary. Replacing
// self publishes only a complete candidate; a rejected frame cannot poison it.
// A frame with missing ancestors is refused here rather than reported as
// pending: these units' callers predate the pending vocabulary and treat it
// as a rejected transfer, which is what `CrdtRejected` has always meant.
macro_rules! unit_methods {
    () => {
        pub fn compatibility(&self) -> Compatibility {
            self.inner.compatibility
        }
        pub fn is_read_only(&self) -> bool {
            !self.compatibility().writable()
        }
        pub fn set_peer_id(&self, peer: u64) -> Result<()> {
            self.inner.require_writable()?;
            self.inner.doc.set_peer_id(peer).map_err(loro_failed)
        }
        pub fn export_snapshot(&self) -> Result<Vec<u8>> {
            self.inner.snapshot()
        }
        pub fn export_updates_since(&self, from: &loro::VersionVector) -> Result<Vec<u8>> {
            self.inner.updates_since(from)
        }
        pub fn version(&self) -> loro::VersionVector {
            self.inner.version()
        }
        pub fn staged_import(&self, frames: &[Vec<u8>]) -> Result<Self> {
            match self.inner.staged_import(frames)? {
                Staged::Applied(inner) => Ok(Self { inner }),
                Staged::Pending => Err(UnitFailure::FrameMissingAncestors.into()),
            }
        }
        pub fn import_frames(&mut self, frames: &[Vec<u8>]) -> Result<()> {
            *self = self.staged_import(frames)?;
            Ok(())
        }
    };
}
use unit_methods;

fn optional(map: &loro::LoroMap, key: &str, value: Option<&str>) -> Result<()> {
    match value {
        Some(v) => map.insert(key, v).map_err(loro_failed)?,
        None if map.get(key).is_some() => map.delete(key).map_err(loro_failed)?,
        None => {}
    }
    Ok(())
}
fn loro_failed(_: loro::LoroError) -> Error {
    UnitFailure::Crdt.into()
}
fn corrupt(message: &'static str) -> Error {
    Error::new(Code::CorruptState, message)
}
fn scope_mismatch() -> Error {
    Error::new(
        Code::ProfileUnitScopeMismatch,
        "Profile unit scope differs from its address",
    )
}
/// Two `IdentityManifest::create` lineages exist for one Profile and disagree
/// about a field the contract calls immutable.
///
/// This is a **report**, never a refusal. See the call site for why: refusing
/// is permanent and self-sealing, merging is self-healing, and the expensive
/// part was never the field itself but the whole dataset pass it ended.
///
/// Logged at `warn` with both values, because two lineages mean a defect
/// upstream — a device minted a manifest for a Profile it did not originate —
/// and this import is the only place in the system that can observe it.
fn identity_lineage_divergence(
    unit: &ProfileUnitRef,
    before: &loro::LoroMap,
    after: &loro::LoroMap,
) {
    for field in ["created_at", "kind"] {
        let (held, incoming) = match field {
            "created_at" => (
                integer(before, field).map(|v| v.to_string()),
                integer(after, field).map(|v| v.to_string()),
            ),
            _ => (string(before, field), string(after, field)),
        };
        if held != incoming {
            tracing::warn!(
                ?unit,
                field,
                held = ?held,
                incoming = ?incoming,
                code = Code::ProfileIdentityConflict.as_str(),
                "profile identity lineages disagree; merging",
            );
        }
    }
}

#[cfg(test)]
mod tests;
