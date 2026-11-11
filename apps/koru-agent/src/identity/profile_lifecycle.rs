// SPDX-License-Identifier: MPL-2.0

//! Ordered publication and receipt of hosted Profile lifecycle trust facts.
//!
//! Hosted commit is already durable when this module is called. It snapshots
//! the peers authorized *before* the tombstone, persists those targets, stores
//! the signed fact, queues delivery, and only then asks sync control to replace
//! authorization. A restart reuses both the operation and target set.

use std::str::FromStr;

use koru_account_core::{
    ProfileLifecycleMutationId, ProfileLifecycleRevision, ProfileLifecycleState,
};
use koru_agent_wire::{
    AccountProfileLifecycleRow, AccountProfileLifecycleState, ProfileCleanupStage,
};
use koru_device::{ApplyOutcome, TrustRecord, TrustRepository as _};

use crate::account::lifecycle::LifecycleRepository;
use crate::{Paths, service::SyncControl};

const MAX_LIFECYCLE_PEERS: usize = 64;

pub(crate) trait LifecycleFanout {
    fn send(&self, peer: koru_device::DeviceId, record: TrustRecord);
    fn refresh_authorization(&self);
}

impl LifecycleFanout for SyncControl {
    fn send(&self, peer: koru_device::DeviceId, record: TrustRecord) {
        self.send_trust_record(peer, record);
    }

    fn refresh_authorization(&self) {
        self.refresh_authorization();
    }
}

pub(crate) fn publish_deleted<F: LifecycleFanout + ?Sized>(
    paths: &Paths,
    descriptor: &koru_device::DeviceDescriptorV1,
    signing_key: &koru_device::DeviceSigningKey,
    control: &F,
    lifecycle: &AccountProfileLifecycleRow,
) -> Result<(), &'static str> {
    let parsed = ParsedDeleted::from_wire(lifecycle)?;
    let mut cleanup =
        LifecycleRepository::open(&paths.profile_lifecycle_db()).map_err(|error| error.code)?;
    let pending = cleanup
        .get(parsed.profile)
        .map_err(|error| error.code)?
        .ok_or("profile_lifecycle_state_corrupt")?;
    if pending.operation != parsed.operation
        || pending.lifecycle_revision != parsed.revision
        || pending.stage < ProfileCleanupStage::HostedCommitted
    {
        return Err("profile_lifecycle_state_corrupt");
    }

    let mut trust =
        super::SqliteTrustRepository::open(&paths.identity_db()).map_err(|error| error.code)?;
    let before = trust.trust_view().map_err(|error| error.code)?;
    let targets = if pending.peer_targets.is_empty() {
        let targets = koru_device::Authorization::devices(&before)
            .into_iter()
            .filter(|peer| peer.all_data || peer.authorized_profiles.contains(&parsed.profile))
            .map(|peer| peer.device_id)
            .take(65)
            .collect::<Vec<_>>();
        cleanup
            .set_peer_targets(parsed.profile, parsed.operation, &targets, now_millis())
            .map_err(|error| error.code)?;
        targets
    } else {
        pending.peer_targets
    };

    let record = before
        .lifecycles
        .get(&parsed.profile)
        .filter(|record| {
            record.lifecycle_revision == parsed.revision.get()
                && record.operation_id == parsed.operation.as_uuid()
                && record.state == koru_device::ProfileLifecycleStateV1::Deleted
        })
        .cloned()
        .unwrap_or_else(|| {
            koru_device::ProfileLifecycleV1::sign(
                koru_device::RecordId::new(),
                descriptor.device_id,
                parsed.profile,
                koru_device::ProfileLifecycleStateV1::Deleted,
                parsed.revision.get(),
                parsed.operation.as_uuid(),
                now_millis(),
                signing_key,
            )
        });
    let verified = record
        .clone()
        .verify(descriptor.signing_public_key)
        .map_err(|_| "invalid_profile_lifecycle")?;
    match trust.apply(verified).map_err(|error| error.code)? {
        ApplyOutcome::Applied { .. } | ApplyOutcome::AlreadyApplied { .. } => {}
        ApplyOutcome::Stale { .. } => return Err("stale_profile_lifecycle"),
    }

    for peer in targets {
        control.send(peer, TrustRecord::ProfileLifecycle(record.clone()));
    }
    // The command queue preserves this order after every SendTrustRecord.
    control.refresh_authorization();
    Ok(())
}

pub(crate) fn publish_active<F: LifecycleFanout + ?Sized>(
    paths: &Paths,
    descriptor: &koru_device::DeviceDescriptorV1,
    signing_key: &koru_device::DeviceSigningKey,
    control: &F,
    lifecycle: &AccountProfileLifecycleRow,
) -> Result<(), &'static str> {
    if lifecycle.lifecycle_state != AccountProfileLifecycleState::Active
        || lifecycle.lifecycle_revision == 0
    {
        return Err("invalid_profile_lifecycle");
    }
    let profile = koru_store_core::ProfileId::parse(&lifecycle.profile_id)
        .ok_or("invalid_profile_lifecycle")?;
    let operation: ProfileLifecycleMutationId = lifecycle
        .operation_id
        .as_deref()
        .ok_or("invalid_profile_lifecycle")?
        .parse()
        .map_err(|_| "invalid_profile_lifecycle")?;
    let mut trust =
        super::SqliteTrustRepository::open(&paths.identity_db()).map_err(|error| error.code)?;
    let before = trust.trust_view().map_err(|error| error.code)?;
    let targets = before
        .devices
        .iter()
        .map(|peer| peer.device_id)
        .take(MAX_LIFECYCLE_PEERS)
        .collect::<Vec<_>>();
    let record = koru_device::ProfileLifecycleV1::sign(
        koru_device::RecordId::new(),
        descriptor.device_id,
        profile,
        koru_device::ProfileLifecycleStateV1::Active,
        lifecycle.lifecycle_revision,
        operation.as_uuid(),
        now_millis(),
        signing_key,
    );
    let verified = record
        .clone()
        .verify(descriptor.signing_public_key)
        .map_err(|_| "invalid_profile_lifecycle")?;
    match trust.apply(verified).map_err(|error| error.code)? {
        ApplyOutcome::Applied { .. } | ApplyOutcome::AlreadyApplied { .. } => {}
        ApplyOutcome::Stale { .. } => return Err("stale_profile_lifecycle"),
    }
    LifecycleRepository::open(&paths.profile_lifecycle_db())
        .map_err(|error| error.code)?
        .observe_trust_active(
            profile,
            ProfileLifecycleRevision::new(lifecycle.lifecycle_revision),
        )
        .map_err(|error| error.code)?;
    for peer in targets {
        control.send(peer, TrustRecord::ProfileLifecycle(record.clone()));
    }
    control.refresh_authorization();
    Ok(())
}

/// Record a lifecycle fact a trusted peer sent, and say whether it leaves
/// this device owing a cleanup.
///
/// `true` only for a deletion whose journal row has not settled. The same
/// signed tombstone arrives again with every device-list exchange for as long
/// as the peers hold it, and answering "deleted" to each of those is what
/// made a finished cleanup start over.
pub(crate) fn observe_received(
    paths: &Paths,
    lifecycle: &koru_device::ProfileLifecycleV1,
) -> Result<bool, &'static str> {
    if lifecycle.state == koru_device::ProfileLifecycleStateV1::Active {
        return LifecycleRepository::open(&paths.profile_lifecycle_db())
            .map_err(|error| error.code)?
            .observe_trust_active(
                lifecycle.profile_id,
                ProfileLifecycleRevision::new(lifecycle.lifecycle_revision),
            )
            .map(|()| false)
            .map_err(|error| error.code);
    }
    let operation =
        ProfileLifecycleMutationId::from_str(&lifecycle.operation_id.hyphenated().to_string())
            .map_err(|_| "invalid_profile_lifecycle")?;
    LifecycleRepository::open(&paths.profile_lifecycle_db())
        .map_err(|error| error.code)?
        .observe_trust_deleted(
            lifecycle.profile_id,
            ProfileLifecycleRevision::new(lifecycle.lifecycle_revision),
            operation,
            now_millis(),
        )
        .map(|record| record.stage < ProfileCleanupStage::NavigationSettled)
        .map_err(|error| error.code)
}

/// Write one cleanup boundary the shell reports, and say whether the cleanup
/// has now settled. A Profile with no row has nothing left to acknowledge,
/// which is settled too.
pub(crate) fn acknowledge_cleanup(
    paths: &Paths,
    profile_id: &str,
    stage: ProfileCleanupStage,
) -> Result<bool, &'static str> {
    let profile = koru_store_core::ProfileId::parse(profile_id).ok_or("invalid_id")?;
    let record = LifecycleRepository::open(&paths.profile_lifecycle_db())
        .map_err(|error| error.code)?
        .acknowledge(profile, stage, now_millis())
        .map_err(|error| error.code)?;
    Ok(record.is_none_or(|record| record.stage >= ProfileCleanupStage::NavigationSettled))
}

pub(crate) fn resume_pending<F: LifecycleFanout + ?Sized>(
    paths: &Paths,
    descriptor: &koru_device::DeviceDescriptorV1,
    signing_key: &koru_device::DeviceSigningKey,
    control: &F,
) -> Result<usize, &'static str> {
    let pending = LifecycleRepository::open(&paths.profile_lifecycle_db())
        .map_err(|error| error.code)?
        .pending()
        .map_err(|error| error.code)?;
    let mut resumed = 0;
    for record in pending {
        if record.lifecycle_state != ProfileLifecycleState::Deleted
            || record.stage < ProfileCleanupStage::HostedCommitted
        {
            continue;
        }
        publish_deleted(paths, descriptor, signing_key, control, &record.wire())?;
        resumed += 1;
    }
    Ok(resumed)
}

/// The content-free deletion markers a newly connected shell must resume.
///
/// This is read while Agent starts, before its event loop exists. `Hello` can
/// therefore replay the cached IDs without putting SQLite work on that loop.
pub(crate) fn pending_deleted_profiles(paths: &Paths) -> Result<Vec<String>, &'static str> {
    let pending = LifecycleRepository::open(&paths.profile_lifecycle_db())
        .map_err(|error| error.code)?
        .pending()
        .map_err(|error| error.code)?;
    Ok(pending
        .into_iter()
        .filter(|record| {
            record.lifecycle_state == ProfileLifecycleState::Deleted
                && record.stage >= ProfileCleanupStage::HostedCommitted
                && record.stage < ProfileCleanupStage::NavigationSettled
        })
        .map(|record| record.profile.to_storage())
        .collect())
}

struct ParsedDeleted {
    profile: koru_store_core::ProfileId,
    revision: ProfileLifecycleRevision,
    operation: ProfileLifecycleMutationId,
}

impl ParsedDeleted {
    fn from_wire(value: &AccountProfileLifecycleRow) -> Result<Self, &'static str> {
        if value.lifecycle_state != AccountProfileLifecycleState::Deleted
            || value.lifecycle_revision == 0
        {
            return Err("invalid_profile_lifecycle");
        }
        Ok(Self {
            profile: koru_store_core::ProfileId::parse(&value.profile_id)
                .ok_or("invalid_profile_lifecycle")?,
            revision: ProfileLifecycleRevision::new(value.lifecycle_revision),
            operation: value
                .operation_id
                .as_deref()
                .ok_or("invalid_profile_lifecycle")?
                .parse()
                .map_err(|_| "invalid_profile_lifecycle")?,
        })
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use koru_account_core::{ProfileLifecycleMutationId, ProfileLifecycleRevision};
    use koru_device::{
        AllDataGrantV1, DeviceDescriptorV1, DeviceId, DeviceSigningKey, RecordId, RelationshipId,
        ReplicationEndpointSecret, TrustRecord, TrustRelationshipV1,
    };

    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Action {
        Send(DeviceId, TrustRecord),
        Refresh,
    }

    #[derive(Default)]
    struct FakeFanout(RefCell<Vec<Action>>);

    impl LifecycleFanout for FakeFanout {
        fn send(&self, peer: DeviceId, record: TrustRecord) {
            self.0.borrow_mut().push(Action::Send(peer, record));
        }

        fn refresh_authorization(&self) {
            self.0.borrow_mut().push(Action::Refresh);
        }
    }

    #[test]
    fn publication_persists_targets_and_record_before_refresh_and_replays_them() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_root(root.path().to_owned());
        let local = crate::identity::IdentityManager::open(&paths, 1).unwrap();
        let peer_key = DeviceSigningKey::generate();
        let peer_endpoint = ReplicationEndpointSecret::generate();
        let peer = DeviceDescriptorV1::new(
            DeviceId::new(),
            "Peer".into(),
            &peer_key,
            peer_endpoint.endpoint_id(),
            2,
        );
        let relationship_id = RelationshipId::new();
        let relationship = TrustRelationshipV1::sign(
            relationship_id,
            local.descriptor.clone(),
            peer.clone(),
            [7; 32],
            3,
            &local.signing_key,
            &peer_key,
        );
        let local_grant = AllDataGrantV1::sign(
            relationship_id,
            peer.device_id,
            local.descriptor.device_id,
            RecordId::new(),
            4,
            &local.signing_key,
        );
        let peer_grant = AllDataGrantV1::sign(
            relationship_id,
            local.descriptor.device_id,
            peer.device_id,
            RecordId::new(),
            4,
            &peer_key,
        );
        let mut trust = crate::identity::SqliteTrustRepository::open(&paths.identity_db()).unwrap();
        trust.apply(relationship.verify().unwrap()).unwrap();
        trust
            .apply(
                local_grant
                    .verify(local.descriptor.signing_public_key)
                    .unwrap(),
            )
            .unwrap();
        trust
            .apply(peer_grant.verify(peer.signing_public_key).unwrap())
            .unwrap();

        let profile = koru_store_core::ProfileId::new();
        let operation = ProfileLifecycleMutationId::mint();
        let revision = ProfileLifecycleRevision::new(1);
        LifecycleRepository::open(&paths.profile_lifecycle_db())
            .unwrap()
            .observe_trust_deleted(profile, revision, operation, 5)
            .unwrap();
        let wire = LifecycleRepository::open(&paths.profile_lifecycle_db())
            .unwrap()
            .get(profile)
            .unwrap()
            .unwrap()
            .wire();
        let fanout = FakeFanout::default();
        publish_deleted(
            &paths,
            &local.descriptor,
            &local.signing_key,
            &fanout,
            &wire,
        )
        .unwrap();

        let first = fanout.0.borrow().clone();
        assert_eq!(first.len(), 2);
        let Action::Send(target, TrustRecord::ProfileLifecycle(first_record)) = &first[0] else {
            panic!("the lifecycle record must be queued first")
        };
        assert_eq!(*target, peer.device_id);
        assert_eq!(first[1], Action::Refresh);
        assert_eq!(
            LifecycleRepository::open(&paths.profile_lifecycle_db())
                .unwrap()
                .get(profile)
                .unwrap()
                .unwrap()
                .peer_targets,
            vec![peer.device_id]
        );
        assert!(
            crate::identity::SqliteTrustRepository::open(&paths.identity_db())
                .unwrap()
                .trust_view()
                .unwrap()
                .profile_is_deleted(profile)
        );

        let replay = FakeFanout::default();
        assert_eq!(
            resume_pending(&paths, &local.descriptor, &local.signing_key, &replay,).unwrap(),
            1
        );
        let replay = replay.0.borrow();
        let Action::Send(replay_target, TrustRecord::ProfileLifecycle(replay_record)) = &replay[0]
        else {
            panic!("restart must replay the same typed lifecycle record")
        };
        assert_eq!(*replay_target, peer.device_id);
        assert_eq!(replay_record.record_id, first_record.record_id);
        assert_eq!(replay[1], Action::Refresh);
    }

    #[test]
    fn startup_replays_only_unfinished_deleted_profile_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_root(root.path().to_owned());
        let profile = koru_store_core::ProfileId::new();
        let operation = ProfileLifecycleMutationId::mint();
        let mut repository = LifecycleRepository::open(&paths.profile_lifecycle_db()).unwrap();
        repository
            .observe_trust_deleted(profile, ProfileLifecycleRevision::new(1), operation, 1)
            .unwrap();

        assert_eq!(
            pending_deleted_profiles(&paths).unwrap(),
            vec![profile.to_storage()]
        );
        for (index, stage) in [
            ProfileCleanupStage::WorkspacesRemoved,
            ProfileCleanupStage::ProfileRemoved,
            ProfileCleanupStage::SettingsCleared,
            ProfileCleanupStage::NavigationSettled,
        ]
        .into_iter()
        .enumerate()
        {
            repository
                .acknowledge(profile, stage, 2 + index as i64)
                .unwrap();
        }
        assert!(pending_deleted_profiles(&paths).unwrap().is_empty());
    }

    /// The loop behind a deletion notice that came back for days. Peers send
    /// the whole device list, this tombstone included, every few minutes; the
    /// settled journal row used to be deleted, so each copy read as a
    /// deletion nobody had cleaned up and the cleanup — and its notice — ran
    /// again.
    #[test]
    fn a_tombstone_redelivered_after_its_cleanup_settled_owes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::for_root(root.path().to_owned());
        let profile = koru_store_core::ProfileId::new();
        let tombstone = koru_device::ProfileLifecycleV1::sign(
            RecordId::new(),
            DeviceId::new(),
            profile,
            koru_device::ProfileLifecycleStateV1::Deleted,
            1,
            ProfileLifecycleMutationId::mint().as_uuid(),
            5,
            &DeviceSigningKey::generate(),
        );

        assert_eq!(observe_received(&paths, &tombstone), Ok(true));
        // Still owed until the shell reports the last boundary: a second copy
        // before then is a chance to retry, not a reason to stop.
        assert_eq!(observe_received(&paths, &tombstone), Ok(true));

        for stage in [
            ProfileCleanupStage::WorkspacesRemoved,
            ProfileCleanupStage::ProfileRemoved,
            ProfileCleanupStage::SettingsCleared,
            ProfileCleanupStage::NavigationSettled,
        ] {
            assert_eq!(
                acknowledge_cleanup(&paths, &profile.to_storage(), stage),
                Ok(stage == ProfileCleanupStage::NavigationSettled),
                "{stage:?}"
            );
        }

        assert_eq!(observe_received(&paths, &tombstone), Ok(false));
        assert!(pending_deleted_profiles(&paths).unwrap().is_empty());
        // A Profile the journal never held has nothing left to acknowledge,
        // and an id that is not one is refused before the journal is opened.
        assert_eq!(
            acknowledge_cleanup(
                &paths,
                &koru_store_core::ProfileId::new().to_storage(),
                ProfileCleanupStage::ProfileRemoved,
            ),
            Ok(true)
        );
        assert_eq!(
            acknowledge_cleanup(&paths, "not-a-profile", ProfileCleanupStage::ProfileRemoved),
            Err("invalid_id")
        );
    }
}
