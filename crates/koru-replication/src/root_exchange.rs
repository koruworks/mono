// SPDX-License-Identifier: MPL-2.0

//! The adapter half of peer protocol 6, shared by every Store that answers
//! a peer: the desktop daemon's `LiveReplicationStore` and the phone's
//! `MobileReplicationStore` call these over their own lock and add nothing
//! of their own, so there is one place where a grant is read off a peer,
//! one place where a Store refusal becomes a wire code, and one provisioning
//! decision (`docs/features/root-catalog/TECHNICAL.final.md` §4.1, §4.4,
//! §7.2).
//!
//! Three lock shapes, named so an adapter cannot get them wrong:
//!
//! - **under the lock** — `bind_root_scope`, `root_catalog`, `root_unit_version`,
//!   `prepare_root_export`, `stage_root_unit`, `commit_root_unit`,
//!   `authorize_workspace`, `admit_peer_subject`, `provision_replica`;
//! - **off the lock** — [`derive_root_export`]: the export boundary's work,
//!   on bytes the prepare step copied out;
//! - **under the lock again, right before the send** — [`recheck_root_export`]:
//!   the policy generation and the peer's standing, so a late callback
//!   discards its payload rather than sending under a policy nobody holds.
//!
//! Every log line names ids and counts; nothing here prints a name, a
//! snapshot or a hidden subject.

use koru_store_core::errors::Code;
use koru_store_core::ports::Catalog;
use koru_store_core::root_catalog::{Derived, ExportSubject, Publication};
use koru_store_core::service::{
    Backend, RootExportSource, RootGrant, RootJoinOutcome, StagedPersonalUnit, StagedRootUnit,
    Store,
};
use koru_store_core::transport::{TransportPath, TransportSubject};
use koru_store_core::{ProfileId, ProvisionedReplica, ReplicaPlacement, WorkspaceId};

use crate::protocol::{
    AuthenticatedPeer, CatalogUnitRef, MODE_REFUSES_TRANSPORT, POLICY_CHANGED, PersonalUnitState,
    ROOT_UNIT_HELD, ROOT_UNIT_NOT_FOUND, ReplicaDescriptor, RootCatalogInventory, RootJoin,
    RootSubjectAdmission, RootUnitState, StoreFailure,
};

/// The grant a peer carries, as the Store's eligibility rule reads it.
pub fn grant(peer: &AuthenticatedPeer) -> RootGrant<'_> {
    RootGrant {
        all_data: peer.all_data,
        profiles: &peer.authorized_profiles,
        workspaces: &peer.authorized_workspaces,
    }
}

fn map_error(error: koru_store_core::Error) -> StoreFailure {
    StoreFailure::new(error.code().as_str()).with_detail(error.message())
}

/// A Store refusal of a root or personal operation as the wire names it:
/// the mode closed the path, or the staged premises moved.
fn map_root_error(error: koru_store_core::Error) -> StoreFailure {
    match error.code() {
        Code::Forbidden => StoreFailure::new(MODE_REFUSES_TRANSPORT).with_detail(error.message()),
        Code::IdempotencyConflict => StoreFailure::new(POLICY_CHANGED).with_detail(error.message()),
        _ => map_error(error),
    }
}

fn to_root_join(outcome: RootJoinOutcome) -> RootJoin {
    match outcome {
        RootJoinOutcome::Applied {
            frontier,
            reconciled,
            ..
        } => RootJoin::Applied {
            frontier: frontier.encode(),
            reconciled,
        },
        RootJoinOutcome::Pending { frontier } => RootJoin::Pending {
            frontier: frontier.encode(),
        },
    }
}

/// Bind the relationship once (§4.1); `false` for a peer that is not the
/// same person's device, which the pass then asks no root unit of.
pub fn bind_root_scope<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    peer: &AuthenticatedPeer,
) -> Result<bool, StoreFailure> {
    if !peer.binds_root_scope() {
        return Ok(false);
    }
    peer.with_root_scope(|| {
        store
            .bind_peer_scope(peer.relationship_id.as_uuid())
            .map(|_| true)
            .map_err(map_root_error)
    })
}

/// The publication job, then the eligible inventory for `peer`.
pub fn root_catalog<C: Catalog, B: Backend>(
    store: &Store<C, B>,
    peer: &AuthenticatedPeer,
) -> Result<RootCatalogInventory, StoreFailure> {
    peer.with_catalog_scope(|| {
        let inventory = store
            .root_export_inventory(&grant(peer))
            .map_err(map_error)?;
        if inventory.held > 0 {
            tracing::info!(
                peer = %peer.device_id.to_storage(),
                held = inventory.held,
                "root_sync direction=local_to_peer action=units_held"
            );
        }
        Ok(RootCatalogInventory {
            units: inventory
                .units
                .into_iter()
                .map(|(unit, version)| RootUnitState {
                    unit,
                    version: version.encode(),
                })
                .collect(),
            personal: inventory
                .personal
                .into_iter()
                .map(|(workspace, version)| PersonalUnitState {
                    workspace,
                    version: version.encode(),
                })
                .collect(),
        })
    })
}

/// The frontier the next request for `unit` carries.
pub fn root_unit_version<C: Catalog, B: Backend>(
    store: &Store<C, B>,
    peer: &AuthenticatedPeer,
    unit: CatalogUnitRef,
) -> Result<Vec<u8>, StoreFailure> {
    peer.with_root_scope(|| {
        store
            .root_unit_frontier(unit)
            .map_err(map_root_error)?
            .map(|version| version.encode())
            .ok_or_else(|| StoreFailure::new(ROOT_UNIT_NOT_FOUND))
    })
}

pub fn personal_version<C: Catalog, B: Backend>(
    store: &Store<C, B>,
    peer: &AuthenticatedPeer,
    workspace: WorkspaceId,
) -> Result<Vec<u8>, StoreFailure> {
    peer.with_root_scope(|| {
        store
            .personal_frontier(workspace)
            .map_err(map_root_error)?
            .map(|version| version.encode())
            .ok_or_else(|| StoreFailure::new(ROOT_UNIT_NOT_FOUND))
    })
}

/// Step one of a send, under the lock: what the export boundary derives
/// from, for this recipient. A subject this device does not hold answers
/// [`ROOT_UNIT_NOT_FOUND`]; one the recipient may not know, or one that may
/// not leave now, answers [`ROOT_UNIT_HELD`] — the same word for both, so a
/// scoped peer learns nothing from the difference.
pub fn prepare_root_export<C: Catalog, B: Backend>(
    store: &Store<C, B>,
    peer: &AuthenticatedPeer,
    subject: ExportSubject,
) -> Result<RootExportSource, StoreFailure> {
    if matches!(subject, ExportSubject::PersonalV2(_)) {
        peer.check_root_scope()?;
    }
    peer.with_catalog_scope(|| match store.root_export_source(&grant(peer), subject) {
        Ok(Some(source)) => Ok(source),
        Ok(None) => Err(StoreFailure::new(ROOT_UNIT_NOT_FOUND)),
        Err(error) if error.code() == Code::Forbidden => {
            Err(StoreFailure::new(ROOT_UNIT_HELD).with_detail(error.message()))
        }
        Err(error) => Err(map_error(error)),
    })
}

/// Step two, off the lock: §4.3 steps 2–4. A hold is answered with
/// [`ROOT_UNIT_HELD`] and logged by subject; never with raw bytes.
pub fn derive_root_export(
    source: &RootExportSource,
    peer: &AuthenticatedPeer,
    recipient: Option<&loro::VersionVector>,
) -> Result<Publication, StoreFailure> {
    let authorization = peer.relationship_id.as_uuid().to_string();
    match source.derive(recipient, authorization) {
        Ok(Derived::Publish(publication)) => {
            if publication.redacted_ops > 0 {
                tracing::info!(
                    peer = %peer.device_id.to_storage(),
                    subject = ?source.subject,
                    redacted_ops = publication.redacted_ops,
                    "root_sync direction=local_to_peer action=history_redacted"
                );
            }
            Ok(publication)
        }
        Ok(Derived::Hold { reason }) => {
            tracing::info!(
                peer = %peer.device_id.to_storage(),
                subject = ?source.subject,
                reason = %reason,
                "root_sync direction=local_to_peer action=unit_held"
            );
            Err(StoreFailure::new(ROOT_UNIT_HELD))
        }
        Err(error) => Err(map_error(error)),
    }
}

/// Step three, under the lock, right before the send (§4.3 step 5, §4.4):
/// the policy generation the bytes were derived under and the recipient's
/// standing. A difference discards the payload.
pub fn recheck_root_export<C: Catalog, B: Backend>(
    store: &Store<C, B>,
    peer: &AuthenticatedPeer,
    publication: &Publication,
) -> Result<(), StoreFailure> {
    if store.policy_generation() != publication.capture.policy_generation
        || publication.capture.authorization != peer.relationship_id.as_uuid().to_string()
        || !peer.still_authorized()
    {
        tracing::info!(
            peer = %peer.device_id.to_storage(),
            "root_sync direction=local_to_peer action=payload_discarded reason=policy_changed"
        );
        return Err(StoreFailure::new(POLICY_CHANGED));
    }
    Ok(())
}

/// Receiving, step one under the lock: stage. The relationship must be
/// scope-bound; the Store refuses a known Local-only subject.
pub fn stage_root_unit<C: Catalog, B: Backend>(
    store: &Store<C, B>,
    peer: &AuthenticatedPeer,
    unit: CatalogUnitRef,
    frames: &[Vec<u8>],
) -> Result<StagedRootUnit, StoreFailure> {
    peer.check_root_scope()?;
    store
        .stage_root_unit(peer.relationship_id.as_uuid(), unit, frames)
        .map_err(map_root_error)
}

/// Receiving, step two under the lock: commit, with the Store's rechecks.
pub fn commit_root_unit<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    peer: &AuthenticatedPeer,
    staged: StagedRootUnit,
    advertised: Option<&loro::VersionVector>,
) -> Result<RootJoin, StoreFailure> {
    commit_root_unit_outcome(store, peer, staged, advertised).map(|(join, _)| join)
}

/// [`commit_root_unit`] keeping the Store's own outcome beside the wire
/// answer, for an adapter that announces what the commit changed (a Profile
/// deletion that arrived from another device is a `profile.deleted` event on
/// this device's bus, Plan 2.2).
pub fn commit_root_unit_outcome<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    peer: &AuthenticatedPeer,
    staged: StagedRootUnit,
    advertised: Option<&loro::VersionVector>,
) -> Result<(RootJoin, RootJoinOutcome), StoreFailure> {
    peer.with_root_scope(|| {
        store
            .commit_staged_root_unit(peer.relationship_id.as_uuid(), staged, advertised)
            .map(|outcome| (to_root_join(outcome.clone()), outcome))
            .map_err(map_root_error)
    })
}

/// **This device's reading of every subject it holds**, for
/// [`ReplicationStore::root_sync_admissions`](crate::protocol::ReplicationStore::root_sync_admissions):
/// the effective mode's answer for the peer path, the ceiling mismatch, and
/// whether a private overlay is still waiting. Active Profiles and live
/// Workspaces only; a bounded catalog read, never on a UI thread.
pub fn root_sync_admissions<C: Catalog, B: Backend>(
    store: &Store<C, B>,
) -> Result<Vec<RootSubjectAdmission>, StoreFailure> {
    let pending: std::collections::BTreeSet<CatalogUnitRef> = store
        .pending_overlay_units()
        .map_err(map_error)?
        .into_iter()
        .collect();
    let mut out = Vec::new();
    let mut push =
        |subject: CatalogUnitRef, transport: TransportSubject| -> Result<(), StoreFailure> {
            let admission = store.transport_admission(transport).map_err(map_error)?;
            out.push(RootSubjectAdmission {
                subject,
                allows_peer: admission.allows_peer(),
                ceiling_mismatch: admission.ceiling_mismatch,
                overlay_pending: pending.contains(&subject),
            });
            Ok(())
        };
    for profile in store.profiles().map_err(map_error)? {
        push(
            CatalogUnitRef::Profile {
                profile: profile.id,
            },
            TransportSubject::Profile(profile.id),
        )?;
    }
    for workspace in store.workspaces().map_err(map_error)? {
        push(
            CatalogUnitRef::Workspace {
                workspace: workspace.id,
            },
            TransportSubject::Workspace(workspace.id),
        )?;
    }
    Ok(out)
}

pub fn stage_personal_unit<C: Catalog, B: Backend>(
    store: &Store<C, B>,
    peer: &AuthenticatedPeer,
    workspace: WorkspaceId,
    frames: &[Vec<u8>],
) -> Result<StagedPersonalUnit, StoreFailure> {
    peer.check_root_scope()?;
    store
        .stage_personal_unit(peer.relationship_id.as_uuid(), workspace, frames)
        .map_err(map_root_error)
}

pub fn commit_personal_unit<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    peer: &AuthenticatedPeer,
    staged: StagedPersonalUnit,
    advertised: Option<&loro::VersionVector>,
) -> Result<RootJoin, StoreFailure> {
    peer.with_root_scope(|| {
        store
            .commit_staged_personal_unit(peer.relationship_id.as_uuid(), staged, advertised)
            .map(to_root_join)
            .map_err(map_root_error)
    })
}

/// The one Workspace authorization rule of §7.2: an exact Workspace grant
/// authorizes by UUID; a Profile grant is evaluated against the Workspace's
/// **current** placement, read from the root, so a move out of a granted
/// Profile closes the peer's access on this very check; all-data evaluates
/// the scope. A Workspace this device does not hold is hidden from a scoped
/// peer. The fence is rechecked in every case.
pub fn authorize_workspace<C: Catalog, B: Backend>(
    store: &Store<C, B>,
    peer: &AuthenticatedPeer,
    workspace: WorkspaceId,
) -> Result<(), StoreFailure> {
    let by_uuid = peer.all_data || peer.authorized_workspaces.contains(&workspace);
    if store.workspace_including_deleted(workspace).is_err() {
        // Hide existence only. Corruption and availability failures remain
        // actionable Store errors and must not look like denial.
        return Err(StoreFailure::new(if by_uuid {
            "workspace_not_found"
        } else {
            "unauthorized_workspace"
        }));
    }
    if !by_uuid {
        let placement = store
            .current_placement_profile(workspace)
            .map_err(map_error)?;
        if !placement.is_some_and(|profile| peer.authorized_profiles.contains(&profile)) {
            return Err(StoreFailure::new("unauthorized_workspace"));
        }
    }
    if !peer.still_authorized() {
        return Err(StoreFailure::new("relationship_revoked"));
    }
    Ok(())
}

/// §4.4's gate on a Workspace for the peer path: may anything about it be
/// listed, served, requested or admitted over a peer right now? The
/// Store's one question, with the blob contract's spelling for a closed
/// mode and the Store's own code for an inconsistent row.
pub fn admit_peer_subject<C: Catalog, B: Backend>(
    store: &Store<C, B>,
    workspace: WorkspaceId,
) -> Result<(), StoreFailure> {
    admit(store, TransportSubject::Workspace(workspace))
}

/// The same gate for a Profile's own units.
pub fn admit_peer_profile<C: Catalog, B: Backend>(
    store: &Store<C, B>,
    profile: ProfileId,
) -> Result<(), StoreFailure> {
    admit(store, TransportSubject::Profile(profile))
}

fn admit<C: Catalog, B: Backend>(
    store: &Store<C, B>,
    subject: TransportSubject,
) -> Result<(), StoreFailure> {
    store
        .admit_transport(subject, TransportPath::Peer)
        .map(|_| ())
        .map_err(|error| match error.code() {
            Code::Forbidden => StoreFailure::new(MODE_REFUSES_TRANSPORT),
            _ => map_error(error),
        })
}

/// **The provisioning decision** (§7.2 "Specific changes", the table in
/// `Store::provision_replica_placed`), with the peer's grant on top:
///
/// | peer | unit known here | rule handed to the Store |
/// | --- | --- | --- |
/// | all-data (scope-bound) | any | `KnownOnly`: root units arrive before inventories; no unit yet is pending, never the descriptor's Profile |
/// | Profile grant | yes | placement must be a granted Profile, else `unauthorized_workspace`; the unit places it |
/// | Profile grant | no | `Hint(descriptor.profile_id)` only when that Profile is granted — this device's own Profile, authorized by the grant — else `unauthorized_workspace` |
/// | Workspace grant only | any | `KnownOnly`: content under this device's already-known placement, or pending; the sender's Profile is never installed |
///
/// A scoped descriptor must carry no Profile metadata; an all-data one
/// carries the Profile's identity for the immutable-creation guard the
/// Store keeps for a Profile it really provisions.
pub fn provision_replica<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
    peer: &AuthenticatedPeer,
    replica: &ReplicaDescriptor,
) -> Result<ProvisionedReplica, StoreFailure> {
    let workspace = replica.workspace_id;
    let hint = replica.profile_id;
    let by_uuid = peer.all_data || peer.authorized_workspaces.contains(&workspace);
    let hint_granted = peer.authorized_profiles.contains(&hint);
    if !by_uuid && !hint_granted {
        return Err(StoreFailure::new("unauthorized_workspace"));
    }
    let profile = if peer.all_data {
        let locale = koru_store_core::text::Locale::parse(&replica.profile_locale)
            .ok_or_else(|| StoreFailure::new("invalid_locale"))?;
        Some(koru_store_core::model::Profile {
            id: hint,
            name: replica.profile_name.clone(),
            purpose: None,
            avatar: None,
            icon: replica.profile_icon.clone(),
            color: replica.profile_color.clone(),
            account: None,
            locale,
            created_at: koru_store_core::model::Timestamp::from_millis(
                replica.profile_created_at_ms,
            ),
            kind: koru_store_core::model::ProfileKind::parse(&replica.profile_kind),
        })
    } else {
        if replica.has_profile_metadata() {
            return Err(StoreFailure::new("scoped_profile_metadata"));
        }
        None
    };
    let known_placement = store
        .current_placement_profile(workspace)
        .map_err(map_error)?;
    let rule = if peer.all_data {
        ReplicaPlacement::KnownOnly
    } else if store.workspace_including_deleted(workspace).is_ok()
        || store
            .root_placement(workspace)
            .map_err(map_error)?
            .is_some()
    {
        // Known here: a Profile-granted peer is admitted by the current
        // placement, a UUID-granted one by the UUID; the descriptor's
        // Profile plays no part.
        if !by_uuid
            && !known_placement.is_some_and(|profile| peer.authorized_profiles.contains(&profile))
        {
            return Err(StoreFailure::new("unauthorized_workspace"));
        }
        ReplicaPlacement::KnownOnly
    } else if hint_granted {
        ReplicaPlacement::Hint(hint)
    } else {
        ReplicaPlacement::KnownOnly
    };
    let record = koru_store_core::model::Workspace {
        id: workspace,
        owner: hint,
        name: replica.workspace_name.clone(),
        created_at: koru_store_core::model::Timestamp::from_millis(replica.workspace_created_at_ms),
        last_opened_at: None,
        icon: replica.workspace_icon.clone(),
        color: replica.workspace_color.clone(),
        pinned: replica.workspace_pinned,
        archived: replica.workspace_archived,
        deleted_at: replica
            .workspace_deleted_at_ms
            .map(koru_store_core::model::Timestamp::from_millis),
        position: 0,
    };
    let fenced = if by_uuid {
        peer.with_authorized_workspace(workspace, || Ok(()))
    } else {
        peer.with_authorized_profile(hint, || Ok(()))
    };
    fenced?;
    store
        .provision_replica_placed(
            profile.as_ref(),
            &record,
            replica.manifest_document,
            rule,
            koru_store_core::model::Timestamp::now(),
        )
        .map_err(map_error)
}

/// The publication job, for the sync pass's start. Failures are logged by
/// the Store per unit and counted here; nothing stops the pass.
pub fn publish_pending_root_overlays<C: Catalog, B: Backend>(
    store: &mut Store<C, B>,
) -> Result<usize, StoreFailure> {
    let outcome = store.publish_pending_overlays().map_err(map_error)?;
    if outcome.published > 0 || outcome.held > 0 {
        tracing::info!(
            published = outcome.published,
            held = outcome.held,
            "root_sync action=publication_job"
        );
    }
    Ok(outcome.published)
}
