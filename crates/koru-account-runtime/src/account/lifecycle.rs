// SPDX-License-Identifier: MPL-2.0

//! Exact local/hosted Profile lifecycle reconciliation.
//!
//! The hosted Device/Profile relation describes authorization and remote
//! availability. It cannot prove local content is still visible: only the
//! Store inventory can. Keeping that join here prevents onboarding from
//! resurrecting a locally removed Profile merely because its old hosted row is
//! still provisioned.

use std::path::Path;
use std::str::FromStr;

use koru_account_core::{
    DeviceId, DeviceProfileState, KoruIdentityId, ProfileId, ProfileLifecycle,
    ProfileLifecycleMutationId, ProfileLifecycleRevision, ProfileLifecycleState,
};
use koru_account_wire::{DeviceSummaryV1, ProfileLifecycleMutationStatusV1};
use koru_agent_wire::{
    AccountProfileLifecycleRow, AccountProfileLifecycleState, ProfileCleanupStage,
};
use koru_store_wire::ProfileLifecycleInventory;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::ServiceError;

/// One Profile's identity as this device's own copy of its Loro identity
/// document reads it.
///
/// **The identity document is the truth for a Profile's name and locale; the
/// account's row is not.** `profiles.discovery_name_snapshot` is written when a
/// Profile is created and when a deleted one is restored, and there is
/// deliberately no route that updates it afterwards — a server that holds an
/// accurate name for every Profile is a liability for a product heading towards
/// encryption, not a field to start maintaining. So the account's copy is a
/// first-run stopgap for a device that has an account session and has not synced
/// an identity document yet, and it must never win over one this device holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalProfileIdentity {
    pub profile_id: ProfileId,
    pub name: String,
    /// BCP 47, absent only where the document does not carry one.
    pub locale: Option<String>,
}

pub trait LocalProfileInventory: Send + Sync {
    fn inventory(&self) -> Result<ProfileLifecycleInventory, ServiceError>;

    /// What each Profile's own identity document says on this device.
    ///
    /// Empty by default, which is the honest answer for an adapter that cannot
    /// reach the Store: its callers then fall back to the account's stale copy
    /// rather than show a list of nameless Profiles. See [`LocalProfileIdentity`]
    /// for why the fallback direction is the whole point.
    fn identities(&self) -> Result<Vec<LocalProfileIdentity>, ServiceError> {
        Ok(Vec::new())
    }

    /// Commit the minimal local Profile after current hosted authorization has
    /// been checked. This does not grant peer trust. Read-only host adapters fail
    /// closed instead of reporting content availability from hosted state alone.
    fn provision(
        &self,
        _profile: ProfileId,
        _initial: &koru_store_wire::ProvisionAccountProfile,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::retryable("profile_provisioning_unavailable"))
    }

    /// **Bring back a Profile this computer removed earlier**, because the
    /// person has just chosen it again under *Import Profiles*.
    ///
    /// The Store's own guarded restore (`restore_profile_local`): it refuses a
    /// Profile that was deleted from the account rather than only removed
    /// here, and checks the retained identity before the Profile becomes
    /// visible. Its Workspaces stay in the Trash, where the removal put them.
    /// Read-only host adapters fail closed.
    fn restore_removed(&self, _profile: ProfileId) -> Result<(), ServiceError> {
        Err(ServiceError::retryable("profile_restore_unavailable"))
    }

    /// A fresh local presence check before granting reciprocal enrollment trust.
    fn authorize(
        &self,
        profile: ProfileId,
        identity: KoruIdentityId,
    ) -> Result<bool, ServiceError> {
        let binding = format!("koru:{identity}");
        Ok(self.inventory()?.profiles.iter().any(|item| {
            item.profile_id == profile
                && item.account_binding == binding
                && item.removed_at_millis.is_none()
        }))
    }
}

#[cfg(test)]
pub struct EmptyProfileInventory;

#[cfg(test)]
impl LocalProfileInventory for EmptyProfileInventory {
    fn inventory(&self) -> Result<ProfileLifecycleInventory, ServiceError> {
        Ok(ProfileLifecycleInventory {
            profiles: Vec::new(),
        })
    }
}

pub fn profile_availability(
    inventory: &ProfileLifecycleInventory,
    account: KoruIdentityId,
    devices: &[DeviceSummaryV1],
    local_device: DeviceId,
    profile: ProfileId,
) -> (bool, bool) {
    let account_binding = format!("koru:{account}");
    let available_on_this_device = inventory.profiles.iter().any(|item| {
        item.profile_id == profile
            && item.account_binding == account_binding
            && item.removed_at_millis.is_none()
    });
    let available_on_another_device = devices
        .iter()
        .filter(|device| !device.revoked && device.device_id != local_device)
        .any(|device| {
            device.profile_states.iter().any(|binding| {
                binding.profile_id == profile && binding.state == DeviceProfileState::Provisioned
            })
        });
    (available_on_this_device, available_on_another_device)
}

const MAX_PEER_TARGETS: usize = 64;

#[derive(Debug)]
pub struct LifecycleError {
    pub code: &'static str,
}

impl LifecycleError {
    fn storage() -> Self {
        Self {
            code: "profile_lifecycle_storage_failed",
        }
    }

    fn corrupt() -> Self {
        Self {
            code: "profile_lifecycle_state_corrupt",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupRecord {
    pub profile: ProfileId,
    pub expected_revision: ProfileLifecycleRevision,
    pub lifecycle_state: ProfileLifecycleState,
    pub lifecycle_revision: ProfileLifecycleRevision,
    pub operation: ProfileLifecycleMutationId,
    pub stage: ProfileCleanupStage,
    pub peer_targets: Vec<DeviceId>,
    pub failure_code: Option<String>,
}

impl CleanupRecord {
    pub fn wire(&self) -> AccountProfileLifecycleRow {
        AccountProfileLifecycleRow {
            profile_id: self.profile.to_string(),
            lifecycle_state: match self.lifecycle_state {
                ProfileLifecycleState::Active => AccountProfileLifecycleState::Active,
                ProfileLifecycleState::Deleted => AccountProfileLifecycleState::Deleted,
            },
            lifecycle_revision: self.lifecycle_revision.get(),
            operation_id: Some(self.operation.to_string()),
            cleanup_stage: Some(self.stage),
            failure_code: self.failure_code.clone(),
        }
    }
}

/// Small agent-owned journal for the cross-process cleanup handshake. It has
/// no Profile name, identity, email, content, or access token. SQLite provides
/// the atomic operation-id allocation that makes an uncertain hosted response
/// replay the exact mutation after a restart.
pub struct LifecycleRepository {
    connection: Connection,
}

impl LifecycleRepository {
    pub fn open(path: &Path) -> Result<Self, LifecycleError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| LifecycleError::storage())?;
        }
        let connection = Connection::open(path).map_err(|_| LifecycleError::storage())?;
        Self::prepare(connection)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn in_memory() -> Self {
        Self::prepare(Connection::open_in_memory().unwrap()).unwrap()
    }

    fn prepare(connection: Connection) -> Result<Self, LifecycleError> {
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| LifecycleError::storage())?;
        connection
            .execute_batch(koru_sqlite::DURABILITY)
            .map_err(|_| LifecycleError::storage())?;
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|_| LifecycleError::storage())?;
        if version == 0 {
            connection
                .execute_batch(include_str!(
                    "../../migrations/016_agent_profile_cleanup.sql"
                ))
                .map_err(|_| LifecycleError::storage())?;
        } else if version == 1 {
            connection
                .execute_batch(
                    "BEGIN IMMEDIATE;
                     CREATE TABLE profile_restore (
                       profile_id TEXT PRIMARY KEY,
                       expected_revision INTEGER NOT NULL CHECK(expected_revision >= 1),
                       operation_id TEXT NOT NULL UNIQUE,
                       lifecycle_revision INTEGER CHECK(lifecycle_revision >= 2),
                       failure_code TEXT,
                       updated_at_millis INTEGER NOT NULL
                     );
                     PRAGMA user_version = 2;
                     COMMIT;",
                )
                .map_err(|_| LifecycleError::storage())?;
        } else if version != 2 {
            return Err(LifecycleError::corrupt());
        }
        Ok(Self { connection })
    }

    pub fn begin_delete(
        &mut self,
        profile: ProfileId,
        expected_revision: ProfileLifecycleRevision,
        now_millis: i64,
    ) -> Result<CleanupRecord, LifecycleError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| LifecycleError::storage())?;
        match read_record(&tx, profile)? {
            // A settled row is the memory of a deletion that finished, not a
            // deletion in flight. Deleting the same Profile again is a new
            // operation, so the memory makes way for it.
            Some(record) if record.stage == ProfileCleanupStage::NavigationSettled => {
                tx.execute(
                    "DELETE FROM profile_cleanup WHERE profile_id = ?1",
                    params![profile.to_string()],
                )
                .map_err(|_| LifecycleError::storage())?;
            }
            Some(record) => {
                tx.commit().map_err(|_| LifecycleError::storage())?;
                return Ok(record);
            }
            None => {}
        }
        let operation = ProfileLifecycleMutationId::mint();
        tx.execute(
            "INSERT INTO profile_cleanup(
               profile_id, expected_revision, lifecycle_state, lifecycle_revision,
               operation_id, stage, peer_targets_json, failure_code, updated_at_millis
             ) VALUES(?1, ?2, 'active', ?2, ?3, 'hosted_pending', '[]', NULL, ?4)",
            params![
                profile.to_string(),
                i64::try_from(expected_revision.get()).map_err(|_| LifecycleError::corrupt())?,
                operation.to_string(),
                now_millis,
            ],
        )
        .map_err(|_| LifecycleError::storage())?;
        tx.commit().map_err(|_| LifecycleError::storage())?;
        self.get(profile)?.ok_or_else(LifecycleError::corrupt)
    }

    pub fn record_hosted(
        &mut self,
        profile: ProfileId,
        operation: ProfileLifecycleMutationId,
        lifecycle: &ProfileLifecycle,
        now_millis: i64,
    ) -> Result<CleanupRecord, LifecycleError> {
        if lifecycle.profile_id != profile
            || lifecycle.operation_id != Some(operation)
            || lifecycle.state != ProfileLifecycleState::Deleted
        {
            return Err(LifecycleError::corrupt());
        }
        let changed = self
            .connection
            .execute(
                "UPDATE profile_cleanup
                 SET lifecycle_state = 'deleted', lifecycle_revision = ?3,
                     stage = CASE WHEN stage = 'hosted_pending' THEN 'hosted_committed' ELSE stage END,
                     failure_code = NULL, updated_at_millis = ?4
                 WHERE profile_id = ?1 AND operation_id = ?2",
                params![
                    profile.to_string(),
                    operation.to_string(),
                    i64::try_from(lifecycle.revision.get())
                        .map_err(|_| LifecycleError::corrupt())?,
                    now_millis,
                ],
            )
            .map_err(|_| LifecycleError::storage())?;
        if changed != 1 {
            return Err(LifecycleError::corrupt());
        }
        self.get(profile)?.ok_or_else(LifecycleError::corrupt)
    }

    /// Persist a deletion found by bounded reconciliation. A pending local
    /// delete keeps its own operation id so its uncertain request can still be
    /// replayed exactly; otherwise the hosted tombstone becomes the cleanup key.
    pub fn observe_deleted(
        &mut self,
        lifecycle: &ProfileLifecycle,
        now_millis: i64,
    ) -> Result<CleanupRecord, LifecycleError> {
        let operation = lifecycle.operation_id.ok_or_else(LifecycleError::corrupt)?;
        if let Some(existing) = self.get(lifecycle.profile_id)? {
            if existing.stage == ProfileCleanupStage::HostedPending {
                return Ok(existing);
            }
            if lifecycle.revision < existing.lifecycle_revision {
                return Ok(existing);
            }
        }
        self.connection
            .execute(
                "INSERT INTO profile_cleanup(
                   profile_id, expected_revision, lifecycle_state, lifecycle_revision,
                   operation_id, stage, peer_targets_json, failure_code, updated_at_millis
                 ) VALUES(?1, ?2, 'deleted', ?3, ?4, 'hosted_committed', '[]', NULL, ?5)
                 ON CONFLICT(profile_id) DO UPDATE SET
                   lifecycle_state = excluded.lifecycle_state,
                   lifecycle_revision = excluded.lifecycle_revision,
                   operation_id = excluded.operation_id,
                   stage = 'hosted_committed', failure_code = NULL,
                   updated_at_millis = excluded.updated_at_millis",
                params![
                    lifecycle.profile_id.to_string(),
                    i64::try_from(lifecycle.revision.get().saturating_sub(1))
                        .map_err(|_| LifecycleError::corrupt())?,
                    i64::try_from(lifecycle.revision.get())
                        .map_err(|_| LifecycleError::corrupt())?,
                    operation.to_string(),
                    now_millis,
                ],
            )
            .map_err(|_| LifecycleError::storage())?;
        self.get(lifecycle.profile_id)?
            .ok_or_else(LifecycleError::corrupt)
    }

    /// Persist a signed peer tombstone before its delivery is acknowledged.
    /// The trust record does not carry account discovery data; cleanup needs
    /// only the exact Profile, hosted ordering fields, and operation id.
    pub fn observe_trust_deleted(
        &mut self,
        profile: ProfileId,
        revision: ProfileLifecycleRevision,
        operation: ProfileLifecycleMutationId,
        now_millis: i64,
    ) -> Result<CleanupRecord, LifecycleError> {
        if revision == ProfileLifecycleRevision::INITIAL {
            return Err(LifecycleError::corrupt());
        }
        if let Some(existing) = self.get(profile)? {
            if revision < existing.lifecycle_revision {
                return Ok(existing);
            }
            if revision == existing.lifecycle_revision {
                if operation != existing.operation
                    || existing.lifecycle_state != ProfileLifecycleState::Deleted
                {
                    return Err(LifecycleError::corrupt());
                }
                return Ok(existing);
            }
        }
        self.connection
            .execute(
                "INSERT INTO profile_cleanup(
                   profile_id, expected_revision, lifecycle_state, lifecycle_revision,
                   operation_id, stage, peer_targets_json, failure_code, updated_at_millis
                 ) VALUES(?1, ?2, 'deleted', ?3, ?4, 'hosted_committed', '[]', NULL, ?5)
                 ON CONFLICT(profile_id) DO UPDATE SET
                   expected_revision = excluded.expected_revision,
                   lifecycle_state = excluded.lifecycle_state,
                   lifecycle_revision = excluded.lifecycle_revision,
                   operation_id = excluded.operation_id,
                   stage = 'hosted_committed', peer_targets_json = '[]', failure_code = NULL,
                   updated_at_millis = excluded.updated_at_millis",
                params![
                    profile.to_string(),
                    i64::try_from(revision.get().saturating_sub(1))
                        .map_err(|_| LifecycleError::corrupt())?,
                    i64::try_from(revision.get()).map_err(|_| LifecycleError::corrupt())?,
                    operation.to_string(),
                    now_millis,
                ],
            )
            .map_err(|_| LifecycleError::storage())?;
        self.get(profile)?.ok_or_else(LifecycleError::corrupt)
    }

    pub fn observe_active(&mut self, lifecycle: &ProfileLifecycle) -> Result<(), LifecycleError> {
        if lifecycle.state != ProfileLifecycleState::Active {
            return Err(LifecycleError::corrupt());
        }
        if let Some(existing) = self.get(lifecycle.profile_id)?
            && lifecycle.revision > existing.lifecycle_revision
        {
            self.connection
                .execute(
                    "DELETE FROM profile_cleanup WHERE profile_id=?1",
                    params![lifecycle.profile_id.to_string()],
                )
                .map_err(|_| LifecycleError::storage())?;
        }
        Ok(())
    }

    /// A proven rebind starts a new owner's lifecycle at revision zero. Retire
    /// the captured old intent independently of revision ordering, before the
    /// caller publishes the new binding. Exact comparison fences replacement.
    pub(super) fn retire_for_rebind(
        &mut self,
        profile: ProfileId,
        expected: Option<ProfileLifecycleMutationId>,
    ) -> Result<(), LifecycleError> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| LifecycleError::storage())?;
        if read_record(&tx, profile)?
            .as_ref()
            .map(|record| record.operation)
            != expected
        {
            return Err(LifecycleError::corrupt());
        }
        tx.execute(
            "DELETE FROM profile_cleanup WHERE profile_id=?1",
            params![profile.to_string()],
        )
        .map_err(|_| LifecycleError::storage())?;
        tx.execute(
            "DELETE FROM profile_restore WHERE profile_id=?1",
            params![profile.to_string()],
        )
        .map_err(|_| LifecycleError::storage())?;
        tx.commit().map_err(|_| LifecycleError::storage())
    }

    pub fn observe_trust_active(
        &mut self,
        profile: ProfileId,
        revision: ProfileLifecycleRevision,
    ) -> Result<(), LifecycleError> {
        if let Some(existing) = self.get(profile)?
            && revision > existing.lifecycle_revision
        {
            self.connection
                .execute(
                    "DELETE FROM profile_cleanup WHERE profile_id=?1",
                    params![profile.to_string()],
                )
                .map_err(|_| LifecycleError::storage())?;
        }
        Ok(())
    }

    pub fn record_failure(
        &mut self,
        profile: ProfileId,
        operation: ProfileLifecycleMutationId,
        failure_code: &'static str,
        now_millis: i64,
    ) -> Result<(), LifecycleError> {
        let changed = self
            .connection
            .execute(
                "UPDATE profile_cleanup SET failure_code = ?3, updated_at_millis = ?4
                 WHERE profile_id = ?1 AND operation_id = ?2",
                params![
                    profile.to_string(),
                    operation.to_string(),
                    failure_code,
                    now_millis
                ],
            )
            .map_err(|_| LifecycleError::storage())?;
        if changed == 1 {
            Ok(())
        } else {
            Err(LifecycleError::corrupt())
        }
    }

    pub fn begin_restore(
        &mut self,
        profile: ProfileId,
        expected_revision: ProfileLifecycleRevision,
        now_millis: i64,
    ) -> Result<ProfileLifecycleMutationId, LifecycleError> {
        if expected_revision == ProfileLifecycleRevision::INITIAL {
            return Err(LifecycleError::corrupt());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| LifecycleError::storage())?;
        let existing: Option<(i64, String)> = tx
            .query_row(
                "SELECT expected_revision,operation_id FROM profile_restore
                 WHERE profile_id=?1",
                params![profile.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| LifecycleError::storage())?;
        if let Some((revision, operation)) = existing
            && u64::try_from(revision).map_err(|_| LifecycleError::corrupt())?
                == expected_revision.get()
        {
            tx.commit().map_err(|_| LifecycleError::storage())?;
            return operation.parse().map_err(|_| LifecycleError::corrupt());
        }
        let operation = ProfileLifecycleMutationId::mint();
        tx.execute(
            "INSERT INTO profile_restore
               (profile_id,expected_revision,operation_id,lifecycle_revision,failure_code,updated_at_millis)
             VALUES(?1,?2,?3,NULL,NULL,?4)
             ON CONFLICT(profile_id) DO UPDATE SET
               expected_revision=excluded.expected_revision,
               operation_id=excluded.operation_id,lifecycle_revision=NULL,
               failure_code=NULL,updated_at_millis=excluded.updated_at_millis",
            params![
                profile.to_string(),
                i64::try_from(expected_revision.get()).map_err(|_| LifecycleError::corrupt())?,
                operation.to_string(),
                now_millis,
            ],
        )
        .map_err(|_| LifecycleError::storage())?;
        tx.commit().map_err(|_| LifecycleError::storage())?;
        Ok(operation)
    }

    pub fn record_restored(
        &mut self,
        lifecycle: &ProfileLifecycle,
        operation: ProfileLifecycleMutationId,
        now_millis: i64,
    ) -> Result<(), LifecycleError> {
        if lifecycle.state != ProfileLifecycleState::Active
            || lifecycle.operation_id != Some(operation)
        {
            return Err(LifecycleError::corrupt());
        }
        let changed = self
            .connection
            .execute(
                "UPDATE profile_restore SET lifecycle_revision=?3,failure_code=NULL,
                   updated_at_millis=?4 WHERE profile_id=?1 AND operation_id=?2",
                params![
                    lifecycle.profile_id.to_string(),
                    operation.to_string(),
                    i64::try_from(lifecycle.revision.get())
                        .map_err(|_| LifecycleError::corrupt())?,
                    now_millis,
                ],
            )
            .map_err(|_| LifecycleError::storage())?;
        if changed != 1 {
            return Err(LifecycleError::corrupt());
        }
        // A higher active lifecycle supersedes every unfinished deletion stage.
        self.connection
            .execute(
                "DELETE FROM profile_cleanup WHERE profile_id=?1
                 AND lifecycle_revision < ?2",
                params![
                    lifecycle.profile_id.to_string(),
                    i64::try_from(lifecycle.revision.get())
                        .map_err(|_| LifecycleError::corrupt())?,
                ],
            )
            .map_err(|_| LifecycleError::storage())?;
        Ok(())
    }

    pub fn set_peer_targets(
        &mut self,
        profile: ProfileId,
        operation: ProfileLifecycleMutationId,
        targets: &[DeviceId],
        now_millis: i64,
    ) -> Result<(), LifecycleError> {
        if targets.len() > MAX_PEER_TARGETS {
            return Err(LifecycleError::corrupt());
        }
        let json =
            serde_json::to_string(&targets.iter().map(ToString::to_string).collect::<Vec<_>>())
                .map_err(|_| LifecycleError::corrupt())?;
        let changed = self
            .connection
            .execute(
                "UPDATE profile_cleanup SET peer_targets_json = ?3, updated_at_millis = ?4
                 WHERE profile_id = ?1 AND operation_id = ?2",
                params![profile.to_string(), operation.to_string(), json, now_millis],
            )
            .map_err(|_| LifecycleError::storage())?;
        if changed == 1 {
            Ok(())
        } else {
            Err(LifecycleError::corrupt())
        }
    }

    pub fn acknowledge(
        &mut self,
        profile: ProfileId,
        stage: ProfileCleanupStage,
        now_millis: i64,
    ) -> Result<Option<CleanupRecord>, LifecycleError> {
        let Some(current) = self.get(profile)? else {
            return Ok(None);
        };
        if stage <= current.stage {
            return Ok(Some(current));
        }
        if stage_index(stage) != stage_index(current.stage) + 1 {
            return Err(LifecycleError::corrupt());
        }
        self.connection
            .execute(
                "UPDATE profile_cleanup SET stage = ?2, failure_code = NULL,
                 updated_at_millis = ?3 WHERE profile_id = ?1",
                params![profile.to_string(), stage_token(stage), now_millis],
            )
            .map_err(|_| LifecycleError::storage())?;
        self.get(profile)
    }

    /// The row for `profile`, **settled ones included**.
    ///
    /// A row that reached `navigation_settled` is kept rather than deleted. It
    /// is the only thing on this device that says a deletion was already
    /// cleaned up: the signed tombstone stays in the trust store and peers
    /// send it again on every device-list exchange, so with the row gone each
    /// re-delivery looked like a deletion nobody had handled and the whole
    /// cleanup — and its notice — ran again. A later revision replaces the
    /// row ([`Self::observe_trust_deleted`], [`Self::begin_delete`]) and a
    /// restore removes it ([`Self::observe_active`]).
    pub fn get(&self, profile: ProfileId) -> Result<Option<CleanupRecord>, LifecycleError> {
        read_record(&self.connection, profile)
    }

    /// Forget a hosted delete the service refused **before applying
    /// anything** ([`refusal_applied_nothing`]). Only the exact operation,
    /// and only while it is still `hosted_pending`: a row the service
    /// committed is never dropped here. Answers whether a row went.
    ///
    /// Kept, such a row had nothing to replay and only blocked the next
    /// attempt: the owner proof is in memory, the journal (schema 2) has no
    /// account column, so after an Agent restart every retry was refused as
    /// `profile_lifecycle_owner_unproven` (2026-11-10).
    pub fn abandon_unapplied(
        &mut self,
        profile: ProfileId,
        operation: ProfileLifecycleMutationId,
    ) -> Result<bool, LifecycleError> {
        let removed = self
            .connection
            .execute(
                "DELETE FROM profile_cleanup
                 WHERE profile_id = ?1 AND operation_id = ?2 AND stage = 'hosted_pending'",
                params![profile.to_string(), operation.to_string()],
            )
            .map_err(|_| LifecycleError::storage())?;
        Ok(removed == 1)
    }

    /// Every deletion whose cleanup is still owed, oldest first. A settled
    /// row owes nothing and is not here; see [`Self::get`] for why it exists.
    pub fn pending(&self) -> Result<Vec<CleanupRecord>, LifecycleError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT profile_id, expected_revision, lifecycle_state, lifecycle_revision,
                        operation_id, stage, peer_targets_json, failure_code
                 FROM profile_cleanup WHERE stage != 'navigation_settled'
                 ORDER BY updated_at_millis, profile_id",
            )
            .map_err(|_| LifecycleError::storage())?;
        let rows = statement
            .query_map([], |row| read_record_row(row))
            .map_err(|_| LifecycleError::storage())?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(|_| LifecycleError::storage())??);
        }
        Ok(records)
    }
}

fn read_record(
    connection: &Connection,
    profile: ProfileId,
) -> Result<Option<CleanupRecord>, LifecycleError> {
    connection
        .query_row(
            "SELECT profile_id, expected_revision, lifecycle_state, lifecycle_revision,
                    operation_id, stage, peer_targets_json, failure_code
             FROM profile_cleanup WHERE profile_id = ?1",
            params![profile.to_string()],
            read_record_row,
        )
        .optional()
        .map_err(|_| LifecycleError::storage())?
        .transpose()
}

fn read_record_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<CleanupRecord, LifecycleError>> {
    let profile: String = row.get(0)?;
    let expected_revision: i64 = row.get(1)?;
    let lifecycle_state: String = row.get(2)?;
    let lifecycle_revision: i64 = row.get(3)?;
    let operation: String = row.get(4)?;
    let stage: String = row.get(5)?;
    let peer_targets: String = row.get(6)?;
    let failure_code = row.get(7)?;
    Ok((|| {
        let peer_targets = serde_json::from_str::<Vec<String>>(&peer_targets)
            .map_err(|_| LifecycleError::corrupt())?;
        if peer_targets.len() > MAX_PEER_TARGETS {
            return Err(LifecycleError::corrupt());
        }
        Ok(CleanupRecord {
            profile: ProfileId::parse(&profile).ok_or_else(LifecycleError::corrupt)?,
            expected_revision: ProfileLifecycleRevision::new(
                u64::try_from(expected_revision).map_err(|_| LifecycleError::corrupt())?,
            ),
            lifecycle_state: match lifecycle_state.as_str() {
                "active" => ProfileLifecycleState::Active,
                "deleted" => ProfileLifecycleState::Deleted,
                _ => return Err(LifecycleError::corrupt()),
            },
            lifecycle_revision: ProfileLifecycleRevision::new(
                u64::try_from(lifecycle_revision).map_err(|_| LifecycleError::corrupt())?,
            ),
            operation: ProfileLifecycleMutationId::from_str(&operation)
                .map_err(|_| LifecycleError::corrupt())?,
            stage: parse_stage(&stage).ok_or_else(LifecycleError::corrupt)?,
            peer_targets: peer_targets
                .into_iter()
                .map(|id| DeviceId::parse(&id).ok_or_else(LifecycleError::corrupt))
                .collect::<Result<Vec<_>, _>>()?,
            failure_code,
        })
    })())
}

fn stage_token(stage: ProfileCleanupStage) -> &'static str {
    match stage {
        ProfileCleanupStage::HostedPending => "hosted_pending",
        ProfileCleanupStage::HostedCommitted => "hosted_committed",
        ProfileCleanupStage::WorkspacesRemoved => "workspaces_removed",
        ProfileCleanupStage::ProfileRemoved => "profile_removed",
        ProfileCleanupStage::SettingsCleared => "settings_cleared",
        ProfileCleanupStage::NavigationSettled => "navigation_settled",
    }
}

fn parse_stage(value: &str) -> Option<ProfileCleanupStage> {
    Some(match value {
        "hosted_pending" => ProfileCleanupStage::HostedPending,
        "hosted_committed" => ProfileCleanupStage::HostedCommitted,
        "workspaces_removed" => ProfileCleanupStage::WorkspacesRemoved,
        "profile_removed" => ProfileCleanupStage::ProfileRemoved,
        "settings_cleared" => ProfileCleanupStage::SettingsCleared,
        "navigation_settled" => ProfileCleanupStage::NavigationSettled,
        _ => return None,
    })
}

fn stage_index(stage: ProfileCleanupStage) -> u8 {
    match stage {
        ProfileCleanupStage::HostedPending => 0,
        ProfileCleanupStage::HostedCommitted => 1,
        ProfileCleanupStage::WorkspacesRemoved => 2,
        ProfileCleanupStage::ProfileRemoved => 3,
        ProfileCleanupStage::SettingsCleared => 4,
        ProfileCleanupStage::NavigationSettled => 5,
    }
}

/// A refusal of the hosted delete that proves the service applied nothing:
/// the 428 that asks for a fresh sign-in is a precondition, checked before
/// any lifecycle change. A pending row it leaves has nothing to replay.
pub fn refusal_applied_nothing(code: &str) -> bool {
    matches!(
        code,
        koru_account_wire::ErrorCode::FRESH_AUTHENTICATION_REQUIRED
            | koru_account_wire::ErrorCode::FRESH_GOOGLE_AUTHENTICATION_REQUIRED
    )
}

pub fn exact_retry(status: ProfileLifecycleMutationStatusV1) -> bool {
    status == ProfileLifecycleMutationStatusV1::ExactRetry
}

pub fn wire_lifecycle(
    lifecycle: &ProfileLifecycle,
    cleanup_stage: Option<ProfileCleanupStage>,
    failure_code: Option<String>,
) -> AccountProfileLifecycleRow {
    AccountProfileLifecycleRow {
        profile_id: lifecycle.profile_id.to_string(),
        lifecycle_state: match lifecycle.state {
            ProfileLifecycleState::Active => AccountProfileLifecycleState::Active,
            ProfileLifecycleState::Deleted => AccountProfileLifecycleState::Deleted,
        },
        lifecycle_revision: lifecycle.revision.get(),
        operation_id: lifecycle
            .operation_id
            .map(|operation| operation.to_string()),
        cleanup_stage,
        failure_code,
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use koru_account_wire::{DeviceProfileSummaryV1, Version1};
    use koru_store_core::ids::ProfileId as StoreProfileId;
    use koru_store_wire::ProfileLifecycleInventoryItem;

    use super::*;

    fn account() -> KoruIdentityId {
        KoruIdentityId::from_str("10000000-0000-4000-8000-000000000001").unwrap()
    }

    fn device(value: &str) -> DeviceId {
        DeviceId::from_uuid(uuid::Uuid::parse_str(value).unwrap())
    }

    fn profile() -> ProfileId {
        ProfileId::from_uuid(uuid::Uuid::parse_str("30000000-0000-4000-8000-000000000003").unwrap())
    }

    fn hosted(device_id: DeviceId, profile_id: ProfileId) -> DeviceSummaryV1 {
        DeviceSummaryV1 {
            version: Version1,
            device_id,
            display_name: "not inspected".into(),
            platform: koru_account_core::DevicePlatform::Windows,
            app_version: "test".into(),
            enrollment_protocol_version: None,
            peer_protocol_version: None,
            endpoint_id_hex: String::new(),
            revoked: false,
            last_seen_at_millis: 0,
            profile_states: vec![DeviceProfileSummaryV1 {
                version: Version1,
                profile_id,
                state: DeviceProfileState::Provisioned,
            }],
        }
    }

    fn deleted(
        profile_id: ProfileId,
        operation_id: ProfileLifecycleMutationId,
    ) -> ProfileLifecycle {
        ProfileLifecycle {
            profile_id,
            koru_identity_id: account(),
            state: ProfileLifecycleState::Deleted,
            revision: ProfileLifecycleRevision::new(1),
            operation_id: Some(operation_id),
            lifecycle_at_millis: Some(2),
        }
    }

    #[test]
    fn current_device_availability_comes_only_from_active_exact_store_inventory() {
        let profile = profile();
        let local = device("20000000-0000-4000-8000-000000000002");
        let devices = [hosted(local, profile)];
        let mut inventory = ProfileLifecycleInventory {
            profiles: vec![ProfileLifecycleInventoryItem {
                profile_id: StoreProfileId::from_uuid(profile.as_uuid()),
                account_binding: format!("koru:{}", account()),
                removed_at_millis: Some(1),
            }],
        };
        assert_eq!(
            profile_availability(&inventory, account(), &devices, local, profile),
            (false, false)
        );
        inventory.profiles[0].removed_at_millis = None;
        assert_eq!(
            profile_availability(&inventory, account(), &devices, local, profile),
            (true, false)
        );
    }

    #[test]
    fn another_device_is_hosted_but_wrong_account_inventory_never_becomes_local() {
        let profile = profile();
        let local = device("20000000-0000-4000-8000-000000000002");
        let remote = device("40000000-0000-4000-8000-000000000004");
        let inventory = ProfileLifecycleInventory {
            profiles: vec![ProfileLifecycleInventoryItem {
                profile_id: StoreProfileId::from_uuid(profile.as_uuid()),
                account_binding: "koru:50000000-0000-4000-8000-000000000005".into(),
                removed_at_millis: None,
            }],
        };
        assert_eq!(
            profile_availability(
                &inventory,
                account(),
                &[hosted(remote, profile)],
                local,
                profile
            ),
            (false, true)
        );
    }

    #[test]
    fn uncertain_hosted_ack_reopens_with_the_exact_operation_id() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile-lifecycle.db");
        let profile = profile();
        let first = LifecycleRepository::open(&path)
            .unwrap()
            .begin_delete(profile, ProfileLifecycleRevision::INITIAL, 1)
            .unwrap();
        drop(first.clone());

        let mut restarted = LifecycleRepository::open(&path).unwrap();
        let retry = restarted
            .begin_delete(profile, ProfileLifecycleRevision::INITIAL, 2)
            .unwrap();
        assert_eq!(retry.operation, first.operation);
        assert_eq!(retry.stage, ProfileCleanupStage::HostedPending);

        let committed = restarted
            .record_hosted(
                profile,
                first.operation,
                &deleted(profile, first.operation),
                3,
            )
            .unwrap();
        assert_eq!(committed.lifecycle_revision.get(), 1);
        assert_eq!(committed.stage, ProfileCleanupStage::HostedCommitted);
    }

    #[test]
    fn every_cleanup_boundary_is_durable_and_strictly_ordered() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("profile-lifecycle.db");
        let profile = profile();
        let mut repository = LifecycleRepository::open(&path).unwrap();
        let pending = repository
            .begin_delete(profile, ProfileLifecycleRevision::INITIAL, 1)
            .unwrap();
        repository
            .record_hosted(
                profile,
                pending.operation,
                &deleted(profile, pending.operation),
                2,
            )
            .unwrap();
        assert_eq!(
            repository
                .acknowledge(profile, ProfileCleanupStage::ProfileRemoved, 3)
                .unwrap_err()
                .code,
            "profile_lifecycle_state_corrupt"
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
                .acknowledge(profile, stage, 10 + index as i64)
                .unwrap();
            drop(repository);
            repository = LifecycleRepository::open(&path).unwrap();
            assert_eq!(repository.get(profile).unwrap().unwrap().stage, stage);
        }
    }

    /// The regression behind a notice that came back every few minutes: the
    /// settled row used to be deleted, so the tombstone a peer re-sends on
    /// every device-list exchange started the cleanup again.
    #[test]
    fn a_settled_deletion_is_remembered_and_a_redelivered_tombstone_owes_nothing() {
        let mut repository = LifecycleRepository::in_memory();
        let profile = profile();
        let operation = ProfileLifecycleMutationId::mint();
        let revision = ProfileLifecycleRevision::new(1);
        repository
            .observe_trust_deleted(profile, revision, operation, 1)
            .unwrap();
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
        assert!(repository.pending().unwrap().is_empty());

        let redelivered = repository
            .observe_trust_deleted(profile, revision, operation, 99)
            .unwrap();
        assert_eq!(redelivered.stage, ProfileCleanupStage::NavigationSettled);
        assert!(repository.pending().unwrap().is_empty());

        // The same Profile deleted again after a restore is a new deletion.
        let again = repository
            .observe_trust_deleted(
                profile,
                ProfileLifecycleRevision::new(3),
                ProfileLifecycleMutationId::mint(),
                100,
            )
            .unwrap();
        assert_eq!(again.stage, ProfileCleanupStage::HostedCommitted);
        assert_eq!(repository.pending().unwrap(), vec![again]);
    }

    #[test]
    fn deleting_a_profile_again_replaces_the_memory_of_its_settled_deletion() {
        let mut repository = LifecycleRepository::in_memory();
        let profile = profile();
        let first = repository
            .begin_delete(profile, ProfileLifecycleRevision::INITIAL, 1)
            .unwrap();
        repository
            .record_hosted(
                profile,
                first.operation,
                &deleted(profile, first.operation),
                2,
            )
            .unwrap();
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
                .acknowledge(profile, stage, 3 + index as i64)
                .unwrap();
        }

        let second = repository
            .begin_delete(profile, ProfileLifecycleRevision::new(2), 10)
            .unwrap();
        assert_ne!(second.operation, first.operation);
        assert_eq!(second.stage, ProfileCleanupStage::HostedPending);
    }

    #[test]
    fn peer_delivery_targets_are_bounded_and_contain_only_device_ids() {
        let mut repository = LifecycleRepository::in_memory();
        let profile = profile();
        let pending = repository
            .begin_delete(profile, ProfileLifecycleRevision::INITIAL, 1)
            .unwrap();
        let targets = (0..MAX_PEER_TARGETS)
            .map(|_| DeviceId::new())
            .collect::<Vec<_>>();
        repository
            .set_peer_targets(profile, pending.operation, &targets, 2)
            .unwrap();
        assert_eq!(
            repository.get(profile).unwrap().unwrap().peer_targets,
            targets
        );
        assert_eq!(
            repository
                .set_peer_targets(
                    profile,
                    pending.operation,
                    &(0..=MAX_PEER_TARGETS)
                        .map(|_| DeviceId::new())
                        .collect::<Vec<_>>(),
                    3,
                )
                .unwrap_err()
                .code,
            "profile_lifecycle_state_corrupt"
        );
    }

    #[test]
    fn discovered_deletion_is_durable_without_a_local_delete_request() {
        let mut repository = LifecycleRepository::in_memory();
        let profile = profile();
        let operation = ProfileLifecycleMutationId::mint();
        let record = repository
            .observe_deleted(&deleted(profile, operation), 1)
            .unwrap();
        assert_eq!(record.operation, operation);
        assert_eq!(record.stage, ProfileCleanupStage::HostedCommitted);
        assert_eq!(repository.pending().unwrap(), vec![record]);
    }

    #[test]
    fn restore_operation_replays_exactly_and_supersedes_pending_deletion() {
        let mut repository = LifecycleRepository::in_memory();
        let profile = profile();
        let deleted_operation = ProfileLifecycleMutationId::mint();
        repository
            .observe_deleted(&deleted(profile, deleted_operation), 1)
            .unwrap();
        let deleted_revision = ProfileLifecycleRevision::new(1);
        let restore_operation = repository
            .begin_restore(profile, deleted_revision, 2)
            .unwrap();
        assert_eq!(
            repository
                .begin_restore(profile, deleted_revision, 3)
                .unwrap(),
            restore_operation
        );
        let active = ProfileLifecycle {
            profile_id: profile,
            koru_identity_id: KoruIdentityId::mint(),
            state: ProfileLifecycleState::Active,
            revision: ProfileLifecycleRevision::new(2),
            operation_id: Some(restore_operation),
            lifecycle_at_millis: Some(4),
        };
        repository
            .record_restored(&active, restore_operation, 4)
            .unwrap();
        assert!(repository.pending().unwrap().is_empty());
    }
}
