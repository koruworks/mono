// SPDX-License-Identifier: MPL-2.0

//! Shared non-secret discovery and ordered vault durability. This module owns
//! no retry loop: the existing account worker retains and retries each effect.
use super::{AccountService, ServiceError};
use crate::identity::{
    AccountDirectorySnapshot, AccountOwner, DirectoryRecord, DirectoryState, SqliteTrustRepository,
};
use koru_oauth_credentials::{CredentialEntry, CredentialStore, SessionCredential};
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(any(test, feature = "test-support"))]
use std::sync::Mutex;

/// Durable effects are injected; tests use the actual in-memory schema 5.
pub trait AccountDirectory: Send + Sync {
    fn activate(
        &self,
        namespace: &str,
        account: koru_account_core::KoruIdentityId,
    ) -> Result<(), ServiceError>;
    fn snapshot(&self, namespace: &str) -> Result<AccountDirectorySnapshot, ServiceError>;
    fn pending(
        &self,
        namespace: &str,
        owner: AccountOwner,
    ) -> Result<DirectoryRecord, ServiceError>;
    fn ready(
        &self,
        namespace: &str,
        record: &DirectoryRecord,
        actual: AccountOwner,
        activate: bool,
        retire: bool,
    ) -> Result<(), ServiceError>;
    fn remove(&self, namespace: &str, record: &DirectoryRecord) -> Result<(), ServiceError>;
    fn retire_legacy(&self, namespace: &str) -> Result<(), ServiceError>;
    fn deactivate(&self, namespace: &str, owner: AccountOwner) -> Result<(), ServiceError>;
}

pub struct SqliteAccountDirectory {
    path: PathBuf,
}
impl SqliteAccountDirectory {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    fn with<T>(
        &self,
        operation: impl FnOnce(&mut SqliteTrustRepository) -> Result<T, koru_device::DeviceError>,
    ) -> Result<T, ServiceError> {
        let mut repository = SqliteTrustRepository::open(&self.path).map_err(persistence_error)?;
        operation(&mut repository).map_err(persistence_error)
    }
}
fn persistence_error(_: koru_device::DeviceError) -> ServiceError {
    ServiceError::retryable("account_directory_unavailable")
}
macro_rules! directory_adapter {
    () => {
        fn activate(
            &self,
            namespace: &str,
            account: koru_account_core::KoruIdentityId,
        ) -> Result<(), ServiceError> {
            self.with(|r| r.activate_account(namespace, account))
        }
        fn snapshot(&self, namespace: &str) -> Result<AccountDirectorySnapshot, ServiceError> {
            self.with(|r| r.account_directory(namespace))
        }
        fn pending(
            &self,
            namespace: &str,
            owner: AccountOwner,
        ) -> Result<DirectoryRecord, ServiceError> {
            self.with(|r| r.begin_account_write(namespace, owner))
        }
        fn ready(
            &self,
            namespace: &str,
            record: &DirectoryRecord,
            actual: AccountOwner,
            activate: bool,
            retire: bool,
        ) -> Result<(), ServiceError> {
            self.with(|r| r.finish_account_write(namespace, record, actual, activate, retire))
        }
        fn remove(&self, namespace: &str, record: &DirectoryRecord) -> Result<(), ServiceError> {
            self.with(|r| r.remove_account(namespace, record))
        }
        fn retire_legacy(&self, namespace: &str) -> Result<(), ServiceError> {
            self.with(|r| r.retire_legacy_credential(namespace))
        }
        fn deactivate(&self, namespace: &str, owner: AccountOwner) -> Result<(), ServiceError> {
            self.with(|r| r.deactivate_account(namespace, owner.account))
        }
    };
}
impl AccountDirectory for SqliteAccountDirectory {
    directory_adapter!();
}

#[cfg(any(test, feature = "test-support"))]
pub struct MemoryAccountDirectory(Mutex<SqliteTrustRepository>);
#[cfg(any(test, feature = "test-support"))]
impl MemoryAccountDirectory {
    pub fn new() -> Self {
        Self(Mutex::new(
            SqliteTrustRepository::in_memory().expect("in-memory directory"),
        ))
    }
    fn with<T>(
        &self,
        operation: impl FnOnce(&mut SqliteTrustRepository) -> Result<T, koru_device::DeviceError>,
    ) -> Result<T, ServiceError> {
        operation(&mut self.0.lock().unwrap_or_else(|e| e.into_inner())).map_err(persistence_error)
    }
}
#[cfg(any(test, feature = "test-support"))]
impl AccountDirectory for MemoryAccountDirectory {
    directory_adapter!();
}

pub struct CredentialPersistence {
    vault: Arc<dyn CredentialStore>,
    legacy: CredentialEntry,
    directory: Option<Arc<dyn AccountDirectory>>,
    active: Option<DirectoryRecord>,
    /// The first slot restore could not use, keyed by its account (`None` is
    /// the legacy slot), and the code a surface shows for it.
    repair: Option<(Option<koru_account_core::KoruIdentityId>, &'static str)>,
    legacy_ready: bool,
    removed_owner: Option<AccountOwner>,
    selection: Option<Option<koru_account_core::KoruIdentityId>>,
    activate_writes: bool,
    discover_namespace: bool,
    adopt_legacy: Arc<std::sync::atomic::AtomicBool>,
    discovered_records: Vec<DirectoryRecord>,
    discovery_required: bool,
}

/// Exact replacement state stays in PendingDurability until all three effects
/// acknowledge. Once prepared, retries never derive the destination anew.
#[derive(Default)]
pub struct PendingCredentialWrite {
    record: Option<DirectoryRecord>,
    vault_acknowledged: bool,
}

impl CredentialPersistence {
    pub fn new(
        vault: Arc<dyn CredentialStore>,
        legacy: CredentialEntry,
        directory: Option<Arc<dyn AccountDirectory>>,
    ) -> Self {
        let legacy_ready = directory.is_none();
        Self {
            vault,
            legacy,
            directory,
            active: None,
            repair: None,
            legacy_ready,
            removed_owner: None,
            selection: None,
            activate_writes: true,
            discover_namespace: true,
            adopt_legacy: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            discovered_records: Vec::new(),
            discovery_required: false,
        }
    }
    pub fn owner(&self) -> Option<AccountOwner> {
        self.active.as_ref().map(|r| r.owner)
    }
    /// The coordinator owns active selection. An inactive effect may update
    /// its credential slot but cannot select it when its acknowledgement arrives.
    pub fn for_selection(mut self, account: Option<koru_account_core::KoruIdentityId>) -> Self {
        self.selection = Some(account);
        self.activate_writes = false;
        self.discover_namespace = account.is_none();
        self.adopt_legacy
            .store(false, std::sync::atomic::Ordering::Release);
        self
    }
    pub fn legacy_adoption_guard(&self) -> Arc<std::sync::atomic::AtomicBool> {
        self.adopt_legacy.clone()
    }
    pub fn take_discovered_records(&mut self) -> Vec<DirectoryRecord> {
        std::mem::take(&mut self.discovered_records)
    }
    pub fn pin_initial_selection(&mut self, account: Option<koru_account_core::KoruIdentityId>) {
        self.selection = Some(account);
    }
    pub fn coordinated(mut self) -> Self {
        self.activate_writes = false;
        self
    }
    pub fn record(&self) -> Option<DirectoryRecord> {
        self.active.clone()
    }
    pub fn repair_code(&self) -> Option<&'static str> {
        self.repair.map(|(_, code)| code)
    }
    pub fn legacy_ready(&self) -> bool {
        self.legacy_ready
    }
    pub fn removed_owner(&self) -> Option<AccountOwner> {
        self.removed_owner
    }
    /// A retired execution context may finish remote cleanup after a newer
    /// incarnation took the same account slot. It never erases that slot.
    pub fn clear_retired(
        &mut self,
        pending: Option<(&SessionCredential, &PendingCredentialWrite)>,
        sign_out: bool,
    ) -> Result<(), ServiceError> {
        let original = self.active.clone();
        let different_destination =
            pending.is_some_and(|(_, write)| match (&original, &write.record) {
                (Some(original), Some(attempt)) => original.owner.account != attempt.owner.account,
                _ => true,
            });
        if sign_out && different_destination {
            // Logout names the acknowledged account, not an unacknowledged
            // attempt's eventual destination. Cancel that attempt while keeping
            // any older destination credential, then erase only the named owner.
            match self.clear(pending, false) {
                Err(error) if error.code == "account_directory_conflict" => {}
                Err(error) => return Err(error),
                Ok(()) => {}
            }
            self.active = original;
            if self.active.is_some() {
                return self.clear_retired(None, true);
            }
            return Ok(());
        }
        match self.clear(pending, sign_out) {
            Err(error) if error.code == "account_directory_conflict" => {
                self.active = None;
                self.removed_owner = None;
                Ok(())
            }
            other => other,
        }
    }
    fn entry(&self, record: &DirectoryRecord) -> Result<CredentialEntry, ServiceError> {
        self.legacy
            .for_account(&record.owner.account.to_string())
            .map_err(|_| ServiceError::terminal("credential_invalid"))
    }
    fn load_slot(
        &self,
        entry: &CredentialEntry,
    ) -> Result<Option<SessionCredential>, ServiceError> {
        self.vault
            .load(entry)
            .map_err(|_| ServiceError::retryable("credential_unavailable"))?
            .map(|value| {
                SessionCredential::decode(&value)
                    .map_err(|_| ServiceError::terminal("credential_invalid"))
            })
            .transpose()
    }

    pub fn restore(
        &mut self,
        service: &mut dyn AccountService,
    ) -> Result<Option<SessionCredential>, ServiceError> {
        let Some(directory) = self.directory.as_ref().cloned() else {
            return self.load_slot(&self.legacy);
        };
        let namespace = self.legacy.namespace();
        let mut snapshot = directory.snapshot(&namespace)?;
        self.discovery_required |= !snapshot.legacy_retired;
        if let Some(selection) = self.selection {
            if !self.discover_namespace && !snapshot.legacy_retired {
                return Err(ServiceError::retryable("credential_migration_pending"));
            }
            snapshot.active = selection;
            if !self.discover_namespace
                || (!self.adopt_legacy.load(std::sync::atomic::Ordering::Acquire)
                    && selection.is_none())
            {
                // An Add attempt owns no existing account. It can reconcile
                // legacy once, but never inspects unrelated held slots first.
                snapshot
                    .records
                    .retain(|r| Some(r.owner.account) == selection);
            }
        }
        self.active = snapshot
            .records
            .iter()
            .find(|record| Some(record.owner.account) == snapshot.active)
            .cloned();
        let restore_selection = snapshot.active;
        self.legacy_ready = snapshot.legacy_retired;
        self.repair = None;
        let mut selected = None;
        let mut failures = std::collections::BTreeMap::new();
        for record in &snapshot.records {
            let selected_slot = snapshot.active == Some(record.owner.account);
            match self.load_slot(&self.entry(record)?) {
                Ok(Some(credential)) => match log_inspection(
                    service.inspect_credential(&credential),
                    selected_slot,
                    record.version,
                ) {
                    Ok(actual) if actual.account == record.owner.account => {
                        let activate = false;
                        if let Err(error) =
                            directory.ready(&namespace, record, actual, activate, false)
                        {
                            failures.insert(Some(record.owner.account), error);
                            continue;
                        }
                        if snapshot.active == Some(actual.account) {
                            self.active = Some(DirectoryRecord {
                                owner: actual,
                                state: DirectoryState::Ready,
                                version: record.version,
                            });
                            selected = Some(credential);
                        }
                    }
                    Ok(_) => {
                        failures.insert(
                            Some(record.owner.account),
                            ServiceError::terminal("credential_owner_mismatch"),
                        );
                    }
                    Err(error) => {
                        // A refusal here is not proof the session is gone.
                        // The inspection presents the *stored* refresh token
                        // and the server keeps only the newest one current,
                        // so a rotation that was answered but never reached
                        // the vault is refused here while `refresh` can still
                        // replay it under the idempotency key
                        // `refresh_retry_key` derives from these same old
                        // bytes. Hand the selected slot through and let the
                        // refresh route conclude: it is the one call that
                        // tells a revoked session apart from a superseded
                        // token, and it clears the vault and publishes
                        // signed-out itself when the session really is gone.
                        // The refusal is still recorded — the inspection log
                        // line and the repair code both stand — it just no
                        // longer decides the outcome.
                        if selected_slot && error.code == "credential_rejected" {
                            selected = Some(credential);
                        }
                        failures.insert(Some(record.owner.account), error);
                    }
                },
                Ok(None) if record.state == DirectoryState::Pending => {
                    if let Err(error) = directory.remove(&namespace, record) {
                        failures.insert(Some(record.owner.account), error);
                    }
                }
                Ok(None) => {
                    failures.insert(
                        Some(record.owner.account),
                        ServiceError::terminal("credential_missing"),
                    );
                }
                Err(error) => {
                    failures.insert(Some(record.owner.account), error);
                }
            }
        }
        let legacy_result = (|| -> Result<(), ServiceError> {
            if !snapshot.legacy_retired {
                let legacy = match self.load_slot(&self.legacy) {
                    Ok(value) => value,
                    Err(error) if unusable(&error) => None,
                    Err(error) => return Err(error),
                };
                match legacy {
                    Some(legacy) => {
                        let owner = match log_inspection(
                            service.inspect_credential(&legacy),
                            snapshot.active.is_none(),
                            0,
                        ) {
                            Ok(owner) => owner,
                            Err(error) if unusable(&error) => {
                                // Only the inspection endpoint's coherent normalized
                                // 401 proves these bytes unusable for this device.
                                // Outage, malformed response and storage failure do
                                // not authorize retirement.
                                directory.retire_legacy(&namespace)?;
                                self.vault.clear(&self.legacy).map_err(|_| {
                                    ServiceError::retryable("credential_unavailable")
                                })?;
                                return Ok(());
                            }
                            Err(error) => return Err(error),
                        };
                        // A valid existing account slot is the newer authority.
                        // Only confirmed absence/invalid payload/normalized 401
                        // permits repair from authenticated unchanged legacy bytes.
                        snapshot = directory.snapshot(&namespace)?;
                        // Selection belongs to this restore context, including
                        // the legacy reconciliation pass after hosted inspection.
                        // A concurrent view switch must not change its owner.
                        snapshot.active = restore_selection;
                        let existing = snapshot
                            .records
                            .iter()
                            .find(|r| r.owner.account == owner.account);
                        let surviving = match existing {
                            Some(record) => match self.load_slot(&self.entry(record)?) {
                                Ok(Some(value)) => match service.inspect_credential(&value) {
                                    Ok(actual) if actual.account == owner.account => {
                                        Some((record.clone(), actual, value))
                                    }
                                    Ok(_) => {
                                        return Err(ServiceError::terminal(
                                            "credential_owner_mismatch",
                                        ));
                                    }
                                    Err(error) if unusable(&error) => None,
                                    Err(error) => return Err(error),
                                },
                                Ok(None) => None,
                                Err(error) if unusable(&error) => None,
                                Err(error) => return Err(error),
                            },
                            None => None,
                        };
                        if let Some((record, actual, value)) = surviving {
                            directory.ready(&namespace, &record, actual, false, true)?;
                            if snapshot.active == Some(actual.account) {
                                self.active = Some(DirectoryRecord {
                                    owner: actual,
                                    state: DirectoryState::Ready,
                                    ..record
                                });
                                selected = Some(value);
                            }
                        } else {
                            let record = directory.pending(&namespace, owner)?;
                            let entry = self.entry(&record)?;
                            self.vault
                                .store(
                                    &entry,
                                    legacy.encode().map_err(|_| {
                                        ServiceError::terminal("credential_invalid")
                                    })?,
                                )
                                .map_err(|_| ServiceError::retryable("credential_unavailable"))?;
                            let activate =
                                (self.adopt_legacy.load(std::sync::atomic::Ordering::Acquire)
                                    && snapshot.active.is_none())
                                    || snapshot.active == Some(owner.account);
                            directory.ready(
                                &namespace,
                                &record,
                                owner,
                                activate && self.activate_writes,
                                true,
                            )?;
                            if activate {
                                self.active = Some(record);
                                selected = Some(legacy);
                            }
                        }
                        failures.remove(&Some(owner.account));
                        self.vault
                            .clear(&self.legacy)
                            .map_err(|_| ServiceError::retryable("credential_unavailable"))?;
                    }
                    None => {
                        directory.retire_legacy(&namespace)?;
                        self.vault
                            .clear(&self.legacy)
                            .map_err(|_| ServiceError::retryable("credential_unavailable"))?;
                    }
                }
            } else {
                // Retirement is authority even if unlink was interrupted. Failed
                // cleanup remains retryable and must never import these bytes.
                self.vault
                    .clear(&self.legacy)
                    .map_err(|_| ServiceError::retryable("credential_unavailable"))?;
            }
            Ok(())
        })();
        if let Err(error) = legacy_result {
            failures.insert(None, error);
        }
        let final_snapshot = directory.snapshot(&namespace)?;
        if self.discovery_required && final_snapshot.legacy_retired {
            self.discovered_records = final_snapshot.records;
            self.discovery_required = false;
        }
        self.legacy_ready = final_snapshot.legacy_retired;
        if let Some((account, mut error)) = failures.into_iter().next() {
            error.code = match error.code {
                "credential_missing" => "account_credential_missing",
                "credential_rejected" => "account_credential_rejected",
                "credential_invalid" => "account_credential_invalid",
                code => code,
            };
            self.repair = Some((account, error.code));
            if selected.is_none() {
                return Err(error);
            }
        }
        Ok(selected)
    }

    pub fn replace(
        &mut self,
        credential: &SessionCredential,
        pending: &mut PendingCredentialWrite,
        service: &mut dyn AccountService,
    ) -> Result<(), ServiceError> {
        let Some(directory) = self.directory.as_ref().cloned() else {
            return self
                .vault
                .store(
                    &self.legacy,
                    credential
                        .encode()
                        .map_err(|_| ServiceError::terminal("credential_invalid"))?,
                )
                .map_err(|_| ServiceError::retryable("credential_unavailable"));
        };
        let namespace = self.legacy.namespace();
        if pending.record.is_none() {
            let owner = service.inspect_credential(credential)?;
            if self
                .selection
                .flatten()
                .is_some_and(|account| account != owner.account)
            {
                return Err(ServiceError::terminal("account_repair_identity_mismatch"));
            }
            pending.record = Some(directory.pending(&namespace, owner)?);
        }
        let record = pending.record.as_ref().expect("prepared directory write");
        // Refuse a removed/replaced destination before any delayed vault write.
        if !directory
            .snapshot(&namespace)?
            .records
            .iter()
            .any(|r| r.version == record.version && r.owner == record.owner)
        {
            return Err(ServiceError::terminal("account_directory_changed"));
        }
        if !pending.vault_acknowledged {
            self.vault
                .store(
                    &self.entry(record)?,
                    credential
                        .encode()
                        .map_err(|_| ServiceError::terminal("credential_invalid"))?,
                )
                .map_err(|_| ServiceError::retryable("credential_unavailable"))?;
            pending.vault_acknowledged = true;
        }
        directory.ready(
            &namespace,
            record,
            record.owner,
            self.activate_writes,
            false,
        )?;
        self.active = Some(DirectoryRecord {
            state: DirectoryState::Ready,
            ..record.clone()
        });
        // A credential for this account has just been accepted and made
        // durable, so the slot restore could not use is repaired. A code about
        // another account's slot is not this acceptance's to clear.
        if self
            .repair
            .is_some_and(|(account, _)| account == Some(record.owner.account))
        {
            self.repair = None;
        }
        if self.selection.is_some() {
            self.selection = Some(Some(record.owner.account));
        }
        Ok(())
    }

    pub fn clear(
        &mut self,
        pending: Option<(&SessionCredential, &PendingCredentialWrite)>,
        sign_out: bool,
    ) -> Result<(), ServiceError> {
        self.removed_owner = None;
        let Some(directory) = self.directory.as_ref().cloned() else {
            return self
                .vault
                .clear(&self.legacy)
                .map_err(|_| ServiceError::retryable("credential_unavailable"));
        };
        let namespace = self.legacy.namespace();
        let record = if let Some((credential, write)) = pending {
            // No replacement reached the vault before a directory ACK. An
            // attempted new sign-in never means sign out the old active slot.
            let Some(record) = &write.record else {
                if let Some(active) = &self.active {
                    directory.deactivate(&namespace, active.owner)?;
                }
                self.active = None;
                return Ok(());
            };
            if let Some(survivor) = self.load_slot(&self.entry(record)?)? {
                if !sign_out
                    && (survivor.session_id != credential.session_id
                        || survivor.refresh_token != credential.refresh_token)
                {
                    // The prior credential survived. Its pending discovery row
                    // remains available for authenticated startup reconciliation.
                    if let Some(active) = &self.active {
                        directory.deactivate(&namespace, active.owner)?;
                    }
                    self.active = None;
                    return Ok(());
                }
            }
            Some(record)
        } else {
            self.active.as_ref()
        };
        if let Some(record) = record {
            let snapshot = directory.snapshot(&namespace)?;
            if snapshot.records.iter().any(|current| {
                current.owner.account == record.owner.account
                    && (current.version != record.version || current.owner != record.owner)
            }) {
                return Err(ServiceError::terminal("account_directory_conflict"));
            }
            self.vault
                .clear(&self.entry(record)?)
                .map_err(|_| ServiceError::retryable("credential_unavailable"))?;
            directory.remove(&namespace, record)?;
            self.removed_owner = Some(record.owner);
            if self
                .active
                .as_ref()
                .is_some_and(|active| active.owner.account == record.owner.account)
            {
                self.active = None;
            }
        }
        if let Some(active) = &self.active {
            directory.deactivate(&namespace, active.owner)?;
        }
        self.active = None;
        Ok(())
    }
}

/// One line per stored slot restore inspects, so a start that is refused can
/// be told apart: the selected slot or another held one, and which code. The
/// inspection presents the stored refresh token, and the server accepts only
/// the newest one there, so a rotation that never reached the vault is
/// refused here as `credential_rejected`. Version `0` is the legacy slot.
///
/// This line is the whole reason the refusal is still recorded: a
/// `credential_rejected` on the selected slot no longer ends the start — the
/// bytes go to `refresh`, which is the only call that can tell a revoked
/// session from a superseded token — so the log is what says a refusal
/// happened at all, and pairing it with the next line ("credential refresh
/// answered", "credential persisted") is how a lost rotation is read off a
/// log afterwards.
fn log_inspection(
    inspected: Result<AccountOwner, ServiceError>,
    selected: bool,
    record_version: i64,
) -> Result<AccountOwner, ServiceError> {
    match &inspected {
        Ok(_) => tracing::info!(
            target: "koru_account",
            selected,
            record_version,
            "restore inspected a stored credential: accepted"
        ),
        Err(error) => tracing::info!(
            target: "koru_account",
            selected,
            record_version,
            code = error.code,
            retryable = error.retryable,
            "restore inspected a stored credential: refused"
        ),
    }
    inspected
}

/// Payload corruption is distinct from a vault that cannot read/decrypt. Only
/// the former and coherent inspection refusal conclusively permit repair.
fn unusable(error: &ServiceError) -> bool {
    matches!(error.code, "credential_invalid" | "credential_rejected")
}

#[cfg(test)]
mod tests;
