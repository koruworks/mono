// SPDX-License-Identifier: MPL-2.0

//! Shared non-secret discovery and ordered vault durability. This module owns
//! no retry loop: the existing account worker retains and retries each effect.
use super::{AccountService, ServiceError};
use crate::identity::{
    AccountDirectorySnapshot, AccountOwner, DirectoryRecord, DirectoryState, SqliteTrustRepository,
};
use crate::ports::{Entropy, SystemEntropy};
use koru_oauth_credentials::{CredentialEntry, CredentialStore, SecretString, SessionCredential};
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
    entropy: Arc<dyn Entropy>,
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
    /// The retry key read back from, or written into, the slot this
    /// persistence last restored or replaced. It is what a refresh of those
    /// stored bytes must present, and `None` means no key is durable for them,
    /// so a rotation performed under one could not be replayed.
    retry_key: Option<[u8; 32]>,
    /// Which stored pair `retry_key` belongs to, as a digest rather than a
    /// second copy of the secret. A key belongs to one token: a write that
    /// stores the same token again keeps it, so a rotation already in flight
    /// under it stays replayable, and only a new pair earns a new key.
    armed_pair: Option<[u8; 32]>,
    /// The session whose credential the active slot holds. A replacement for
    /// the *same* session is a rotation: its owner and its slot are already
    /// known, so [`Self::replace`] does not ask the service who owns it.
    active_session: Option<String>,
}

/// Exact replacement state stays in PendingDurability until all three effects
/// acknowledge. Once prepared, retries never derive the destination anew.
#[derive(Default)]
pub struct PendingCredentialWrite {
    record: Option<DirectoryRecord>,
    vault_acknowledged: bool,
    /// Minted once per replacement, so a retry of a delayed vault write stores
    /// the same key the first attempt did rather than a second random one.
    retry_key: Option<[u8; 32]>,
}

/// What one credential slot holds: the session credential's own JSON, plus the
/// random key a refresh of *that* token must present as its idempotency key.
///
/// The two live in one vault value on purpose. The key is written in the same
/// acknowledged write as the token it belongs to, so it is durable before any
/// refresh can present it; clearing the slot clears both; and a process that
/// restarts mid-rotation reads the key out of the very slot it reads the token
/// from, which is what lets it replay the rotation the service already
/// performed instead of losing it. `SessionCredential::decode` ignores the
/// extra member, so every reader that wants only the credential is unchanged —
/// `a_stored_slot_is_still_a_credential_and_holds_a_key_the_token_cannot_give`
/// holds that.
#[derive(serde::Serialize)]
struct StoredSlot<'a> {
    #[serde(flatten)]
    credential: &'a SessionCredential,
    refresh_retry_key: String,
}

#[derive(serde::Deserialize)]
struct StoredRetryKey {
    refresh_retry_key: Option<String>,
}

pub(crate) fn encode_slot(
    credential: &SessionCredential,
    retry_key: &[u8; 32],
) -> Result<SecretString, ServiceError> {
    let value = serde_json::to_string(&StoredSlot {
        credential,
        refresh_retry_key: base64::engine::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            retry_key,
        ),
    })
    .map_err(|_| ServiceError::terminal("credential_invalid"))?;
    SecretString::new(value).map_err(|_| ServiceError::terminal("credential_invalid"))
}

/// Which pair a retry key belongs to. A digest, so remembering it never means
/// holding the secret twice.
fn pair_fingerprint(credential: &SessionCredential) -> [u8; 32] {
    let mut input = Vec::new();
    input.extend_from_slice(credential.session_id.as_bytes());
    input.push(0);
    input.extend_from_slice(credential.refresh_token.as_bytes());
    *blake3::hash(&input).as_bytes()
}

/// A slot written by a build that had no retry key of its own carries none;
/// restore mints one for it rather than refusing the session.
pub(super) fn decode_retry_key(secret: &SecretString) -> Option<[u8; 32]> {
    let stored: StoredRetryKey = serde_json::from_str(secret.expose()).ok()?;
    base64::engine::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        stored.refresh_retry_key?,
    )
    .ok()?
    .try_into()
    .ok()
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
            entropy: Arc::new(SystemEntropy),
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
            retry_key: None,
            armed_pair: None,
            active_session: None,
        }
    }
    pub fn owner(&self) -> Option<AccountOwner> {
        self.active.as_ref().map(|r| r.owner)
    }
    /// The durable idempotency key for a refresh of the credential this
    /// persistence last handed out, which the worker arms on the service
    /// before it can send one. `None` means no key reached the vault, and the
    /// refresh then refuses rather than performing a rotation it could never
    /// replay.
    pub fn refresh_retry_key(&self) -> Option<[u8; 32]> {
        self.retry_key
    }
    /// The armed key, topping the slot up if nothing is durable for these
    /// bytes yet — a slot written by a build that had no key of its own, or
    /// one whose top-up met a vault that was briefly unavailable. Already
    /// armed, it answers without touching the vault.
    pub fn ensure_refresh_retry_key(&mut self, credential: &SessionCredential) -> Option<[u8; 32]> {
        if self.retry_key.is_none() {
            let entry = match &self.active {
                Some(record) => self.entry(record).ok()?,
                None => self.legacy.clone(),
            };
            let stored = self
                .load_slot_keyed(&entry)
                .ok()
                .flatten()
                .and_then(|(_, key)| key);
            self.arm_retry_key(&entry, credential, stored);
        }
        self.retry_key
    }
    fn mint_retry_key(&self) -> [u8; 32] {
        let mut key = [0u8; 32];
        self.entropy.fill_bytes(&mut key);
        key
    }
    /// Arm the key a credential restore is about to hand out will rotate
    /// under: the one its slot held, or, for a slot written by a build that
    /// kept no key, a fresh one written into that slot now — before the
    /// credential is handed to anything that could refresh it. A key that
    /// cannot be made durable arms nothing: the session stays usable for
    /// everything that is not a rotation, and the refresh refuses.
    fn arm_retry_key(
        &mut self,
        entry: &CredentialEntry,
        credential: &SessionCredential,
        stored: Option<[u8; 32]>,
    ) {
        self.retry_key = None;
        self.armed_pair = None;
        if let Some(key) = stored {
            self.retry_key = Some(key);
            self.armed_pair = Some(pair_fingerprint(credential));
            return;
        }
        let key = self.mint_retry_key();
        if encode_slot(credential, &key)
            .and_then(|value| {
                self.vault
                    .store(entry, value)
                    .map_err(|_| ServiceError::retryable("credential_unavailable"))
            })
            .is_ok()
        {
            self.retry_key = Some(key);
            self.armed_pair = Some(pair_fingerprint(credential));
        }
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
        Ok(self
            .load_slot_keyed(entry)?
            .map(|(credential, _)| credential))
    }
    /// The slot's credential and the retry key stored beside it, from the one
    /// load. A vault load is a platform round trip on a phone, so the key is
    /// read where the token is read and never looked up again.
    fn load_slot_keyed(
        &self,
        entry: &CredentialEntry,
    ) -> Result<Option<(SessionCredential, Option<[u8; 32]>)>, ServiceError> {
        self.vault
            .load(entry)
            .map_err(|_| ServiceError::retryable("credential_unavailable"))?
            .map(|value| {
                SessionCredential::decode(&value)
                    .map(|credential| (credential, decode_retry_key(&value)))
                    .map_err(|_| ServiceError::terminal("credential_invalid"))
            })
            .transpose()
    }

    pub fn restore(
        &mut self,
        service: &mut dyn AccountService,
    ) -> Result<Option<SessionCredential>, ServiceError> {
        let Some(directory) = self.directory.as_ref().cloned() else {
            let Some((restored, retry_key)) = self.load_slot_keyed(&self.legacy)? else {
                self.retry_key = None;
                return Ok(None);
            };
            let legacy = self.legacy.clone();
            self.arm_retry_key(&legacy, &restored, retry_key);
            return Ok(Some(restored));
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
        // The retry key that came out of the same slot as `selected`, so the
        // refresh that may rotate those bytes presents the key already stored
        // beside them. `None` beside a selected credential means that slot
        // holds no key yet and one has to be written before it is handed out.
        let mut selected_key = None;
        let mut failures = std::collections::BTreeMap::new();
        for record in &snapshot.records {
            let selected_slot = snapshot.active == Some(record.owner.account);
            match self.load_slot_keyed(&self.entry(record)?) {
                Ok(Some((credential, slot_key))) => match log_inspection(
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
                            selected_key = slot_key;
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
                        // replay it under the retry key stored in this very
                        // slot beside those same old bytes — the key the lost
                        // rotation itself presented, which is why the replay
                        // is the same rotation and not a second one. Hand the
                        // selected slot through and let the
                        // refresh route conclude: it is the one call that
                        // tells a revoked session apart from a superseded
                        // token, and it clears the vault and publishes
                        // signed-out itself when the session really is gone.
                        // The refusal is still recorded — the inspection log
                        // line and the repair code both stand — it just no
                        // longer decides the outcome.
                        if selected_slot && error.code == "credential_rejected" {
                            selected = Some(credential);
                            selected_key = slot_key;
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
                let legacy = match self.load_slot_keyed(&self.legacy) {
                    Ok(value) => value,
                    Err(error) if unusable(&error) => None,
                    Err(error) => return Err(error),
                };
                match legacy {
                    Some((legacy, legacy_key)) => {
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
                            Some(record) => match self.load_slot_keyed(&self.entry(record)?) {
                                Ok(Some((value, slot_key))) => {
                                    match service.inspect_credential(&value) {
                                        Ok(actual) if actual.account == owner.account => {
                                            Some((record.clone(), actual, value, slot_key))
                                        }
                                        Ok(_) => {
                                            return Err(ServiceError::terminal(
                                                "credential_owner_mismatch",
                                            ));
                                        }
                                        Err(error) if unusable(&error) => None,
                                        Err(error) => return Err(error),
                                    }
                                }
                                Ok(None) => None,
                                Err(error) if unusable(&error) => None,
                                Err(error) => return Err(error),
                            },
                            None => None,
                        };
                        if let Some((record, actual, value, slot_key)) = surviving {
                            directory.ready(&namespace, &record, actual, false, true)?;
                            if snapshot.active == Some(actual.account) {
                                self.active = Some(DirectoryRecord {
                                    owner: actual,
                                    state: DirectoryState::Ready,
                                    ..record
                                });
                                selected = Some(value);
                                selected_key = slot_key;
                            }
                        } else {
                            let record = directory.pending(&namespace, owner)?;
                            let entry = self.entry(&record)?;
                            // The bytes move to their account slot with the
                            // retry key that belongs to them, so a rotation
                            // in flight across this one-time migration is
                            // still replayable. A legacy slot written before
                            // keys existed has none, and gets one here.
                            let carried = legacy_key.unwrap_or_else(|| self.mint_retry_key());
                            self.vault
                                .store(&entry, encode_slot(&legacy, &carried)?)
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
                                selected_key = Some(carried);
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
        // The credential about to be handed out can be refreshed, and a
        // refresh may rotate it, so the key that rotation will present has to
        // be durable now. It is the one that came out of the slot these bytes
        // came from, which is what makes a replay after a stop present the
        // same key the lost rotation did.
        self.retry_key = None;
        self.armed_pair = None;
        self.active_session = selected
            .as_ref()
            .filter(|_| self.active.is_some())
            .map(|credential| credential.session_id.clone());
        if let Some(credential) = &selected {
            let entry = match &self.active {
                Some(record) => self.entry(record)?,
                None => self.legacy.clone(),
            };
            self.arm_retry_key(&entry, credential, selected_key);
        }
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
        // Minted before the write and kept with the pending write, so the key
        // a later refresh of these bytes must present becomes durable in the
        // same acknowledged write as the bytes themselves — no second write,
        // and nothing to lose between the two. A write that stores a pair this
        // persistence is already armed for keeps its key: the key belongs to
        // the token, and replacing it would strand a rotation already in
        // flight under it.
        let retry_key = *pending
            .retry_key
            .get_or_insert_with(|| match self.retry_key {
                Some(key) if self.armed_pair == Some(pair_fingerprint(credential)) => key,
                _ => self.mint_retry_key(),
            });
        let Some(directory) = self.directory.as_ref().cloned() else {
            self.vault
                .store(&self.legacy, encode_slot(credential, &retry_key)?)
                .map_err(|_| ServiceError::retryable("credential_unavailable"))?;
            self.retry_key = Some(retry_key);
            self.armed_pair = Some(pair_fingerprint(credential));
            return Ok(());
        };
        let namespace = self.legacy.namespace();
        // **A rotation asks the service nothing on its way to the vault.** The
        // service has already replaced the pair these bytes supersede, so
        // until the vault write lands the only copy of a working refresh
        // token is in this process's memory. A rotation changes neither the
        // account nor the session, so its owner is the one it was restored
        // with: the write used to wait behind a hosted inspection as well,
        // and a process that stopped during that round trip came back signed
        // out.
        //
        // The directory's *pending* row still goes first, as it does for
        // every replacement: while the vault may hold bytes the directory
        // has not acknowledged, the row has to say so, or another view of
        // the same account would read them as acknowledged authority
        // (`switching`'s tests hold that). Writing the vault ahead of it was
        // tried and broke exactly that.
        //
        // One read decides it: a slot whose directory record was removed or
        // replaced — a sign-out, or a newer incarnation of the account — is
        // no longer a rotation, and is asked about like any other credential.
        let rotation = pending.record.is_none()
            && self.active_session.as_deref() == Some(credential.session_id.as_str())
            && match &self.active {
                Some(active) => directory
                    .snapshot(&namespace)?
                    .records
                    .iter()
                    .any(|r| r.version == active.version && r.owner == active.owner),
                None => false,
            };
        let rotated_owner = self
            .active
            .as_ref()
            .filter(|_| rotation)
            .map(|active| active.owner);
        if pending.record.is_none() {
            // The owner of a rotated session is the owner it was restored
            // with; only a credential this persistence has not held is asked
            // about.
            let owner = match rotated_owner {
                Some(owner) => owner,
                None => service.inspect_credential(credential)?,
            };
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
                .store(&self.entry(record)?, encode_slot(credential, &retry_key)?)
                .map_err(|_| ServiceError::retryable("credential_unavailable"))?;
            pending.vault_acknowledged = true;
        }
        self.retry_key = Some(retry_key);
        self.armed_pair = Some(pair_fingerprint(credential));
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
        self.active_session = Some(credential.session_id.clone());
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
        // The key lives in the slot, so erasing the slot erases it. Nothing
        // may be armed for bytes that no longer exist.
        self.retry_key = None;
        self.armed_pair = None;
        self.active_session = None;
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
