// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::account::{Cancel, ServiceOutcome, ServiceProgress};
use koru_agent_wire::AccountCommand;
use koru_oauth_credentials::{CredentialContext, SessionCredentialStore};
use std::collections::HashMap;
use std::path::Path;

fn owner(account: u8, auth: u8) -> AccountOwner {
    AccountOwner {
        account: format!("10000000-0000-4000-8000-{account:012}")
            .parse()
            .unwrap(),
        auth_identity: format!("20000000-0000-4000-8000-{auth:012}")
            .parse()
            .unwrap(),
    }
}
fn legacy() -> CredentialEntry {
    CredentialContext::new(
        "nightly",
        Path::new("/injected/root"),
        koru_device::DeviceId::parse("30000000-0000-4000-8000-000000000003").unwrap(),
    )
    .unwrap()
    .entry("https://account.invalid")
    .unwrap()
}
fn credential(value: &str) -> SessionCredential {
    SessionCredential::new(value.into(), format!("secret-{value}")).unwrap()
}
struct Inspector(HashMap<String, AccountOwner>);
impl AccountService for Inspector {
    fn execute(
        &mut self,
        _: u64,
        _: AccountCommand,
        _: Option<&SessionCredential>,
        _: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        panic!("discovery must not execute or refresh")
    }
    fn inspect_credential(
        &mut self,
        credential: &SessionCredential,
    ) -> Result<AccountOwner, ServiceError> {
        if credential.session_id == "rejected" {
            return Err(ServiceError::terminal("credential_rejected"));
        }
        self.0
            .get(&credential.session_id)
            .copied()
            .ok_or_else(|| ServiceError::retryable("account_unavailable"))
    }
}
fn inspector() -> Inspector {
    Inspector(HashMap::from([
        ("old".into(), owner(1, 1)),
        ("new".into(), owner(1, 2)),
        ("second".into(), owner(2, 3)),
    ]))
}
fn saved(vault: &SessionCredentialStore, entry: &CredentialEntry) -> Option<String> {
    vault
        .load(entry)
        .unwrap()
        .map(|s| SessionCredential::decode(&s).unwrap().session_id.clone())
}

#[test]
fn logout_of_b_preserves_a_when_repair_destination_is_unknown_or_refused() {
    for boundary in ["inspect", "different_owner"] {
        for sign_out in [false, true] {
            let directory = Arc::new(FaultDirectory::new());
            let vault = Arc::new(FaultVault::new());
            let legacy = legacy();
            let namespace = legacy.namespace();
            directory.retire_legacy(&namespace).unwrap();
            for (owner, value) in [(owner(1, 1), "old"), (owner(2, 3), "second")] {
                let row = directory.pending(&namespace, owner).unwrap();
                vault
                    .store(
                        &legacy.for_account(&owner.account.to_string()).unwrap(),
                        credential(value).encode().unwrap(),
                    )
                    .unwrap();
                directory
                    .ready(&namespace, &row, owner, value == "second", false)
                    .unwrap();
            }
            let a_entry = legacy
                .for_account(&owner(1, 1).account.to_string())
                .unwrap();
            let b_entry = legacy
                .for_account(&owner(2, 3).account.to_string())
                .unwrap();
            let original_a = vault.load(&a_entry).unwrap().unwrap();
            let mut persistence =
                CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory.clone()))
                    .for_selection(Some(owner(2, 3).account));
            persistence.restore(&mut inspector()).unwrap();
            let mut inspector = inspector();
            match boundary {
                "inspect" => {
                    inspector.0.remove("new");
                }
                "different_owner" => {}
                _ => unreachable!(),
            }
            let next = credential("new");
            let mut write = PendingCredentialWrite::default();
            assert!(
                persistence
                    .replace(&next, &mut write, &mut inspector)
                    .is_err()
            );
            assert!(
                write.record.is_none(),
                "a different repair owner is refused before any directory or vault write"
            );
            persistence
                .clear_retired(Some((&next, &write)), sign_out)
                .unwrap();
            assert_eq!(
                vault.load(&a_entry).unwrap().unwrap().expose(),
                original_a.expose(),
                "{boundary}: A survivor belongs to another held account"
            );
            assert_eq!(
                vault.load(&b_entry).unwrap().is_none(),
                sign_out,
                "{boundary}: explicit logout names only B"
            );
            let mut restored_a =
                CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory.clone()))
                    .for_selection(Some(owner(1, 1).account));
            assert_eq!(
                restored_a
                    .restore(&mut super::tests::inspector())
                    .unwrap()
                    .unwrap()
                    .session_id,
                "old"
            );
            assert_eq!(restored_a.owner(), Some(owner(1, 1)));
        }
    }
}

#[test]
fn every_replacement_crash_boundary_recovers_actual_surviving_auth_identity() {
    for boundary in 0..4 {
        let directory = Arc::new(MemoryAccountDirectory::new());
        let vault = Arc::new(SessionCredentialStore::default());
        let legacy = legacy();
        let namespace = legacy.namespace();
        directory.retire_legacy(&namespace).unwrap();
        let mut writer =
            CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory.clone()));
        writer
            .replace(
                &credential("old"),
                &mut PendingCredentialWrite::default(),
                &mut inspector(),
            )
            .unwrap();
        if boundary >= 1 {
            let pending = directory.pending(&namespace, owner(1, 2)).unwrap();
            if boundary >= 2 {
                vault
                    .store(
                        &legacy
                            .for_account(&owner(1, 2).account.to_string())
                            .unwrap(),
                        credential("new").encode().unwrap(),
                    )
                    .unwrap();
            }
            if boundary >= 3 {
                directory
                    .ready(&namespace, &pending, owner(1, 2), true, false)
                    .unwrap();
            }
        }
        let mut restarted =
            CredentialPersistence::new(vault.clone(), legacy, Some(directory.clone()));
        let restored = restarted.restore(&mut inspector()).unwrap().unwrap();
        let expected = if boundary < 2 {
            owner(1, 1)
        } else {
            owner(1, 2)
        };
        assert_eq!(restarted.owner(), Some(expected));
        assert_eq!(inspector().inspect_credential(&restored).unwrap(), expected);
        assert_eq!(
            directory.snapshot(&namespace).unwrap().records[0].owner,
            expected
        );
    }
}

#[test]
fn legacy_migration_crash_before_ready_restores_and_retirement_survives_logout() {
    for boundary in 0..5 {
        let directory = Arc::new(MemoryAccountDirectory::new());
        let vault = Arc::new(SessionCredentialStore::default());
        let legacy = legacy();
        let namespace = legacy.namespace();
        let account_entry = legacy
            .for_account(&owner(1, 1).account.to_string())
            .unwrap();
        vault
            .store(&legacy, credential("old").encode().unwrap())
            .unwrap();
        if boundary >= 1 {
            let pending = directory.pending(&namespace, owner(1, 1)).unwrap();
            if boundary >= 2 {
                vault
                    .store(&account_entry, credential("old").encode().unwrap())
                    .unwrap();
            }
            if boundary >= 3 {
                directory
                    .ready(&namespace, &pending, owner(1, 1), true, true)
                    .unwrap();
            }
            if boundary >= 4 {
                vault.clear(&legacy).unwrap();
            }
        }
        let mut restarted =
            CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory.clone()));
        assert_eq!(
            restarted
                .restore(&mut inspector())
                .unwrap()
                .unwrap()
                .session_id,
            "old"
        );
        assert_eq!(saved(&vault, &account_entry), Some("old".into()));
        assert!(directory.snapshot(&namespace).unwrap().legacy_retired);
        restarted.clear(None, true).unwrap();
        // Even an unlink ACK loss leaving old bytes cannot resurrect sign-out.
        vault
            .store(&legacy, credential("old").encode().unwrap())
            .unwrap();
        let mut again =
            CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory.clone()));
        assert!(again.restore(&mut inspector()).unwrap().is_none());
        assert!(saved(&vault, &legacy).is_none());
    }
}

#[test]
fn inactive_missing_or_offline_credential_never_blocks_active_account() {
    for has_unusable in [false, true] {
        let directory = Arc::new(MemoryAccountDirectory::new());
        let vault = Arc::new(SessionCredentialStore::default());
        let legacy = legacy();
        let namespace = legacy.namespace();
        directory.retire_legacy(&namespace).unwrap();
        let bad = directory.pending(&namespace, owner(2, 3)).unwrap();
        directory
            .ready(&namespace, &bad, bad.owner, false, false)
            .unwrap();
        if has_unusable {
            vault
                .store(
                    &legacy.for_account(&bad.owner.account.to_string()).unwrap(),
                    credential("unknown").encode().unwrap(),
                )
                .unwrap();
        }
        let mut writer =
            CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory.clone()));
        writer
            .replace(
                &credential("old"),
                &mut PendingCredentialWrite::default(),
                &mut inspector(),
            )
            .unwrap();
        let mut restarted = CredentialPersistence::new(vault, legacy, Some(directory));
        assert_eq!(
            restarted
                .restore(&mut inspector())
                .unwrap()
                .unwrap()
                .session_id,
            "old"
        );
        assert!(restarted.repair_code().is_some());
    }
}

/// Restore's inspection presents the *stored* refresh token and the server
/// keeps only the newest one current, so a rotation that never reached the
/// vault is refused here. That refusal is not proof the session is gone, and
/// restore must hand the bytes on rather than return an error: only `refresh`,
/// which replays the rotation under a key derived from these same old bytes,
/// can tell the two apart. The refusal is still recorded as the repair code.
#[test]
fn a_refused_selected_slot_is_handed_on_for_the_refresh_to_judge() {
    let directory = Arc::new(MemoryAccountDirectory::new());
    let vault = Arc::new(SessionCredentialStore::default());
    let legacy = legacy();
    let namespace = legacy.namespace();
    directory.retire_legacy(&namespace).unwrap();
    let mut writer =
        CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory.clone()));
    writer
        .replace(
            &credential("old"),
            &mut PendingCredentialWrite::default(),
            &mut inspector(),
        )
        .unwrap();
    let slot = legacy
        .for_account(&owner(1, 1).account.to_string())
        .unwrap();
    vault
        .store(&slot, credential("rejected").encode().unwrap())
        .unwrap();
    let mut restarted = CredentialPersistence::new(vault.clone(), legacy, Some(directory));
    let restored = restarted
        .restore(&mut inspector())
        .expect("a refused inspection must not end the start")
        .expect("the refused bytes are what the refresh has to replay");
    assert_eq!(restored.session_id, "rejected");
    assert_eq!(
        restarted.repair_code(),
        Some("account_credential_rejected"),
        "the refusal is still recorded for diagnosis"
    );
    assert!(
        saved(&vault, &slot).is_some(),
        "nothing is erased on a refusal the refresh has not judged yet"
    );
}

#[test]
fn restore_logs_which_stored_slot_was_refused_without_a_value() {
    let directory = Arc::new(MemoryAccountDirectory::new());
    let vault = Arc::new(SessionCredentialStore::default());
    let legacy = legacy();
    let namespace = legacy.namespace();
    directory.retire_legacy(&namespace).unwrap();
    let mut writer =
        CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory.clone()));
    writer
        .replace(
            &credential("old"),
            &mut PendingCredentialWrite::default(),
            &mut inspector(),
        )
        .unwrap();
    let held = directory.pending(&namespace, owner(2, 3)).unwrap();
    directory
        .ready(&namespace, &held, held.owner, false, false)
        .unwrap();
    vault
        .store(
            &legacy.for_account(&held.owner.account.to_string()).unwrap(),
            credential("rejected").encode().unwrap(),
        )
        .unwrap();
    let mut restarted = CredentialPersistence::new(vault, legacy, Some(directory));
    let lines = crate::account::tests::account_log_lines(|| {
        assert!(restarted.restore(&mut inspector()).unwrap().is_some());
    });
    assert_eq!(
        lines,
        [
            "message=restore inspected a stored credential: accepted selected=true record_version=1"
                .to_owned(),
            format!(
                "message=restore inspected a stored credential: refused selected=false \
                 record_version={} code=\"credential_rejected\" retryable=false",
                held.version
            ),
        ],
    );
    assert!(lines.iter().all(|line| !line.contains("secret-")));
}

#[test]
fn two_accounts_restore_without_provider_and_namespaces_do_not_share_retirement() {
    let directory = Arc::new(MemoryAccountDirectory::new());
    let vault = Arc::new(SessionCredentialStore::default());
    let legacy = legacy();
    let namespace = legacy.namespace();
    directory.retire_legacy(&namespace).unwrap();
    let mut writer =
        CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory.clone()));
    writer
        .replace(
            &credential("old"),
            &mut PendingCredentialWrite::default(),
            &mut inspector(),
        )
        .unwrap();
    writer
        .replace(
            &credential("second"),
            &mut PendingCredentialWrite::default(),
            &mut inspector(),
        )
        .unwrap();
    for account in [owner(1, 1), owner(2, 3)] {
        directory
            .with(|r| r.activate_account(&namespace, account.account))
            .unwrap();
        let mut restarted =
            CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory.clone()));
        let restored = restarted.restore(&mut inspector()).unwrap().unwrap();
        assert_eq!(inspector().inspect_credential(&restored).unwrap(), account);
    }
    assert_eq!(directory.snapshot(&namespace).unwrap().records.len(), 2);
    assert!(!directory.snapshot(&"b".repeat(64)).unwrap().legacy_retired);
}

struct FaultDirectory {
    inner: MemoryAccountDirectory,
    fault: Mutex<Option<(&'static str, bool)>>,
}
impl FaultDirectory {
    fn new() -> Self {
        Self {
            inner: MemoryAccountDirectory::new(),
            fault: Mutex::new(None),
        }
    }
    fn arm(&self, name: &'static str, after: bool) {
        *self.fault.lock().unwrap() = Some((name, after));
    }
    fn run<T>(
        &self,
        name: &'static str,
        operation: impl FnOnce() -> Result<T, ServiceError>,
    ) -> Result<T, ServiceError> {
        let fault = {
            let mut armed = self.fault.lock().unwrap();
            if armed.as_ref().is_some_and(|(value, _)| *value == name) {
                armed.take()
            } else {
                None
            }
        };
        if fault == Some((name, false)) {
            return Err(ServiceError::retryable("account_directory_unavailable"));
        }
        let value = operation()?;
        if fault == Some((name, true)) {
            return Err(ServiceError::retryable("account_directory_unavailable"));
        }
        Ok(value)
    }
}
impl AccountDirectory for FaultDirectory {
    fn activate(
        &self,
        n: &str,
        account: koru_account_core::KoruIdentityId,
    ) -> Result<(), ServiceError> {
        self.run("activate", || self.inner.activate(n, account))
    }
    fn snapshot(&self, n: &str) -> Result<AccountDirectorySnapshot, ServiceError> {
        self.run("snapshot", || self.inner.snapshot(n))
    }
    fn pending(&self, n: &str, o: AccountOwner) -> Result<DirectoryRecord, ServiceError> {
        self.run("pending", || self.inner.pending(n, o))
    }
    fn ready(
        &self,
        n: &str,
        r: &DirectoryRecord,
        a: AccountOwner,
        active: bool,
        retire: bool,
    ) -> Result<(), ServiceError> {
        self.run("ready", || self.inner.ready(n, r, a, active, retire))
    }
    fn remove(&self, n: &str, r: &DirectoryRecord) -> Result<(), ServiceError> {
        self.run("remove", || self.inner.remove(n, r))
    }
    fn retire_legacy(&self, n: &str) -> Result<(), ServiceError> {
        self.run("retire", || self.inner.retire_legacy(n))
    }
    fn deactivate(&self, n: &str, o: AccountOwner) -> Result<(), ServiceError> {
        self.run("deactivate", || self.inner.deactivate(n, o))
    }
}
struct FaultVault {
    inner: SessionCredentialStore,
    fault: Mutex<Option<(&'static str, bool)>>,
}
impl FaultVault {
    fn new() -> Self {
        Self {
            inner: SessionCredentialStore::default(),
            fault: Mutex::new(None),
        }
    }
    fn arm(&self, name: &'static str, after: bool) {
        *self.fault.lock().unwrap() = Some((name, after));
    }
    fn run<T>(
        &self,
        name: &'static str,
        operation: impl FnOnce() -> Result<T, koru_oauth_credentials::Error>,
    ) -> Result<T, koru_oauth_credentials::Error> {
        let fault = {
            let mut armed = self.fault.lock().unwrap();
            if armed.as_ref().is_some_and(|(value, _)| *value == name) {
                armed.take()
            } else {
                None
            }
        };
        if fault == Some((name, false)) {
            return Err(koru_oauth_credentials::Error::Unavailable);
        }
        let value = operation()?;
        if fault == Some((name, true)) {
            return Err(koru_oauth_credentials::Error::Unavailable);
        }
        Ok(value)
    }
}
impl CredentialStore for FaultVault {
    fn load(
        &self,
        e: &CredentialEntry,
    ) -> Result<Option<koru_oauth_credentials::SecretString>, koru_oauth_credentials::Error> {
        self.run("load", || self.inner.load(e))
    }
    fn store(
        &self,
        e: &CredentialEntry,
        s: koru_oauth_credentials::SecretString,
    ) -> Result<(), koru_oauth_credentials::Error> {
        self.run("store", || self.inner.store(e, s))
    }
    fn clear(&self, e: &CredentialEntry) -> Result<(), koru_oauth_credentials::Error> {
        self.run("clear", || self.inner.clear(e))
    }
}

#[test]
fn replacement_effect_failures_and_lost_acks_keep_exact_bytes_discoverable() {
    for effect in ["pending", "store", "ready"] {
        for after in [false, true] {
            let directory = Arc::new(FaultDirectory::new());
            let vault = Arc::new(FaultVault::new());
            let legacy = legacy();
            directory.retire_legacy(&legacy.namespace()).unwrap();
            let mut writer =
                CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory.clone()));
            writer
                .replace(
                    &credential("old"),
                    &mut PendingCredentialWrite::default(),
                    &mut inspector(),
                )
                .unwrap();
            if effect == "store" {
                vault.arm(effect, after);
            } else {
                directory.arm(effect, after);
            }
            let mut pending = PendingCredentialWrite::default();
            assert!(
                writer
                    .replace(&credential("new"), &mut pending, &mut inspector())
                    .is_err()
            );
            writer
                .replace(&credential("new"), &mut pending, &mut inspector())
                .unwrap();
            let mut restart =
                CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory));
            assert_eq!(
                restart
                    .restore(&mut inspector())
                    .unwrap()
                    .unwrap()
                    .session_id,
                "new"
            );
            assert_eq!(restart.owner(), Some(owner(1, 2)));
            assert_eq!(
                saved(
                    &vault.inner,
                    &legacy
                        .for_account(&owner(1, 1).account.to_string())
                        .unwrap()
                ),
                Some("new".into())
            );
        }
    }
}

#[test]
fn immediate_restart_after_each_real_write_failure_finds_all_surviving_slots() {
    for adding_second in [false, true] {
        for effect in ["pending", "store", "ready"] {
            for after in [false, true] {
                let directory = Arc::new(FaultDirectory::new());
                let vault = Arc::new(FaultVault::new());
                let legacy = legacy();
                let namespace = legacy.namespace();
                directory.retire_legacy(&namespace).unwrap();
                let mut writer = CredentialPersistence::new(
                    vault.clone(),
                    legacy.clone(),
                    Some(directory.clone()),
                );
                writer
                    .replace(
                        &credential("old"),
                        &mut PendingCredentialWrite::default(),
                        &mut inspector(),
                    )
                    .unwrap();
                if effect == "store" {
                    vault.arm(effect, after);
                } else {
                    directory.arm(effect, after);
                }
                let next = if adding_second { "second" } else { "new" };
                assert!(
                    writer
                        .replace(
                            &credential(next),
                            &mut PendingCredentialWrite::default(),
                            &mut inspector()
                        )
                        .is_err()
                );
                drop(writer); // Process boundary: no living exact retry first.
                let mut restart = CredentialPersistence::new(
                    vault.clone(),
                    legacy.clone(),
                    Some(directory.clone()),
                );
                let _ = restart.restore(&mut inspector()).unwrap();
                let wrote = effect == "ready" || (effect == "store" && after);
                let snapshot = directory.snapshot(&namespace).unwrap();
                for record in &snapshot.records {
                    let stored = vault
                        .load(
                            &legacy
                                .for_account(&record.owner.account.to_string())
                                .unwrap(),
                        )
                        .unwrap()
                        .unwrap();
                    let credential = SessionCredential::decode(&stored).unwrap();
                    assert_eq!(
                        record.owner,
                        inspector().inspect_credential(&credential).unwrap()
                    );
                    assert_eq!(record.state, DirectoryState::Ready);
                }
                assert_eq!(
                    snapshot.records.len(),
                    if adding_second && wrote { 2 } else { 1 }
                );
                let actual = saved(
                    &vault.inner,
                    &legacy
                        .for_account(
                            &owner(if adding_second { 2 } else { 1 }, 1)
                                .account
                                .to_string(),
                        )
                        .unwrap(),
                );
                assert_eq!(
                    actual.as_deref(),
                    if wrote {
                        Some(next)
                    } else if adding_second {
                        None
                    } else {
                        Some("old")
                    }
                );
            }
        }
    }
}

#[test]
fn cancelling_new_account_durability_never_clears_older_account() {
    for effect in ["inspect", "pending", "store", "ready"] {
        for after in [false, true] {
            let directory = Arc::new(FaultDirectory::new());
            let vault = Arc::new(FaultVault::new());
            let legacy = legacy();
            directory.retire_legacy(&legacy.namespace()).unwrap();
            let mut writer =
                CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory.clone()));
            writer
                .replace(
                    &credential("old"),
                    &mut PendingCredentialWrite::default(),
                    &mut inspector(),
                )
                .unwrap();
            let mut inspect = inspector();
            if effect == "inspect" {
                inspect.0.remove("second");
            } else if effect == "store" {
                vault.arm(effect, after);
            } else {
                directory.arm(effect, after);
            }
            let mut pending = PendingCredentialWrite::default();
            assert!(
                writer
                    .replace(&credential("second"), &mut pending, &mut inspect)
                    .is_err()
            );
            writer
                .clear(Some((&credential("second"), &pending)), false)
                .unwrap();
            assert_eq!(
                saved(
                    &vault.inner,
                    &legacy
                        .for_account(&owner(1, 1).account.to_string())
                        .unwrap()
                ),
                Some("old".into())
            );
            assert!(
                saved(
                    &vault.inner,
                    &legacy
                        .for_account(&owner(2, 3).account.to_string())
                        .unwrap()
                )
                .is_none()
            );
            let mut restart = CredentialPersistence::new(vault, legacy, Some(directory));
            assert!(restart.restore(&mut inspector()).unwrap().is_none());
        }
    }
}

#[test]
fn unavailable_vault_preserves_pending_and_ready_and_legacy_retirement_retries() {
    for effect in ["pending", "store", "ready", "clear"] {
        for after in [false, true] {
            let directory = Arc::new(FaultDirectory::new());
            let vault = Arc::new(FaultVault::new());
            let legacy = legacy();
            vault
                .store(&legacy, credential("old").encode().unwrap())
                .unwrap();
            if ["store", "clear"].contains(&effect) {
                vault.arm(effect, after);
            } else {
                directory.arm(effect, after);
            }
            let mut writer =
                CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory.clone()));
            let result = writer.restore(&mut inspector());
            assert!(result.is_err() || writer.repair_code().is_some());
            let mut restart =
                CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory.clone()));
            assert_eq!(
                restart
                    .restore(&mut inspector())
                    .unwrap()
                    .unwrap()
                    .session_id,
                "old"
            );
            assert!(
                directory
                    .snapshot(&legacy.namespace())
                    .unwrap()
                    .legacy_retired
            );
            assert!(saved(&vault.inner, &legacy).is_none());
        }
    }
    let directory = Arc::new(FaultDirectory::new());
    let vault = Arc::new(FaultVault::new());
    let legacy = legacy();
    let record = directory.pending(&legacy.namespace(), owner(1, 1)).unwrap();
    vault.arm("load", false);
    let mut restart = CredentialPersistence::new(vault, legacy.clone(), Some(directory.clone()));
    assert!(restart.restore(&mut inspector()).is_err());
    assert_eq!(
        directory.snapshot(&legacy.namespace()).unwrap().records,
        vec![record]
    );
}

#[test]
fn lost_remove_ack_is_idempotent_and_cannot_remove_a_newer_record() {
    let directory = Arc::new(FaultDirectory::new());
    let vault = Arc::new(FaultVault::new());
    let legacy = legacy();
    let namespace = legacy.namespace();
    directory.retire_legacy(&namespace).unwrap();
    let mut writer = CredentialPersistence::new(vault, legacy, Some(directory.clone()));
    writer
        .replace(
            &credential("old"),
            &mut PendingCredentialWrite::default(),
            &mut inspector(),
        )
        .unwrap();
    let old = directory.snapshot(&namespace).unwrap().records[0].clone();
    directory.arm("remove", true);
    assert!(writer.clear(None, true).is_err());
    writer.clear(None, true).unwrap();
    let new = directory.pending(&namespace, owner(1, 2)).unwrap();
    assert!(directory.remove(&namespace, &old).is_err());
    assert_eq!(directory.snapshot(&namespace).unwrap().records, vec![new]);
}

#[derive(Clone)]
struct WorkerFlow {
    starts: Arc<std::sync::atomic::AtomicUsize>,
    inspection_unavailable: Arc<std::sync::atomic::AtomicBool>,
    logouts: std::sync::mpsc::Sender<String>,
}
impl AccountService for WorkerFlow {
    fn fork(&self) -> Option<Box<dyn AccountService>> {
        Some(Box::new(self.clone()))
    }
    fn execute(
        &mut self,
        generation: u64,
        command: AccountCommand,
        restored: Option<&SessionCredential>,
        _: &Cancel,
        _: &mut dyn FnMut(ServiceProgress),
    ) -> Result<ServiceOutcome, ServiceError> {
        use crate::account::{CredentialChange, signed_out};
        use koru_agent_wire::{AccountOperationResult, AccountStage, AccountState};
        let mut state = AccountState {
            generation,
            stage: AccountStage::SignedIn,
            capability: true,
            ..Default::default()
        };
        let (credential, result) = match command {
            AccountCommand::StartGoogle { .. } => {
                self.starts
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                (
                    CredentialChange::Replace(credential("second")),
                    AccountOperationResult::SignedIn,
                )
            }
            AccountCommand::Logout => {
                self.logouts
                    .send(restored.map(|r| r.session_id.clone()).unwrap_or_default())
                    .unwrap();
                state = signed_out(generation);
                (CredentialChange::Remove, AccountOperationResult::SignedOut)
            }
            _ => (CredentialChange::Keep, AccountOperationResult::SignedIn),
        };
        Ok(ServiceOutcome {
            state,
            credential,
            result,
            follow_up: None,
        })
    }
    fn inspect_credential(
        &mut self,
        value: &SessionCredential,
    ) -> Result<AccountOwner, ServiceError> {
        if value.session_id == "rejected" {
            return Err(ServiceError::terminal("credential_rejected"));
        }
        if value.session_id == "second"
            && self
                .inspection_unavailable
                .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(ServiceError::retryable("account_unavailable"));
        }
        if value.session_id == "legacy-second" {
            return Ok(owner(2, 3));
        }
        inspector().inspect_credential(value)
    }
}
struct FixedClock(std::time::Instant);
impl crate::ports::Clock for FixedClock {
    fn now_millis(&self) -> Option<i64> {
        Some(1_000)
    }
    fn monotonic(&self) -> std::time::Instant {
        self.0
    }
}
fn wait_stage(
    control: &crate::account::AccountControl,
    events: &std::sync::mpsc::Receiver<crate::account::WorkerEvent>,
    stage: koru_agent_wire::AccountStage,
) {
    while control.state().stage != stage {
        events
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
    }
}

#[test]
fn shared_worker_cancels_exact_pending_owner_at_every_pre_ack_boundary() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    for effect in ["inspect", "pending", "store", "ready"] {
        for after in [false, true] {
            for cancel_attempt in [false, true] {
                let directory = Arc::new(FaultDirectory::new());
                let vault = Arc::new(FaultVault::new());
                let legacy = legacy();
                let namespace = legacy.namespace();
                directory.retire_legacy(&namespace).unwrap();
                let mut setup = CredentialPersistence::new(
                    vault.clone(),
                    legacy.clone(),
                    Some(directory.clone()),
                );
                setup
                    .replace(
                        &credential("old"),
                        &mut PendingCredentialWrite::default(),
                        &mut inspector(),
                    )
                    .unwrap();
                let starts = Arc::new(AtomicUsize::new(0));
                let unavailable = Arc::new(AtomicBool::new(false));
                let (logout, logouts) = std::sync::mpsc::channel();
                let (notify, events) = std::sync::mpsc::channel();
                let control = crate::account::AccountControl::start_with_directory(
                    WorkerFlow {
                        starts: starts.clone(),
                        inspection_unavailable: unavailable.clone(),
                        logouts: logout,
                    },
                    vault.clone(),
                    legacy.clone(),
                    Some(directory.clone()),
                    Arc::new(FixedClock(std::time::Instant::now())),
                    true,
                    move |event| {
                        let _ = notify.send(event);
                    },
                );
                wait_stage(&control, &events, koru_agent_wire::AccountStage::SignedIn);
                if effect == "inspect" {
                    unavailable.store(true, Ordering::SeqCst);
                } else if effect == "store" {
                    vault.arm(effect, after);
                } else {
                    directory.arm(effect, after);
                }
                let generation = control
                    .enqueue(
                        7,
                        AccountCommand::AddAccount {
                            locale: "en".into(),
                        },
                    )
                    .unwrap();
                wait_stage(&control, &events, koru_agent_wire::AccountStage::Failed);
                let requested = if cancel_attempt {
                    AccountCommand::Cancel { generation }
                } else {
                    AccountCommand::Logout
                };
                let terminal_generation = control.enqueue(7, requested).unwrap();
                let result =
                    crate::account::tests::terminal_result(&events, 7, terminal_generation);
                assert!(matches!(
                    result,
                    koru_agent_wire::AccountOperationResult::SignedOut
                        | koru_agent_wire::AccountOperationResult::Cancelled
                ));
                assert_eq!(
                    logouts
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .unwrap(),
                    "second"
                );
                assert_eq!(starts.load(Ordering::SeqCst), 1);
                assert_eq!(
                    saved(
                        &vault.inner,
                        &legacy
                            .for_account(&owner(1, 1).account.to_string())
                            .unwrap()
                    ),
                    Some("old".into())
                );
                assert!(
                    saved(
                        &vault.inner,
                        &legacy
                            .for_account(&owner(2, 3).account.to_string())
                            .unwrap()
                    )
                    .is_none()
                );
                assert_eq!(
                    directory.snapshot(&namespace).unwrap().active,
                    Some(owner(1, 1).account),
                    "cancelling an Add attempt preserves the prior durable selection"
                );
                drop(control);
            }
        }
    }
}

struct LegacyUnavailableVault {
    inner: SessionCredentialStore,
    legacy: CredentialEntry,
    unavailable: std::sync::atomic::AtomicBool,
}
impl CredentialStore for LegacyUnavailableVault {
    fn load(
        &self,
        entry: &CredentialEntry,
    ) -> Result<Option<koru_oauth_credentials::SecretString>, koru_oauth_credentials::Error> {
        if entry == &self.legacy && self.unavailable.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(koru_oauth_credentials::Error::Unavailable);
        }
        self.inner.load(entry)
    }
    fn store(
        &self,
        entry: &CredentialEntry,
        secret: koru_oauth_credentials::SecretString,
    ) -> Result<(), koru_oauth_credentials::Error> {
        self.inner.store(entry, secret)
    }
    fn clear(&self, entry: &CredentialEntry) -> Result<(), koru_oauth_credentials::Error> {
        self.inner.clear(entry)
    }
}

#[test]
fn unresolved_legacy_blocks_new_signin_even_when_another_account_restores() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    for usable_active in [false, true] {
        let directory = Arc::new(FaultDirectory::new());
        let legacy = legacy();
        let namespace = legacy.namespace();
        let vault = Arc::new(LegacyUnavailableVault {
            inner: SessionCredentialStore::default(),
            legacy: legacy.clone(),
            unavailable: AtomicBool::new(true),
        });
        vault
            .inner
            .store(&legacy, credential("legacy-second").encode().unwrap())
            .unwrap();
        if usable_active {
            let row = directory.pending(&namespace, owner(1, 1)).unwrap();
            directory
                .ready(&namespace, &row, row.owner, true, false)
                .unwrap();
            vault
                .store(
                    &legacy.for_account(&row.owner.account.to_string()).unwrap(),
                    credential("old").encode().unwrap(),
                )
                .unwrap();
        }
        let starts = Arc::new(AtomicUsize::new(0));
        let (logout, logouts) = std::sync::mpsc::channel();
        let (notify, events) = std::sync::mpsc::channel();
        let control = crate::account::AccountControl::start_with_directory(
            WorkerFlow {
                starts: starts.clone(),
                inspection_unavailable: Arc::new(AtomicBool::new(false)),
                logouts: logout,
            },
            vault.clone(),
            legacy.clone(),
            Some(directory.clone()),
            Arc::new(FixedClock(std::time::Instant::now())),
            true,
            move |event| {
                let _ = notify.send(event);
            },
        );
        wait_stage(
            &control,
            &events,
            if usable_active {
                koru_agent_wire::AccountStage::SignedIn
            } else {
                koru_agent_wire::AccountStage::Failed
            },
        );
        control
            .enqueue(
                7,
                AccountCommand::AddAccount {
                    locale: "en".into(),
                },
            )
            .unwrap();
        // A Status round trip is ordered behind the attempted sign-in and proves
        // it did not reach the provider, without a timing/sleep assertion.
        let status = control.enqueue(8, AccountCommand::Status).unwrap();
        let _ = crate::account::tests::terminal_result(&events, 8, status);
        assert_eq!(starts.load(Ordering::SeqCst), 0);
        assert!(!directory.snapshot(&namespace).unwrap().legacy_retired);
        vault.unavailable.store(false, Ordering::SeqCst);
        // Explicit Retry resumes the migration gate; a fresh user action then
        // owns sign-in if no original provider command was admitted.
        let retry = control.enqueue(7, AccountCommand::Retry).unwrap();
        let _ = crate::account::tests::terminal_result(&events, 7, retry);
        if starts.load(Ordering::SeqCst) == 0 {
            let signin = control
                .enqueue(
                    7,
                    AccountCommand::StartGoogle {
                        locale: "en".into(),
                    },
                )
                .unwrap();
            let _ = crate::account::tests::terminal_result(&events, 7, signin);
        }
        assert!(directory.snapshot(&namespace).unwrap().legacy_retired);
        let logout = control.enqueue(7, AccountCommand::Logout).unwrap();
        let _ = crate::account::tests::terminal_result(&events, 7, logout);
        loop {
            let session = logouts
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap();
            if session == "second" {
                break;
            }
            assert_eq!(
                session, "legacy-second",
                "only the superseded imported session may be retired first"
            );
        }
        assert!(saved(&vault.inner, &legacy).is_none());
        let mut restart =
            CredentialPersistence::new(vault.clone(), legacy, Some(directory.clone()));
        assert!(restart.restore(&mut inspector()).unwrap().is_none());
        assert!(
            directory
                .snapshot(&namespace)
                .unwrap()
                .records
                .iter()
                .all(|r| r.owner.account != owner(2, 3).account)
        );
        drop(control);
    }
}

#[test]
fn rejected_legacy_and_missing_or_rejected_ready_slot_allow_fresh_signin() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    for situation in [
        "legacy_rejected",
        "legacy_corrupt",
        "ready_missing",
        "ready_rejected",
        "ready_unavailable",
        "ready_corrupt",
    ] {
        let directory = Arc::new(FaultDirectory::new());
        let vault = Arc::new(SessionCredentialStore::default());
        let legacy = legacy();
        let namespace = legacy.namespace();
        if situation.starts_with("legacy_") {
            vault
                .store(
                    &legacy,
                    if situation == "legacy_corrupt" {
                        koru_oauth_credentials::SecretString::new("corrupt-payload").unwrap()
                    } else {
                        credential("rejected").encode().unwrap()
                    },
                )
                .unwrap();
        } else {
            let record = directory.pending(&namespace, owner(2, 3)).unwrap();
            directory
                .ready(&namespace, &record, record.owner, true, true)
                .unwrap();
            let entry = legacy
                .for_account(&record.owner.account.to_string())
                .unwrap();
            match situation {
                "ready_rejected" => vault
                    .store(&entry, credential("rejected").encode().unwrap())
                    .unwrap(),
                "ready_unavailable" => vault
                    .store(&entry, credential("unavailable").encode().unwrap())
                    .unwrap(),
                "ready_corrupt" => vault
                    .store(
                        &entry,
                        koru_oauth_credentials::SecretString::new("invalid-json").unwrap(),
                    )
                    .unwrap(),
                _ => {}
            }
        }
        let starts = Arc::new(AtomicUsize::new(0));
        let (logout, _) = std::sync::mpsc::channel();
        let (notify, events) = std::sync::mpsc::channel();
        let control = crate::account::AccountControl::start_with_directory(
            WorkerFlow {
                starts: starts.clone(),
                inspection_unavailable: Arc::new(AtomicBool::new(false)),
                logouts: logout,
            },
            vault.clone(),
            legacy.clone(),
            Some(directory.clone()),
            Arc::new(FixedClock(std::time::Instant::now())),
            true,
            move |event| {
                let _ = notify.send(event);
            },
        );
        wait_stage(
            &control,
            &events,
            match situation {
                _ if situation.starts_with("legacy_") => koru_agent_wire::AccountStage::SignedOut,
                // A refused inspection no longer ends the start. The bytes go
                // to the scheduled retry, whose refresh is the only call that
                // can tell a revoked session from a rotation that never
                // reached the vault, and this fixture's refresh answers.
                "ready_rejected" => koru_agent_wire::AccountStage::SignedIn,
                _ => koru_agent_wire::AccountStage::Failed,
            },
        );
        if situation == "ready_rejected" {
            // The refusal is still recorded, as the repair code beside a
            // signed-in stage that the host's `actionable_failure_code`
            // deliberately does not act on. What must not happen is a
            // `Failed`/`SignedOut` stage carrying it, which is the sign-in
            // surface.
            let state = control.state();
            assert_eq!(state.stage, koru_agent_wire::AccountStage::SignedIn);
            assert_eq!(
                state.failure_code.as_deref(),
                Some("account_credential_rejected")
            );
        }
        assert!(directory.snapshot(&namespace).unwrap().legacy_retired);
        if situation == "ready_unavailable" {
            control.enqueue(7, AccountCommand::Retry).unwrap();
            let status = control.enqueue(8, AccountCommand::Status).unwrap();
            let _ = crate::account::tests::terminal_result(&events, 8, status);
            assert!(
                control.requester_active(7),
                "discovery retry was rebound to a nonzero generation"
            );
        }
        let signin = control
            .enqueue(
                7,
                AccountCommand::StartGoogle {
                    locale: "en".into(),
                },
            )
            .unwrap();
        assert!(matches!(
            crate::account::tests::terminal_result(&events, 7, signin),
            koru_agent_wire::AccountOperationResult::SignedIn
        ));
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        if !situation.starts_with("legacy_") {
            assert_eq!(
                directory.snapshot(&namespace).unwrap().records.len(),
                1,
                "repair reconnects the same held account rather than adding another"
            );
        }
        assert!(saved(&vault, &legacy).is_none());
        drop(control);
    }
}

#[test]
fn anonymous_add_reconciles_recovered_legacy_without_adopting_its_owner() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let directory = Arc::new(FaultDirectory::new());
    let legacy = legacy();
    let vault = Arc::new(LegacyUnavailableVault {
        inner: SessionCredentialStore::default(),
        legacy: legacy.clone(),
        unavailable: AtomicBool::new(true),
    });
    vault
        .inner
        .store(&legacy, credential("old").encode().unwrap())
        .unwrap();
    let (logout, _logouts) = std::sync::mpsc::channel();
    let (notify, events) = std::sync::mpsc::channel();
    let starts = Arc::new(AtomicUsize::new(0));
    let control = crate::account::AccountControl::start_with_directory(
        WorkerFlow {
            starts: starts.clone(),
            inspection_unavailable: Arc::new(AtomicBool::new(false)),
            logouts: logout,
        },
        vault.clone(),
        legacy.clone(),
        Some(directory.clone()),
        Arc::new(FixedClock(std::time::Instant::now())),
        true,
        move |event| {
            let _ = notify.send(event);
        },
    );
    wait_stage(&control, &events, koru_agent_wire::AccountStage::Failed);
    control
        .enqueue(
            7,
            AccountCommand::AddAccount {
                locale: "en".into(),
            },
        )
        .unwrap();
    let status = control.enqueue(8, AccountCommand::Status).unwrap();
    let _ = crate::account::tests::terminal_result(&events, 8, status);
    let anonymous = control.state().active_scope;
    assert!(control.state().active_account_id.is_none());
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    vault.unavailable.store(false, Ordering::SeqCst);
    let retry = control.enqueue(7, AccountCommand::Retry).unwrap();
    let _ = crate::account::tests::terminal_result(&events, 7, retry);
    assert_eq!(control.state().active_scope, anonymous);
    assert!(
        control.state().active_account_id.is_none(),
        "legacy migration cannot take over the Add intent"
    );
    assert!(
        control
            .state()
            .held_accounts
            .iter()
            .any(|held| held.account_id == owner(1, 1).account.to_string()
                && held.scope_id != anonymous)
    );
    let signin = control
        .enqueue(
            7,
            AccountCommand::StartGoogle {
                locale: "en".into(),
            },
        )
        .unwrap();
    assert!(matches!(
        crate::account::tests::terminal_result(&events, 7, signin),
        koru_agent_wire::AccountOperationResult::SignedIn
    ));
    assert_eq!(control.state().active_scope, anonymous);
    assert_eq!(
        control.state().active_account_id,
        Some(owner(2, 3).account.to_string())
    );
    assert!(
        control
            .state()
            .held_accounts
            .iter()
            .any(|held| held.account_id == owner(1, 1).account.to_string())
    );
    assert_eq!(
        saved(
            &vault.inner,
            &legacy
                .for_account(&owner(1, 1).account.to_string())
                .unwrap()
        ),
        Some("old".into())
    );
    assert_eq!(starts.load(Ordering::SeqCst), 1);
}

#[test]
fn anonymous_add_after_legacy_retirement_does_not_inspect_held_slots() {
    let directory = Arc::new(FaultDirectory::new());
    let vault = Arc::new(SessionCredentialStore::default());
    let legacy = legacy();
    let row = directory.pending(&legacy.namespace(), owner(1, 1)).unwrap();
    directory
        .ready(&legacy.namespace(), &row, row.owner, true, true)
        .unwrap();
    vault
        .store(
            &legacy.for_account(&row.owner.account.to_string()).unwrap(),
            credential("old").encode().unwrap(),
        )
        .unwrap();
    let mut rejecting = inspector();
    rejecting.0.clear();
    let mut persistence =
        CredentialPersistence::new(vault, legacy, Some(directory)).for_selection(None);
    assert!(persistence.restore(&mut rejecting).unwrap().is_none());
    assert!(persistence.record().is_none());
    assert!(persistence.take_discovered_records().is_empty());
}

#[test]
fn valid_legacy_repairs_conclusively_unusable_destination_but_never_unreadable_vault() {
    for state in ["missing", "rejected", "corrupt", "unavailable"] {
        let directory = Arc::new(FaultDirectory::new());
        let legacy = legacy();
        let namespace = legacy.namespace();
        let entry = legacy
            .for_account(&owner(1, 2).account.to_string())
            .unwrap();
        let vault = Arc::new(LegacyUnavailableVault {
            inner: SessionCredentialStore::default(),
            legacy: entry.clone(),
            unavailable: std::sync::atomic::AtomicBool::new(state == "unavailable"),
        });
        let row = directory.pending(&namespace, owner(1, 2)).unwrap();
        directory
            .ready(&namespace, &row, row.owner, true, false)
            .unwrap();
        vault
            .inner
            .store(&legacy, credential("old").encode().unwrap())
            .unwrap();
        match state {
            "rejected" => vault
                .inner
                .store(&entry, credential("rejected").encode().unwrap())
                .unwrap(),
            "corrupt" => vault
                .inner
                .store(
                    &entry,
                    koru_oauth_credentials::SecretString::new("corrupt-payload").unwrap(),
                )
                .unwrap(),
            "unavailable" => vault
                .inner
                .store(&entry, credential("new").encode().unwrap())
                .unwrap(),
            _ => {}
        }
        let mut restore =
            CredentialPersistence::new(vault.clone(), legacy.clone(), Some(directory.clone()));
        let result = restore.restore(&mut inspector());
        if state == "unavailable" {
            assert!(result.is_err());
            assert!(!restore.legacy_ready());
            assert_eq!(saved(&vault.inner, &legacy), Some("old".into()));
            assert_eq!(saved(&vault.inner, &entry), Some("new".into()));
            assert_eq!(
                directory.snapshot(&namespace).unwrap().records[0],
                DirectoryRecord {
                    state: DirectoryState::Ready,
                    ..row
                }
            );
        } else {
            assert_eq!(result.unwrap().unwrap().session_id, "old");
            assert!(restore.legacy_ready());
            assert!(restore.repair_code().is_none());
            assert_eq!(restore.owner(), Some(owner(1, 1)));
            assert_eq!(saved(&vault.inner, &entry), Some("old".into()));
            assert!(saved(&vault.inner, &legacy).is_none());
        }
    }
}

/// What `refresh_retry_key` used to be: blake3 over a fixed prefix and the
/// refresh token itself. Holding the token was therefore enough to compute a
/// valid exact retry and collect the sealed new pair out of the service's
/// window, which is why Q11 step 2 replaced it with random bytes the device
/// persists. Kept here as the value the stored key must never equal.
fn derived_from_token(refresh_token: &str) -> [u8; 32] {
    let mut input = Vec::with_capacity(38 + refresh_token.len());
    input.extend_from_slice(b"koru.account.refresh-retry.v1\0");
    input.extend_from_slice(refresh_token.as_bytes());
    *blake3::hash(&input).as_bytes()
}

/// One account row, ready, holding `session` — written the way the code
/// writes it, so the slot carries the retry key its next rotation must
/// present. Answers the vault, the directory and that key.
fn signed_in(
    session: &str,
) -> (
    Arc<SessionCredentialStore>,
    Arc<MemoryAccountDirectory>,
    [u8; 32],
) {
    let vault = Arc::new(SessionCredentialStore::default());
    let directory = Arc::new(MemoryAccountDirectory::new());
    directory.retire_legacy(&legacy().namespace()).unwrap();
    let mut persistence =
        CredentialPersistence::new(vault.clone(), legacy(), Some(directory.clone()));
    persistence
        .replace(
            &credential(session),
            &mut PendingCredentialWrite::default(),
            &mut inspector(),
        )
        .unwrap();
    let key = persistence
        .refresh_retry_key()
        .expect("a stored credential is armed to rotate");
    (vault, directory, key)
}

/// Q11 step 2. The key a rotation presents is random and lives in the slot
/// beside the token it rotates, so it is durable before the request goes out
/// and holding the token is not enough to produce it. The slot is still the
/// credential it always was — `SessionCredential::decode` has to keep reading
/// it, because every other reader of the vault wants only that.
#[test]
fn a_stored_slot_is_still_a_credential_and_holds_a_key_the_token_cannot_give() {
    let (vault, _, first) = signed_in("old");
    let entry = legacy()
        .for_account(&owner(1, 1).account.to_string())
        .unwrap();
    let stored = vault.load(&entry).unwrap().unwrap();
    assert_eq!(
        SessionCredential::decode(&stored).unwrap().refresh_token,
        "secret-old",
        "the slot restores as the credential it is"
    );
    assert_eq!(decode_retry_key(&stored), Some(first));
    assert_ne!(
        first,
        derived_from_token("secret-old"),
        "a stolen refresh token must not be enough to compute its retry key"
    );

    let (_, _, second) = signed_in("old");
    assert_ne!(
        first, second,
        "each stored pair gets its own key, so one rotation's retry buys nothing elsewhere"
    );
}

/// The maintainer's 5.3-second window, in the part of it this change does not
/// remove: the service has answered a rotation and the device stops inside the
/// owner round trip `replace` still owes before the vault write. The new pair
/// never reaches the vault — but it is not lost, because the next process
/// presents the *same* token under the *same* key and the service answers the
/// same rotation again. A freshly minted key would be a different rotation,
/// and the pair really would be gone.
#[test]
fn a_rotation_lost_before_the_vault_write_is_replayed_under_the_same_key() {
    let (vault, directory, armed) = signed_in("old");
    let entry = legacy()
        .for_account(&owner(1, 1).account.to_string())
        .unwrap();

    // A new process restores the stored pair and is armed with the key the
    // vault holds beside it; that is the key its refresh will present.
    let mut restarted =
        CredentialPersistence::new(vault.clone(), legacy(), Some(directory.clone()));
    assert_eq!(
        restarted
            .restore(&mut inspector())
            .unwrap()
            .unwrap()
            .refresh_token,
        "secret-old"
    );
    assert_eq!(restarted.refresh_retry_key(), Some(armed));

    // The service answers the rotation; the device stops in the owner round
    // trip, so nothing is written.
    let mut stalled = inspector();
    stalled.0.remove("new");
    assert!(
        restarted
            .replace(
                &credential("new"),
                &mut PendingCredentialWrite::default(),
                &mut stalled,
            )
            .is_err()
    );
    let stored = vault.load(&entry).unwrap().unwrap();
    assert_eq!(
        SessionCredential::decode(&stored).unwrap().refresh_token,
        "secret-old",
        "the superseded pair is what survived the stop"
    );

    // The process after the stop: same token, same key, so the refresh route
    // replays that one rotation instead of asking for a new one.
    let mut after_stop = CredentialPersistence::new(vault.clone(), legacy(), Some(directory));
    assert_eq!(
        after_stop
            .restore(&mut inspector())
            .unwrap()
            .unwrap()
            .refresh_token,
        "secret-old"
    );
    assert_eq!(
        after_stop.refresh_retry_key(),
        Some(armed),
        "the replay presents the key the lost rotation presented"
    );

    // The replay's answer is made durable, and it brings its own next key.
    after_stop
        .replace(
            &credential("new"),
            &mut PendingCredentialWrite::default(),
            &mut inspector(),
        )
        .unwrap();
    let rotated = vault
        .load(
            &legacy()
                .for_account(&owner(1, 2).account.to_string())
                .unwrap(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        SessionCredential::decode(&rotated).unwrap().refresh_token,
        "secret-new"
    );
    assert_ne!(
        decode_retry_key(&rotated),
        Some(armed),
        "the pair that replaced it does not reuse the spent key"
    );
}

/// A slot written by a build that had no key of its own. Restore gives it one
/// and makes it durable before handing the credential out, because a refresh
/// of these bytes is the very next thing that happens; a second start reads
/// that same key back rather than minting another.
#[test]
fn a_slot_written_without_a_key_is_given_one_durably_at_restore() {
    let vault = Arc::new(SessionCredentialStore::default());
    let directory = Arc::new(MemoryAccountDirectory::new());
    let namespace = legacy().namespace();
    directory.retire_legacy(&namespace).unwrap();
    let row = directory.pending(&namespace, owner(1, 1)).unwrap();
    let entry = legacy()
        .for_account(&owner(1, 1).account.to_string())
        .unwrap();
    vault
        .store(&entry, credential("old").encode().unwrap())
        .unwrap();
    directory
        .ready(&namespace, &row, owner(1, 1), true, false)
        .unwrap();
    assert_eq!(
        decode_retry_key(&vault.load(&entry).unwrap().unwrap()),
        None
    );

    let mut first = CredentialPersistence::new(vault.clone(), legacy(), Some(directory.clone()));
    first.restore(&mut inspector()).unwrap().unwrap();
    let minted = first
        .refresh_retry_key()
        .expect("restore mints a missing key");
    assert_eq!(
        decode_retry_key(&vault.load(&entry).unwrap().unwrap()),
        Some(minted),
        "the key is durable before the credential is handed out"
    );

    let mut second = CredentialPersistence::new(vault.clone(), legacy(), Some(directory));
    second.restore(&mut inspector()).unwrap().unwrap();
    assert_eq!(
        second.refresh_retry_key(),
        Some(minted),
        "a later start reads the stored key instead of minting a second one"
    );
}

/// A retry key belongs to one stored pair. Writing the same pair again — a
/// retried durability step, or a refresh the service answered without
/// rotating — keeps it, because a rotation already in flight under that key
/// has to stay replayable; only a new pair earns a new key.
#[test]
fn storing_the_same_pair_keeps_its_retry_key_and_a_new_pair_earns_its_own() {
    let vault = Arc::new(SessionCredentialStore::default());
    let directory = Arc::new(MemoryAccountDirectory::new());
    directory.retire_legacy(&legacy().namespace()).unwrap();
    let mut persistence =
        CredentialPersistence::new(vault.clone(), legacy(), Some(directory.clone()));
    let entry = legacy()
        .for_account(&owner(1, 1).account.to_string())
        .unwrap();

    persistence
        .replace(
            &credential("old"),
            &mut PendingCredentialWrite::default(),
            &mut inspector(),
        )
        .unwrap();
    let first = persistence.refresh_retry_key().unwrap();
    let bytes = vault.load(&entry).unwrap().unwrap().expose().to_owned();

    persistence
        .replace(
            &credential("old"),
            &mut PendingCredentialWrite::default(),
            &mut inspector(),
        )
        .unwrap();
    assert_eq!(persistence.refresh_retry_key(), Some(first));
    assert_eq!(
        vault.load(&entry).unwrap().unwrap().expose(),
        bytes,
        "storing the pair it already holds leaves the slot as it was"
    );

    persistence
        .replace(
            &credential("second"),
            &mut PendingCredentialWrite::default(),
            &mut inspector(),
        )
        .unwrap();
    assert_ne!(
        persistence.refresh_retry_key(),
        Some(first),
        "a different pair rotates under a key of its own"
    );
}
