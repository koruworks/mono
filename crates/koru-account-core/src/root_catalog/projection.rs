// SPDX-License-Identifier: MPL-2.0

//! The replayable discovery projection (TECHNICAL.final §7.3 "Cloud stores
//! admitted units durably, then advances a transactional discovery
//! projection and receipt. Projection frontiers are per-unit; do not
//! manufacture a global vector for independent documents or let an old job
//! overwrite a newer placement. Missing rows are not destructive evidence").
//!
//! The projection is a pure reducer over admitted units. Each job carries
//! the unit, the admitted snapshot and the row revision that snapshot was
//! stored under; the **projection watermark** of a unit is the revision the
//! projection has committed for it. A job below the watermark is stale and
//! changes nothing; a job at the watermark is a replay and changes nothing;
//! a job above it applies and advances the watermark. Applying the same job
//! twice is therefore idempotent and a watermark never regresses, which is
//! what a crash between "unit stored" and "projection committed" needs: the
//! service replays every unit whose `revision > projection_watermark`.
//!
//! Coverage is computed, not declared: a receipt is `Complete` only when the
//! projection's watermark covers every admitted unit's revision, and
//! `Partial` names exactly the units it does not. A missing unit is absent,
//! never deleted.

use std::collections::BTreeMap;

use koru_store_core::profile_units::PersonalManifest;
use koru_store_core::root_catalog::{CatalogUnitRef, ProfileCatalogUnit, WorkspaceCatalogUnit};
use serde::{Deserialize, Serialize};

use super::{CloudUnitRef, HOSTED_READER_PEER};
use crate::{ProfileId, WorkspaceId};

/// A subject's lifecycle as the unit's facts reduce it (§4.2: deleted while
/// a deletion id is named by no restore; purge evidence is terminal).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SubjectLifecycle {
    Live,
    Deleted,
    Purged,
}

/// One Workspace as discovery sees it: where it is placed, by which
/// revision, whether it is live, and how far the projection has read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceProjection {
    /// The current placement: a Profile, or `None` for *explicitly
    /// unassigned*. Never an invented Profile.
    pub placement: Option<ProfileId>,
    /// The unit revision at which the current placement was first committed
    /// to the projection. A grant cache keyed on it invalidates on a move
    /// and on nothing else.
    pub placement_revision: i64,
    pub lifecycle: SubjectLifecycle,
    /// The unit revision the projection has committed for this Workspace.
    pub watermark: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileProjection {
    pub lifecycle: SubjectLifecycle,
    pub watermark: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonalProjection {
    pub watermark: i64,
}

/// One projection job: a unit as stored, at the revision it was stored.
#[derive(Clone, Copy, Debug)]
pub struct ProjectionJob<'a> {
    pub unit: CloudUnitRef,
    pub snapshot: &'a [u8],
    pub revision: i64,
}

/// What applying a job did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    /// The watermark advanced to the job's revision.
    Applied,
    /// The job's revision is at or below the watermark: a replay or a job
    /// that lost its race to a newer one. Nothing changed.
    Stale { watermark: i64 },
}

/// A job whose snapshot the projection cannot read. The unit was admitted,
/// so this is corruption between storage and projection, never a reason to
/// project a default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionRefusal {
    pub unit: CloudUnitRef,
    pub detail: String,
}

/// An admitted unit's current revision, as the inventory lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitFrontier {
    pub unit: CloudUnitRef,
    pub revision: i64,
}

/// What a receipt says about how much of the admitted catalog the
/// projection it was computed from has read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Coverage {
    Complete,
    Partial { pending: Vec<CloudUnitRef> },
}

/// The committed discovery projection of one catalog scope.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryProjection {
    workspaces: BTreeMap<WorkspaceId, WorkspaceProjection>,
    profiles: BTreeMap<ProfileId, ProfileProjection>,
    personal: BTreeMap<WorkspaceId, PersonalProjection>,
}

impl DiscoveryProjection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn workspace(&self, workspace: WorkspaceId) -> Option<&WorkspaceProjection> {
        self.workspaces.get(&workspace)
    }

    pub fn profile(&self, profile: ProfileId) -> Option<&ProfileProjection> {
        self.profiles.get(&profile)
    }

    pub fn workspaces(&self) -> impl Iterator<Item = (WorkspaceId, &WorkspaceProjection)> {
        self.workspaces.iter().map(|(id, w)| (*id, w))
    }

    pub fn profiles(&self) -> impl Iterator<Item = (ProfileId, &ProfileProjection)> {
        self.profiles.iter().map(|(id, p)| (*id, p))
    }

    /// The projection watermark of a unit: the revision committed for it,
    /// `None` when the projection has never read it.
    pub fn watermark(&self, unit: CloudUnitRef) -> Option<i64> {
        match unit {
            CloudUnitRef::ProfileCatalog { subject_id } => {
                self.profiles.get(&subject_id).map(|p| p.watermark)
            }
            CloudUnitRef::WorkspaceCatalog { subject_id } => {
                self.workspaces.get(&subject_id).map(|w| w.watermark)
            }
            CloudUnitRef::PersonalV2 { subject_id } => {
                self.personal.get(&subject_id).map(|p| p.watermark)
            }
        }
    }

    /// The live Workspaces currently placed under `profile` (`None`: the
    /// explicitly unassigned ones). This is the grouping a discovery count
    /// uses: a moved Workspace appears under its destination and nowhere
    /// else, so it is counted once.
    pub fn placed_under(
        &self,
        profile: Option<ProfileId>,
    ) -> impl Iterator<Item = WorkspaceId> + '_ {
        self.workspaces
            .iter()
            .filter(move |(_, w)| w.lifecycle == SubjectLifecycle::Live && w.placement == profile)
            .map(|(id, _)| *id)
    }

    /// Apply one job. See the module doc for the watermark rule.
    pub fn apply(&mut self, job: &ProjectionJob<'_>) -> Result<Applied, ProjectionRefusal> {
        if let Some(watermark) = self.watermark(job.unit)
            && job.revision <= watermark
        {
            return Ok(Applied::Stale { watermark });
        }
        match job.unit {
            CloudUnitRef::WorkspaceCatalog { subject_id } => {
                let unit = WorkspaceCatalogUnit::from_snapshot(
                    job.snapshot,
                    subject_id,
                    HOSTED_READER_PEER,
                )
                .map_err(|refusal| refuse(job.unit, refusal.to_string()))?;
                let placement = unit.placement().and_then(|placement| placement.profile);
                let lifecycle = lifecycle(unit.is_live(), !unit.purged().is_empty());
                let entry = self
                    .workspaces
                    .entry(subject_id)
                    .or_insert(WorkspaceProjection {
                        placement,
                        placement_revision: job.revision,
                        lifecycle,
                        watermark: job.revision,
                    });
                if entry.placement != placement {
                    entry.placement = placement;
                    entry.placement_revision = job.revision;
                }
                entry.lifecycle = lifecycle;
                entry.watermark = job.revision;
            }
            CloudUnitRef::ProfileCatalog { subject_id } => {
                let unit =
                    ProfileCatalogUnit::from_snapshot(job.snapshot, subject_id, HOSTED_READER_PEER)
                        .map_err(|refusal| refuse(job.unit, refusal.to_string()))?;
                self.profiles.insert(
                    subject_id,
                    ProfileProjection {
                        lifecycle: lifecycle(unit.is_live(), !unit.purged().is_empty()),
                        watermark: job.revision,
                    },
                );
            }
            CloudUnitRef::PersonalV2 { subject_id } => {
                PersonalManifest::from_snapshot_v2(job.snapshot, subject_id)
                    .map_err(|error| refuse(job.unit, error.message().to_owned()))?;
                self.personal.insert(
                    subject_id,
                    PersonalProjection {
                        watermark: job.revision,
                    },
                );
            }
        }
        Ok(Applied::Applied)
    }

    /// Which admitted units this projection has not yet read at their
    /// current revision. `Complete` iff none.
    pub fn coverage(&self, admitted: &[UnitFrontier]) -> Coverage {
        let pending: Vec<_> = admitted
            .iter()
            .filter(|frontier| {
                self.watermark(frontier.unit)
                    .is_none_or(|watermark| watermark < frontier.revision)
            })
            .map(|frontier| frontier.unit)
            .collect();
        if pending.is_empty() {
            Coverage::Complete
        } else {
            Coverage::Partial { pending }
        }
    }
}

/// Reduce a set of jobs into a projection, in the order given. The order
/// does not matter for the result when every job is the newest revision of
/// its unit; when it is not, the watermark rule decides.
pub fn project<'a>(
    jobs: impl IntoIterator<Item = ProjectionJob<'a>>,
) -> Result<DiscoveryProjection, ProjectionRefusal> {
    let mut projection = DiscoveryProjection::new();
    for job in jobs {
        projection.apply(&job)?;
    }
    Ok(projection)
}

fn lifecycle(live: bool, purged: bool) -> SubjectLifecycle {
    if live {
        SubjectLifecycle::Live
    } else if purged {
        SubjectLifecycle::Purged
    } else {
        SubjectLifecycle::Deleted
    }
}

fn refuse(unit: CloudUnitRef, detail: String) -> ProjectionRefusal {
    ProjectionRefusal { unit, detail }
}

/// Every unit the projection should hold for a scope, for the replay after a
/// crash: `(unit, revision)` of every admitted row. The service passes its
/// inventory here and replays the rows [`DiscoveryProjection::coverage`]
/// names as pending.
pub fn replay_needed(
    projection: &DiscoveryProjection,
    admitted: &[UnitFrontier],
) -> Vec<CatalogUnitRef> {
    match projection.coverage(admitted) {
        Coverage::Complete => Vec::new(),
        Coverage::Partial { pending } => pending
            .into_iter()
            .filter_map(CloudUnitRef::as_catalog)
            .collect(),
    }
}
