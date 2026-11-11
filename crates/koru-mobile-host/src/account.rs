// SPDX-License-Identifier: MPL-2.0

//! Platform mechanisms around the shared worker; this module owns no account
//! transitions or trust database. Activation requires the migrated trust store.
pub(crate) mod activation;
pub(crate) mod cloud;
mod cloud_sync;
pub use activation::TrustedAccountConfig;
pub(crate) use activation::activate;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use koru_account_runtime::account::{
    AccountControl, AccountService, Cancel, ServiceError, WorkerEvent,
};
use koru_account_runtime::delivery::{
    Callback, NativeSignInRequest, NativeSignInSession, SignInDelivery, SignInSession,
};
use koru_agent_wire::{AccountCommand, AccountOperationResult, AccountStage, AccountState};
use koru_contract::mobile_proto as wire;
use koru_oauth_credentials::{CredentialEntry, CredentialStore, Error, SecretString};
use zeroize::Zeroize;

const WAIT: Duration = Duration::from_secs(30);
/// A browser sign-in is bounded by its hosted flow (ten minutes); this is the
/// phone's own ceiling on a tab that never returns.
const BROWSER_WAIT: Duration = Duration::from_secs(10 * 60);
const MAX_CALLBACK_URL_BYTES: usize = 8 * 1024;
const RESULT_CAPACITY: usize = 16;

/// One outstanding native proof and one vault operation. Polling does not
/// consume prompts; a lost JNI reply can be retried using the same correlation.
pub struct PlatformBridge {
    server_client_id: String,
    state: Mutex<PlatformState>,
    changed: Condvar,
    clock: Arc<dyn koru_account_runtime::ports::Clock>,
    vault_wait: Duration,
}
#[derive(Default)]
struct PlatformState {
    next_vault: u64,
    next_browser: u64,
    native: Option<NativePending>,
    vault: Option<VaultPending>,
    browser: Option<BrowserPending>,
}
/// One browser sign-in: the page the worker asked to open, and the App Link
/// that came back. Held from `bind` until the session drops.
struct BrowserPending {
    request_id: u64,
    generation: Option<u64>,
    url: Option<String>,
    reply: Option<Result<Vec<(String, String)>, ServiceError>>,
}
struct NativePending {
    request: NativeSignInRequest,
    response: Option<Result<SecretString, ServiceError>>,
}
struct VaultPending {
    id: u64,
    action: wire::AccountVaultAction,
    entry: String,
    credential: Option<SecretString>,
    response: Option<Result<Option<SecretString>, Error>>,
}

impl PlatformBridge {
    pub fn new(server_client_id: String) -> Result<Arc<Self>, &'static str> {
        Self::with_clock(
            server_client_id,
            Arc::new(koru_account_runtime::ports::SystemClock),
        )
    }

    pub fn with_clock(
        server_client_id: String,
        clock: Arc<dyn koru_account_runtime::ports::Clock>,
    ) -> Result<Arc<Self>, &'static str> {
        if server_client_id.is_empty()
            || server_client_id.len() > 256
            || !server_client_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._".contains(&b))
        {
            return Err("google_client_id_invalid");
        }
        Ok(Arc::new(Self {
            server_client_id,
            state: Mutex::new(PlatformState::default()),
            changed: Condvar::new(),
            clock,
            vault_wait: WAIT,
        }))
    }

    fn native_reply(&self, mut reply: wire::NativeSignInReply) -> Result<(), &'static str> {
        let token = reply.id_token.take().map(secret_bytes).transpose()?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let pending = state.native.as_mut().ok_or("account_response_stale")?;
        if pending.request.generation != reply.generation
            || pending.request.request_id != reply.request_id
            || pending.response.is_some()
        {
            return Err("account_response_stale");
        }
        pending.response = Some(if reply.cancelled {
            Err(ServiceError::terminal("cancelled"))
        } else {
            token.ok_or_else(|| ServiceError::terminal("native_sign_in_failed"))
        });
        self.changed.notify_all();
        Ok(())
    }

    /// The worker published a provider page for `generation`: a new prompt.
    fn browser_opened(&self, generation: u64, url: String) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(next) = state.next_browser.checked_add(1) else {
            return;
        };
        state.next_browser = next;
        if let Some(pending) = state.browser.as_mut() {
            pending.request_id = next;
            pending.generation = Some(generation);
            pending.url = Some(url);
            pending.reply = None;
            self.changed.notify_all();
        }
    }

    fn browser_reply(&self, mut reply: wire::BrowserSignInReply) -> Result<(), &'static str> {
        let url = reply.callback_url.take();
        let fields = url.as_deref().map(app_link_fields).transpose();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let pending = state.browser.as_mut().ok_or("account_response_stale")?;
        if pending.generation != Some(reply.generation)
            || pending.request_id != reply.request_id
            || pending.reply.is_some()
        {
            return Err("account_response_stale");
        }
        pending.reply = Some(if reply.cancelled {
            Err(ServiceError::terminal("cancelled"))
        } else {
            fields?.ok_or(ServiceError::terminal("callback_invalid"))
        });
        self.changed.notify_all();
        Ok(())
    }

    fn vault_reply(&self, mut reply: wire::AccountVaultReply) -> Result<(), &'static str> {
        let credential = reply.credential.take().map(secret_bytes).transpose()?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let pending = state.vault.as_mut().ok_or("account_response_stale")?;
        if pending.id != reply.request_id || pending.response.is_some() {
            return Err("account_response_stale");
        }
        if pending.action != wire::AccountVaultAction::Load && credential.is_some() {
            return Err("account_payload_invalid");
        }
        pending.response = Some(if reply.success {
            Ok(credential)
        } else {
            Err(Error::Unavailable)
        });
        self.changed.notify_all();
        Ok(())
    }

    fn vault(
        &self,
        entry: &CredentialEntry,
        action: wire::AccountVaultAction,
        credential: Option<SecretString>,
    ) -> Result<Option<SecretString>, Error> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.vault.is_some() {
            return Err(Error::Unavailable);
        }
        state.next_vault = state.next_vault.checked_add(1).ok_or(Error::Unavailable)?;
        let id = state.next_vault;
        state.vault = Some(VaultPending {
            id,
            action,
            entry: entry.account().into(),
            credential,
            response: None,
        });
        let deadline = self.clock.monotonic() + self.vault_wait;
        loop {
            if let Some(response) = state.vault.as_mut().and_then(|p| p.response.take()) {
                state.vault = None;
                return response;
            }
            let remaining = deadline.saturating_duration_since(self.clock.monotonic());
            if remaining.is_zero() {
                state.vault = None;
                return Err(Error::Unavailable);
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

impl CredentialStore for PlatformBridge {
    fn load(&self, entry: &CredentialEntry) -> Result<Option<SecretString>, Error> {
        self.vault(entry, wire::AccountVaultAction::Load, None)
    }
    fn store(&self, entry: &CredentialEntry, value: SecretString) -> Result<(), Error> {
        self.vault(entry, wire::AccountVaultAction::Store, Some(value))
            .map(|_| ())
    }
    fn clear(&self, entry: &CredentialEntry) -> Result<(), Error> {
        self.vault(entry, wire::AccountVaultAction::Clear, None)
            .map(|_| ())
    }
}

/// Cloning this adapter clones only the mailbox handle.
pub struct NativeDelivery(pub Arc<PlatformBridge>);
impl SignInDelivery for NativeDelivery {
    /// A browser sign-in (Apple, which has no Android sheet). The phone has no
    /// loopback a browser can be trusted to reach, so the result returns
    /// through the website's verified App Link and Kotlin hands it back.
    fn bind(&self) -> Result<Box<dyn SignInSession>, ServiceError> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.browser.is_some() {
            return Err(ServiceError::terminal("browser_sign_in_busy"));
        }
        state.browser = Some(BrowserPending {
            request_id: 0,
            generation: None,
            url: None,
            reply: None,
        });
        Ok(Box::new(BrowserSession(Arc::clone(&self.0))))
    }
    fn supports_native_token(&self) -> bool {
        true
    }
    fn bind_native(&self) -> Result<Box<dyn NativeSignInSession>, ServiceError> {
        Ok(Box::new(NativeDelivery(Arc::clone(&self.0))))
    }
}
impl NativeSignInSession for NativeDelivery {
    fn wait(
        &mut self,
        request: &NativeSignInRequest,
        cancel: &Cancel,
    ) -> Result<koru_account_wire::GoogleIdToken, ServiceError> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.native.is_some() {
            return Err(ServiceError::terminal("native_sign_in_busy"));
        }
        let now = self
            .0
            .clock
            .now_millis()
            .ok_or_else(|| ServiceError::terminal("clock_unavailable"))?;
        let remaining_millis = request.expires_at_millis.saturating_sub(now);
        if remaining_millis <= 0 {
            return Err(ServiceError::terminal("native_sign_in_expired"));
        }
        state.native = Some(NativePending {
            request: request.clone(),
            response: None,
        });
        // The service enforces hosted expiry; this additionally bounds an SDK
        // that never answers. A backgrounded platform may resume and retry.
        let deadline = self.0.clock.monotonic()
            + Duration::from_millis(remaining_millis as u64).min(Duration::from_secs(120));
        loop {
            if let Some(response) = state.native.as_mut().and_then(|p| p.response.take()) {
                state.native = None;
                return response
                    .map(|value| koru_account_wire::GoogleIdToken(value.expose().to_owned()));
            }
            if cancel.cancelled() || self.0.clock.monotonic() >= deadline {
                state.native = None;
                return Err(ServiceError::terminal(if cancel.cancelled() {
                    "cancelled"
                } else {
                    "native_sign_in_timeout"
                }));
            }
            state = self
                .0
                .changed
                .wait_timeout(state, Duration::from_millis(100))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

struct BrowserSession(Arc<PlatformBridge>);
impl Drop for BrowserSession {
    fn drop(&mut self) {
        self.0
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .browser = None;
    }
}
impl SignInSession for BrowserSession {
    fn callback_port(&self) -> u16 {
        0
    }
    fn app_link_return(&self) -> bool {
        true
    }
    /// Waits for the App Link whose `state` is this leg's. A link for another
    /// leg (a stale tab, a second tap) is dropped and the wait continues,
    /// exactly as the desktop loopback ignores a foreign request.
    fn wait(&mut self, expected: &str, cancel: &Cancel) -> Result<Callback, ServiceError> {
        let deadline = self.0.clock.monotonic() + BROWSER_WAIT;
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let pending = state
                .browser
                .as_mut()
                .ok_or_else(|| ServiceError::terminal("browser_sign_in_lost"))?;
            match pending.reply.take() {
                Some(Ok(fields))
                    if fields
                        .iter()
                        .any(|(name, value)| name == "state" && value == expected) =>
                {
                    pending.url = None;
                    return Ok(Callback::new(fields));
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    pending.url = None;
                    return Err(error);
                }
                None => {}
            }
            if cancel.cancelled() || self.0.clock.monotonic() >= deadline {
                pending.url = None;
                return Err(ServiceError::terminal(if cancel.cancelled() {
                    "cancelled"
                } else {
                    "browser_sign_in_timeout"
                }));
            }
            state = self
                .0
                .changed
                .wait_timeout(state, Duration::from_millis(100))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// The query of an App Link return, as the loopback would have decoded it.
/// Only the fixed HTTPS path is accepted; the state and PKCE checks after this
/// are what bind it to the flow.
fn app_link_fields(url: &str) -> Result<Vec<(String, String)>, &'static str> {
    if url.len() > MAX_CALLBACK_URL_BYTES {
        return Err("account_payload_invalid");
    }
    let parsed = url::Url::parse(url).map_err(|_| "account_payload_invalid")?;
    if parsed.scheme() != "https"
        || parsed.path() != koru_account_wire::APP_LINK_CALLBACK_PATH
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err("account_payload_invalid");
    }
    Ok(parsed.query_pairs().into_owned().collect())
}

#[derive(Default)]
struct Results {
    next: u64,
    evicted_through: u64,
    values: VecDeque<wire::MobileAccountResult>,
    owners: HashMap<u64, u64>,
}

pub struct MobileAccount {
    configuration: Option<TrustedAccountConfig>,
    control: AccountControl,
    bridge: Arc<PlatformBridge>,
    results: Arc<Mutex<Results>>,
    enrollment: Option<Arc<dyn koru_account_runtime::enrollment::Driver>>,
    /// The Cloud coordinator, kept to be *read*: its snapshot is the phone's
    /// counterpart of the desktop Store's live permit set, which is where a
    /// Workspace's Cloud sync stage comes from.
    cloud: Option<Arc<dyn koru_account_runtime::cloud::Driver>>,
}
impl MobileAccount {
    /// Composition seam. The caller must supply a service already composed
    /// with its sole trust repository/enrollment driver. Production activation
    /// is performed only after the Plan1.6 JSON migration succeeds.
    pub fn compose(
        service: koru_account_runtime::account::ProductionAccountService,
        bridge: Arc<PlatformBridge>,
        entry: CredentialEntry,
        directory: Arc<dyn koru_account_runtime::account::directory::AccountDirectory>,
        enrollment: Arc<dyn koru_account_runtime::enrollment::Driver>,
        cloud: Option<Arc<dyn koru_account_runtime::cloud::Driver>>,
    ) -> Arc<Self> {
        let mut service = service
            .with_directory_namespace(entry.namespace())
            .with_enrollment(Arc::clone(&enrollment));
        if let Some(cloud) = &cloud {
            service = service.with_cloud(Arc::clone(cloud));
        }
        let mut value = Self::start_inner(service, bridge, entry, Some(directory));
        value
            .control
            .set_enrollment_control(Arc::clone(&enrollment));
        value.enrollment = Some(enrollment);
        // The Cloud coordinator is activated, refreshed and deactivated by
        // the same decisions as enrollment (HOST-CONTROL §4).
        if let Some(cloud) = cloud {
            value.control.set_cloud_control(Arc::clone(&cloud));
            value.cloud = Some(cloud);
        }
        Arc::new(value)
    }

    /// **Whether a live Cloud permit covers this Workspace, and what it is
    /// doing** — the phone's answer to the desktop Store's
    /// `ControlHost::cloud_workspace_stage`.
    ///
    /// On the desktop the gateway keeps the live permits the Agent's
    /// coordinator installed and the stage its driver last set; on a phone
    /// the coordinator runs in this process and its snapshot *is* that set,
    /// one record per live dataset with the stage its last pass reported.
    /// `None` when no record names the Workspace — not on Cloud, or no Cloud
    /// attached — which is exactly the desktop's `None`.
    pub(crate) fn cloud_workspace_stage(
        &self,
        workspace: uuid::Uuid,
    ) -> Option<(&'static str, Option<&'static str>)> {
        self.cloud
            .as_ref()?
            .snapshot()
            .records
            .into_iter()
            .find(|record| match record.dataset {
                koru_device::cloud::CloudReplicaPrincipalV1::Workspace { workspace_id, .. } => {
                    workspace_id.as_uuid() == workspace
                }
                koru_device::cloud::CloudReplicaPrincipalV1::Profile { .. } => false,
            })
            .map(|record| (record.stage.as_str(), record.error_code))
    }

    /// A durable local commit landed: schedule the next Cloud pass behind
    /// the coordinator's trailing debounce.
    pub(crate) fn wake_cloud(&self) {
        self.control.wake_cloud();
    }

    #[cfg(test)]
    fn start(
        service: impl AccountService,
        bridge: Arc<PlatformBridge>,
        entry: CredentialEntry,
    ) -> Self {
        Self::start_inner(service, bridge, entry, None)
    }

    fn start_inner(
        service: impl AccountService,
        bridge: Arc<PlatformBridge>,
        entry: CredentialEntry,
        directory: Option<Arc<dyn koru_account_runtime::account::directory::AccountDirectory>>,
    ) -> Self {
        let results = Arc::new(Mutex::new(Results::default()));
        let events = Arc::clone(&results);
        let browser = Arc::clone(&bridge);
        let credentials: Arc<dyn CredentialStore> = bridge.clone();
        let control = AccountControl::start_with_directory(
            service,
            credentials,
            entry,
            directory,
            bridge.clock.clone(),
            false,
            move |event| {
                if let WorkerEvent::Browser {
                    generation, url, ..
                } = &event
                {
                    browser.browser_opened(*generation, url.0.clone());
                }
                if let WorkerEvent::Result {
                    owner_scope,
                    requester,
                    generation,
                    result,
                } = event
                {
                    let mut values = events.lock().unwrap_or_else(|e| e.into_inner());
                    values.next += 1;
                    let mut value = operation_result(values.next, requester, generation, result);
                    value.owner_scope = owner_scope;
                    if values.values.len() == RESULT_CAPACITY {
                        values.evicted_through =
                            values.values.pop_front().expect("full ring").sequence;
                    }
                    values.values.push_back(value);
                }
            },
        );
        Self {
            configuration: None,
            control,
            bridge,
            results,
            enrollment: None,
            cloud: None,
        }
    }

    pub fn command(&self, request: &wire::AccountCommandRequest) -> Result<u64, &'static str> {
        self.command_with_admission(request, |_| {})
            .map(|a| a.generation)
    }
    pub fn command_owned(
        &self,
        request: &wire::AccountCommandRequest,
    ) -> Result<koru_account_runtime::account::AccountAdmission, &'static str> {
        self.command_with_admission(request, |_| {})
    }

    fn command_with_admission(
        &self,
        request: &wire::AccountCommandRequest,
        admitted: impl FnOnce(u64),
    ) -> Result<koru_account_runtime::account::AccountAdmission, &'static str> {
        koru_contract::mobile_account::command(request)?;
        let action =
            wire::AccountAction::try_from(request.action).map_err(|_| "account_action_unknown")?;
        let command = match action {
            wire::AccountAction::SwitchAccount => AccountCommand::SwitchAccount {
                account_id: request.account_id.clone(),
            },
            wire::AccountAction::AddAccount => AccountCommand::AddAccount {
                locale: request.locale.clone(),
            },
            wire::AccountAction::StartGoogle => AccountCommand::StartGoogle {
                locale: request.locale.clone(),
            },
            wire::AccountAction::StartApple => AccountCommand::StartApple {
                locale: request.locale.clone(),
            },
            // V2 gave the email journey an address, a code and a password, and
            // this action carries none of them. V2-C adds the fields and the
            // Compose screens together; until then the action is refused rather
            // than sent half-formed.
            wire::AccountAction::StartEmail => return Err("account_action_unsupported"),
            wire::AccountAction::Cancel => AccountCommand::Cancel {
                generation: request.generation,
            },
            wire::AccountAction::Retry => AccountCommand::Retry,
            wire::AccountAction::Logout => AccountCommand::Logout,
            wire::AccountAction::ListProfiles => AccountCommand::ListProfileChoices,
            wire::AccountAction::SelectProfiles => AccountCommand::SelectProfiles {
                profile_ids: request.profile_ids.clone(),
            },
            wire::AccountAction::RetryEnrollment => AccountCommand::RetryEnrollment {
                profile_id: (!request.profile_id.is_empty()).then(|| request.profile_id.clone()),
            },
            // The phone edge correlates an answer by requester/generation/
            // sequence, not by `request_id`, which it only echoes. Quoting the
            // requester keeps the field a real id on this edge rather than a
            // zero that reads as "unset".
            wire::AccountAction::SetProfileCloudSync => AccountCommand::SetProfileCloudSync {
                request_id: request.requester,
                profile_id: request.profile_id.clone(),
                enabled: request.cloud_sync_enabled.unwrap_or(false),
            },
            wire::AccountAction::InspectCloudSync => AccountCommand::InspectCloudSync {
                request_id: request.requester,
            },
            wire::AccountAction::CancelEnrollment => AccountCommand::CancelEnrollment,
            wire::AccountAction::ConfirmAccountCreation => AccountCommand::ConfirmAccountCreation,
            wire::AccountAction::CancelAccountCreation => AccountCommand::CancelAccountCreation,
            wire::AccountAction::Unspecified => return Err("account_action_unknown"),
        };
        // Enqueue is nonblocking and releases the control's active-map guard.
        // Serialize admission + owner registration with poll so an old false
        // ownership observation cannot be paired with a newly admitted state.
        let mut results = self.results.lock().unwrap_or_else(|e| e.into_inner());
        results.owners.retain(|generation, requester| {
            self.control
                .requester_generation_active(*requester, *generation)
        });
        if results.owners.len() >= 16
            && !matches!(
                command,
                AccountCommand::Cancel { .. } | AccountCommand::Logout
            )
        {
            return Err("account_queue_full");
        }
        let admission = self
            .control
            .enqueue_owned(request.requester, request.expected_owner_scope, command)
            .map_err(|e| match e {
                koru_account_runtime::account::EnqueueError::Full => "account_queue_full",
                koru_account_runtime::account::EnqueueError::NotOwner => "account_not_owner",
                koru_account_runtime::account::EnqueueError::ScopeChanged => {
                    "account_scope_changed"
                }
            })?;
        let generation = admission.generation;
        admitted(generation);
        // Retained operations may belong to inactive accounts. Never evict a
        // living SDK request just because another account issued newer commands.
        results.owners.insert(generation, request.requester);
        Ok(admission)
    }

    pub fn platform(&self, request: &mut wire::AccountPlatformRequest) -> Result<(), &'static str> {
        let valid = koru_contract::mobile_account::platform(request);
        if let Err(error) = valid {
            zeroize_response(request);
            return Err(error);
        }
        use wire::account_platform_request::Response;
        match request.response.take().ok_or("account_response_missing")? {
            Response::SignIn(mut reply) => {
                let owner = self
                    .results
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .owners
                    .get(&reply.generation)
                    .copied();
                if owner != Some(request.requester) {
                    if let Some(bytes) = &mut reply.id_token {
                        bytes.zeroize();
                    }
                    return Err("account_not_owner");
                }
                self.bridge.native_reply(reply)
            }
            Response::Browser(mut reply) => {
                let owner = self
                    .results
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .owners
                    .get(&reply.generation)
                    .copied();
                if owner != Some(request.requester) {
                    if let Some(url) = &mut reply.callback_url {
                        url.zeroize();
                    }
                    return Err("account_not_owner");
                }
                self.bridge.browser_reply(reply)
            }
            Response::Vault(reply) => self.bridge.vault_reply(reply),
            Response::Foreground(value) => {
                self.control.set_foreground(value);
                Ok(())
            }
        }
    }

    pub fn poll(
        &self,
        requester: u64,
        after: u64,
        include_platform: bool,
    ) -> wire::AccountPollReply {
        let results = self.results.lock().unwrap_or_else(|e| e.into_inner());
        let state = self.bridge.state.lock().unwrap_or_else(|e| e.into_inner());
        // No active-map guard survives this call. Reading ownership before
        // state ensures a false observation precedes the terminal state read;
        // queued commands and scheduled retry envelopes remain active.
        let requester_active = self.control.requester_active(requester);
        let mut diagnostic = status(self.control.state());
        diagnostic.requester_active = Some(requester_active);
        for held in &mut diagnostic.held_accounts {
            held.requester_active = Some(
                self.control
                    .requester_scope_active(requester, held.scope_id),
            );
        }
        wire::AccountPollReply {
            status: Some(diagnostic),
            results: results
                .values
                .iter()
                .filter(|v| v.requester == requester && v.sequence > after)
                .cloned()
                .collect(),
            sign_in: state
                .native
                .as_ref()
                .filter(|p| {
                    include_platform
                        && results.owners.get(&p.request.generation) == Some(&requester)
                })
                .map(|p| wire::NativeSignInPrompt {
                    generation: p.request.generation,
                    request_id: p.request.request_id,
                    nonce: p.request.nonce.clone(),
                    expires_at_millis: p.request.expires_at_millis,
                    reauthentication: p.request.reauthentication,
                    server_client_id: self.bridge.server_client_id.clone(),
                }),
            vault: state.vault.as_ref().filter(|_| include_platform).map(|p| {
                wire::AccountVaultPrompt {
                    request_id: p.id,
                    action: p.action as i32,
                    entry: p.entry.clone(),
                    credential: p
                        .credential
                        .as_ref()
                        .map(|v| v.expose().as_bytes().to_vec()),
                }
            }),
            browser: state
                .browser
                .as_ref()
                .filter(|p| {
                    include_platform
                        && p.reply.is_none()
                        && p.generation
                            .is_some_and(|g| results.owners.get(&g) == Some(&requester))
                })
                .and_then(|p| {
                    Some(wire::BrowserSignInPrompt {
                        generation: p.generation?,
                        request_id: p.request_id,
                        url: p.url.clone()?,
                    })
                }),
            error_code: (after < results.evicted_through).then(|| "account_result_gap".into()),
        }
    }
}

fn secret_bytes(bytes: Vec<u8>) -> Result<SecretString, &'static str> {
    match String::from_utf8(bytes) {
        Ok(value) => SecretString::new(value).map_err(|_| "account_payload_invalid"),
        Err(error) => {
            error.into_bytes().zeroize();
            Err("account_payload_invalid")
        }
    }
}

pub(crate) fn zeroize_response(request: &mut wire::AccountPlatformRequest) {
    use wire::account_platform_request::Response;
    match request.response.as_mut() {
        Some(Response::SignIn(r)) => {
            if let Some(v) = &mut r.id_token {
                v.zeroize();
            }
        }
        Some(Response::Vault(r)) => {
            if let Some(v) = &mut r.credential {
                v.zeroize();
            }
        }
        Some(Response::Browser(r)) => {
            if let Some(v) = &mut r.callback_url {
                v.zeroize();
            }
        }
        _ => {}
    }
}

pub(crate) fn unavailable() -> wire::AccountPollReply {
    wire::AccountPollReply {
        status: Some(wire::MobileAccountStatus {
            stage: "unavailable".into(),
            failure_code: Some("account_trust_not_ready".into()),
            requester_active: Some(false),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn status(value: AccountState) -> wire::MobileAccountStatus {
    let stage = account_stage(value.stage);
    let enrollment = enrollment_stage(value.enrollment);
    wire::MobileAccountStatus {
        active_scope: value.active_scope,
        active_account_id: value.active_account_id,
        held_accounts: value
            .held_accounts
            .into_iter()
            .map(|h| wire::MobileHeldAccount {
                scope_id: h.scope_id,
                account_id: h.account_id,
                auth_identity_id: h.auth_identity_id,
                stage: account_stage(h.stage).into(),
                credential_ready: h.credential_ready,
                failure_code: h.failure_code,
                active_generations: h.active_generations,
                requester_active: None,
            })
            .collect(),
        capability: value.capability,
        stage: stage.into(),
        generation: value.generation,
        profile_count: value.profile_count.min(u32::MAX as usize) as u32,
        queue_depth: value.queue_depth.min(u32::MAX as usize) as u32,
        enrollment_stage: enrollment.into(),
        enrollment_profile_id: (value.enrollments.len() == 1)
            .then(|| value.enrollments[0].profile_id.clone()),
        enrollments: value.enrollments.into_iter().map(enrollment_row).collect(),
        failure_code: value.failure_code,
        retry_at_millis: value.retry_at_millis,
        enrollment_failure_code: value.enrollment_failure_code,
        enrollment_retry_at_millis: value.enrollment_retry_at_millis,
        requester_active: None,
    }
}
fn enrollment_row(
    value: koru_agent_wire::AccountProfileEnrollment,
) -> wire::MobileProfileEnrollment {
    wire::MobileProfileEnrollment {
        profile_id: value.profile_id,
        stage: enrollment_stage(value.state).into(),
        failure_code: value.failure_code,
        retry_at_millis: value.retry_at_millis,
    }
}
fn account_stage(value: AccountStage) -> &'static str {
    match value {
        // The phone's runtime never produces `NotConfigured` — it is the
        // desktop Agent's answer for a build with no Koru Cloud — and the
        // phone has no spelling for it yet, so it keeps the one it has.
        AccountStage::NotConfigured | AccountStage::Unavailable => "unavailable",
        AccountStage::AwaitingAccountCreation => "awaiting_account_creation",
        AccountStage::SignedOut => "signed_out",
        AccountStage::Restoring => "restoring",
        AccountStage::WaitingForBrowser => "waiting_for_provider",
        AccountStage::AwaitingEmailCode => "awaiting_email_code",
        AccountStage::AwaitingEmailPassword => "awaiting_email_password",
        AccountStage::Exchanging => "exchanging",
        AccountStage::SignedIn => "signed_in",
        AccountStage::Refreshing => "refreshing",
        AccountStage::Cancelled => "cancelled",
        AccountStage::Failed => "failed",
    }
}

fn enrollment_stage(value: koru_agent_wire::EnrollmentState) -> &'static str {
    match value {
        koru_agent_wire::EnrollmentState::Idle => "idle",
        koru_agent_wire::EnrollmentState::Waiting => "waiting",
        koru_agent_wire::EnrollmentState::Enrolling => "enrolling",
        koru_agent_wire::EnrollmentState::Complete => "complete",
        koru_agent_wire::EnrollmentState::Failed => "failed",
    }
}

fn operation_result(
    sequence: u64,
    requester: u64,
    generation: u64,
    value: AccountOperationResult,
) -> wire::MobileAccountResult {
    let mut result = wire::MobileAccountResult {
        sequence,
        requester,
        generation,
        ..Default::default()
    };
    result.kind = match value {
        AccountOperationResult::AccountSelected {
            scope_id,
            account_id,
        } => {
            result.selected_scope = Some(scope_id);
            result.selected_account_id = account_id;
            "account_selected"
        }
        AccountOperationResult::SignedIn => "signed_in",
        // Desktop-only card query has no mobile command or projection yet.
        AccountOperationResult::CloudSubscription { .. } => "completed",
        AccountOperationResult::AccountCreationRequired => "account_creation_required",
        AccountOperationResult::SignedOut => "signed_out",
        AccountOperationResult::Cancelled => "cancelled",
        AccountOperationResult::Failed { code } => {
            result.error_code = Some(code);
            "failed"
        }
        AccountOperationResult::CloudSync { profiles, .. } => {
            result.cloud_sync = profiles
                .into_iter()
                .map(|row| wire::MobileCloudSyncProfile {
                    profile_id: row.profile_id,
                    enabled: row.enabled,
                    workspaces: row.workspaces,
                    synced_workspaces: row.synced_workspaces,
                    error_code: row.error,
                })
                .collect();
            "cloud_sync"
        }
        AccountOperationResult::ProfilesSelected { profiles } => {
            result.enrollments = profiles.into_iter().map(enrollment_row).collect();
            "profiles_selected"
        }
        AccountOperationResult::ProfileChoices {
            display_name,
            choices,
        } => {
            result.account = Some(wire::MobileAccountSummary { display_name });
            result.profiles = choices
                .into_iter()
                .map(|v| wire::MobileAccountProfile {
                    account_id: v.profile.koru_identity_id,
                    profile_id: v.profile.profile_id,
                    name: v.profile.name,
                    locale: v.profile.locale,
                    available_locally: v.available_on_this_device,
                    available_remotely: v.available_on_another_device,
                })
                .collect();
            "profiles"
        }
        _ => "completed",
    }
    .into();
    result
}

/// Same inventory question as desktop, answered on the embedded Store worker.
pub struct EmbeddedProfileInventory(pub koru_store_server::StoreWorkerClient);
impl koru_account_runtime::account::lifecycle::LocalProfileInventory for EmbeddedProfileInventory {
    fn provision(
        &self,
        profile: koru_account_core::ProfileId,
        initial: &koru_store_wire::ProvisionAccountProfile,
    ) -> Result<(), ServiceError> {
        let binding =
            koru_store_core::ProfileAccountBinding::parse_identity_id(&initial.identity_id)
                .map_err(|_| ServiceError::terminal("profile_not_authorized"))?;
        let locale = initial
            .locale
            .as_deref()
            .map(koru_store_core::Locale::parse)
            .unwrap_or(Some(koru_store_core::Locale::default()))
            .ok_or_else(|| ServiceError::terminal("profile_not_authorized"))?;
        let initial = initial.clone();
        self.0
            .with_store(move |store| {
                store.provision_account_profile(
                    profile,
                    binding,
                    &initial.name,
                    locale,
                    koru_store_core::Timestamp::from_millis(initial.created_at_millis),
                )
            })
            .map_err(|_| ServiceError::retryable("profile_provisioning_unavailable"))?
            .map(|_| ())
            .map_err(|error| match error.code {
                koru_store_core::errors::Code::ProfileAccountConflict
                | koru_store_core::errors::Code::ProfileRemoved => {
                    ServiceError::terminal("profile_not_authorized")
                }
                _ => ServiceError::retryable("profile_provisioning_unavailable"),
            })
    }

    /// The same guarded restore desktop asks its Store for — see
    /// `LocalProfileInventory::restore_removed`.
    fn restore_removed(&self, profile: koru_account_core::ProfileId) -> Result<(), ServiceError> {
        use koru_store_core::errors::Code;
        self.0
            .with_store(move |store| store.restore_profile_local(profile))
            .map_err(|_| ServiceError::retryable("profile_provisioning_unavailable"))?
            .map(|_| ())
            .map_err(|error| match error.code {
                Code::ProfileRemoved => ServiceError::terminal("profile_deleted"),
                Code::ProfileNotFound | Code::ProfileReserved => {
                    ServiceError::terminal("profile_not_authorized")
                }
                Code::VersionUnavailable | Code::CorruptState | Code::ProfileUnitScopeMismatch => {
                    ServiceError::terminal("profile_restore_unavailable")
                }
                _ => ServiceError::retryable("profile_provisioning_unavailable"),
            })
    }

    fn inventory(&self) -> Result<koru_store_wire::ProfileLifecycleInventory, ServiceError> {
        let rows = self
            .0
            .with_store(|store| store.profile_lifecycle_inventory())
            .map_err(|_| ServiceError::retryable("profile_inventory_unavailable"))?
            .map_err(|_| ServiceError::retryable("profile_inventory_unavailable"))?;
        Ok(inventory_rows(rows))
    }
}

fn inventory_rows(
    rows: Vec<koru_store_core::ports::ProfileLifecycleInventoryItem>,
) -> koru_store_wire::ProfileLifecycleInventory {
    koru_store_wire::ProfileLifecycleInventory {
        profiles: rows
            .into_iter()
            .map(|row| koru_store_wire::ProfileLifecycleInventoryItem {
                profile_id: row.profile,
                account_binding: row.account_binding.to_storage(),
                removed_at_millis: row.removed_at.map(|v| v.millis()),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests;
