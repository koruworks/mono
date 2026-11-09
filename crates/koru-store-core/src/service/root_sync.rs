// SPDX-License-Identifier: MPL-2.0

//! **The root catalog between devices** — the Store half of peer protocol 6
//! (`docs/features/root-catalog/TECHNICAL.final.md` §4.1, §4.2, §4.3, §4.4,
//! §7.2). Nothing here knows a device, a connection or a frame; the
//! replication adapters hand in a relationship id and a grant and get back
//! what may leave, what came in, and the receipt that says so.
//!
//! ```text
//! sending                                        receiving
//!   bind_peer_scope(relationship)   once per        bind_peer_scope(relationship)
//!   root_export_inventory(grant)    eligible units  root_unit_frontier(unit) → request version
//!   root_export_source(grant, u)    → export::derive  stage_root_unit(rel, u, frames)   off the lock
//!     (visible set, generation)        Hold | bytes  commit_staged_root_unit(rel, staged)  recheck,
//!                                                     publish_root: unit + projections + receipt
//! ```
//!
//! Four rules hold the module together:
//!
//! - **Scope is bound once, by authentication, never by a name.** An
//!   `all_data` pairing is the same person's device; its validated
//!   relationship maps to this device's user scope the first time it is seen
//!   and the mapping is kept in `store_state` ([`peer_scope_key`]). A Profile
//!   or Workspace grant is a content relationship and is never bound, so its
//!   root frames are refused and never reach the catalog (§7.2 last bullet).
//! - **Eligibility is the recipient's grant and this device's policy,
//!   together.** A unit is framed only when the recipient may know its
//!   subject and the subject's device-local transport admission allows the
//!   peer path; a hidden subject is in no list, count or digest. Every byte
//!   that leaves passes [`derive`], which redacts historical references to
//!   Profiles the recipient may not know and *holds* a unit whose current
//!   state names one — nothing raw ever stands in.
//! - **Staging is off the lock; commit rechecks.** [`Store::stage_root_unit`]
//!   is a read: it forks the held unit, applies, validates and answers a
//!   candidate with the revision, overlay bytes and policy generation it was
//!   built against. [`Store::commit_staged_root_unit`] refuses if any of the
//!   three moved, then publishes unit, projections and receipt in one
//!   catalog transaction. A frame with missing ancestors is `Pending`:
//!   nothing is applied and the receipt stays at `staged`.
//! - **The receipt is a value in `store_state`, not a log line.** Phases
//!   `prepared → staged → locally_committed → reconciled` and the accepted
//!   frontier per `(relationship, unit)` live under [`join_receipt_key`]:
//!   upserted by key, read back to answer "what do I ask for next", never
//!   regressed by an older frontier. `sync_state_events` is the pruned,
//!   newest-first device history; a receipt must survive pruning and be
//!   addressed by key, which is what `store_state` is for (the Profile
//!   deletion job sits beside it for the same reason).

use super::root_lifecycle::earliest_active_deletion;
use super::root_move::{RootUnit, fold_overlay, transport_subject};
use super::*;
use crate::ids::UserScopeId;
use crate::ports::{
    ProfileCatalogState, RootCatalogOverlay, RootCatalogUnitCommit, RootCatalogUnitRow,
    RootProjection, RootPublication, RootUnitWrite, WorkspaceReminderDue,
};
use crate::profile_units::PersonalManifest;
use crate::root_catalog::{
    CatalogUnitKind, CatalogUnitRef, Derived, ExportScope, ExportSubject, Placement, PolicyCapture,
    ProfileCatalogUnit, Staged, WorkspaceCatalogUnit, derive,
};
use crate::transport::TransportSubject;
use loro::VersionVector;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Prefix of the `store_state` key that binds one paired relationship to
/// this device's user scope: `root_catalog.peer_scope.<relationship id>`,
/// value the scope id. Written once, by [`Store::bind_peer_scope`], for an
/// `all_data` relationship; read before any root frame of that relationship
/// is applied. Never replicated.
pub const K_PEER_SCOPE_PREFIX: &str = "root_catalog.peer_scope.";

/// Prefix of the `store_state` key of one join receipt:
/// `root_catalog.join.<relationship id>.<kind>.<subject id>`, JSON of
/// [`RootJoinReceipt`]. `kind` is `profile_catalog`, `workspace_catalog` or
/// `personal_v2`.
pub const K_JOIN_RECEIPT_PREFIX: &str = "root_catalog.join.";

pub fn peer_scope_key(relationship: Uuid) -> String {
    format!("{K_PEER_SCOPE_PREFIX}{relationship}")
}

pub fn join_receipt_key(relationship: Uuid, unit: JoinUnit) -> String {
    format!(
        "{K_JOIN_RECEIPT_PREFIX}{relationship}.{}.{}",
        unit.kind(),
        unit.subject_id()
    )
}

/// Which unit a receipt is about. The three unit families a peer exchanges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JoinUnit {
    ProfileCatalog { profile: ProfileId },
    WorkspaceCatalog { workspace: WorkspaceId },
    PersonalV2 { workspace: WorkspaceId },
}

impl JoinUnit {
    pub fn catalog(unit: CatalogUnitRef) -> Self {
        match unit {
            CatalogUnitRef::Profile { profile } => Self::ProfileCatalog { profile },
            CatalogUnitRef::Workspace { workspace } => Self::WorkspaceCatalog { workspace },
        }
    }

    pub fn kind(self) -> &'static str {
        match self {
            Self::ProfileCatalog { .. } => CatalogUnitKind::Profile.as_str(),
            Self::WorkspaceCatalog { .. } => CatalogUnitKind::Workspace.as_str(),
            Self::PersonalV2 { .. } => "personal_v2",
        }
    }

    pub fn subject_id(self) -> String {
        match self {
            Self::ProfileCatalog { profile } => profile.to_storage(),
            Self::WorkspaceCatalog { workspace } | Self::PersonalV2 { workspace } => {
                workspace.to_storage()
            }
        }
    }
}

/// The join phases of §7.2. `Prepared` is the in-memory state between
/// scope check and staging and is never persisted; `Staged` is persisted
/// only for a pending frame (ancestors missing), so the next request asks
/// from the right frontier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinPhase {
    Prepared,
    Staged,
    LocallyCommitted,
    Reconciled,
}

/// One `(relationship, unit)` join receipt (§4.1 "The join receipt records
/// source identity, authorized scope, operation ID, accepted unit
/// frontier(s) and phase").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootJoinReceipt {
    pub relationship: Uuid,
    pub scope: UserScopeId,
    pub unit: JoinUnit,
    pub phase: JoinPhase,
    /// The accepted frontier: the held unit's version after the last
    /// applied frame, as an encoded [`VersionVector`]. Never regresses.
    pub frontier: Vec<u8>,
    /// The id of the operation that last moved this receipt. Opaque.
    pub operation_id: Uuid,
    pub at_ms: i64,
}

impl RootJoinReceipt {
    pub fn frontier_vv(&self) -> VersionVector {
        VersionVector::decode(&self.frontier).unwrap_or_default()
    }
}

/// What a recipient is allowed to know, as the adapter reads it off the
/// authenticated peer. Not authority over placement: an `all_data` grant is
/// the same person's device and sees every exportable subject; a Profile
/// grant sees its Profiles and the Workspaces *currently placed* under them;
/// a Workspace grant sees that Workspace's unit and no Profile.
#[derive(Debug, Clone, Copy)]
pub struct RootGrant<'a> {
    pub all_data: bool,
    pub profiles: &'a BTreeSet<ProfileId>,
    pub workspaces: &'a BTreeSet<WorkspaceId>,
}

/// The eligible root inventory for one recipient: every unit this device
/// may frame for it, with the authoritative version. `held` counts subjects
/// the recipient may know but that cannot leave right now (a pending
/// overlay, a missing or inconsistent mode row, a hidden current placement)
/// — a number, never an id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RootInventory {
    pub units: Vec<(CatalogUnitRef, VersionVector)>,
    pub personal: Vec<(WorkspaceId, VersionVector)>,
    pub held: usize,
}

/// What the export boundary needs for one recipient, read under the Store
/// lock so [`derive`] can run off it: the authoritative bytes, the set of
/// Profiles this recipient may know (the `visible` closure), and the policy
/// generation to recheck before sending. A Profile id in a unit's history
/// that is not in `visible` — hidden here, or unknown here — is redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootExportSource {
    pub subject: ExportSubject,
    pub snapshot: Vec<u8>,
    pub visible: BTreeSet<ProfileId>,
    pub generation: u64,
}

impl RootExportSource {
    /// §4.3 steps 2–4 for one recipient. `authorization` is the caller's
    /// opaque token for the recipient's standing, echoed back in the
    /// publication's capture for the recheck before the send.
    pub fn derive(
        &self,
        recipient: Option<&VersionVector>,
        authorization: String,
    ) -> Result<Derived> {
        let visible = |profile: ProfileId| self.visible.contains(&profile);
        let scope = ExportScope::new(
            &visible,
            PolicyCapture {
                policy_generation: self.generation,
                authorization,
            },
        );
        Ok(derive(self.subject, &self.snapshot, &scope, recipient)?)
    }
}

/// A root unit candidate, staged off the lock and waiting for its commit.
/// Everything the commit rechecks is carried here: the durable revision and
/// overlay bytes the candidate was built from and the policy generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedRootUnit {
    pub unit: CatalogUnitRef,
    /// `None` when a frame needed ancestors the unit does not have: nothing
    /// to commit, the receipt moves to `staged` at `frontier`.
    candidate: Option<Vec<u8>>,
    expected_revision: Option<i64>,
    overlay_before: Option<Vec<u8>>,
    clear_overlay: bool,
    /// The held version before staging; the frontier a pending receipt
    /// carries and the baseline an idempotent re-send compares against.
    before: VersionVector,
    /// The candidate's version: the frontier after commit.
    frontier: VersionVector,
    generation: u64,
}

impl StagedRootUnit {
    pub fn is_pending(&self) -> bool {
        self.candidate.is_none()
    }

    /// The frontier a request should carry now: the candidate's version
    /// once committed, the held version while pending.
    pub fn frontier(&self) -> &VersionVector {
        if self.candidate.is_some() {
            &self.frontier
        } else {
            &self.before
        }
    }
}

/// A personal v2 candidate, staged the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedPersonalUnit {
    pub workspace: WorkspaceId,
    candidate: Option<Vec<u8>>,
    writer_peer: u64,
    held_before: Option<Vec<u8>>,
    before: VersionVector,
    frontier: VersionVector,
    generation: u64,
    due_rows: Vec<WorkspaceReminderDue>,
    changes: Vec<(Option<ReminderId>, ReminderChange)>,
}

impl StagedPersonalUnit {
    pub fn is_pending(&self) -> bool {
        self.candidate.is_none()
    }

    pub fn frontier(&self) -> &VersionVector {
        if self.candidate.is_some() {
            &self.frontier
        } else {
            &self.before
        }
    }
}

/// What a commit did (§7.2 join phases).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootJoinOutcome {
    /// Locally committed; `reconciled` when the frontier covers what the
    /// sender advertised.
    Applied {
        frontier: VersionVector,
        reconciled: bool,
        /// The effective placement changed (Workspace units only).
        placement_changed: bool,
        /// The effective trash state changed.
        lifecycle_changed: bool,
    },
    /// Ancestors missing: nothing applied, receipt at `staged`.
    Pending { frontier: VersionVector },
}

/// The publication job's answer: overlays folded into their durable unit
/// this pass, and overlays still waiting (not yet admitted, or a failed
/// fold left for the next pass).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PublishedOverlays {
    pub published: usize,
    pub held: usize,
}

/// Which one-way projection a Workspace root import writes beside its unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportProjection {
    Deleted,
    Restored,
    Placement,
}

/// Whether `grant` may know a Workspace currently placed under `placement`:
/// the same person's device may, an exact Workspace grant may, a Profile
/// grant may while the placement names one of its Profiles — and matches
/// nothing for an unassigned or pending placement.
pub fn grant_covers_workspace(
    grant: &RootGrant<'_>,
    workspace: WorkspaceId,
    placement: Option<ProfileId>,
) -> bool {
    grant.all_data
        || grant.workspaces.contains(&workspace)
        || placement.is_some_and(|profile| grant.profiles.contains(&profile))
}

/// A staged candidate whose premises moved before its commit — the policy
/// generation, the overlay or (through the unit's compare-and-set) the
/// durable revision. Reported as a stale operation: the candidate is
/// discarded, local work is kept, and the next pass stages again.
fn policy_changed() -> Error {
    Error::new(
        Code::IdempotencyConflict,
        "the policy generation moved between staging and commit; the candidate is discarded",
    )
}

fn not_eligible() -> Error {
    Error::new(
        Code::Forbidden,
        "this unit is not eligible for the recipient on the peer path",
    )
}

impl<C: Catalog, B: Backend> Store<C, B> {
    // ------------------------------------------------------- scope binding

    /// Bind `relationship` to this device's user scope, once. A relationship
    /// already bound to this scope is a no-op; one bound to another scope
    /// (a second account's catalog on this device, §4.1 "Two already-bound
    /// different accounts cannot join") refuses. The caller has established
    /// that the relationship is an `all_data` pairing; nothing here reads a
    /// container name.
    pub fn bind_peer_scope(&mut self, relationship: Uuid) -> Result<UserScopeId> {
        let scope = self.catalog.local_user_scope()?;
        let key = peer_scope_key(relationship);
        match self.catalog.state(&key)? {
            Some(stored) => {
                let bound = UserScopeId::parse(&stored).ok_or_else(|| {
                    Error::new(
                        Code::CorruptState,
                        "a peer scope binding is not a canonical scope id",
                    )
                })?;
                if bound != scope {
                    return Err(Error::new(
                        Code::Forbidden,
                        "this relationship is bound to another user scope",
                    ));
                }
            }
            None => {
                self.catalog.set_state(&key, &scope.to_storage())?;
                tracing::info!(relationship = %relationship, "root_sync action=scope_bound");
            }
        }
        Ok(scope)
    }

    /// The scope `relationship` is bound to, if any.
    pub fn peer_scope(&self, relationship: Uuid) -> Result<Option<UserScopeId>> {
        self.catalog
            .state(&peer_scope_key(relationship))?
            .map(|stored| {
                UserScopeId::parse(&stored).ok_or_else(|| {
                    Error::new(
                        Code::CorruptState,
                        "a peer scope binding is not a canonical scope id",
                    )
                })
            })
            .transpose()
    }

    fn require_peer_scope(&self, relationship: Uuid) -> Result<UserScopeId> {
        let scope = self.catalog.local_user_scope()?;
        match self.peer_scope(relationship)? {
            Some(bound) if bound == scope => Ok(scope),
            Some(_) => Err(Error::new(
                Code::Forbidden,
                "this relationship is bound to another user scope",
            )),
            None => Err(Error::new(
                Code::Forbidden,
                "this relationship is not bound to a user scope; root frames are refused",
            )),
        }
    }

    // -------------------------------------------------------- the readers

    /// The Profile a Workspace is currently placed under on this device, as
    /// authorization and the content catalog read it: the root placement
    /// (overlay first, a Holder reads as unassigned) when a unit exists, the
    /// `workspaces.owner` projection only for a row without one. `None` is
    /// unassigned or pending; a Profile-granted peer matches nothing then.
    pub fn current_placement_profile(&self, workspace: WorkspaceId) -> Result<Option<ProfileId>> {
        if let Some(placement) = self.root_placement(workspace)? {
            return Ok(placement.profile);
        }
        Ok(self
            .workspace_including_deleted(workspace)
            .ok()
            .map(|row| row.owner))
    }

    /// Whether `subject` has a mode row that admits the peer path. A subject
    /// with no row yet (a Workspace this device has not provisioned) is
    /// neither admitted nor refused here; callers decide which direction the
    /// absence falls (inbound discovery admits, export holds).
    fn peer_admission(&self, subject: TransportSubject) -> Option<bool> {
        self.transport_admission(subject)
            .ok()
            .map(|admission| admission.allows_peer())
    }

    /// §4.4 "For known LocalOnly subjects, block inbound metadata as well as
    /// outbound payloads": a subject whose mode row closes the peer path
    /// admits no root or personal frame. An unknown subject is discovery and
    /// is admitted; content download for it is gated separately.
    fn refuse_blocked_inbound(&self, subject: TransportSubject) -> Result<()> {
        match self.peer_admission(subject) {
            Some(false) => Err(Error::new(
                Code::Forbidden,
                "this device's transport mode is Local-only here; inbound root metadata is refused",
            )),
            _ => Ok(()),
        }
    }

    /// The durable unit's version — what remote frames merge into and what a
    /// request carries. `None` when this device holds no unit for the
    /// subject; a refusal when its mode blocks inbound metadata.
    pub fn root_unit_frontier(&self, unit: CatalogUnitRef) -> Result<Option<VersionVector>> {
        self.refuse_blocked_inbound(transport_subject(unit))?;
        let scope = self.catalog.local_user_scope()?;
        let Some(row) = self.catalog.root_catalog_unit(scope, unit)? else {
            return Ok(None);
        };
        Ok(Some(match unit {
            CatalogUnitRef::Workspace { .. } => {
                WorkspaceCatalogUnit::open(&row.snapshot, unit, self.peer_id)?.version()
            }
            CatalogUnitRef::Profile { .. } => {
                ProfileCatalogUnit::open(&row.snapshot, unit, self.peer_id)?.version()
            }
        }))
    }

    /// The personal v2 unit's version for `workspace`, or `None` when this
    /// scope holds none; refused when the Workspace's mode blocks inbound.
    pub fn personal_frontier(&self, workspace: WorkspaceId) -> Result<Option<VersionVector>> {
        self.refuse_blocked_inbound(TransportSubject::Workspace(workspace))?;
        let scope = self.catalog.local_user_scope()?;
        let Some(state) = self.catalog.workspace_personal_unit(scope, workspace)? else {
            return Ok(None);
        };
        Ok(Some(
            PersonalManifest::from_snapshot_v2(&state.snapshot, workspace)?.version(),
        ))
    }

    /// The receipt for one `(relationship, unit)`, if a join has happened.
    pub fn join_receipt(
        &self,
        relationship: Uuid,
        unit: JoinUnit,
    ) -> Result<Option<RootJoinReceipt>> {
        self.catalog
            .state(&join_receipt_key(relationship, unit))?
            .map(|json| {
                serde_json::from_str(&json)
                    .map_err(|_| Error::new(Code::CorruptState, "invalid root join receipt"))
            })
            .transpose()
    }

    // --------------------------------------------------------- the sender

    /// The Profiles `grant` may know: every Profile whose mode admits the
    /// peer path for the same person's device, the granted ones (when
    /// exportable) for a Profile grant, none for a Workspace grant. This is
    /// the `visible` set of every export for that recipient; a Profile id
    /// this device does not hold at all is not in it and is redacted.
    pub fn exportable_profiles(&self, grant: &RootGrant<'_>) -> Result<BTreeSet<ProfileId>> {
        let mut visible = BTreeSet::new();
        if !grant.all_data && grant.profiles.is_empty() {
            return Ok(visible);
        }
        for profile in self.catalog.profiles_including_removed()? {
            if !grant.all_data && !grant.profiles.contains(&profile.id) {
                continue;
            }
            if self.peer_admission(TransportSubject::Profile(profile.id)) == Some(true) {
                visible.insert(profile.id);
            }
        }
        Ok(visible)
    }

    /// The authoritative durable snapshot of `unit` if it may leave now, as
    /// [`Self::eligible_export_source`] answers it — with a missing or
    /// inconsistent mode row read as a hold rather than a failure of the
    /// whole inventory. Nothing leaves either way.
    fn export_source_or_hold(&self, unit: CatalogUnitRef) -> Result<Option<Vec<u8>>> {
        match self.eligible_export_source(unit) {
            Ok(source) => Ok(source),
            Err(error) if error.code() == Code::CorruptState => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The eligible root inventory for one recipient (§4.2 "Directory
    /// inventories are built from eligible units for the recipient and
    /// transport, not replicated as an all-subject list"). Run the
    /// publication job first so a pending overlay has had its chance.
    pub fn root_export_inventory(&self, grant: &RootGrant<'_>) -> Result<RootInventory> {
        let scope = self.catalog.local_user_scope()?;
        let visible = self.exportable_profiles(grant)?;
        let mut inventory = RootInventory::default();
        for (unit, _) in self
            .catalog
            .root_catalog_units(scope, CatalogUnitKind::Profile)?
        {
            let Some(profile) = unit.profile() else {
                continue;
            };
            if !visible.contains(&profile) {
                // Not for this recipient, or not exportable here: not even
                // a count. A Workspace-granted peer sees no Profile unit.
                continue;
            }
            match self.export_source_or_hold(unit)? {
                Some(snapshot) => {
                    let version =
                        ProfileCatalogUnit::open(&snapshot, unit, self.peer_id)?.version();
                    inventory.units.push((unit, version));
                }
                None => inventory.held += 1,
            }
        }
        let mut listed_workspaces = BTreeSet::new();
        for (unit, _) in self
            .catalog
            .root_catalog_units(scope, CatalogUnitKind::Workspace)?
        {
            let Some(workspace) = unit.workspace() else {
                continue;
            };
            let Some(snapshot) = self.export_source_or_hold(unit)? else {
                // Held or not exportable. Counted only when the recipient
                // could have known the subject, so a hidden Workspace is not
                // even a number in the answer.
                if grant_covers_workspace(grant, workspace, None) {
                    inventory.held += 1;
                }
                continue;
            };
            let held = WorkspaceCatalogUnit::open(&snapshot, unit, self.peer_id)?;
            let placement = held.placement().and_then(|placement| placement.profile);
            if !grant_covers_workspace(grant, workspace, placement) {
                continue;
            }
            if let Some(profile) = placement
                && !visible.contains(&profile)
            {
                // The current placement names a Profile this recipient may
                // not know: `derive` would hold, so the unit is not listed.
                inventory.held += 1;
                continue;
            }
            listed_workspaces.insert(workspace);
            inventory.units.push((unit, held.version()));
        }
        if grant.all_data {
            for (workspace, state) in self.catalog.workspace_personal_units(scope)? {
                if !listed_workspaces.contains(&workspace) {
                    continue;
                }
                let version =
                    PersonalManifest::from_snapshot_v2(&state.snapshot, workspace)?.version();
                inventory.personal.push((workspace, version));
            }
        }
        Ok(inventory)
    }

    /// What the export boundary derives from for one recipient and one
    /// subject: `Ok(None)` when this device holds no such unit, a refusal
    /// when the recipient may not know it or it may not leave now. The
    /// caller runs [`RootExportSource::derive`] off the lock and rechecks
    /// `generation` and the recipient's standing before the send.
    pub fn root_export_source(
        &self,
        grant: &RootGrant<'_>,
        subject: ExportSubject,
    ) -> Result<Option<RootExportSource>> {
        let scope = self.catalog.local_user_scope()?;
        let visible = self.exportable_profiles(grant)?;
        let generation = self.policy_generation();
        let snapshot = match subject {
            ExportSubject::ProfileCatalog(profile) => {
                let unit = CatalogUnitRef::Profile { profile };
                if self.catalog.root_catalog_unit(scope, unit)?.is_none() {
                    return Ok(None);
                }
                if !visible.contains(&profile) {
                    return Err(not_eligible());
                }
                self.export_source_or_hold(unit)?.ok_or_else(not_eligible)?
            }
            ExportSubject::WorkspaceCatalog(workspace) => {
                let unit = CatalogUnitRef::Workspace { workspace };
                if self.catalog.root_catalog_unit(scope, unit)?.is_none() {
                    return Ok(None);
                }
                let snapshot = self.export_source_or_hold(unit)?.ok_or_else(not_eligible)?;
                let placement = WorkspaceCatalogUnit::open(&snapshot, unit, self.peer_id)?
                    .placement()
                    .and_then(|placement| placement.profile);
                if !grant_covers_workspace(grant, workspace, placement) {
                    return Err(not_eligible());
                }
                snapshot
            }
            ExportSubject::PersonalV2(workspace) => {
                if !grant.all_data {
                    return Err(not_eligible());
                }
                let Some(state) = self.catalog.workspace_personal_unit(scope, workspace)? else {
                    return Ok(None);
                };
                // Personal data follows the Workspace's policy (§7.3): the
                // Workspace's own unit must be exportable for its personal
                // unit to be.
                let unit = CatalogUnitRef::Workspace { workspace };
                if self.export_source_or_hold(unit)?.is_none() {
                    return Err(not_eligible());
                }
                state.snapshot
            }
        };
        Ok(Some(RootExportSource {
            subject,
            snapshot,
            visible,
            generation,
        }))
    }

    // ------------------------------------------------------- the receiver

    /// Stage a peer's frames for one root unit (§7.2 `prepared → staged`).
    /// A read: the held unit is forked, the frames applied, the complete
    /// candidate validated under the unit's own validator — a frame whose
    /// scope header names another subject is refused — and the result
    /// returned with everything the commit rechecks. The relationship must
    /// be scope-bound and the subject must not be a known Local-only one.
    pub fn stage_root_unit(
        &self,
        relationship: Uuid,
        unit: CatalogUnitRef,
        frames: &[Vec<u8>],
    ) -> Result<StagedRootUnit> {
        let scope = self.require_peer_scope(relationship)?;
        self.refuse_blocked_inbound(transport_subject(unit))?;
        let generation = self.policy_generation();
        let durable = self.catalog.root_catalog_unit(scope, unit)?;
        let overlay = self.catalog.root_catalog_overlay(scope, unit)?;
        let admitted = self.peer_admission(transport_subject(unit)) == Some(true);
        match unit {
            CatalogUnitRef::Workspace { .. } => self.stage_root_frames::<WorkspaceCatalogUnit>(
                unit,
                durable.as_ref(),
                overlay.as_ref(),
                admitted,
                frames,
                generation,
            ),
            CatalogUnitRef::Profile { .. } => self.stage_root_frames::<ProfileCatalogUnit>(
                unit,
                durable.as_ref(),
                overlay.as_ref(),
                admitted,
                frames,
                generation,
            ),
        }
    }

    fn stage_root_frames<U: RootUnit>(
        &self,
        unit: CatalogUnitRef,
        durable: Option<&RootCatalogUnitRow>,
        overlay: Option<&RootCatalogOverlay>,
        admitted: bool,
        frames: &[Vec<u8>],
        generation: u64,
    ) -> Result<StagedRootUnit> {
        // The base the frames merge into: the durable unit, with a pending
        // overlay folded in when export is now admitted (the same fold a
        // move makes), else the durable unit alone and the overlay left for
        // the publication job.
        let (base, clear_overlay): (Option<U>, bool) = match (durable, overlay) {
            (durable, Some(overlay)) if admitted => (
                Some(fold_overlay::<U>(unit, durable, overlay, self.peer_id)?),
                true,
            ),
            (Some(row), _) => (Some(U::open(&row.snapshot, unit, self.peer_id)?), false),
            (None, _) => (None, false),
        };
        let before = base.as_ref().map(RootUnit::version).unwrap_or_default();
        let staged = match &base {
            Some(held) => held.staged_import(frames)?,
            None => U::from_frames(frames, unit, self.peer_id)?,
        };
        let (candidate, frontier) = match staged {
            Staged::Applied(candidate) => {
                let frontier = candidate.version();
                (Some(candidate.snapshot()?), frontier)
            }
            Staged::Pending => (None, before.clone()),
        };
        Ok(StagedRootUnit {
            unit,
            candidate,
            expected_revision: durable.map(|row| row.revision),
            overlay_before: overlay.map(|overlay| overlay.private_snapshot.clone()),
            clear_overlay,
            before,
            frontier,
            generation,
        })
    }

    /// Commit a staged root unit (§7.2 `staged → locally_committed →
    /// reconciled`): recheck the scope, the inbound block, the policy
    /// generation and the overlay the candidate was built from; let the
    /// unit's compare-and-set revision refuse a unit that moved; then
    /// publish the unit, the projections a changed placement or lifecycle
    /// implies and the receipt in one catalog transaction. A pending stage
    /// writes only the `staged` receipt. The same frontier twice is
    /// idempotent: nothing is rewritten, the receipt only advances.
    pub fn commit_staged_root_unit(
        &mut self,
        relationship: Uuid,
        staged: StagedRootUnit,
        advertised: Option<&VersionVector>,
    ) -> Result<RootJoinOutcome> {
        let scope = self.require_peer_scope(relationship)?;
        let unit = staged.unit;
        self.refuse_blocked_inbound(transport_subject(unit))?;
        if self.policy_generation() != staged.generation {
            return Err(policy_changed());
        }
        let overlay_now = self
            .catalog
            .root_catalog_overlay(scope, unit)?
            .map(|overlay| overlay.private_snapshot);
        if overlay_now != staged.overlay_before {
            return Err(Error::new(
                Code::IdempotencyConflict,
                "the private overlay changed while the candidate was staged",
            ));
        }
        let receipt_unit = JoinUnit::catalog(unit);
        let Some(candidate) = staged.candidate else {
            self.write_pending_receipt(relationship, scope, receipt_unit, &staged.before)?;
            tracing::info!(
                relationship = %relationship,
                unit = ?unit,
                "root_sync direction=peer_to_local phase=staged pending=missing_ancestors"
            );
            return Ok(RootJoinOutcome::Pending {
                frontier: staged.before,
            });
        };
        let frontier = staged.frontier;
        let reconciled = advertised.is_none_or(|advertised| frontier.includes_vv(advertised));
        let phase = if reconciled {
            JoinPhase::Reconciled
        } else {
            JoinPhase::LocallyCommitted
        };
        let receipt = self.receipt(relationship, scope, receipt_unit, phase, &frontier)?;
        let unchanged = frontier == staged.before && !staged.clear_overlay;
        if unchanged {
            // The same frontier again, or frames this unit already had. The
            // unit is not rewritten; only the receipt may advance, and one
            // already there is not written again.
            self.advance_receipt(relationship, receipt_unit, &receipt)?;
            return Ok(RootJoinOutcome::Applied {
                frontier,
                reconciled,
                placement_changed: false,
                lifecycle_changed: false,
            });
        }
        let write = RootUnitWrite::Durable {
            commit: RootCatalogUnitCommit {
                unit,
                snapshot: candidate.clone(),
                expected_revision: staged.expected_revision,
            },
            clear_overlay: staged.clear_overlay,
        };
        let state = vec![(
            join_receipt_key(relationship, receipt_unit),
            Some(json(&receipt)?),
        )];
        let (placement_changed, lifecycle_changed) = match unit {
            CatalogUnitRef::Workspace { workspace } => {
                self.commit_workspace_root_import(scope, workspace, &candidate, write, state)?
            }
            CatalogUnitRef::Profile { profile } => {
                let changed =
                    self.commit_profile_root_import(scope, profile, &candidate, write, state)?;
                (false, changed)
            }
        };
        if placement_changed {
            // A placement that arrived from elsewhere moves this device's
            // ceiling for the Workspace exactly as a local move does.
            self.transport.invalidate();
        }
        tracing::info!(
            relationship = %relationship,
            unit = ?unit,
            phase = ?phase,
            placement_changed,
            lifecycle_changed,
            "root_sync direction=peer_to_local action=unit_committed"
        );
        Ok(RootJoinOutcome::Applied {
            frontier,
            reconciled,
            placement_changed,
            lifecycle_changed,
        })
    }

    /// The Workspace half of a commit: the effective unit before and after,
    /// and the one-way projection the difference implies on a `workspaces`
    /// row this device holds. No row yet means no projection: provisioning
    /// builds the row from the unit. A placement naming a Profile this
    /// device does not hold, holds hidden, or reads as a Holder is pending
    /// (§7.2 "show pending/recovery as applicable") and leaves the row's
    /// owner as it is; a lifecycle change on a row whose owner is hidden is
    /// deferred the same way, since the unit is what every reader asks
    /// first.
    fn commit_workspace_root_import(
        &mut self,
        scope: UserScopeId,
        workspace: WorkspaceId,
        candidate: &[u8],
        write: RootUnitWrite,
        state: Vec<(String, Option<String>)>,
    ) -> Result<(bool, bool)> {
        let unit = CatalogUnitRef::Workspace { workspace };
        let before = self.effective_workspace_unit(workspace)?;
        let after = WorkspaceCatalogUnit::from_snapshot(candidate, workspace, self.peer_id)?;
        let placement_before = before
            .as_ref()
            .and_then(WorkspaceCatalogUnit::placement)
            .map(|placement| self.holder_reads_unassigned(placement))
            .transpose()?;
        let placement_after = after
            .placement()
            .map(|placement| self.holder_reads_unassigned(placement))
            .transpose()?;
        let deleted_before = before.as_ref().and_then(earliest_active_deletion);
        let deleted_after = earliest_active_deletion(&after);
        let placement_changed = placement_before != placement_after;
        let lifecycle_changed = deleted_before.is_some() != deleted_after.is_some();

        let row = self.workspace_including_deleted(workspace).ok();
        let mut projected = row.clone();
        let mut projection = None;
        let mut documents = Vec::new();
        if let (Some(row), Some(projected)) = (&row, projected.as_mut()) {
            if placement_changed
                && let Some(Placement {
                    profile: Some(profile),
                    ..
                }) = placement_after
                && profile != row.owner
            {
                if self.catalog.profile_catalog_state(profile)? == Some(ProfileCatalogState::Active)
                {
                    projected.owner = profile;
                    projected.position = self.next_workspace_position(profile)?;
                } else {
                    tracing::info!(
                        workspace = %workspace,
                        "root_sync action=placement_pending reason=profile_not_held"
                    );
                }
            }
            // A `workspaces` row can only be written while its owner Profile
            // is visible (`RootProjection`'s ordering rule); `projected.owner`
            // is the row's owner or the active Profile chosen just above.
            let projectable = self.catalog.profile_catalog_state(projected.owner)?
                == Some(ProfileCatalogState::Active);
            if projectable {
                projection = match (deleted_before.is_some(), deleted_after) {
                    (false, Some(at)) => {
                        projected.deleted_at = Some(at);
                        Some(ImportProjection::Deleted)
                    }
                    (true, None) => {
                        projected.deleted_at = None;
                        documents = self.workspace_documents(workspace)?;
                        Some(ImportProjection::Restored)
                    }
                    _ => (projected.owner != row.owner).then_some(ImportProjection::Placement),
                };
            } else if lifecycle_changed || placement_changed {
                tracing::info!(
                    workspace = %workspace,
                    "root_sync action=projection_deferred reason=owner_hidden"
                );
            }
        }
        let projections: Vec<RootProjection<'_>> = match (projection, projected.as_ref()) {
            (Some(ImportProjection::Deleted), Some(projected)) => {
                vec![RootProjection::WorkspaceDeleted(projected)]
            }
            (Some(ImportProjection::Restored), Some(projected)) => {
                vec![RootProjection::WorkspaceRestored {
                    workspace: projected,
                    documents: &documents,
                }]
            }
            (Some(ImportProjection::Placement), Some(projected)) => {
                vec![RootProjection::Placement(projected)]
            }
            _ => Vec::new(),
        };
        self.catalog.publish_root(
            scope,
            &RootPublication {
                units: vec![(unit, write)],
                projections,
                state,
            },
        )?;
        if projection == Some(ImportProjection::Deleted) {
            // A durably deleted Workspace is usable by no writer here.
            self.open.remove(&workspace);
        }
        Ok((placement_changed, lifecycle_changed))
    }

    /// The Profile half of a commit: a deletion that became active hides the
    /// row through the shared visibility projection; one that was restored
    /// shows it again. A device-local removal (`removed_at` with a live
    /// unit) is untouched, because the diff is on the unit's facts and not
    /// on the column (§2.1).
    fn commit_profile_root_import(
        &mut self,
        scope: UserScopeId,
        profile: ProfileId,
        candidate: &[u8],
        write: RootUnitWrite,
        state: Vec<(String, Option<String>)>,
    ) -> Result<bool> {
        let unit = CatalogUnitRef::Profile { profile };
        let before = self.effective_profile_unit(profile)?;
        let after = ProfileCatalogUnit::from_snapshot(candidate, profile, self.peer_id)?;
        let deleted_before = before.as_ref().is_some_and(ProfileCatalogUnit::is_deleted);
        let deleted_after = after.is_deleted();
        let changed = deleted_before != deleted_after;
        let row_present = self.catalog.profile_including_removed(profile)?.is_some();
        let mut projections = Vec::new();
        if changed && row_present && !self.profile_is_holder(profile)? {
            if deleted_after {
                let deletions = after.deletions();
                let at = after
                    .active_deletions()
                    .into_iter()
                    .filter_map(|id| deletions.get(&id).map(|record| record.at_ms))
                    .min()
                    .map(Timestamp::from_millis)
                    .unwrap_or_else(Timestamp::now);
                projections.push(RootProjection::ProfileHidden { profile, at });
            } else {
                projections.push(RootProjection::ProfileVisible { profile });
            }
        }
        self.catalog.publish_root(
            scope,
            &RootPublication {
                units: vec![(unit, write)],
                projections,
                state,
            },
        )?;
        Ok(changed)
    }

    /// Write `receipt` unless one at least as far along for the same
    /// frontier is already there, or one with a frontier beyond it is — an
    /// older frontier never regresses a receipt.
    fn advance_receipt(
        &mut self,
        relationship: Uuid,
        unit: JoinUnit,
        receipt: &RootJoinReceipt,
    ) -> Result<()> {
        let frontier = receipt.frontier_vv();
        if let Some(held) = self.join_receipt(relationship, unit)? {
            let held_frontier = held.frontier_vv();
            if held_frontier == frontier && held.phase >= receipt.phase {
                return Ok(());
            }
            if held_frontier != frontier && held_frontier.includes_vv(&frontier) {
                return Ok(());
            }
        }
        self.catalog
            .set_state(&join_receipt_key(relationship, unit), &json(receipt)?)
    }

    fn receipt(
        &self,
        relationship: Uuid,
        scope: UserScopeId,
        unit: JoinUnit,
        phase: JoinPhase,
        frontier: &VersionVector,
    ) -> Result<RootJoinReceipt> {
        Ok(RootJoinReceipt {
            relationship,
            scope,
            unit,
            phase,
            frontier: frontier.encode(),
            operation_id: Uuid::new_v4(),
            at_ms: Timestamp::now().millis(),
        })
    }

    fn write_pending_receipt(
        &mut self,
        relationship: Uuid,
        scope: UserScopeId,
        unit: JoinUnit,
        frontier: &VersionVector,
    ) -> Result<()> {
        if let Some(held) = self.join_receipt(relationship, unit)? {
            // A pending frame after a committed one: the receipt stays where
            // it is. The frontier it carries is still what to ask from.
            if held.phase >= JoinPhase::LocallyCommitted || held.frontier_vv() == *frontier {
                return Ok(());
            }
        }
        let receipt = self.receipt(relationship, scope, unit, JoinPhase::Staged, frontier)?;
        self.catalog
            .set_state(&join_receipt_key(relationship, unit), &json(&receipt)?)
    }

    // --------------------------------------------------- personal v2 frames

    /// Stage a peer's frames for the personal v2 unit of `workspace` under
    /// this scope (§5.1, §7.2): the held unit forked and the candidate
    /// validated as the Workspace's v2 unit; a first arrival imported into
    /// an empty document under a fresh persisted writer. The Workspace row
    /// must exist — personal rows reference it — so the sync pass pulls
    /// personal units after content bootstrap, as it did for v1.
    pub fn stage_personal_unit(
        &self,
        relationship: Uuid,
        workspace: WorkspaceId,
        frames: &[Vec<u8>],
    ) -> Result<StagedPersonalUnit> {
        let scope = self.require_peer_scope(relationship)?;
        self.refuse_blocked_inbound(TransportSubject::Workspace(workspace))?;
        if self.workspace_including_deleted(workspace).is_err() {
            return Err(Error::bare(Code::WorkspaceNotFound));
        }
        let generation = self.policy_generation();
        let held = self.catalog.workspace_personal_unit(scope, workspace)?;
        let held_doc = held
            .as_ref()
            .map(|state| Self::open_personal(workspace, state))
            .transpose()?;
        let before = held_doc
            .as_ref()
            .map(PersonalManifest::version)
            .unwrap_or_default();
        let staged = match &held_doc {
            Some(doc) => doc.stage_v2(frames)?,
            None => PersonalManifest::v2_from_frames(frames, workspace, mint_peer_id())?,
        };
        let Staged::Applied(candidate) = staged else {
            return Ok(StagedPersonalUnit {
                workspace,
                candidate: None,
                writer_peer: held.as_ref().map_or(0, |state| state.writer_peer_id),
                held_before: held.map(|state| state.snapshot),
                before: before.clone(),
                frontier: before,
                generation,
                due_rows: Vec::new(),
                changes: Vec::new(),
            });
        };
        if candidate.is_read_only() {
            return Err(Error::bare(Code::SchemaTooNew));
        }
        let old_rows = held_doc
            .as_ref()
            .map(|doc| doc.reminders())
            .unwrap_or_default()
            .into_iter()
            .map(|row| (row.rule.id, row))
            .collect();
        let old_actions = held_doc
            .as_ref()
            .map(|doc| doc.raw_reminder_actions())
            .unwrap_or_default();
        let old_cleared = held_doc
            .as_ref()
            .map(|doc| doc.cleared_reminder_actions())
            .unwrap_or_default();
        let new_rows = candidate
            .reminders()
            .into_iter()
            .map(|row| (row.rule.id, row))
            .collect();
        let changes = classify_reminder_changes(
            &old_rows,
            &new_rows,
            &old_actions,
            &candidate.raw_reminder_actions(),
            &old_cleared,
            &candidate.cleared_reminder_actions(),
            &ReminderSettings::default(),
            &ReminderSettings::default(),
        );
        let due_rows = candidate
            .reminder_due_hints()
            .into_iter()
            .map(|(reminder, due_at_ms)| WorkspaceReminderDue {
                workspace,
                reminder,
                due_at_ms,
            })
            .collect();
        Ok(StagedPersonalUnit {
            workspace,
            frontier: candidate.version(),
            candidate: Some(candidate.export_snapshot()?),
            writer_peer: candidate.writer_peer(),
            held_before: held.map(|state| state.snapshot),
            before,
            generation,
            due_rows,
            changes,
        })
    }

    /// Commit a staged personal unit: recheck scope, inbound block, policy
    /// generation and that the held row is the one staged against, then
    /// commit the unit, rebuild its due index from the staged rows under
    /// the compare-and-set and write the receipt. Reminder wakes name the
    /// Workspace's current placement, when it has one.
    pub fn commit_staged_personal_unit(
        &mut self,
        relationship: Uuid,
        staged: StagedPersonalUnit,
        advertised: Option<&VersionVector>,
    ) -> Result<RootJoinOutcome> {
        let scope = self.require_peer_scope(relationship)?;
        let workspace = staged.workspace;
        self.refuse_blocked_inbound(TransportSubject::Workspace(workspace))?;
        if self.policy_generation() != staged.generation {
            return Err(policy_changed());
        }
        let held_now = self
            .catalog
            .workspace_personal_unit(scope, workspace)?
            .map(|state| state.snapshot);
        if held_now != staged.held_before {
            return Err(Error::new(
                Code::IdempotencyConflict,
                "the personal unit changed while the candidate was staged",
            ));
        }
        let receipt_unit = JoinUnit::PersonalV2 { workspace };
        let Some(candidate) = staged.candidate else {
            self.write_pending_receipt(relationship, scope, receipt_unit, &staged.before)?;
            return Ok(RootJoinOutcome::Pending {
                frontier: staged.before,
            });
        };
        let frontier = staged.frontier;
        let reconciled = advertised.is_none_or(|advertised| frontier.includes_vv(advertised));
        let phase = if reconciled {
            JoinPhase::Reconciled
        } else {
            JoinPhase::LocallyCommitted
        };
        let receipt = self.receipt(relationship, scope, receipt_unit, phase, &frontier)?;
        if frontier != staged.before || staged.held_before.is_none() {
            self.catalog.commit_workspace_personal_unit(
                scope,
                workspace,
                &candidate,
                staged.writer_peer,
            )?;
            // The index is disposable; a stale compare-and-set here only
            // means the reconciliation pass rebuilds it later.
            let _ = self.catalog.replace_workspace_due_index(
                scope,
                workspace,
                &candidate,
                &frontier.encode(),
                &staged.due_rows,
            )?;
            if let Some(profile) = self.current_placement_profile(workspace)? {
                for (reminder, change) in staged.changes {
                    self.record_reminder_change(profile, reminder, change);
                }
            }
        }
        self.advance_receipt(relationship, receipt_unit, &receipt)?;
        tracing::info!(
            relationship = %relationship,
            workspace = %workspace,
            phase = ?phase,
            "root_sync direction=peer_to_local action=personal_committed"
        );
        Ok(RootJoinOutcome::Applied {
            frontier,
            reconciled,
            placement_changed: false,
            lifecycle_changed: false,
        })
    }

    // ---------------------------------------------------- publication job

    /// **The publication job** (§4.3, §4.4; Plan 1.2's carried-forward
    /// item): every pending private overlay whose subject's admission now
    /// allows export is published into its durable unit. Idempotent — an
    /// overlay that may still not export is left exactly as it is — and a
    /// failure is logged by address and left for the next pass; one held
    /// subject never stops the others. Run at the start of every sync pass,
    /// before the inventory is framed.
    pub fn publish_pending_overlays(&mut self) -> Result<PublishedOverlays> {
        let scope = self.catalog.local_user_scope()?;
        let mut outcome = PublishedOverlays::default();
        for (unit, _) in self.catalog.root_catalog_overlays(scope)? {
            match self.publish_pending_overlay(unit) {
                Ok(true) => outcome.published += 1,
                Ok(false) => outcome.held += 1,
                Err(error) => {
                    outcome.held += 1;
                    tracing::warn!(
                        unit = ?unit,
                        error_code = error.code().as_str(),
                        "root_sync action=overlay_publication_failed; left for the next pass"
                    );
                }
            }
        }
        Ok(outcome)
    }
}

fn json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|e| Error::new(Code::Internal, format!("receipt is not serializable: {e}")))
}

#[cfg(test)]
mod tests;
