// SPDX-License-Identifier: MPL-2.0

//! Admission of one subject unit into the account's hosted catalog
//! (TECHNICAL.final §7.3 "Cloud stores admitted units durably"; §4.3 "Every
//! resend, blob route, bundle-like fallback and forwarding hop uses the same
//! export boundary"; the roadmap's Plan 3.1 "Private metadata cannot enter
//! Cloud through root units, personal migration provenance or peer-only
//! history").
//!
//! The candidate is built exactly as a receiving device builds it: the held
//! snapshot forked and the frames applied, or a first arrival imported into
//! an empty document, then validated under the unit's own validator — the
//! root-catalog one for a catalog unit, the personal v2 one for a personal
//! unit. The hosted side adds three checks of its own: the unit's current
//! placement names a Profile the account holds or nothing; a personal unit
//! follows its Workspace unit; and the whole history goes through
//! [`derive`] with the account's Cloud-eligible Profiles as the visible set,
//! so a historical reference to a Profile that never reached Cloud is
//! redacted before the bytes are stored and a unit whose *current* placement
//! names one is held, never stored raw. What is stored is the derived
//! snapshot, which carries every operation id of the source, so the
//! frontier the device receipts is the frontier Cloud holds.

use std::collections::BTreeSet;

use koru_store_core::profile_units::PersonalManifest;
use koru_store_core::root_catalog::{
    CatalogUnitRef, Derived, ExportScope, HoldReason, PolicyCapture, ProfileCatalogUnit, Staged,
    WorkspaceCatalogUnit, derive,
};
use loro::VersionVector;
use serde::{Deserialize, Serialize};

use super::{CloudUnitRef, HOSTED_READER_PEER};
use crate::{CatalogScopeId, ProfileId, WorkspaceId};

/// What the service knows about the account's catalog when it admits a unit,
/// read in the admitting transaction.
#[derive(Clone, Copy, Debug)]
pub struct AdmissionContext<'a> {
    /// The session's scope: the row key every admitted unit is filed under.
    pub catalog_scope_id: CatalogScopeId,
    /// The Profiles the account holds — bound to it and not deleted — as the
    /// account tables say. A placement naming any other Profile is refused.
    pub account_profiles: &'a BTreeSet<ProfileId>,
    /// The Profiles whose Profile catalog unit this scope has admitted. A
    /// Profile unit reaches Cloud only when the device's mode for it admits
    /// the Cloud path, so this is the set of Cloud-eligible Profiles (§4.4)
    /// and the `visible` set of every derivation on the hosted side.
    pub eligible_profiles: &'a BTreeSet<ProfileId>,
    /// The Workspaces whose Workspace catalog unit this scope has admitted.
    /// A personal unit is admitted only for one of them (§7.3 "user-personal
    /// state follows the workspace's policy").
    pub held_workspaces: &'a BTreeSet<WorkspaceId>,
}

/// The row the scope already holds for the unit, if any.
#[derive(Clone, Copy, Debug)]
pub struct HeldUnit<'a> {
    pub snapshot: &'a [u8],
    pub revision: i64,
}

/// Why frames for a unit were refused outright. A refusal stores nothing and
/// leaves the held row as it was.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum AdmissionRefusal {
    /// The candidate fails the unit's own validator: a frame whose header
    /// names another subject, an unknown root or key, a malformed id, a
    /// live Workspace unit without a placement, a frame that does not decode.
    UnitInvalid { detail: String },
    /// A newer build wrote the unit; this build cannot judge what its
    /// history names and does not store it.
    FormatTooNew { found: i64, understood: i64 },
    /// The current placement names a Profile the account does not hold.
    PlacementForeign { profile: ProfileId },
    /// A personal unit for a Workspace whose catalog unit this scope has not
    /// admitted.
    WorkspaceUnitRequired { workspace: WorkspaceId },
    /// The export boundary refused the history: an unsupported operation,
    /// a container the validator does not allow, a malformed reference.
    ExportRefused { detail: String },
    /// The address is not a canonical v4 subject id.
    AddressNotCanonical,
}

impl AdmissionRefusal {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnitInvalid { .. } => "root_unit_invalid",
            Self::FormatTooNew { .. } => "root_unit_format_too_new",
            Self::PlacementForeign { .. } => "root_placement_foreign",
            Self::WorkspaceUnitRequired { .. } => "root_workspace_unit_required",
            Self::ExportRefused { .. } => "root_export_refused",
            Self::AddressNotCanonical => "root_unit_address_invalid",
        }
    }
}

/// Why an otherwise valid unit is held rather than stored: the same reasons
/// the export boundary holds a publication, plus nothing of the hosted
/// side's own. A hold is retried when the account's eligible set changes —
/// typically when the Profile the placement names arrives as its own unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionHold {
    CurrentPlacementHidden,
    OwnerHidden,
    LifecycleDiffers,
}

impl From<HoldReason> for AdmissionHold {
    fn from(reason: HoldReason) -> Self {
        match reason {
            HoldReason::CurrentPlacementHidden => Self::CurrentPlacementHidden,
            HoldReason::OwnerHidden => Self::OwnerHidden,
            HoldReason::LifecycleDiffers => Self::LifecycleDiffers,
        }
    }
}

impl AdmissionHold {
    pub const fn code(self) -> &'static str {
        match self {
            Self::CurrentPlacementHidden => "root_unit_held",
            Self::OwnerHidden => "root_unit_held",
            Self::LifecycleDiffers => "root_unit_held",
        }
    }
}

/// What admission decided. Only `Stored` writes a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admitted {
    /// Write `snapshot` as the unit's one durable snapshot, replacing exactly
    /// `expected_revision` (`None` inserts a row that must not exist), and
    /// answer `frontier` as the accepted frontier. `redacted_ops` says how
    /// many historical references the derivation nulled; it is a number for
    /// the log, never a list of what they named.
    Stored {
        unit: CloudUnitRef,
        snapshot: Vec<u8>,
        frontier: Vec<u8>,
        redacted_ops: usize,
        expected_revision: Option<i64>,
    },
    /// A frame needed ancestors the held unit does not have. Nothing is
    /// written; the device is asked from `frontier`, the held version.
    Pending {
        unit: CloudUnitRef,
        frontier: Vec<u8>,
    },
    /// The frames brought nothing the held unit did not have: an idempotent
    /// re-send. Nothing is written.
    Unchanged {
        unit: CloudUnitRef,
        frontier: Vec<u8>,
    },
    /// Valid, but not storable now (see [`AdmissionHold`]). Nothing is
    /// written, and the raw candidate is not kept either.
    Held {
        unit: CloudUnitRef,
        reason: AdmissionHold,
    },
}

impl Admitted {
    pub fn unit(&self) -> CloudUnitRef {
        match self {
            Self::Stored { unit, .. }
            | Self::Pending { unit, .. }
            | Self::Unchanged { unit, .. }
            | Self::Held { unit, .. } => *unit,
        }
    }
}

/// Admit frames for one Profile or Workspace catalog unit under `ctx`.
pub fn admit_root_unit(
    ctx: &AdmissionContext<'_>,
    unit: CatalogUnitRef,
    held: Option<HeldUnit<'_>>,
    frames: &[Vec<u8>],
) -> Result<Admitted, AdmissionRefusal> {
    let address = CloudUnitRef::catalog(unit);
    if !address.is_canonical() {
        return Err(AdmissionRefusal::AddressNotCanonical);
    }
    let candidate = match unit {
        CatalogUnitRef::Workspace { workspace } => {
            let staged = match held {
                Some(held) => {
                    let current = WorkspaceCatalogUnit::from_snapshot(
                        held.snapshot,
                        workspace,
                        HOSTED_READER_PEER,
                    )
                    .map_err(invalid)?;
                    let before = current.version();
                    match current.staged_import(frames).map_err(invalid)? {
                        Staged::Pending => return Ok(pending(address, &before)),
                        Staged::Applied(next) if next.version() == before => {
                            return Ok(Admitted::Unchanged {
                                unit: address,
                                frontier: before.encode(),
                            });
                        }
                        Staged::Applied(next) => next,
                    }
                }
                None => {
                    match WorkspaceCatalogUnit::from_frames(frames, workspace, HOSTED_READER_PEER)
                        .map_err(invalid)?
                    {
                        Staged::Pending => return Ok(pending(address, &VersionVector::default())),
                        Staged::Applied(next) => next,
                    }
                }
            };
            if staged.is_read_only() {
                return Err(too_new(staged.compatibility()));
            }
            if let Some(profile) = staged.placement().and_then(|placement| placement.profile)
                && !ctx.account_profiles.contains(&profile)
            {
                return Err(AdmissionRefusal::PlacementForeign { profile });
            }
            staged.snapshot().map_err(invalid)?
        }
        CatalogUnitRef::Profile { profile } => {
            let staged = match held {
                Some(held) => {
                    let current = ProfileCatalogUnit::from_snapshot(
                        held.snapshot,
                        profile,
                        HOSTED_READER_PEER,
                    )
                    .map_err(invalid)?;
                    let before = current.version();
                    match current.staged_import(frames).map_err(invalid)? {
                        Staged::Pending => return Ok(pending(address, &before)),
                        Staged::Applied(next) if next.version() == before => {
                            return Ok(Admitted::Unchanged {
                                unit: address,
                                frontier: before.encode(),
                            });
                        }
                        Staged::Applied(next) => next,
                    }
                }
                None => match ProfileCatalogUnit::from_frames(frames, profile, HOSTED_READER_PEER)
                    .map_err(invalid)?
                {
                    Staged::Pending => return Ok(pending(address, &VersionVector::default())),
                    Staged::Applied(next) => next,
                },
            };
            if staged.is_read_only() {
                return Err(too_new(staged.compatibility()));
            }
            if !ctx.account_profiles.contains(&profile) {
                return Err(AdmissionRefusal::PlacementForeign { profile });
            }
            staged.snapshot().map_err(invalid)?
        }
    };
    derive_for_storage(ctx, address, &candidate, held.map(|held| held.revision))
}

/// Admit frames for one personal v2 unit under `ctx`. The Workspace's own
/// catalog unit must already be admitted; the legacy `meta.owner_profile_id`
/// a migrated unit carries goes through the same redaction as a placement.
pub fn admit_personal_unit(
    ctx: &AdmissionContext<'_>,
    workspace: WorkspaceId,
    held: Option<HeldUnit<'_>>,
    frames: &[Vec<u8>],
) -> Result<Admitted, AdmissionRefusal> {
    let address = CloudUnitRef::personal(workspace);
    if !address.is_canonical() {
        return Err(AdmissionRefusal::AddressNotCanonical);
    }
    if !ctx.held_workspaces.contains(&workspace) {
        return Err(AdmissionRefusal::WorkspaceUnitRequired { workspace });
    }
    let staged = match held {
        Some(held) => {
            let current = PersonalManifest::from_snapshot_v2(held.snapshot, workspace)
                .map_err(store_invalid)?;
            let before = current.version();
            match current.stage_v2(frames).map_err(store_invalid)? {
                Staged::Pending => return Ok(pending(address, &before)),
                Staged::Applied(next) if next.version() == before => {
                    return Ok(Admitted::Unchanged {
                        unit: address,
                        frontier: before.encode(),
                    });
                }
                Staged::Applied(next) => next,
            }
        }
        None => match PersonalManifest::v2_from_frames(frames, workspace, HOSTED_READER_PEER)
            .map_err(store_invalid)?
        {
            Staged::Pending => return Ok(pending(address, &VersionVector::default())),
            Staged::Applied(next) => next,
        },
    };
    let candidate = staged.export_snapshot().map_err(store_invalid)?;
    derive_for_storage(ctx, address, &candidate, held.map(|held| held.revision))
}

/// §4.3 on the hosted side: the validated candidate through the export
/// boundary with the Cloud-eligible Profiles visible, stored as the derived
/// snapshot or held.
fn derive_for_storage(
    ctx: &AdmissionContext<'_>,
    unit: CloudUnitRef,
    candidate: &[u8],
    expected_revision: Option<i64>,
) -> Result<Admitted, AdmissionRefusal> {
    let visible = |profile: ProfileId| ctx.eligible_profiles.contains(&profile);
    let scope = ExportScope::new(
        &visible,
        PolicyCapture {
            policy_generation: 0,
            authorization: ctx.catalog_scope_id.to_string(),
        },
    );
    match derive(unit.export_subject(), candidate, &scope, None) {
        Ok(Derived::Publish(publication)) => Ok(Admitted::Stored {
            unit,
            snapshot: publication.bytes,
            frontier: publication.to.encode(),
            redacted_ops: publication.redacted_ops,
            expected_revision,
        }),
        Ok(Derived::Hold { reason }) => Ok(Admitted::Held {
            unit,
            reason: reason.into(),
        }),
        Err(refusal) => Err(AdmissionRefusal::ExportRefused {
            detail: refusal.to_string(),
        }),
    }
}

fn pending(unit: CloudUnitRef, frontier: &VersionVector) -> Admitted {
    Admitted::Pending {
        unit,
        frontier: frontier.encode(),
    }
}

fn invalid(refusal: koru_store_core::root_catalog::CatalogUnitRefusal) -> AdmissionRefusal {
    AdmissionRefusal::UnitInvalid {
        detail: refusal.to_string(),
    }
}

fn store_invalid(error: koru_store_core::Error) -> AdmissionRefusal {
    AdmissionRefusal::UnitInvalid {
        detail: error.message().to_owned(),
    }
}

fn too_new(compatibility: koru_store_core::version::Compatibility) -> AdmissionRefusal {
    match compatibility {
        koru_store_core::version::Compatibility::TooNew { found, understood } => {
            AdmissionRefusal::FormatTooNew { found, understood }
        }
        koru_store_core::version::Compatibility::Writable => AdmissionRefusal::UnitInvalid {
            detail: "unit reported read-only without a newer format".into(),
        },
    }
}
