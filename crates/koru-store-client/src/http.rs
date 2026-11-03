// SPDX-License-Identifier: MPL-2.0

//! Calling an explicitly configured internal Store transport.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use koru_store_core::ProfileAccountBinding;
use koru_store_core::bundle::{Bundle, ImportMode};
use koru_store_core::errors::{Code, Error, Result};
use koru_store_core::ids::{
    DocumentId, DocumentVersionId, HistoryEntryId, ProfileId, ReminderId, RuleRevisionId,
    WorkspaceId,
};
use koru_store_core::model::{Blob, BlobHash, DocumentEntry, Profile, Workspace};
use koru_store_core::personal_attributes::EntityRef;
use koru_store_core::reminders::{
    OccurrenceAction, ReminderRule, ReminderSettings, StoredReminder,
};
// **The bodies this sends are the ones the store declares.** Every call below
// used to build a `serde_json::json!` literal against a type it could not name
// — two of which were not even `pub` — so a field renamed in the store went on
// being sent under its old name and arrived as a silent `None`.
pub use koru_store_wire::attributes::{
    AttributeQuery, AttributeQueryResponse, AttributeRow, DateWindow, DueFilter, InstantWindow,
};
pub use koru_store_wire::reminders::{
    ActionRequest, ClearedHistory, ReminderQuery, ReminderQueryResponse, ReminderSnapshot,
    ReminderSnapshotRow, ReminderTarget, RevisionRequest, RuleRequest, SettingsPatch,
};
pub use koru_store_wire::search::{
    Navigation, SearchPage, SearchQuery, SearchRequest, SearchResponse,
};
pub use koru_store_wire::tasks::{TaskQuery, TaskQueryResponse, TaskRow};
pub use koru_store_wire::{
    ApplyPrivateChanges, AssociateBlob, BindProfileAccount, CaptureHistory, ClearProfileAccount, Created,
    DocumentLocation, DocumentOutline, EnsuredFileNote, Failure, HistoryEntry, HistoryPage,
    HistoryQuery, ImportQuery, LocalProfileRemoval, LocalProfileRestoration, ManifestRef,
    NewDocument, NewProfile, NewWorkspace, OutlineHeading, PersonalAttributeValue,
    PersonalAttributesResponse, PersonalDictionaryResponse, ProfileAccountBindingOutcome,
    PrivateChange, PrivateState, ProfileAccountClearOutcome, ProfileAppearance,
    ProfileLifecycleInventory, RelabelHistory,
    RenameProfile, RenameWorkspace, SeedDocumentRequest, SeedDocumentWithCandidateRequest,
    SetDictionaryWord, SetPersonalAttribute, Status, Stopping, StoreBackup, StoreBackupResult,
    WorkspaceAppearance, WorkspaceOrder, WorkspaceUsage,
};

/// The Store's durable answer to content-first document creation.
pub struct SeededCreate {
    pub id: DocumentId,
    pub initial_update: Vec<u8>,
    pub replayed: bool,
}

/// An explicitly supplied internal endpoint and service credential.
///
/// This adapter does not discover or launch infrastructure. Desktop product
/// callers obtain these values from the public API gateway boundary; hermetic
/// server tests inject them directly.
#[derive(Clone)]
pub struct Discovery {
    pub port: u16,
    pub token: String,
    /// Epoch milliseconds, supplied by the Store's rendezvous response.
    pub expires_at_millis: i64,
}

impl std::fmt::Debug for Discovery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Discovery")
            .field("port", &self.port)
            .field("token", &"[REDACTED]")
            .field("expires_at_millis", &self.expires_at_millis)
            .finish()
    }
}

impl Discovery {
    /// Validate a loopback endpoint supplied by the owning boundary.
    pub fn from_loopback_endpoint(
        api_endpoint: &str,
        token: impl Into<String>,
        expires_at_millis: i64,
    ) -> Result<Self> {
        // The store always binds loopback, so the port is the whole of the
        // address and `base_url`/`sync_url` can rebuild both endpoints from it.
        // Parsed rather than trusted: a store that ever answered with something
        // other than `http://127.0.0.1:<port>` is one this client must not
        // silently follow.
        let port = api_endpoint
            .strip_prefix("http://127.0.0.1:")
            .and_then(|p| p.parse::<u16>().ok())
            .ok_or_else(|| {
                Error::new(
                    Code::CorruptState,
                    format!(
                        "the store answered with {}, which is not a loopback address",
                        api_endpoint
                    ),
                )
            })?;
        Ok(Self {
            port,
            token: token.into(),
            expires_at_millis,
        })
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn sync_url(&self) -> String {
        format!("ws://127.0.0.1:{}/v1/sync", self.port)
    }

    pub(crate) fn catalog_watch_url(&self) -> String {
        format!(
            "ws://127.0.0.1:{}/v1/internal/workspaces/catalog-watch",
            self.port
        )
    }
}

/// The management half of the API.
#[derive(Clone)]
pub struct Client {
    current: Arc<RwLock<Current>>,
    /// Serializes a refresh across all clones. A ten-tab window must not mint
    /// ten credentials because their shared one is five minutes from expiry.
    refresh_gate: Arc<Mutex<()>>,
    /// A public-boundary client may supply its own rendezvous without making
    /// this Store wire adapter know what issued the credential. Desktop
    /// products use the API gateway callback.
    renewal: Option<Renewal>,
    http: reqwest::blocking::Client,
    /// Blob hydration may legitimately wait offline until peer state changes.
    /// Keep that long-lived response off the ordinary client's 30-second
    /// request budget; connection establishment is still bounded because the
    /// Store itself is loopback-only.
    hydration_http: reqwest::blocking::Client,
}

/// Transport-neutral rediscovery supplied by the boundary that owns a
/// credential. The callback returns the same wire endpoint shape this adapter
/// already consumes; it does not expose gateway discovery types here.
pub type Renewal = Arc<dyn Fn() -> Result<Discovery> + Send + Sync>;

/// A response body read incrementally — what [`Client::get_streaming`] hands
/// back. A newtype rather than the transport's own response type, so reqwest
/// stays this crate's implementation detail.
pub struct StreamingBody {
    response: reqwest::blocking::Response,
}

impl std::io::Read for StreamingBody {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.response, buf)
    }
}

/// Whether an authenticated request may be repeated after the connection
/// failed before an answer arrived.
///
/// Authentication rejection is always safe to retry once: Gateway admission
/// runs before every handler. `Safe` additionally permits one rediscovery and
/// retry after an unknown transport outcome, so callers must select it only
/// for reads or operations carrying their own replay identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryPolicy {
    AuthenticationOnly,
    Safe,
}

/// A machine-readable non-success response from a sibling public domain.
/// The transport keeps the token intact; the owning namespace decides what it
/// means rather than teaching the Store client another domain's vocabulary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicFailure {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub waiting_on: Vec<String>,
    /// **The whole refusal, for the domains whose refusals carry facts.**
    ///
    /// Most do not: a code and a message are the whole of what a caller can
    /// act on. The workspace lifecycle operation is the exception the rev-4
    /// contract requires — its partial failure (S24) answers with the same
    /// `outcome` a success carries, because a caller reporting "the terminals
    /// are closed and the catalog is unchanged" needs the fields, not a
    /// sentence about them. Kept as the raw body rather than typed here, so
    /// the transport still learns no other domain's vocabulary; the owning
    /// namespace deserializes what it knows is there.
    ///
    /// `None` when the body was not an object — nothing depends on it, so no
    /// caller has to handle its absence specially.
    pub body: Option<serde_json::Value>,
}

#[derive(Debug)]
pub enum PublicError {
    Transport(Error),
    Failure(PublicFailure),
}

impl std::fmt::Display for PublicError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => error.fmt(formatter),
            Self::Failure(failure) => write!(formatter, "{}: {}", failure.code, failure.message),
        }
    }
}

impl std::error::Error for PublicError {}

impl From<Error> for PublicError {
    fn from(error: Error) -> Self {
        Self::Transport(error)
    }
}

pub type PublicResult<T> = std::result::Result<T, PublicError>;

#[derive(Clone)]
struct Current {
    discovery: Discovery,
    /// Refresh before the credential can expire in flight. The Store owns the
    /// lifetime, so this is derived from the expiry it just granted rather than
    /// a client-side copy of the TTL policy.
    refresh_at_millis: i64,
}

impl Current {
    fn new(discovery: Discovery) -> Self {
        Self::at(discovery, now_millis())
    }

    fn at(discovery: Discovery, now: i64) -> Self {
        const MAX_EARLY_REFRESH_MS: i64 = 5 * 60 * 1_000;
        // **A credential with no granted expiry has no deadline.** `Discovery`
        // is a plain struct with public fields, so a caller that builds one by
        // hand leaves this at zero — and zero read as a deadline is a deadline
        // permanently in the past, which turns every single request into a
        // rendezvous handshake. Quiet, and a hundred times the cost of the call
        // it precedes.
        if discovery.expires_at_millis <= 0 {
            return Self {
                refresh_at_millis: i64::MAX,
                discovery,
            };
        }
        let lifetime = discovery.expires_at_millis.saturating_sub(now).max(0);
        let early = (lifetime / 10).min(MAX_EARLY_REFRESH_MS);
        Self {
            refresh_at_millis: discovery.expires_at_millis.saturating_sub(early),
            discovery,
        }
    }
}

fn now_millis() -> i64 {
    koru_store_core::model::Timestamp::now().millis()
}

impl Client {
    pub fn new(discovery: Discovery) -> Self {
        Self::with_renewal_option(discovery, None)
    }

    /// Build a client whose public owner performs rediscovery and renewal.
    ///
    /// `koru-api-client` is the shipping caller. Keeping the callback there
    /// means a rejected gateway bearer can never fall back to Store discovery
    /// or mint a second credential family in a product process.
    pub fn with_renewal(discovery: Discovery, renewal: Renewal) -> Self {
        Self::with_renewal_option(discovery, Some(renewal))
    }

    fn with_renewal_option(discovery: Discovery, renewal: Option<Renewal>) -> Self {
        Self {
            current: Arc::new(RwLock::new(Current::new(discovery))),
            refresh_gate: Arc::new(Mutex::new(())),
            renewal,
            http: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("build the bounded Store HTTP client"),
            hydration_http: reqwest::blocking::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("build the bounded Store hydration client"),
        }
    }

    /// Clone this client with a shorter ordinary-request timeout.
    ///
    /// The endpoint, credential state and renewal gate remain shared, while
    /// only this clone's ordinary HTTP adapter receives the narrower bound.
    /// Long-lived blob hydration keeps its dedicated transport. This is for a
    /// supervised worker whose own shutdown deadline is shorter than the
    /// process-wide default; normal Store callers retain the 30-second bound.
    pub fn with_request_timeout(&self, timeout: std::time::Duration) -> Self {
        Self {
            current: Arc::clone(&self.current),
            refresh_gate: Arc::clone(&self.refresh_gate),
            renewal: self.renewal.clone(),
            http: reqwest::blocking::Client::builder()
                .timeout(timeout)
                .build()
                .expect("build the caller-bounded Store HTTP client"),
            hydration_http: self.hydration_http.clone(),
        }
    }

    /// Where this client is currently pointed, and what it is currently
    /// holding.
    ///
    /// A snapshot rather than a borrow: renewal replaces both the address and
    /// the credential behind an `RwLock`, so a reference would have to hold
    /// that lock for as long as the caller kept it. Anything that needs to
    /// stay current asks again.
    pub fn discovery(&self) -> Discovery {
        self.endpoint()
    }

    fn endpoint(&self) -> Discovery {
        self.current
            .read()
            .expect("store discovery lock poisoned")
            .discovery
            .clone()
    }

    fn refresh(&self, reason: &'static str) -> Result<()> {
        let renewal = self.renewal.as_ref().ok_or_else(|| {
            Error::new(
                Code::StorageFailed,
                "the owning boundary did not supply credential renewal",
            )
        })?;
        let discovery = renewal()?;
        let expires_at_millis = discovery.expires_at_millis;
        *self.current.write().expect("store discovery lock poisoned") = Current::new(discovery);
        crate::store_session_debug!("renewed reason={reason} expires_at_ms={expires_at_millis}");
        Ok(())
    }

    /// Renew if the granted lifetime is nearly up.
    ///
    /// **A client that cannot rediscover is not a failed request.** `Client::new`
    /// takes an address and a credential and no way back to the rendezvous, so
    /// `refresh` can only answer "not possible" for one — and answering that
    /// here would refuse the call *before sending it*, with a token the store
    /// would still have accepted. Explicitly injected integration clients are
    /// built that way. Let the request go; a credential that really has expired
    /// comes back as a 401 and says so.
    fn refresh_if_due(&self) -> Result<()> {
        if self.renewal.is_none() {
            return Ok(());
        }
        if now_millis()
            < self
                .current
                .read()
                .expect("store discovery lock poisoned")
                .refresh_at_millis
        {
            return Ok(());
        }
        let _gate = self.refresh_gate.lock().unwrap_or_else(|e| e.into_inner());
        if now_millis()
            >= self
                .current
                .read()
                .expect("store discovery lock poisoned")
                .refresh_at_millis
        {
            self.refresh("expiring")?;
        }
        Ok(())
    }

    /// Renew after an authentication rejection. The retry is safe: Store
    /// authentication runs before every handler, so a 401 cannot have applied
    /// a write — pinned by `a_rejected_mutation_does_not_reach_its_handler` in
    /// `apps/koru-store-server/tests/api.rs`, on a delete, because that is the case
    /// nobody wants to discover twice. If another clone renewed first, keep its
    /// newer credential.
    fn refresh_after_rejection(&self, rejected_token: &str) -> Result<()> {
        // Same reasoning as `refresh_if_due`, at the other end: without a way
        // back to the rendezvous the 401 is the answer, and replacing it with
        // "cannot rediscover" would hide the real one.
        if self.renewal.is_none() {
            return Ok(());
        }
        let _gate = self.refresh_gate.lock().unwrap_or_else(|e| e.into_inner());
        if self
            .current
            .read()
            .expect("store discovery lock poisoned")
            .discovery
            .token
            == rejected_token
        {
            self.refresh("rejected")?;
        }
        Ok(())
    }

    fn send_with_retry<F>(&self, request: F) -> Result<reqwest::blocking::Response>
    where
        F: FnMut(&Discovery) -> reqwest::blocking::RequestBuilder,
    {
        self.send_with_retry_policy(RetryPolicy::Safe, request)
    }

    fn send_with_retry_policy<F>(
        &self,
        retry: RetryPolicy,
        mut request: F,
    ) -> Result<reqwest::blocking::Response>
    where
        F: FnMut(&Discovery) -> reqwest::blocking::RequestBuilder,
    {
        self.refresh_if_due()?;
        let endpoint = self.endpoint();
        let first = request(&endpoint).send();
        match first {
            Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
                self.refresh_after_rejection(&endpoint.token)?;
                let renewed = self.endpoint();
                // Nothing to retry *with* if the credential did not change:
                // sending the rejected one again would spend a second round
                // trip to be told the same thing, and turn one 401 into two in
                // the store's access log.
                if renewed.token == endpoint.token {
                    return Ok(response);
                }
                request(&renewed).send().map_err(unreachable_store)
            }
            Ok(response) => Ok(response),
            Err(error) if retry == RetryPolicy::AuthenticationOnly => Err(unreachable_store(error)),
            Err(error) => {
                let original = error.to_string();
                self.refresh("unreachable").map_err(|refresh| {
                    Error::new(
                        Code::StorageFailed,
                        format!(
                            "could not reach the store: {original}; rediscovery failed: {refresh}"
                        ),
                    )
                })?;
                request(&self.endpoint()).send().map_err(unreachable_store)
            }
        }
    }

    pub fn status(&self) -> Result<Status> {
        self.get("/v1/status")
    }

    /// Ask the store to stop. It answers first and then trips the shutdown, so
    /// a success here means the request was accepted, not that the process has
    /// gone.
    /// What the store says about its own state.
    ///
    /// Returned as raw JSON rather than a typed report: the shape belongs to
    /// `koru-store-api`, it is a *diagnostic* surface that gains fields
    /// whenever something new is worth seeing, and a client that deserialized
    /// it strictly would fail to read a newer store for no reason. A viewer
    /// reads the fields it knows and ignores the rest.
    pub fn debug_store(&self) -> Result<serde_json::Value> {
        self.get("/v1/debug/store")
    }

    /// One of the store's diagnostic reports, by name — `projection`, and
    /// whatever follows it.
    ///
    /// Raw JSON on purpose, like [`debug_store`](Self::debug_store): a debug
    /// route describes what a component is doing this week, and typing it here
    /// would make every counter a compatibility surface.
    pub fn debug(&self, what: &str) -> Result<serde_json::Value> {
        self.get(&format!("/v1/debug/{what}"))
    }

    pub fn shutdown(&self) -> Result<()> {
        self.post_empty("/v1/shutdown")
    }

    /// Stop the store **only if nothing is still connected**, and say which
    /// happened.
    ///
    /// For the last window on its way out: the agent stops the store when it
    /// quits, but once the agent has gone nothing else would, and no window
    /// can see the others. The store answers, because it is the only party
    /// that can do so without a race.
    ///
    /// Ask *after* closing your own sockets — an ordinary HTTP request holds
    /// none, so what is counted is everybody else.
    pub fn shutdown_if_idle(&self) -> Result<bool> {
        let stopping: koru_store_wire::Stopping =
            self.post_query("/v1/shutdown?only_if_idle=true")?;
        Ok(stopping.stopping)
    }

    /// Ask the live Store to write one consistent portable backup.
    ///
    /// This request may legitimately take longer than the ordinary 30-second
    /// management timeout for a large Store, so only this operation gets a
    /// wider bound.
    pub fn backup_store(&self, destination: &Path) -> Result<PathBuf> {
        let destination = destination.to_string_lossy().into_owned();
        let response = self.send_with_retry(|discovery| {
            self.http
                .post(format!("{}/v1/backup", discovery.base_url()))
                .bearer_auth(&discovery.token)
                .timeout(std::time::Duration::from_secs(15 * 60))
                .json(&StoreBackup {
                    destination: destination.clone(),
                })
        })?;
        let result: StoreBackupResult = self.decode(response)?;
        Ok(PathBuf::from(result.destination))
    }

    // --------------------------------------------------------------- search

    /// Search every workspace the store holds.
    ///
    /// **A POST, and the query is in the body.** A URL is written to the access
    /// log, and the store's log rotates daily for a week; a search is not
    /// something to leave there. Nothing about this call is a mutation.
    ///
    /// The answer names any workspace it may be behind on rather than waiting
    /// for it — see [`SearchPage::updating_workspaces`]. A store running no
    /// index answers `search_unavailable`, which is a store whose search is
    /// stale rather than one that has lost anything.
    pub fn search(&self, request: &SearchRequest) -> Result<SearchResponse> {
        self.post("/v1/search", request)
    }

    /// Record that a search led somewhere.
    ///
    /// Machine-local and derived: it feeds the recent-query list and a
    /// workspace's recency, and reaches no document and no shared profile. **A
    /// query earns its place in the history by leading somewhere**, which is
    /// why the text is sent here rather than with the search.
    pub fn record_navigation(&self, navigation: &Navigation) -> Result<()> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .post(format!("{}/v1/search/navigated", discovery.base_url()))
                .bearer_auth(&discovery.token)
                .json(navigation)
        })?;
        self.check(response)?;
        Ok(())
    }

    // ------------------------------------------------------------- profiles

    /// Create a persona.
    ///
    /// `locale` is **the person's**, not the language the caller is drawing in;
    /// `purpose` is free text and `avatar` a token (`color:purple` today,
    /// `blob:<hash>` when pictures arrive). All three are carried by the store
    /// and interpreted by nobody but the client that wrote them.
    pub fn create_profile(
        &self,
        name: &str,
        purpose: Option<&str>,
        avatar: Option<&str>,
        locale: Option<&str>,
    ) -> Result<Profile> {
        self.create_profile_with_appearance(name, purpose, avatar, None, None, locale)
    }

    /// Create a persona with the emoji and color chosen during onboarding.
    pub fn create_profile_with_appearance(
        &self,
        name: &str,
        purpose: Option<&str>,
        avatar: Option<&str>,
        icon: Option<&str>,
        color: Option<&str>,
        locale: Option<&str>,
    ) -> Result<Profile> {
        self.post(
            "/v1/profiles",
            &NewProfile {
                name: name.to_owned(),
                locale: locale.map(str::to_owned),
                purpose: purpose.map(str::to_owned),
                avatar: avatar.map(str::to_owned),
                icon: icon.map(str::to_owned),
                color: color.map(str::to_owned),
            },
        )
    }

    pub fn profiles(&self) -> Result<Vec<Profile>> {
        self.get("/v1/profiles")
    }

    /// Content-free active and locally removed Profile inventory for the
    /// trusted Agent lifecycle worker. Ordinary product listings stay active-only.
    pub fn profile_lifecycle_inventory(&self) -> Result<ProfileLifecycleInventory> {
        self.get("/v1/internal/profiles/lifecycle-inventory")
    }

    pub fn provision_account_profile(
        &self,
        id: ProfileId,
        body: &koru_store_wire::ProvisionAccountProfile,
    ) -> Result<koru_store_core::Profile> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .put(format!(
                    "{}/v1/internal/profiles/{}/account-provisioning",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .bearer_auth(&discovery.token)
                .json(body)
        })?;
        self.decode(response)
    }

    /// Atomically compare and set the exact Koru identity bound to a Profile.
    /// A mismatch is a conflict and never overwrites the current binding.
    pub fn bind_profile_account(
        &self,
        id: ProfileId,
        expected: Option<ProfileAccountBinding>,
        new: ProfileAccountBinding,
    ) -> Result<ProfileAccountBindingOutcome> {
        let body = BindProfileAccount {
            expected_identity_id: expected.map(ProfileAccountBinding::identity_id),
            new_identity_id: new.identity_id(),
        };
        let response = self.send_with_retry(|discovery| {
            self.http
                .put(format!(
                    "{}/v1/profiles/{}/account",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .bearer_auth(&discovery.token)
                .json(&body)
        })?;
        self.decode(response)
    }

    pub fn clear_profile_account(
        &self,
        id: ProfileId,
        expected: ProfileAccountBinding,
    ) -> Result<ProfileAccountClearOutcome> {
        let body = ClearProfileAccount {
            expected_identity_id: expected.identity_id(),
        };
        let response = self.send_with_retry(|discovery| {
            self.http
                .delete(format!(
                    "{}/v1/profiles/{}/account",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .bearer_auth(&discovery.token)
                .json(&body)
        })?;
        self.decode(response)
    }

    /// Hide one empty Profile in this installation's Store catalog while preserving
    /// all recovery data. Settings cleanup remains the caller's responsibility.
    /// Retries return `AlreadyRemoved` after the first durable marker write.
    pub fn remove_profile_local(&self, id: ProfileId) -> Result<LocalProfileRemoval> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .delete(format!(
                    "{}/v1/profiles/{}/local",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .bearer_auth(&discovery.token)
        })?;
        self.decode(response)
    }

    /// Make a retained locally removed Profile visible again. Workspace
    /// tombstones remain in Trash.
    pub fn restore_profile_local(&self, id: ProfileId) -> Result<LocalProfileRestoration> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .put(format!(
                    "{}/v1/profiles/{}/local",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .bearer_auth(&discovery.token)
        })?;
        self.decode(response)
    }

    pub fn rename_profile(&self, id: ProfileId, name: &str) -> Result<Profile> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .patch(format!(
                    "{}/v1/profiles/{}",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .bearer_auth(&discovery.token)
                .json(&RenameProfile {
                    name: name.to_owned(),
                })
        })?;
        self.decode(response)
    }

    pub fn set_profile_appearance(
        &self,
        id: ProfileId,
        icon: Option<&str>,
        color: Option<&str>,
    ) -> Result<Profile> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .patch(format!(
                    "{}/v1/profiles/{}/appearance",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .bearer_auth(&discovery.token)
                .json(&ProfileAppearance {
                    icon: icon.map(str::to_owned),
                    color: color.map(str::to_owned),
                })
        })?;
        self.decode(response)
    }

    /// This profile's personal attributes for one entity.
    ///
    /// `entity` is the canonical reference,
    /// `<kind>:<workspace>:<document>:<node>`. An entity nothing was filed
    /// under answers an empty map, and an unset attribute is an absent key.
    pub fn personal_attributes(
        &self,
        id: ProfileId,
        entity: &str,
    ) -> Result<PersonalAttributesResponse> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .get(format!(
                    "{}/v1/profiles/{}/personal-attributes",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .query(&[("entity", entity)])
                .bearer_auth(&discovery.token)
        })?;
        self.decode(response)
    }

    /// Set one personal attribute, or clear it with `None`. Answers the
    /// entity's attributes after the write.
    pub fn set_personal_attribute(
        &self,
        id: ProfileId,
        entity: &str,
        key: &str,
        value: Option<PersonalAttributeValue>,
    ) -> Result<PersonalAttributesResponse> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .put(format!(
                    "{}/v1/profiles/{}/personal-attributes",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .bearer_auth(&discovery.token)
                .json(&SetPersonalAttribute {
                    entity: entity.to_owned(),
                    key: key.to_owned(),
                    value: value.clone(),
                })
        })?;
        self.decode(response)
    }

    // ------------------------------------------------- document-private state

    /// One reader's device-local private view state for one document. A
    /// reader who never changed anything gets an empty state at revision 0.
    pub fn document_private_state(
        &self,
        profile: ProfileId,
        document: DocumentId,
    ) -> Result<PrivateState> {
        self.get(&format!(
            "/v1/profiles/{}/documents/{}/private-state",
            profile.to_storage(),
            document.to_storage()
        ))
    }

    /// Apply up to [`MAX_BATCH`](koru_store_core::document_private::MAX_BATCH)
    /// explicit changes in one atomic save. Safe to retry: each change names
    /// the value it wants.
    pub fn apply_document_private_changes(
        &self,
        profile: ProfileId,
        document: DocumentId,
        changes: Vec<PrivateChange>,
    ) -> Result<PrivateState> {
        self.patch(
            &format!(
                "/v1/profiles/{}/documents/{}/private-state",
                profile.to_storage(),
                document.to_storage()
            ),
            &ApplyPrivateChanges { changes },
        )
    }

    // ------------------------------------------------------------- reminders

    pub fn reminder(&self, profile: ProfileId, reminder: ReminderId) -> Result<StoredReminder> {
        self.get(&format!(
            "/v1/profiles/{}/reminders/{}",
            profile.to_storage(),
            reminder.to_storage()
        ))
    }

    pub fn reminders_for(
        &self,
        profile: ProfileId,
        entity: &EntityRef,
    ) -> Result<Vec<StoredReminder>> {
        let entity = entity.to_storage();
        let response = self.send_with_retry(|discovery| {
            self.http
                .get(format!(
                    "{}/v1/profiles/{}/reminders",
                    discovery.base_url(),
                    profile.to_storage()
                ))
                .query(&[("entity", entity.as_str())])
                .bearer_auth(&discovery.token)
        })?;
        self.decode(response)
    }

    /// This profile's whole personal spelling dictionary.
    pub fn personal_dictionary(&self, id: ProfileId) -> Result<PersonalDictionaryResponse> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .get(format!(
                    "{}/v1/profiles/{}/dictionary",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .bearer_auth(&discovery.token)
        })?;
        self.decode(response)
    }

    pub fn create_reminder(
        &self,
        profile: ProfileId,
        rule: ReminderRule,
    ) -> Result<StoredReminder> {
        self.post(
            &format!("/v1/profiles/{}/reminders", profile.to_storage()),
            &RuleRequest { rule },
        )
    }

    pub fn update_reminder(
        &self,
        profile: ProfileId,
        reminder: ReminderId,
        rule: ReminderRule,
    ) -> Result<StoredReminder> {
        self.put(
            &format!(
                "/v1/profiles/{}/reminders/{}",
                profile.to_storage(),
                reminder.to_storage()
            ),
            &RuleRequest { rule },
        )
    }

    pub fn pause_reminder(
        &self,
        profile: ProfileId,
        reminder: ReminderId,
        revision: RuleRevisionId,
    ) -> Result<StoredReminder> {
        self.post(
            &format!(
                "/v1/profiles/{}/reminders/{}/pause",
                profile.to_storage(),
                reminder.to_storage()
            ),
            &RevisionRequest { revision },
        )
    }

    pub fn resume_reminder(
        &self,
        profile: ProfileId,
        reminder: ReminderId,
        revision: RuleRevisionId,
    ) -> Result<StoredReminder> {
        self.post(
            &format!(
                "/v1/profiles/{}/reminders/{}/resume",
                profile.to_storage(),
                reminder.to_storage()
            ),
            &RevisionRequest { revision },
        )
    }

    pub fn delete_reminder(
        &self,
        profile: ProfileId,
        reminder: ReminderId,
    ) -> Result<StoredReminder> {
        self.delete_query(&format!(
            "/v1/profiles/{}/reminders/{}",
            profile.to_storage(),
            reminder.to_storage()
        ))
    }

    pub fn snooze_reminder(
        &self,
        profile: ProfileId,
        reminder: ReminderId,
        action: OccurrenceAction,
    ) -> Result<StoredReminder> {
        self.post(
            &format!(
                "/v1/profiles/{}/reminders/{}/snooze",
                profile.to_storage(),
                reminder.to_storage()
            ),
            &ActionRequest { action },
        )
    }

    pub fn handle_reminder(
        &self,
        profile: ProfileId,
        reminder: ReminderId,
        action: OccurrenceAction,
    ) -> Result<StoredReminder> {
        self.post(
            &format!(
                "/v1/profiles/{}/reminders/{}/handle",
                profile.to_storage(),
                reminder.to_storage()
            ),
            &ActionRequest { action },
        )
    }

    pub fn handle_reminder_in(
        &self,
        profile: ProfileId,
        workspace: WorkspaceId,
        reminder: ReminderId,
        action: OccurrenceAction,
    ) -> Result<StoredReminder> {
        self.post(
            &format!(
                "/v1/profiles/{}/workspaces/{}/reminders/{}/handle",
                profile.to_storage(),
                workspace.to_storage(),
                reminder.to_storage()
            ),
            &ActionRequest { action },
        )
    }

    pub fn clear_reminder_history(&self, profile: ProfileId) -> Result<ClearedHistory> {
        self.delete_query(&format!(
            "/v1/profiles/{}/reminder-history",
            profile.to_storage()
        ))
    }

    pub fn clear_reminder_history_for_target(
        &self,
        profile: ProfileId,
        target: &koru_store_core::EntityRef,
    ) -> Result<ClearedHistory> {
        let path = format!("/v1/profiles/{}/reminder-history", profile.to_storage());
        let query = koru_store_wire::reminders::ClearHistoryQuery {
            entity: Some(target.to_storage()),
        };
        let response = self.send_with_retry(|discovery| {
            self.http
                .delete(format!("{}{path}", discovery.base_url()))
                .bearer_auth(&discovery.token)
                .query(&query)
        })?;
        self.decode(response)
    }

    /// Add (`present`) or remove one personal-dictionary word. Answers the
    /// whole dictionary after the write.
    pub fn set_dictionary_word(
        &self,
        id: ProfileId,
        language: &str,
        word: &str,
        present: bool,
    ) -> Result<PersonalDictionaryResponse> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .put(format!(
                    "{}/v1/profiles/{}/dictionary",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .bearer_auth(&discovery.token)
                .json(&SetDictionaryWord {
                    language: language.to_owned(),
                    word: word.to_owned(),
                    present,
                })
        })?;
        self.decode(response)
    }

    /// Clear every personal-dictionary word currently visible to this device.
    pub fn clear_personal_dictionary(&self, id: ProfileId) -> Result<PersonalDictionaryResponse> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .delete(format!(
                    "{}/v1/profiles/{}/dictionary",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .bearer_auth(&discovery.token)
        })?;
        self.decode(response)
    }

    /// The Welcome page rows of `rows` that are ticked on this installation,
    /// in the order asked. Machine-wide: every Profile here shares them.
    pub fn welcome_progress(&self, rows: &[&str]) -> Result<Vec<String>> {
        let answer: koru_store_wire::WelcomeProgress = self.get(&format!(
            "/v1/internal/welcome-progress?rows={}",
            rows.join(",")
        ))?;
        Ok(answer.done)
    }

    /// Tick one Welcome page row for every Profile on this installation.
    pub fn mark_welcome_done(&self, row: &str) -> Result<()> {
        let _: koru_store_wire::WelcomeProgress = self.put(
            &format!("/v1/internal/welcome-progress/{row}"),
            &serde_json::json!({}),
        )?;
        Ok(())
    }

    pub fn reminder_settings(&self, profile: ProfileId) -> Result<ReminderSettings> {
        self.get(&format!(
            "/v1/profiles/{}/reminder-settings",
            profile.to_storage()
        ))
    }

    pub fn update_reminder_settings(
        &self,
        profile: ProfileId,
        patch: SettingsPatch,
    ) -> Result<ReminderSettings> {
        self.patch(
            &format!("/v1/profiles/{}/reminder-settings", profile.to_storage()),
            &patch,
        )
    }

    pub fn query_reminders(
        &self,
        profile: ProfileId,
        query: ReminderQuery,
    ) -> Result<ReminderQueryResponse> {
        self.post(
            &format!("/v1/profiles/{}/reminders/query", profile.to_storage()),
            &query,
        )
    }

    pub fn reminder_reconciliation(&self, profile: ProfileId) -> Result<ReminderSnapshot> {
        self.get(&format!(
            "/v1/profiles/{}/reminders/reconciliation",
            profile.to_storage()
        ))
    }

    // ----------------------------------------------------------- workspaces

    pub fn create_workspace(&self, owner: ProfileId, name: &str) -> Result<Workspace> {
        self.post(
            "/v1/workspaces",
            &NewWorkspace {
                owner,
                name: name.to_owned(),
            },
        )
    }

    pub fn workspaces(&self) -> Result<Vec<Workspace>> {
        self.get("/v1/workspaces")
    }

    /// Document and byte totals for a profile's complete workspace table.
    pub fn workspace_usage(&self, profile: ProfileId) -> Result<Vec<WorkspaceUsage>> {
        self.get(&format!(
            "/v1/profiles/{}/workspace-usage",
            profile.to_storage()
        ))
    }

    pub fn workspaces_including_deleted(&self) -> Result<Vec<Workspace>> {
        self.get("/v1/workspaces?include_deleted=true")
    }

    pub fn reorder_workspaces(
        &self,
        owner: ProfileId,
        workspaces: &[WorkspaceId],
    ) -> Result<Vec<Workspace>> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .put(format!(
                    "{}/v1/profiles/{}/workspace-order",
                    discovery.base_url(),
                    owner.to_storage()
                ))
                .bearer_auth(&discovery.token)
                .json(&WorkspaceOrder {
                    workspaces: workspaces.to_vec(),
                })
        })?;
        self.decode(response)
    }

    pub fn delete_workspace(&self, id: WorkspaceId) -> Result<Workspace> {
        self.delete_workspace_with_force(id, false)
    }

    pub fn delete_workspace_with_force(&self, id: WorkspaceId, force: bool) -> Result<Workspace> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .delete(format!(
                    "{}/v1/workspaces/{}?force={force}",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .bearer_auth(&discovery.token)
        })?;
        self.decode(response)
    }

    pub fn restore_workspace(&self, id: WorkspaceId) -> Result<Workspace> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .post(format!(
                    "{}/v1/workspaces/{}/restore",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .bearer_auth(&discovery.token)
        })?;
        self.decode(response)
    }

    /// Export one complete, import-compatible workspace bundle.
    pub fn export_workspace(&self, workspace: WorkspaceId) -> Result<Vec<u8>> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .post(format!(
                    "{}/v1/workspaces/{}/export",
                    discovery.base_url(),
                    workspace.to_storage()
                ))
                .bearer_auth(&discovery.token)
        })?;
        let response = self.check(response)?;
        response
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(|error| Error::new(Code::StorageFailed, format!("bundle body: {error}")))
    }

    /// Import a complete workspace through the same public API every client
    /// uses. Encoding lives in `koru-store-wire`, beside the server decoder.
    pub fn import_workspace(
        &self,
        bundle: &Bundle,
        mode: ImportMode,
        owner: ProfileId,
    ) -> Result<Workspace> {
        // `Bytes`, not `Vec<u8>`: `send_with_retry`'s closure is `FnMut` and so
        // must be able to build the body twice. A bundle is the whole
        // workspace, and copying it per attempt would be the largest
        // allocation on the path.
        let body = bytes::Bytes::from(koru_store_wire::bundle::encode(bundle)?);
        let response = self.send_with_retry(|discovery| {
            self.http
                .post(format!(
                    "{}/v1/import?mode={}&owner={}",
                    discovery.base_url(),
                    mode.as_str(),
                    owner.to_storage(),
                ))
                .bearer_auth(&discovery.token)
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .body(body.clone())
        })?;
        self.decode(response)
    }

    // ------------------------------------------------------------ documents

    /// Register a document at the end of the workspace root.
    ///
    /// The convenience for a caller with no opinion about where it lands.
    /// [`create_document_at`](Self::create_document_at) is the one that places
    /// it.
    pub fn create_document(
        &self,
        workspace: WorkspaceId,
        title: &str,
        kind: &str,
    ) -> Result<DocumentId> {
        self.create_document_at(workspace, title, kind, None, None)
    }

    /// Register a document **under `parent`, at `index` among its siblings**.
    ///
    /// `None` for either means the workspace root and the end of the list. The
    /// index is the manifest's own coordinate — children of `parent`, trashed
    /// ones counted — so `entry.ordinal + 1` is *below that entry*, the same
    /// arithmetic a drop below a row in Explore does.
    pub fn create_document_at(
        &self,
        workspace: WorkspaceId,
        title: &str,
        kind: &str,
        parent: Option<DocumentId>,
        index: Option<usize>,
    ) -> Result<DocumentId> {
        let created: Created = self.post(
            &format!("/v1/workspaces/{}/documents", workspace.to_storage()),
            &NewDocument {
                id: None,
                title: title.to_owned(),
                kind: kind.to_owned(),
                parent,
                index,
                trashed_at: None,
                mime: None,
                snapshot: None,
            },
        )?;
        Ok(created.id)
    }

    /// Register a document with its complete initial snapshot in one Store
    /// request. The Store commits the opaque bytes before broadcasting the
    /// manifest row, so peers can never observe an imported empty page.
    pub fn import_document(
        &self,
        workspace: WorkspaceId,
        title: &str,
        kind: &str,
        snapshot: Vec<u8>,
    ) -> Result<DocumentId> {
        self.import_document_at(workspace, DocumentId::new(), title, kind, None, snapshot)
    }

    /// Register a parented document with a client-minted identity and complete
    /// initial snapshot. Used by importers whose entries refer to one another.
    pub fn import_document_at(
        &self,
        workspace: WorkspaceId,
        id: DocumentId,
        title: &str,
        kind: &str,
        parent: Option<DocumentId>,
        snapshot: Vec<u8>,
    ) -> Result<DocumentId> {
        self.import_document_at_position(workspace, id, title, kind, None, parent, None, snapshot)
    }

    /// Register an imported document with optional file metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn import_document_at_with_mime(
        &self,
        workspace: WorkspaceId,
        id: DocumentId,
        title: &str,
        kind: &str,
        mime: Option<&str>,
        parent: Option<DocumentId>,
        snapshot: Vec<u8>,
    ) -> Result<DocumentId> {
        self.import_document_at_position(workspace, id, title, kind, mime, parent, None, snapshot)
    }

    /// Register an imported document at an explicit sibling position.
    ///
    /// Importers normally preserve their source order by registering parents
    /// before children and appending each child. Synthetic diagnostics are the
    /// exception: `FAILED` belongs first beneath the imported root so it is not
    /// buried after a large notebook.
    #[allow(clippy::too_many_arguments)]
    pub fn import_document_at_position(
        &self,
        workspace: WorkspaceId,
        id: DocumentId,
        title: &str,
        kind: &str,
        mime: Option<&str>,
        parent: Option<DocumentId>,
        index: Option<usize>,
        snapshot: Vec<u8>,
    ) -> Result<DocumentId> {
        self.import_document_at_position_trashed(
            workspace, id, title, kind, mime, parent, index, None, snapshot,
        )
    }

    /// Register an imported document at an explicit position and optionally
    /// create its manifest row already in Trash at the supplied source time.
    #[allow(clippy::too_many_arguments)]
    pub fn import_document_at_position_trashed(
        &self,
        workspace: WorkspaceId,
        id: DocumentId,
        title: &str,
        kind: &str,
        mime: Option<&str>,
        parent: Option<DocumentId>,
        index: Option<usize>,
        trashed_at: Option<koru_store_core::model::Timestamp>,
        snapshot: Vec<u8>,
    ) -> Result<DocumentId> {
        let created: Created = self.post(
            &format!("/v1/workspaces/{}/documents", workspace.to_storage()),
            &NewDocument {
                id: Some(id),
                title: title.to_owned(),
                kind: kind.to_owned(),
                mime: mime.map(str::to_owned),
                parent,
                index,
                trashed_at,
                snapshot: Some(snapshot),
            },
        )?;
        Ok(created.id)
    }

    /// The one note a file carries, creating it when it has none.
    ///
    /// Idempotent — the answer is the note's id whether this call created it or
    /// it was already there, so "Add note" and "Open note" are the same call.
    /// [`EnsuredFileNote::created`] says which of the two happened.
    pub fn ensure_file_note(
        &self,
        workspace: WorkspaceId,
        file: DocumentId,
    ) -> Result<EnsuredFileNote> {
        self.post_query(&format!(
            "/v1/workspaces/{}/documents/{}/note",
            workspace.to_storage(),
            file.to_storage()
        ))
    }

    /// Move a document (and its explore subtree) into another workspace.
    ///
    /// Document ids are preserved. See the store's `transfer_documents`.
    pub fn transfer_documents(
        &self,
        source: WorkspaceId,
        target: WorkspaceId,
        root: DocumentId,
        parent: Option<DocumentId>,
        index: usize,
    ) -> Result<Vec<DocumentId>> {
        use koru_store_wire::{TransferDocuments, Transferred};
        let transferred: Transferred = self.post(
            &format!("/v1/workspaces/{}/transfer", source.to_storage()),
            &TransferDocuments {
                target,
                root,
                parent,
                index,
            },
        )?;
        Ok(transferred.ids)
    }

    /// Atomically expose a document only after its opaque initial content is
    /// durable. A replay returns the original update bytes and document id.
    #[allow(clippy::too_many_arguments)] // wire operation mirrors the complete seeded document
    pub fn seed_document(
        &self,
        workspace: WorkspaceId,
        title: &str,
        kind: &str,
        parent: Option<DocumentId>,
        index: Option<usize>,
        idempotency_key: &str,
        request_hash: &str,
        initial_update: Vec<u8>,
    ) -> Result<SeededCreate> {
        let request = SeedDocumentRequest {
            title: title.to_owned(),
            kind: kind.to_owned(),
            parent,
            index,
            idempotency_key: idempotency_key.to_owned(),
            request_hash: request_hash.to_owned(),
            initial_update,
        };
        let response = self.send_with_retry(|discovery| {
            self.http
                .post(format!(
                    "{}/v1/internal/workspaces/{}/documents/seeded",
                    discovery.base_url(),
                    workspace.to_storage()
                ))
                .bearer_auth(&discovery.token)
                .json(&request)
        })?;
        Self::seeded_create_response(self.check(response)?)
    }

    /// Seed a document whose identity was minted by a trusted domain adapter.
    /// The Store still owns atomic content-first publication.
    #[allow(clippy::too_many_arguments)]
    pub fn seed_document_with_candidate(
        &self,
        workspace: WorkspaceId,
        title: &str,
        kind: &str,
        parent: Option<DocumentId>,
        index: Option<usize>,
        candidate: DocumentId,
        idempotency_key: &str,
        request_hash: &str,
        initial_update: Vec<u8>,
    ) -> Result<SeededCreate> {
        let request = SeedDocumentWithCandidateRequest {
            title: title.to_owned(),
            kind: kind.to_owned(),
            candidate,
            parent,
            index,
            idempotency_key: idempotency_key.to_owned(),
            request_hash: request_hash.to_owned(),
            initial_update,
        };
        let response = self.send_with_retry(|discovery| {
            self.http
                .post(format!(
                    "{}/v2/internal/workspaces/{}/documents/seeded",
                    discovery.base_url(),
                    workspace.to_storage()
                ))
                .bearer_auth(&discovery.token)
                .json(&request)
        })?;
        Self::seeded_create_response(self.check(response)?)
    }

    fn seeded_create_response(response: reqwest::blocking::Response) -> Result<SeededCreate> {
        let id = response
            .headers()
            .get("x-koru-document-id")
            .and_then(|value| value.to_str().ok())
            .and_then(DocumentId::parse)
            .ok_or_else(|| Error::new(Code::Internal, "seed response has no document id"))?;
        let replayed = response
            .headers()
            .get("x-koru-idempotent-replay")
            .and_then(|value| value.to_str().ok())
            == Some("true");
        let initial_update = response
            .bytes()
            .map_err(|error| {
                Error::new(Code::Internal, format!("unreadable seed response: {error}"))
            })?
            .to_vec();
        Ok(SeededCreate {
            id,
            initial_update,
            replayed,
        })
    }

    pub fn documents(&self, workspace: WorkspaceId) -> Result<Vec<DocumentEntry>> {
        self.get(&format!(
            "/v1/workspaces/{}/documents",
            workspace.to_storage()
        ))
    }

    /// Resolve a stable document id through the Store's authoritative catalog.
    pub fn locate_document(&self, document: DocumentId) -> Result<DocumentLocation> {
        self.get(&format!("/v1/documents/{}/location", document.to_storage()))
    }

    /// The document's headings, for a picker offering jump targets.
    ///
    /// Derived data: it trails a live edit by the projector's debounce, and
    /// `projected` is how a caller tells "not indexed yet" from "no headings".
    pub fn document_outline(&self, document: DocumentId) -> Result<DocumentOutline> {
        self.get(&format!("/v1/documents/{}/outline", document.to_storage()))
    }

    /// The nodes matching an attribute query, across workspaces, from the
    /// store's read model.
    ///
    /// Derived data, like the outline: the answer trails a live edit by the
    /// projector's debounce, and `updating_workspaces` names any workspace it
    /// may be behind on. Rows carry `(workspace, document, node)` together —
    /// a node id alone means nothing outside its document.
    pub fn query_attributes(&self, query: &AttributeQuery) -> Result<AttributeQueryResponse> {
        self.post("/v1/attributes/query", query)
    }

    /// What is outstanding, across workspaces, from the store's read model.
    ///
    /// **The attribute query's second question**, and derived data in exactly
    /// the same way: the answer trails a live edit by the projector's debounce,
    /// and `updating_workspaces` names any workspace it may be behind on rather
    /// than making the caller wait for it.
    ///
    /// Paging is a keyset on the `task:` reference, which is also each row's
    /// identity — so `next_after` is a row that exists rather than a count that
    /// shifts under an edit. `restarted` says the cursor named a row the filter
    /// no longer matches (completed, removed or binned between pages) and the
    /// answer is page one; a caller that could not tell that from a
    /// continuation would silently draw the first page twice.
    pub fn query_tasks(&self, query: &TaskQuery) -> Result<TaskQueryResponse> {
        self.post("/v1/tasks/query", query)
    }

    /// The document a client joins to see what a workspace contains.
    pub fn manifest(&self, workspace: WorkspaceId) -> Result<DocumentId> {
        let reference: ManifestRef = self.get(&format!(
            "/v1/workspaces/{}/manifest",
            workspace.to_storage()
        ))?;
        Ok(reference.document)
    }

    /// Rename a workspace, in the catalog and in its manifest.
    pub fn rename_workspace(&self, id: WorkspaceId, name: &str) -> Result<Workspace> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .patch(format!(
                    "{}/v1/workspaces/{}",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .bearer_auth(&discovery.token)
                .json(&RenameWorkspace {
                    name: name.to_owned(),
                })
        })?;
        self.decode(response)
    }

    /// Set how a workspace appears in a switcher: icon, colour, pinned,
    /// archived.
    ///
    /// **All four, every time.** They are edited together and the caller knows
    /// all of them, so there is no *absent versus null* to get wrong.
    pub fn set_workspace_appearance(
        &self,
        id: WorkspaceId,
        icon: Option<&str>,
        color: Option<&str>,
        pinned: bool,
        archived: bool,
    ) -> Result<Workspace> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .patch(format!(
                    "{}/v1/workspaces/{}/appearance",
                    discovery.base_url(),
                    id.to_storage()
                ))
                .bearer_auth(&discovery.token)
                .json(&WorkspaceAppearance {
                    icon: icon.map(str::to_owned),
                    color: color.map(str::to_owned),
                    pinned,
                    archived,
                })
        })?;
        self.decode(response)
    }

    /// Store bytes, and get back the hash they are filed under.
    ///
    /// **The caller does not choose the hash and does not compute one.** The
    /// store hashes what it receives and files the blob at that address; the
    /// returned [`Blob::hash`] is what a document should record. That is what
    /// makes content addressing hold across two processes — two hash functions
    /// for one blob would be a second thing to keep in agreement, and the one
    /// that disagreed would be an image that renders as a grey box on the other
    /// machine.
    ///
    /// Storing the same bytes twice is not an error and does not make a second
    /// copy: the address is the content.
    ///
    /// `filename` is percent-encoded on the way out, because a filename is
    /// arbitrary Unicode and a header value is not — a raw newline in one would
    /// split the request. The store decodes it symmetrically.
    pub fn put_blob(
        &self,
        workspace: WorkspaceId,
        bytes: Vec<u8>,
        mime: &str,
        filename: Option<&str>,
    ) -> Result<Blob> {
        let filename = filename.map(percent_encode);
        // As in `import_workspace`: a pasted image must not be copied once per
        // send attempt just because the closure has to be `FnMut`.
        let bytes = bytes::Bytes::from(bytes);
        let response = self.send_with_retry(|discovery| {
            let mut request = self
                .http
                .put(format!(
                    "{}/v1/workspaces/{}/blobs",
                    discovery.base_url(),
                    workspace.to_storage()
                ))
                .bearer_auth(&discovery.token)
                .header(reqwest::header::CONTENT_TYPE, mime)
                .body(bytes.clone());
            if let Some(name) = &filename {
                request = request.header("x-koru-filename", name);
            }
            request
        })?;
        self.decode(response)
    }

    /// Stream a blob from a source that can be reconstructed for one retry.
    ///
    /// The factory, rather than one `Read`, is the retry contract: each HTTP
    /// attempt starts from byte zero. The body remains streaming in reqwest and
    /// is never accumulated by this client.
    pub fn put_blob_from<F>(
        &self,
        workspace: WorkspaceId,
        factory: F,
        mime: &str,
        filename: Option<&str>,
    ) -> Result<Blob>
    where
        F: Fn() -> Box<dyn std::io::Read + Send>,
    {
        let filename = filename.map(percent_encode);
        let response = self.send_with_retry(|discovery| {
            let mut request = self
                .http
                .put(format!(
                    "{}/v1/workspaces/{}/blobs",
                    discovery.base_url(),
                    workspace.to_storage()
                ))
                .bearer_auth(&discovery.token)
                .header(reqwest::header::CONTENT_TYPE, mime)
                .body(reqwest::blocking::Body::new(factory()));
            if let Some(name) = &filename {
                request = request.header("x-koru-filename", name);
            }
            request
        })?;
        self.decode(response)
    }

    /// The bytes behind a hash, verified by the store against the address they
    /// are filed under.
    pub fn get_blob(&self, workspace: WorkspaceId, hash: &BlobHash) -> Result<Vec<u8>> {
        let response = self.send_with_retry(|discovery| {
            self.hydration_http
                .get(format!(
                    "{}/v1/workspaces/{}/blobs/{}",
                    discovery.base_url(),
                    workspace.to_storage(),
                    hash.as_str()
                ))
                .bearer_auth(&discovery.token)
        })?;
        let response = self.check(response)?;
        response
            .bytes()
            .map(|b| b.to_vec())
            .map_err(|e| Error::new(Code::StorageFailed, format!("blob body: {e}")))
    }

    /// Associate `target` with a blob `source` holds, before publishing CRDT
    /// references to it there. **No bytes move** — one object per store root,
    /// and the association is the permission.
    pub fn associate_blob(
        &self,
        source: WorkspaceId,
        target: WorkspaceId,
        hash: &BlobHash,
    ) -> Result<Blob> {
        self.post(
            &format!(
                "/v1/workspaces/{}/blobs/{}",
                source.to_storage(),
                hash.as_str()
            ),
            &AssociateBlob { target },
        )
    }

    /// Every blob this workspace holds. What a garbage collector, or a person
    /// wondering where the disk went, would ask.
    pub fn blobs(&self, workspace: WorkspaceId) -> Result<Vec<Blob>> {
        self.get(&format!("/v1/workspaces/{}/blobs", workspace.to_storage()))
    }

    /// A document's whole state, as bytes ready to `import`.
    pub fn snapshot(&self, workspace: WorkspaceId, document: DocumentId) -> Result<Vec<u8>> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .post(format!(
                    "{}/v1/workspaces/{}/snapshot/{}",
                    discovery.base_url(),
                    workspace.to_storage(),
                    document.to_storage()
                ))
                .bearer_auth(&discovery.token)
        })?;
        let response = self.check(response)?;
        response
            .bytes()
            .map(|b| b.to_vec())
            .map_err(|e| Error::new(Code::StorageFailed, e.to_string()))
    }

    pub fn capture_history(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
        request: &CaptureHistory,
    ) -> Result<HistoryEntry> {
        self.post(
            &format!(
                "/v1/workspaces/{}/documents/{}/history",
                workspace.to_storage(),
                document.to_storage()
            ),
            request,
        )
    }

    pub fn history(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
        query: &HistoryQuery,
    ) -> Result<HistoryPage> {
        let mut path = format!(
            "/v1/workspaces/{}/documents/{}/history?limit={}",
            workspace.to_storage(),
            document.to_storage(),
            query.limit.unwrap_or(50).clamp(1, 200)
        );
        if let (Some(at), Some(id)) = (query.before_at, query.before_id) {
            path.push_str(&format!("&before_at={at}&before_id={}", id.to_storage()));
        }
        self.get(&path)
    }

    /// **Hold this workspace's pictures, or let them go again** (D256).
    ///
    /// Device-local, and documents are untouched by it: what stops a document
    /// is the transport mode, which is a durable choice and a different call.
    /// Answers the report as it stands afterwards, so a surface that pressed
    /// it does not have to ask again to draw the result.
    pub fn pause_blob_transfer(
        &self,
        workspace: WorkspaceId,
        paused: bool,
    ) -> Result<koru_store_wire::internal::BlobTransferReport> {
        self.post(
            &format!("/v1/workspaces/{}/blobs/pause", workspace.to_storage()),
            &koru_store_wire::PauseBlobTransfer { paused },
        )
    }

    /// **Clear local files**: every copy Koru Cloud holds leaves this device,
    /// every reference stays, and the bytes come back the way they came the
    /// first time. Answers the numbers, never a name.
    pub fn release_local_blobs(
        &self,
        workspace: WorkspaceId,
    ) -> Result<koru_store_wire::internal::BlobReleaseReport> {
        self.post_query(&format!(
            "/v1/workspaces/{}/blobs/release",
            workspace.to_storage()
        ))
    }

    /// **The local Store operation behind the Profile prompt** (D258).
    ///
    /// The Profile moves; `include_workspaces` says whether its Workspaces
    /// come with it, in one group. A Workspace somebody already chose for is
    /// never in that group, whichever way this is answered.
    ///
    /// Nothing hosted follows from it (D230): another device is told nothing
    /// and decides for itself.
    pub fn set_profile_transport_mode(
        &self,
        profile: ProfileId,
        mode: &str,
        include_workspaces: bool,
    ) -> Result<koru_store_wire::ProfileTransportModeOutcome> {
        self.put(
            &format!("/v1/profiles/{}/transport-mode", profile.to_storage()),
            &koru_store_wire::SetProfileTransportMode {
                mode: mode.to_owned(),
                include_workspaces,
            },
        )
    }

    /// What Cloud sync has done to one document — one round trip, no content.
    pub fn cloud_sync_status(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<koru_store_wire::internal::CloudSyncStatus> {
        self.cloud_sync_status_for(workspace, document, None)
    }

    /// The same round trip, **plus the one file a surface is explaining**
    /// (D259 §4).
    ///
    /// One call rather than two because two polls are two moments: a pane
    /// that drew the Workspace's counts from one answer and the selected
    /// file's state from another would be showing a contradiction of exactly
    /// the kind D259 exists to remove.
    pub fn cloud_sync_status_for(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
        file: Option<DocumentId>,
    ) -> Result<koru_store_wire::internal::CloudSyncStatus> {
        let path = format!(
            "/v1/workspaces/{}/documents/{}/cloud-sync",
            workspace.to_storage(),
            document.to_storage()
        );
        match file {
            Some(file) => self.get(&format!(
                "{path}?file={}",
                percent_encode(&file.to_storage())
            )),
            None => self.get(&path),
        }
    }

    /// **One document's own Cloud state**: whether Koru Cloud holds it as it
    /// is now, and how many of the images and other files it references Koru
    /// Cloud holds durably.
    ///
    /// Counts only. Read-only on the Store's side: asking is not a trigger.
    pub fn cloud_sync_coverage(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<koru_store_wire::internal::CloudDocumentCoverage> {
        self.get(&format!(
            "/v1/workspaces/{}/documents/{}/cloud-sync/coverage",
            workspace.to_storage(),
            document.to_storage()
        ))
    }

    /// **Which documents in this Workspace are waiting on a file Koru Cloud
    /// does not hold yet**, and the devices known to have held those files.
    ///
    /// Titles, counts and device names; never a byte of content or a file's
    /// address. Read-only on the Store's side: asking is not a trigger.
    pub fn cloud_sync_waiting(
        &self,
        workspace: WorkspaceId,
    ) -> Result<koru_store_wire::CloudWaitingDocuments> {
        self.get(&format!(
            "/v1/workspaces/{}/cloud-sync/waiting",
            workspace.to_storage()
        ))
    }

    pub fn relabel_history(
        &self,
        workspace: WorkspaceId,
        entry: HistoryEntryId,
        request: &RelabelHistory,
    ) -> Result<HistoryEntry> {
        self.post(
            &format!(
                "/v1/workspaces/{}/history/{}/label",
                workspace.to_storage(),
                entry.to_storage()
            ),
            request,
        )
    }

    pub fn history_snapshot(
        &self,
        workspace: WorkspaceId,
        entry: HistoryEntryId,
    ) -> Result<Vec<u8>> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .post(format!(
                    "{}/v1/workspaces/{}/history/{}/snapshot",
                    discovery.base_url(),
                    workspace.to_storage(),
                    entry.to_storage()
                ))
                .bearer_auth(&discovery.token)
        })?;
        let response = self.check(response)?;
        response
            .bytes()
            .map(|b| b.to_vec())
            .map_err(|e| Error::new(Code::StorageFailed, e.to_string()))
    }

    /// The immutable bytes a `DocumentVersionId` names — what a delegated
    /// run's pin resolves to (ROADMAP §7.2). `Ok(None)` when the workspace
    /// holds no such version: never captured there, or purged. The bytes
    /// are checked here against the checksum the store sent beside them,
    /// so a transfer that corrupted them is `corrupt_state` rather than a
    /// snapshot that fails to import later.
    pub fn version_snapshot(
        &self,
        workspace: WorkspaceId,
        version: DocumentVersionId,
    ) -> Result<Option<VersionSnapshot>> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .get(format!(
                    "{}/v1/workspaces/{}/versions/{}/snapshot",
                    discovery.base_url(),
                    workspace.to_storage(),
                    version.to_storage()
                ))
                .bearer_auth(&discovery.token)
        })?;
        let response = match self.check(response) {
            Ok(response) => response,
            Err(error) if error.code == Code::DocumentVersionNotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let header = |name: &str| -> Result<String> {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
                .ok_or_else(|| Error::new(Code::Internal, format!("version response lacks {name}")))
        };
        let document = DocumentId::parse(&header("x-koru-document-id")?)
            .ok_or_else(|| Error::new(Code::Internal, "version response names no document"))?;
        let checksum = header("x-koru-checksum")?;
        let captured_at = header("x-koru-captured-at")?
            .parse::<i64>()
            .map(koru_store_core::model::Timestamp::from_millis)
            .map_err(|_| Error::new(Code::Internal, "version response capture time"))?;
        let snapshot = response
            .bytes()
            .map(|b| b.to_vec())
            .map_err(|e| Error::new(Code::StorageFailed, e.to_string()))?;
        if koru_store_core::lifecycle::checksum(&snapshot) != checksum {
            return Err(Error::new(
                Code::CorruptState,
                "version bytes do not match the checksum the store sent",
            ));
        }
        Ok(Some(VersionSnapshot {
            id: version,
            document,
            checksum,
            captured_at,
            snapshot,
        }))
    }

    // ---------------------------------------------- sibling domain namespaces

    /// Generic authenticated JSON calls, for sibling domain namespaces —
    /// `koru-api-client`'s `events()` — that ride this wire engine (the
    /// endpoint, credential renewal, the single 401 retry) without this crate
    /// learning their vocabulary. Mechanism goes down; the domain stays with
    /// its owner (PRINCIPLES). Store operations keep their typed methods;
    /// these exist so a second worker behind the same gateway does not grow a
    /// second copy of `send_with_retry`.
    pub fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.get(path)
    }

    pub fn post_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        self.post(path, body)
    }

    pub fn put_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .put(format!("{}{path}", discovery.base_url()))
                .bearer_auth(&discovery.token)
                .json(body)
        })?;
        self.decode(response)
    }

    /// An authenticated GET whose body the caller reads incrementally — the
    /// SSE tail. The per-request deadline caps the whole read, so a healthy
    /// stream is cut and resumed by the caller's cursor rather than living
    /// forever against the engine's 30-second default; it is also what makes
    /// a subscription thread joinable in bounded time.
    pub fn get_streaming(
        &self,
        path: &str,
        accept: &str,
        deadline: std::time::Duration,
    ) -> Result<StreamingBody> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .get(format!("{}{path}", discovery.base_url()))
                .bearer_auth(&discovery.token)
                .header(reqwest::header::ACCEPT, accept)
                .timeout(deadline)
        })?;
        let response = self.check(response)?;
        Ok(StreamingBody { response })
    }

    /// Authenticated JSON/streaming mechanisms for sibling public namespaces.
    ///
    /// These preserve the public stable failure token rather than translating
    /// it through Store's `Code` enum. The namespace owning that token performs
    /// the typed mapping. All requests still share this client's endpoint,
    /// credential, renewal gate, and one-retry engine.
    pub fn get_public_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> PublicResult<T> {
        let response = self
            .send_with_retry_policy(RetryPolicy::Safe, |discovery| {
                self.http
                    .get(format!("{}{path}", discovery.base_url()))
                    .bearer_auth(&discovery.token)
            })
            .map_err(PublicError::Transport)?;
        self.decode_public(response)
    }

    pub fn post_public_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        retry: RetryPolicy,
    ) -> PublicResult<T> {
        let response = self
            .send_with_retry_policy(retry, |discovery| {
                self.http
                    .post(format!("{}{path}", discovery.base_url()))
                    .bearer_auth(&discovery.token)
                    .json(body)
            })
            .map_err(PublicError::Transport)?;
        self.decode_public(response)
    }

    pub fn put_public_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        retry: RetryPolicy,
    ) -> PublicResult<T> {
        let response = self
            .send_with_retry_policy(retry, |discovery| {
                self.http
                    .put(format!("{}{path}", discovery.base_url()))
                    .bearer_auth(&discovery.token)
                    .json(body)
            })
            .map_err(PublicError::Transport)?;
        self.decode_public(response)
    }

    /// A public `PATCH` of named fields.
    ///
    /// Added for the settings authority, whose whole point is that a body
    /// names the fields it changes and no others. `RetryPolicy` is the
    /// caller's: a field-scoped patch is idempotent and safe to retry, but
    /// that is a property of the route rather than of the verb, so it is not
    /// assumed here.
    pub fn patch_public_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        retry: RetryPolicy,
    ) -> PublicResult<T> {
        let response = self
            .send_with_retry_policy(retry, |discovery| {
                self.http
                    .patch(format!("{}{path}", discovery.base_url()))
                    .bearer_auth(&discovery.token)
                    .json(body)
            })
            .map_err(PublicError::Transport)?;
        self.decode_public(response)
    }

    pub fn delete_public_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        retry: RetryPolicy,
    ) -> PublicResult<T> {
        let response = self
            .send_with_retry_policy(retry, |discovery| {
                self.http
                    .delete(format!("{}{path}", discovery.base_url()))
                    .bearer_auth(&discovery.token)
                    .json(body)
            })
            .map_err(PublicError::Transport)?;
        self.decode_public(response)
    }

    pub fn get_public_streaming(
        &self,
        path: &str,
        accept: &str,
        deadline: std::time::Duration,
    ) -> PublicResult<StreamingBody> {
        let response = self
            .send_with_retry_policy(RetryPolicy::Safe, |discovery| {
                self.http
                    .get(format!("{}{path}", discovery.base_url()))
                    .bearer_auth(&discovery.token)
                    .header(reqwest::header::ACCEPT, accept)
                    .timeout(deadline)
            })
            .map_err(PublicError::Transport)?;
        let response = self.check_public(response)?;
        Ok(StreamingBody { response })
    }

    // ------------------------------------------------------------- internals

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .get(format!("{}{path}", discovery.base_url()))
                .bearer_auth(&discovery.token)
        })?;
        self.decode(response)
    }

    fn post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .post(format!("{}{path}", discovery.base_url()))
                .bearer_auth(&discovery.token)
                .json(body)
        })?;
        self.decode(response)
    }

    fn put<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .put(format!("{}{path}", discovery.base_url()))
                .bearer_auth(&discovery.token)
                .json(body)
        })?;
        self.decode(response)
    }

    fn patch<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .patch(format!("{}{path}", discovery.base_url()))
                .bearer_auth(&discovery.token)
                .json(body)
        })?;
        self.decode(response)
    }

    fn delete_query<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .delete(format!("{}{path}", discovery.base_url()))
                .bearer_auth(&discovery.token)
        })?;
        self.decode(response)
    }

    /// A POST with no body whose reply *is* worth reading.
    fn post_query<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .post(format!("{}{path}", discovery.base_url()))
                .bearer_auth(&discovery.token)
        })?;
        self.decode(response)
    }

    /// A POST whose reply is not worth reading.
    ///
    /// The response is still *checked* — a shutdown that was refused for want
    /// of a scope has to reach the caller — but the body is discarded rather
    /// than deserialized into a unit, which `serde_json` would reject on any
    /// reply that is not literally `null`.
    fn post_empty(&self, path: &str) -> Result<()> {
        let response = self.send_with_retry(|discovery| {
            self.http
                .post(format!("{}{path}", discovery.base_url()))
                .bearer_auth(&discovery.token)
        })?;
        self.check(response)?;
        Ok(())
    }

    fn decode<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::blocking::Response,
    ) -> Result<T> {
        let response = self.check(response)?;
        response
            .json()
            .map_err(|e| Error::new(Code::Internal, format!("unreadable response: {e}")))
    }

    fn decode_public<T: serde::de::DeserializeOwned>(
        &self,
        response: reqwest::blocking::Response,
    ) -> PublicResult<T> {
        let response = self.check_public(response)?;
        response.json().map_err(|error| {
            PublicError::Transport(Error::new(
                Code::Internal,
                format!("unreadable response: {error}"),
            ))
        })
    }

    fn check_public(
        &self,
        response: reqwest::blocking::Response,
    ) -> PublicResult<reqwest::blocking::Response> {
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status().as_u16();
        let body = response.text().unwrap_or_default();
        if status == 404 && body.trim().is_empty() {
            return Err(PublicError::Failure(PublicFailure {
                status,
                code: "api_version_unsupported".into(),
                message: "the running Koru Gateway does not provide this endpoint; restart it and try again".into(),
                waiting_on: Vec::new(),
                body: None,
            }));
        }
        let failure = serde_json::from_str::<Failure>(&body).map_err(|_| {
            PublicError::Transport(Error::new(
                Code::Internal,
                format!("unreadable public failure response (HTTP {status})"),
            ))
        })?;
        Err(PublicError::Failure(PublicFailure {
            status,
            code: failure.code,
            message: failure.message,
            waiting_on: failure.waiting_on,
            body: serde_json::from_str(&body).ok(),
        }))
    }

    /// Turn a failed response back into the store's own error.
    ///
    /// **The code is preserved, not the status.** A caller branches on
    /// `workspace_not_found`, never on 404 — which is what lets the same client
    /// work over a transport that has no status codes at all.
    fn check(&self, response: reqwest::blocking::Response) -> Result<reqwest::blocking::Response> {
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let body = response.text().unwrap_or_default();
        // An Axum router-level 404 has no Store `Failure` body. This is what an
        // older Store answers when a newer client calls an endpoint introduced
        // after that daemon started. Preserve that distinction so the shell can
        // ask for the one useful remedy instead of reporting `internal`.
        if status == reqwest::StatusCode::NOT_FOUND && body.trim().is_empty() {
            return Err(Error::new(
                Code::ApiVersionUnsupported,
                "the running Koru Store does not provide this endpoint; restart Store and try again",
            ));
        }
        let Ok(failure) = serde_json::from_str::<Failure>(&body) else {
            return Err(Error::new(Code::Internal, body));
        };
        // `waiting_on` is display data — the devices a parked hydration is
        // waiting for, so a client can name them in its own sentence. It rides
        // the `Error` structurally; `Display` renders the `\u{1f}`-joined
        // suffix for consumers that only have the message, and `message` stays
        // untranslated text either way — a client that shows it raw has
        // already lost.
        Err(Error::new(
            code_from(&failure.code),
            format!("{}: {}", failure.code, failure.message),
        )
        .with_waiting_on(failure.waiting_on)
        // **And `hold` the same way** (D259): the blob engine's own
        // vocabulary, which is not this crate's `Code` set and must not be
        // recovered by parsing the developer text above. The missing-image
        // placeholder branches on it to say the true thing rather than rung
        // 1's peer sentence.
        .with_hold(failure.hold))
    }
}

/// Map a stable token back to this build's [`Code`].
///
/// An unrecognised token becomes [`Code::Internal`] with the token kept in the
/// message — a client built against an older store must not lose the fact that
/// something failed just because it has not heard of the reason.
fn code_from(token: &str) -> Code {
    Code::ALL
        .iter()
        .copied()
        .find(|c| c.as_str() == token)
        .unwrap_or(Code::Internal)
}

/// One immutable document version, as `GET /v1/workspaces/{id}/versions/{version}/snapshot`
/// answers it: the bytes, verified against the store's checksum, and the
/// document and moment they belong to. The `Debug` prints no bytes — a
/// snapshot is a person's document.
#[derive(Clone, PartialEq, Eq)]
pub struct VersionSnapshot {
    pub id: DocumentVersionId,
    pub document: DocumentId,
    /// The store's checksum of `snapshot`, which the bytes were verified
    /// against on receipt.
    pub checksum: String,
    pub captured_at: koru_store_core::model::Timestamp,
    pub snapshot: Vec<u8>,
}

impl std::fmt::Debug for VersionSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VersionSnapshot")
            .field("id", &self.id)
            .field("document", &self.document)
            .field("checksum", &self.checksum)
            .field("bytes", &self.snapshot.len())
            .finish_non_exhaustive()
    }
}

/// A request that never got an answer.
///
/// **[`Code::StoreUnreachable`], not `StorageFailed`.** The two used to be one
/// code, so a caller holding a long-lived client could not tell "that store has
/// gone" — worth re-dialling, because the port and the session credential both
/// die with the process — from "that store's disk failed", which re-dialling
/// cannot help. `koru-mcp` is the caller that pays for the difference: it is a
/// bridge that outlives store restarts.
fn unreachable_store(e: reqwest::Error) -> Error {
    Error::new(
        Code::StoreUnreachable,
        format!("could not reach the store: {e}"),
    )
}

/// Where a store keeps its files, given a root. Exposed so a caller can point
/// at a store it started itself.
pub fn root_of(path: impl Into<PathBuf>) -> PathBuf {
    path.into()
}

#[cfg(test)]
mod tests;

/// Percent-encode everything that is not unreserved ASCII.
///
/// The mirror of the store's own decoder. A filename can hold a newline, a
/// colon or any Unicode at all; a header value can hold none of those.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
