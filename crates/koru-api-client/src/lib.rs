// SPDX-License-Identifier: MPL-2.0

//! The one public client boundary used by Koru products.
//!
//! Store's mature wire client remains an implementation detail. Callers enter
//! Store through [`Client::store`] and AI through [`Client::ai`]; both use the
//! same discovered gateway endpoint and renewable bearer authority.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use bytes::Bytes;
use koru_ai_core::{
    AiProviderId, ChatId, DocumentExportId, InferenceAttemptId, MessageId, ToolInvocationId,
};
use koru_ai_wire::{
    AiSearchRequest, AiSearchResponse, AttemptResponse, BranchChatRequest, BranchChatResponse,
    CancelAttemptResponse, CancelModelPullResponse, ChatToolPolicyResponse, ChatTranscript,
    ClearHistoryResponse, ClearUsageResponse, CreateChatRequest, CreateChatResponse,
    DeleteChatResponse, ExportChatRequest, ExportChatResponse, Failure as AiFailure,
    FeedbackRequest, FeedbackResponse, ListChatsToolRequest, ListChatsToolResponse, ModelPullEvent,
    OllamaSettingsView, OpenAiSettingsView, ProviderSettingsResponse, ProviderValidationResponse,
    PullOllamaModelAccepted, PullOllamaModelRequest, ReadChatToolRequest, ReadChatToolResponse,
    RetryTurnRequest, SearchChatsToolRequest, SearchChatsToolResponse, SelectProviderRequest,
    SendTurnAccepted, SendTurnRequest, StreamEvent, ToolDecisionResponse, UpdateChatModelRequest,
    UpdateChatModelResponse, UpdateChatPrivacyRequest, UpdateChatPrivacyResponse,
    UpdateChatToolPolicyRequest, UpdateOllamaSettingsRequest, UpdateOpenAiSettingsRequest,
    UsageResponse,
};

use koru_store_core::errors::{Code, Error, Result};

pub use koru_api_wire::{
    FederatedSearchHit, FederatedSearchPage, FederatedSearchRequest, FederatedSearchResponse,
    SearchSource, SearchSourceState, SearchSourceStatus, WorkspaceLifecycleAction,
};
pub mod events;
pub mod grants;
pub use events::{
    CatalogChange, CatalogFlags, CatalogWatch, Cursor, EventsClient, Filter, Receipt, Subscription,
    catalog_change,
};
pub mod settings;
pub use settings::link::{
    ConfirmedFailure, ConfirmedOutcome, ConfirmedWrite, GlobalLink, LINK_SCOPES, PatchReply,
    SectionLink, TerminalLink,
};
pub use settings::{
    Pending, SectionFlags, SettingsApiFailure, SettingsClient, SettingsError, SettingsResult,
    SettingsWatch, Timeout, section_changed,
};
pub mod sessions;
pub use sessions::{
    SessionApiFailure, SessionFailureCode, SessionSubscription, SessionsClient, SessionsError,
    SessionsResult,
};
pub mod artifacts;
pub use artifacts::{ArtifactsClient, ArtifactsResult};
#[cfg(feature = "delegate")]
pub mod runs;
#[cfg(feature = "delegate")]
pub use runs::{RunSubscription, RunsClient, RunsResult};
#[cfg(feature = "delegate")]
pub mod schedules;
#[cfg(feature = "delegate")]
pub use schedules::{ScheduleSubscription, SchedulesClient, SchedulesResult};
pub mod catalog;
pub use catalog::{
    CatalogClient, WORKSPACE_IN_USE, WorkspaceApiFailure, WorkspaceError, WorkspaceResult,
};

/// The scope that gates archive, soft-delete and restore.
///
/// **In neither the default nor the allowed set.** Naming it is not enough; a
/// caller must also be vouched for by the Agent that launched it — see
/// [`Client::connect_first_party`]. Spelled again in `koru-api-gateway-core`,
/// which this crate deliberately does not link;
/// `the_destructive_scope_literals_agree` pins the two copies together.
pub const WORKSPACE_DESTROY_SCOPE: &str = "workspace.destroy";

/// The scope that gates moving a workspace to another profile.
///
/// **In neither set either, and deliberately not the same scope as
/// [`WORKSPACE_DESTROY_SCOPE`].** A move destroys nothing, and a window vouched
/// for so a person can empty a workspace is not thereby a window that may hand
/// their work to a different persona. Both take the Agent's vouching; a caller
/// names whichever it needs. Spelled again in `koru-api-gateway-core`, which
/// this crate deliberately does not link;
/// `the_migration_scope_literals_agree` pins the two copies together.
pub const WORKSPACE_MIGRATE_SCOPE: &str = "workspace.migrate";

/// Explicit lifecycle authority. Ordinary API clients intentionally omit it.
/// Spelled again in `koru-api-gateway-core` — this crate deliberately does
/// not link it; `the_control_scope_literals_agree` in the gateway's tests
/// pins the two copies together.
pub const CONTROL_SCOPE: &str = "gateway.control";

/// Reading the event journal and its stream.
///
/// An ordinary scope, in the gateway's default set — but **naming any scope
/// filters rather than adds**, so a caller that asks for `settings.write` and
/// wants to hear a change fact has to name this one too. It was a bare literal
/// at every such call site until SP-6b needed a third. Spelled again in
/// `koru-api-gateway-core`, which this crate deliberately does not link;
/// `the_events_read_scope_literals_agree` pins the copies, the way
/// [`CONTROL_SCOPE`]'s are pinned.
pub const EVENTS_READ_SCOPE: &str = "events.read";

pub use koru_store_client::{
    Ack, Connection, Created, Discovery, DocumentOutline, Event, Failure, ImportQuery,
    LocalProfileRemoval, LocalProfileRestoration, ManifestRef, Navigation, NewDocument, NewProfile,
    NewWorkspace, OutlineHeading, ProfileAccountBindingOutcome, ProfileAccountClearOutcome,
    ProfileAppearance, ProfileLifecycleInventory, ProfileLifecycleInventoryItem,
    ProvisionAccountProfile, RecentQuery, RenameProfile, RenameWorkspace, SearchFacets, SearchHit,
    SearchPage, SearchQuery, SearchRequest, SearchResponse, Status, Stopping, WorkspaceAppearance,
};
pub use koru_store_client::{debug, store_session_debug, sync, sync_debug};

#[derive(Clone)]
pub struct Client {
    store: koru_store_client::Client,
    capabilities: Vec<String>,
    gateway: Option<Arc<GatewaySession>>,
    http: Option<reqwest::blocking::Client>,
    stream_http: Option<reqwest::blocking::Client>,
}

type GatewayRenewal = Arc<dyn Fn() -> Result<koru_api_discovery::GatewayHello> + Send + Sync>;

struct GatewaySession {
    current: RwLock<koru_api_discovery::GatewayHello>,
    refresh_gate: Mutex<()>,
    renewal: Option<GatewayRenewal>,
}

impl GatewaySession {
    fn current(&self) -> Result<koru_api_discovery::GatewayHello> {
        let refresh_at = {
            let current = self.current.read().expect("gateway session");
            let now = now_millis();
            let lifetime = current.session.expires_at_millis.saturating_sub(now).max(0);
            current
                .session
                .expires_at_millis
                .saturating_sub((lifetime / 10).min(5 * 60 * 1_000))
        };
        if self.renewal.is_some() && now_millis() >= refresh_at {
            self.refresh("expiring")
        } else {
            Ok(self.current.read().expect("gateway session").clone())
        }
    }

    fn refresh(&self, _reason: &'static str) -> Result<koru_api_discovery::GatewayHello> {
        let _gate = self
            .refresh_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.refresh_locked()
    }

    fn refresh_after_rejection(&self, rejected: &str) -> Result<koru_api_discovery::GatewayHello> {
        let _gate = self
            .refresh_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self
            .current
            .read()
            .expect("gateway session")
            .session
            .expose()
            != rejected
        {
            return Ok(self.current.read().expect("gateway session").clone());
        }
        self.refresh_locked()
    }

    fn refresh_locked(&self) -> Result<koru_api_discovery::GatewayHello> {
        let renewal = self.renewal.as_ref().ok_or_else(|| {
            Error::new(Code::StorageFailed, "the gateway session cannot be renewed")
        })?;
        let renewed = renewal()?;
        *self.current.write().expect("gateway session") = renewed.clone();
        Ok(renewed)
    }
}

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("build the bounded gateway HTTP client")
}

fn stream_http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("build the bounded gateway stream client")
}

impl Client {
    /// Explicit endpoint construction is retained for hermetic Store adapter
    /// tests. Shipping callers use gateway discovery through `open`/`open_at`.
    pub fn new(discovery: Discovery) -> Self {
        Self {
            store: koru_store_client::Client::new(discovery),
            capabilities: vec!["store".into(), "ai-disabled".into()],
            gateway: None,
            http: None,
            stream_http: None,
        }
    }

    /// Explicit endpoint plus boundary-owned renewal for embedded/test hosts.
    pub fn new_with_renewal(discovery: Discovery, renewal: koru_store_client::Renewal) -> Self {
        Self {
            store: koru_store_client::Client::with_renewal(discovery, renewal),
            capabilities: vec!["store".into(), "ai-disabled".into()],
            gateway: None,
            http: None,
            stream_http: None,
        }
    }

    pub fn open(segment: &str) -> Result<Self> {
        Self::connect(segment)
    }

    pub fn open_with_scopes(segment: &str, scopes: &[&str]) -> Result<Self> {
        Self::connect_with_scopes(segment, scopes)
    }

    pub fn open_at(segment: &str, root: impl AsRef<Path>) -> Result<Self> {
        Self::connect_at(segment, root)
    }

    pub fn open_at_with_scopes(
        segment: &str,
        root: impl AsRef<Path>,
        scopes: &[&str],
    ) -> Result<Self> {
        Self::connect_at_with_scopes(segment, root, scopes)
    }

    pub fn ensure(segment: &str) -> Result<Self> {
        Self::ensure_with_scopes(segment, &[])
    }

    pub fn ensure_with_scopes(segment: &str, scopes: &[&str]) -> Result<Self> {
        if let Ok(client) = Self::connect_with_scopes(segment, scopes) {
            return Ok(client);
        }
        ensure_gateway_with_agent(segment)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match Self::connect_with_scopes(segment, scopes) {
                Ok(client) => return Ok(client),
                Err(error) if std::time::Instant::now() >= deadline => return Err(error),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(25)),
            }
        }
    }

    pub fn connect(segment: &str) -> Result<Self> {
        Self::connect_with_scopes(segment, &[])
    }

    pub fn connect_with_scopes(segment: &str, scopes: &[&str]) -> Result<Self> {
        let scopes = scopes
            .iter()
            .map(|scope| (*scope).to_owned())
            .collect::<Vec<_>>();
        let hello = identity(segment, scopes.clone());
        let found = koru_api_discovery::connect(segment, &hello).map_err(discovery_error)?;
        let segment = segment.to_owned();
        Self::from_gateway_with_renewal(
            found,
            Arc::new(move || discover_gateway(&segment, None, &scopes)),
        )
    }

    pub fn connect_at(segment: &str, root: impl AsRef<Path>) -> Result<Self> {
        Self::connect_at_with_scopes(segment, root, &[])
    }

    pub fn connect_at_with_scopes(
        segment: &str,
        root: impl AsRef<Path>,
        scopes: &[&str],
    ) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let scopes = scopes
            .iter()
            .map(|scope| (*scope).to_owned())
            .collect::<Vec<_>>();
        let hello = identity(segment, scopes.clone());
        let found =
            koru_api_discovery::connect_at(segment, &root, &hello).map_err(discovery_error)?;
        let segment = segment.to_owned();
        Self::from_gateway_with_renewal(
            found,
            Arc::new(move || discover_gateway(&segment, Some(&root), &scopes)),
        )
    }

    /// **Connect as a window the Agent launched, carrying a grant.**
    ///
    /// The one constructor that can come back holding
    /// [`WORKSPACE_DESTROY_SCOPE`]. Three things happen in order, and the order
    /// is the whole of it:
    ///
    /// 1. this process asks the Agent to vouch for it ([`grants::request`]).
    ///    The Agent answers only for a connection whose **transport-read** pid
    ///    is one it launched, so a caller cannot ask on somebody else's behalf
    ///    and this call has to be made from the process that wants the scope;
    /// 2. the grant travels in this session's hello and is spent by being
    ///    validated, so a token that leaks afterwards is worthless;
    /// 3. the gateway asks the Agent about it only because the hello also names
    ///    a scope that needs vouching — every other hello is admitted exactly
    ///    as it was.
    ///
    /// **No Agent is an answer, not a failure.** A grant is meaningful only
    /// from a running supervisor that launched this process, so when there is
    /// none — a standalone run, a rooted rehearsal, a developer's `cargo run` —
    /// this connects without one and comes back holding the ordinary scopes.
    /// The caller finds out by asking what it got: the surface renders its
    /// destructive rows disabled with a reason rather than pretending they will
    /// work. Failing the whole connection would mean a window that cannot be
    /// vouched for cannot open at all, which is a much worse answer than a
    /// window that cannot archive.
    ///
    /// **Renewal asks again.** A grant is single-use, so the refresh closure
    /// requests a fresh one; a session that renewed with the spent token would
    /// silently lose the scope on the first reconnection.
    pub fn connect_first_party(segment: &str, scopes: &[&str]) -> Result<Self> {
        Self::connect_first_party_inner(segment, None, scopes)
    }

    /// [`Self::connect_first_party`] against a rooted installation.
    ///
    /// A rooted product must reach the rooted Agent that launched it, or a
    /// rehearsal would be asking somebody's real installation to vouch for it.
    pub fn connect_at_first_party(
        segment: &str,
        root: impl AsRef<Path>,
        scopes: &[&str],
    ) -> Result<Self> {
        Self::connect_first_party_inner(segment, Some(root.as_ref().to_path_buf()), scopes)
    }

    fn connect_first_party_inner(
        segment: &str,
        root: Option<std::path::PathBuf>,
        scopes: &[&str],
    ) -> Result<Self> {
        let scopes = scopes
            .iter()
            .map(|scope| (*scope).to_owned())
            .collect::<Vec<_>>();
        let hello = first_party_identity(segment, root.as_deref(), scopes.clone());
        let found = match &root {
            Some(root) => koru_api_discovery::connect_at(segment, root, &hello),
            None => koru_api_discovery::connect(segment, &hello),
        }
        .map_err(discovery_error)?;
        let segment = segment.to_owned();
        Self::from_gateway_with_renewal(
            found,
            Arc::new(move || {
                // A fresh grant per renewal: the last one was spent at
                // admission, and presenting it again would be presenting
                // nothing.
                let hello = first_party_identity(&segment, root.as_deref(), scopes.clone());
                let found = match &root {
                    Some(root) => koru_api_discovery::connect_at(&segment, root, &hello),
                    None => koru_api_discovery::connect(&segment, &hello),
                }
                .map_err(discovery_error)?;
                Ok(found)
            }),
        )
    }

    pub fn from_gateway(gateway: koru_api_discovery::GatewayHello) -> Result<Self> {
        Self::build(gateway, None)
    }

    fn from_gateway_with_renewal(
        gateway: koru_api_discovery::GatewayHello,
        renewal: GatewayRenewal,
    ) -> Result<Self> {
        Self::build(gateway, Some(renewal))
    }

    fn build(
        gateway: koru_api_discovery::GatewayHello,
        renewal: Option<GatewayRenewal>,
    ) -> Result<Self> {
        let discovery = store_discovery(&gateway)?;
        let capabilities = gateway.capabilities.clone();
        let gateway = Arc::new(GatewaySession {
            current: RwLock::new(gateway),
            refresh_gate: Mutex::new(()),
            renewal,
        });
        let store = match gateway.renewal.is_some() {
            true => {
                let session = Arc::clone(&gateway);
                koru_store_client::Client::with_renewal(
                    discovery,
                    Arc::new(move || store_discovery(&session.refresh("store")?)),
                )
            }
            false => koru_store_client::Client::new(discovery),
        };
        Ok(Self {
            store,
            capabilities,
            gateway: Some(gateway),
            http: Some(http_client()),
            stream_http: Some(stream_http_client()),
        })
    }

    pub fn store(&self) -> StoreClient<'_> {
        StoreClient { client: self }
    }

    pub fn ai(&self) -> AiClient<'_> {
        AiClient { client: self }
    }

    /// The delegated-runs namespace: `client.runs().create(..)`, `.run(..)`,
    /// `.stream(..)`. Capability-gated like [`Client::artifacts`].
    #[cfg(feature = "delegate")]
    pub fn runs(&self) -> RunsClient<'_> {
        RunsClient { client: self }
    }

    /// Scheduled delegated work: create/list plus content-free invalidation.
    #[cfg(feature = "delegate")]
    pub fn schedules(&self) -> SchedulesClient<'_> {
        SchedulesClient { client: self }
    }

    /// The events namespace: `client.events().send(..)`, `.read(..)`,
    /// `.on(..)`. Capability-gated like [`Client::ai`].
    pub fn events(&self) -> EventsClient<'_> {
        EventsClient { client: self }
    }

    /// Public rooted process sessions. This namespace shares Gateway
    /// discovery, credentials, renewal, and the one-safe-retry engine with
    /// Store and Events while keeping session failures typed.
    pub fn sessions(&self) -> SessionsClient<'_> {
        SessionsClient { client: self }
    }

    /// The typed settings namespace: `client.settings().global()`,
    /// `.patch_terminal(..)`, `.global_when_ready()`.
    ///
    /// **Not capability-gated.** The settings authority is not a worker that
    /// may fail to spawn — it is a file reader that the Gateway either has
    /// paths for or does not, and its routes say `capability_unavailable`
    /// themselves when it does not. A capability string here would be a second
    /// place for the same answer to be wrong.
    pub fn settings(&self) -> SettingsClient<'_> {
        SettingsClient { client: self }
    }

    /// The typed catalog namespace: `client.catalog().workspaces()`,
    /// `.confirmation(..)`, `.lifecycle(..)`.
    ///
    /// **Named for the catalog rather than for workspaces**, because
    /// `Client` dereferences to the Store client and that one already answers
    /// `workspaces()` — a second inherent method of the same name would
    /// silently shadow it at every call site in the workspace. `catalog` is
    /// also the word the event vocabulary already uses for exactly this pair of
    /// lists ([`CatalogChange`]).
    ///
    /// **Not capability-gated.** The catalog is the Store's and the operation
    /// is the gateway's own; both answer for themselves when they cannot —
    /// `capability_unavailable` from a gateway nobody supervises, which is a
    /// truer answer than a missing capability string, because the routes exist
    /// and it is the bridge behind them that does not.
    /// The Artifact Store: families, immutable versions, provenance. Bytes
    /// go in through `store().put_blob` and come back through
    /// `artifacts().content`.
    pub fn artifacts(&self) -> ArtifactsClient<'_> {
        ArtifactsClient { client: self }
    }

    pub fn catalog(&self) -> CatalogClient<'_> {
        CatalogClient { client: self }
    }

    pub fn capabilities(&self) -> &[String] {
        &self.capabilities
    }

    pub fn search(&self, request: &FederatedSearchRequest) -> Result<FederatedSearchResponse> {
        if self.gateway.is_none() {
            let response = self.store.search(&SearchRequest {
                query: request.query.clone(),
                include_facets: request.include_facets,
                recent_queries: request.recent_queries,
            })?;
            return Ok(FederatedSearchResponse {
                page: FederatedSearchPage {
                    hits: response
                        .page
                        .hits
                        .into_iter()
                        .map(|hit| FederatedSearchHit::Store { hit })
                        .collect(),
                    next_cursor: response.page.next_offset.map(|offset| offset.to_string()),
                    updating_workspaces: response.page.updating_workspaces,
                    request_seq: response.page.request_seq,
                    corrected_query: response.page.corrected_query,
                },
                facets: response.facets,
                recent_queries: response.recent_queries,
                sources: vec![SearchSourceStatus {
                    source: SearchSource::Store,
                    state: SearchSourceState::Complete,
                    error_code: None,
                }],
            });
        }
        self.gateway_json("/v1/search", request)
    }

    fn gateway_json<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        request: &impl serde::Serialize,
    ) -> Result<T> {
        self.ai()
            .json(
                reqwest::Method::POST,
                path.to_owned(),
                Some(json_body(request).map_err(ai_to_store_error)?),
            )
            .map_err(ai_to_store_error)
    }

    fn gateway_get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.ai()
            .json(reqwest::Method::GET, path.to_owned(), None)
            .map_err(ai_to_store_error)
    }
}

pub struct StoreClient<'a> {
    client: &'a Client,
}

impl StoreClient<'_> {
    /// The embedded Store's own status and fine-grained capabilities. The
    /// Gateway's `/v1/status` intentionally reports domain availability, so a
    /// namespaced route preserves Store feature negotiation through it.
    pub fn status(&self) -> Result<koru_store_client::Status> {
        if self.client.gateway.is_none() {
            self.client.store.status()
        } else {
            self.client.gateway_get("/v1/store/status")
        }
    }

    pub fn search(&self, request: &SearchRequest) -> Result<SearchResponse> {
        if self.client.gateway.is_none() {
            self.client.store.search(request)
        } else {
            self.client.gateway_json("/v1/store/search", request)
        }
    }
}

impl std::ops::Deref for StoreClient<'_> {
    type Target = koru_store_client::Client;

    fn deref(&self) -> &Self::Target {
        &self.client.store
    }
}

fn store_discovery(
    gateway: &koru_api_discovery::GatewayHello,
) -> Result<koru_store_client::Discovery> {
    koru_store_client::Discovery::from_loopback_endpoint(
        &gateway.api_endpoint,
        gateway.session.expose(),
        gateway.session.expires_at_millis,
    )
}

fn discover_gateway(
    segment: &str,
    root: Option<&PathBuf>,
    scopes: &[String],
) -> Result<koru_api_discovery::GatewayHello> {
    let hello = identity(segment, scopes.to_vec());
    match root {
        Some(root) => koru_api_discovery::connect_at(segment, root, &hello),
        None => koru_api_discovery::connect(segment, &hello),
    }
    .map_err(discovery_error)
}

impl std::ops::Deref for Client {
    type Target = koru_store_client::Client;

    fn deref(&self) -> &Self::Target {
        &self.store
    }
}

pub struct AiClient<'a> {
    client: &'a Client,
}

pub type AiResult<T> = std::result::Result<T, AiFailure>;

pub struct AiResponseStream {
    pub accepted: SendTurnAccepted,
    pub events: AiEventStream,
}

pub struct AiEventStream {
    reader: BufReader<reqwest::blocking::Response>,
    finished: bool,
}

pub struct ModelPullResponseStream {
    pub accepted: PullOllamaModelAccepted,
    pub events: ModelPullEventStream,
}

pub struct ModelPullEventStream {
    reader: BufReader<reqwest::blocking::Response>,
    finished: bool,
}

impl AiClient<'_> {
    pub fn available(&self) -> bool {
        self.client.capabilities.iter().any(|value| value == "ai")
    }

    pub fn provider_settings(&self) -> AiResult<ProviderSettingsResponse> {
        self.json(
            reqwest::Method::GET,
            "/v1/ai/providers/settings".into(),
            None,
        )
    }

    pub fn select_provider(
        &self,
        request: &SelectProviderRequest,
    ) -> AiResult<ProviderSettingsResponse> {
        self.json(
            reqwest::Method::PUT,
            "/v1/ai/providers/selection".into(),
            Some(json_body(request)?),
        )
    }

    pub fn update_openai_settings(
        &self,
        request: &UpdateOpenAiSettingsRequest,
    ) -> AiResult<OpenAiSettingsView> {
        self.json(
            reqwest::Method::PUT,
            "/v1/ai/providers/openai".into(),
            Some(json_body(request)?),
        )
    }

    pub fn update_ollama_settings(
        &self,
        request: &UpdateOllamaSettingsRequest,
    ) -> AiResult<OllamaSettingsView> {
        self.json(
            reqwest::Method::PUT,
            "/v1/ai/providers/ollama".into(),
            Some(json_body(request)?),
        )
    }

    pub fn validate_provider(
        &self,
        provider: &AiProviderId,
    ) -> AiResult<ProviderValidationResponse> {
        self.json(
            reqwest::Method::POST,
            format!("/v1/ai/providers/{provider}/validate"),
            None,
        )
    }

    pub fn discover_agent_models(
        &self,
        provider: &AiProviderId,
    ) -> AiResult<koru_ai_wire::AgentModelsResponse> {
        self.json(
            reqwest::Method::POST,
            format!("/v1/ai/providers/{provider}/models"),
            None,
        )
    }

    pub fn pull_ollama_model(
        &self,
        request: &PullOllamaModelRequest,
    ) -> AiResult<ModelPullResponseStream> {
        let response = checked(self.execute(
            reqwest::Method::POST,
            "/v1/ai/providers/ollama/pulls".into(),
            Some(json_body(request)?),
            true,
        )?)?;
        let mut events = ModelPullEventStream {
            reader: BufReader::new(response),
            finished: false,
        };
        let Some((name, data)) = events.next_frame()? else {
            return Err(protocol_failure(
                "The model-pull response ended before acceptance.",
            ));
        };
        if name != "accepted" {
            return Err(protocol_failure(
                "The model-pull response did not begin with acceptance.",
            ));
        }
        let accepted = serde_json::from_str(&data)
            .map_err(|_| protocol_failure("The model-pull acceptance was invalid."))?;
        Ok(ModelPullResponseStream { accepted, events })
    }

    pub fn cancel_ollama_pull(&self) -> AiResult<CancelModelPullResponse> {
        self.json(
            reqwest::Method::POST,
            "/v1/ai/providers/ollama/pulls/cancel".into(),
            None,
        )
    }

    pub fn create_chat(&self, request: &CreateChatRequest) -> AiResult<CreateChatResponse> {
        self.json(
            reqwest::Method::POST,
            "/v1/ai/chats".into(),
            Some(json_body(request)?),
        )
    }

    pub fn list_chats(&self) -> AiResult<koru_ai_wire::ListChatsResponse> {
        self.json(reqwest::Method::GET, "/v1/ai/chats".into(), None)
    }

    pub fn read_chat(&self, chat: ChatId) -> AiResult<ChatTranscript> {
        self.json(reqwest::Method::GET, format!("/v1/ai/chats/{chat}"), None)
    }

    pub fn rename_chat(
        &self,
        chat: ChatId,
        request: &koru_ai_wire::RenameChatRequest,
    ) -> AiResult<koru_ai_wire::RenameChatResponse> {
        self.json(
            reqwest::Method::PATCH,
            format!("/v1/ai/chats/{chat}"),
            Some(json_body(request)?),
        )
    }

    pub fn update_chat_model(
        &self,
        chat: ChatId,
        request: &UpdateChatModelRequest,
    ) -> AiResult<UpdateChatModelResponse> {
        self.json(
            reqwest::Method::PATCH,
            format!("/v1/ai/chats/{chat}/model"),
            Some(json_body(request)?),
        )
    }

    pub fn update_chat_privacy(
        &self,
        chat: ChatId,
        request: &UpdateChatPrivacyRequest,
    ) -> AiResult<UpdateChatPrivacyResponse> {
        self.json(
            reqwest::Method::PATCH,
            format!("/v1/ai/chats/{chat}/privacy"),
            Some(json_body(request)?),
        )
    }

    pub fn update_chat_folder(
        &self,
        chat: ChatId,
        request: &koru_ai_wire::UpdateChatFolderRequest,
    ) -> AiResult<koru_ai_wire::UpdateChatFolderResponse> {
        self.json(
            reqwest::Method::PATCH,
            format!("/v1/ai/chats/{chat}/folder"),
            Some(json_body(request)?),
        )
    }

    pub fn tool_list_chats(
        &self,
        request: &ListChatsToolRequest,
    ) -> AiResult<ListChatsToolResponse> {
        self.json(
            reqwest::Method::POST,
            "/v1/ai/tools/chats/list".into(),
            Some(json_body(request)?),
        )
    }

    pub fn tool_search_chats(
        &self,
        request: &SearchChatsToolRequest,
    ) -> AiResult<SearchChatsToolResponse> {
        self.json(
            reqwest::Method::POST,
            "/v1/ai/tools/chats/search".into(),
            Some(json_body(request)?),
        )
    }

    pub fn tool_read_chat(&self, request: &ReadChatToolRequest) -> AiResult<ReadChatToolResponse> {
        self.json(
            reqwest::Method::POST,
            "/v1/ai/tools/chats/read".into(),
            Some(json_body(request)?),
        )
    }

    pub fn update_chat_tool_policy(
        &self,
        chat: ChatId,
        request: &UpdateChatToolPolicyRequest,
    ) -> AiResult<ChatToolPolicyResponse> {
        self.json(
            reqwest::Method::PUT,
            format!("/v1/ai/chats/{chat}/tool-policy"),
            Some(json_body(request)?),
        )
    }

    pub fn read_chat_tool_policy(&self, chat: ChatId) -> AiResult<ChatToolPolicyResponse> {
        self.json(
            reqwest::Method::GET,
            format!("/v1/ai/chats/{chat}/tool-policy"),
            None,
        )
    }

    pub fn usage(&self, chat: Option<ChatId>) -> AiResult<UsageResponse> {
        let path = chat.map_or_else(
            || "/v1/ai/usage".to_owned(),
            |chat| format!("/v1/ai/chats/{chat}/usage"),
        );
        self.json(reqwest::Method::GET, path, None)
    }

    pub fn delete_chat(&self, chat: ChatId) -> AiResult<DeleteChatResponse> {
        self.json(
            reqwest::Method::DELETE,
            format!("/v1/ai/chats/{chat}"),
            None,
        )
    }

    pub fn clear_history(&self) -> AiResult<ClearHistoryResponse> {
        self.json(reqwest::Method::DELETE, "/v1/ai/chats".into(), None)
    }

    pub fn clear_usage(&self) -> AiResult<ClearUsageResponse> {
        self.json(reqwest::Method::DELETE, "/v1/ai/usage".into(), None)
    }

    pub fn search(&self, request: &AiSearchRequest) -> AiResult<AiSearchResponse> {
        self.json(
            reqwest::Method::POST,
            "/v1/ai/search".into(),
            Some(json_body(request)?),
        )
    }

    pub fn feedback(
        &self,
        message: MessageId,
        request: &FeedbackRequest,
    ) -> AiResult<FeedbackResponse> {
        self.json(
            reqwest::Method::PUT,
            format!("/v1/ai/messages/{message}/feedback"),
            Some(json_body(request)?),
        )
    }

    pub fn branch_chat(
        &self,
        chat: ChatId,
        request: &BranchChatRequest,
    ) -> AiResult<BranchChatResponse> {
        self.json(
            reqwest::Method::POST,
            format!("/v1/ai/chats/{chat}/branches"),
            Some(json_body(request)?),
        )
    }

    pub fn send_turn(&self, request: &SendTurnRequest) -> AiResult<AiResponseStream> {
        let response = self.execute(
            reqwest::Method::POST,
            format!("/v1/ai/chats/{}/turns", request.chat_id),
            Some(json_body(request)?),
            true,
        )?;
        self.ai_stream(checked(response)?)
    }

    pub fn retry_turn(
        &self,
        chat: ChatId,
        request: &RetryTurnRequest,
    ) -> AiResult<AiResponseStream> {
        let response = checked(self.execute(
            reqwest::Method::POST,
            format!("/v1/ai/chats/{chat}/retries"),
            Some(json_body(request)?),
            true,
        )?)?;
        self.ai_stream(response)
    }

    pub fn export_chat(
        &self,
        chat: ChatId,
        request: &ExportChatRequest,
    ) -> AiResult<ExportChatResponse> {
        self.json(
            reqwest::Method::POST,
            format!("/v1/ai/chats/{chat}/exports"),
            Some(json_body(request)?),
        )
    }

    pub fn read_export(&self, export: DocumentExportId) -> AiResult<ExportChatResponse> {
        self.json(
            reqwest::Method::GET,
            format!("/v1/ai/exports/{export}"),
            None,
        )
    }

    fn ai_stream(&self, response: reqwest::blocking::Response) -> AiResult<AiResponseStream> {
        let mut events = AiEventStream {
            reader: BufReader::new(response),
            finished: false,
        };
        let Some((name, data)) = events.next_frame()? else {
            return Err(protocol_failure("The AI response ended before acceptance."));
        };
        if name != "accepted" {
            return Err(protocol_failure(
                "The AI response did not begin with durable acceptance.",
            ));
        }
        let accepted = serde_json::from_str(&data)
            .map_err(|_| protocol_failure("The AI acceptance event was invalid."))?;
        Ok(AiResponseStream { accepted, events })
    }

    pub fn read_attempt(&self, attempt: InferenceAttemptId) -> AiResult<AttemptResponse> {
        self.json(
            reqwest::Method::GET,
            format!("/v1/ai/attempts/{attempt}"),
            None,
        )
    }

    pub fn cancel_attempt(&self, attempt: InferenceAttemptId) -> AiResult<CancelAttemptResponse> {
        self.json(
            reqwest::Method::POST,
            format!("/v1/ai/attempts/{attempt}/cancel"),
            None,
        )
    }

    /// Approve a pending tool call. `choice` is the id of the option a local
    /// agent offered for its permission request; a workspace tool takes none.
    pub fn approve_tool_call(
        &self,
        tool: ToolInvocationId,
        choice: Option<String>,
    ) -> AiResult<ToolDecisionResponse> {
        self.json(
            reqwest::Method::POST,
            format!("/v1/ai/tool-calls/{tool}/approve"),
            tool_decision_body(choice)?,
        )
    }

    pub fn deny_tool_call(
        &self,
        tool: ToolInvocationId,
        choice: Option<String>,
    ) -> AiResult<ToolDecisionResponse> {
        self.json(
            reqwest::Method::POST,
            format!("/v1/ai/tool-calls/{tool}/deny"),
            tool_decision_body(choice)?,
        )
    }

    /// Store one picture or file for a local agent chat's next message
    /// (chat-attachments). The name and size are checked here first, as the
    /// service will check them again with the content; the bytes are the
    /// body, so no path crosses. Blocking: call it off the UI thread.
    pub fn put_attachment(
        &self,
        chat: ChatId,
        attachment: koru_ai_core::ChatAttachmentId,
        file_name: &str,
        source: koru_ai_core::AttachmentSource,
        bytes: Bytes,
    ) -> AiResult<koru_ai_wire::ChatAttachmentResponse> {
        if bytes.len() as u64 > koru_ai_core::attachments::MAX_ATTACHMENT_BYTES {
            return Err(local_attachment_failure(
                koru_ai_wire::attachment_codes::TOO_LARGE,
                "The attachment is larger than the limit.",
            ));
        }
        if koru_ai_core::attachments::allowed_type(file_name).is_none() {
            return Err(local_attachment_failure(
                koru_ai_wire::attachment_codes::TYPE_NOT_ALLOWED,
                "This type of file can't be attached.",
            ));
        }
        let path = format!(
            "/v1/ai/chats/{chat}/attachments/{attachment}?name={}&source={}",
            percent_encode_query(file_name),
            source.as_token()
        );
        checked(self.execute_with(
            reqwest::Method::PUT,
            path,
            Some((bytes, "application/octet-stream")),
            false,
        )?)?
        .json()
        .map_err(|_| protocol_failure("The AI response body was invalid."))
    }

    /// Remove a staged attachment. A sent one answers `attachment_sent`.
    pub fn remove_attachment(
        &self,
        chat: ChatId,
        attachment: koru_ai_core::ChatAttachmentId,
    ) -> AiResult<koru_ai_wire::RemoveAttachmentResponse> {
        self.json(
            reqwest::Method::DELETE,
            format!("/v1/ai/chats/{chat}/attachments/{attachment}"),
            None,
        )
    }

    /// An attachment's stored original.
    pub fn attachment_bytes(
        &self,
        chat: ChatId,
        attachment: koru_ai_core::ChatAttachmentId,
    ) -> AiResult<Vec<u8>> {
        self.bytes(format!("/v1/ai/chats/{chat}/attachments/{attachment}"))
    }

    /// A picture attachment's PNG thumbnail, at most 512 px on its long edge.
    pub fn attachment_thumbnail(
        &self,
        chat: ChatId,
        attachment: koru_ai_core::ChatAttachmentId,
    ) -> AiResult<Vec<u8>> {
        self.bytes(format!(
            "/v1/ai/chats/{chat}/attachments/{attachment}/thumbnail"
        ))
    }

    fn bytes(&self, path: String) -> AiResult<Vec<u8>> {
        checked(self.execute(reqwest::Method::GET, path, None, false)?)?
            .bytes()
            .map(|bytes| bytes.to_vec())
            .map_err(|_| transport_failure_message())
    }

    fn json<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: String,
        body: Option<Bytes>,
    ) -> AiResult<T> {
        checked(self.execute(method, path, body, false)?)?
            .json()
            .map_err(|_| protocol_failure("The AI response body was invalid."))
    }

    fn execute(
        &self,
        method: reqwest::Method,
        path: String,
        body: Option<Bytes>,
        streaming: bool,
    ) -> AiResult<reqwest::blocking::Response> {
        self.execute_with(
            method,
            path,
            body.map(|body| (body, "application/json")),
            streaming,
        )
    }

    fn execute_with(
        &self,
        method: reqwest::Method,
        path: String,
        body: Option<(Bytes, &'static str)>,
        streaming: bool,
    ) -> AiResult<reqwest::blocking::Response> {
        let session = self.client.gateway.as_ref().ok_or_else(|| AiFailure {
            code: "ai_unavailable".into(),
            message: "This client was not created from a Koru API gateway session.".into(),
            retryable: true,
            details: Default::default(),
        })?;
        let current = session.current().map_err(gateway_failure)?;
        let http = if streaming {
            self.client.stream_http.as_ref()
        } else {
            self.client.http.as_ref()
        }
        .expect("a gateway session owns its HTTP adapters");
        let send = |current: &koru_api_discovery::GatewayHello| {
            let mut request = http
                .request(method.clone(), format!("{}{}", current.api_endpoint, path))
                .bearer_auth(current.session.expose());
            if let Some((body, content_type)) = &body {
                request = request
                    .header(reqwest::header::CONTENT_TYPE, *content_type)
                    .body(body.clone());
            }
            request.send()
        };
        match send(&current) {
            Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
                let renewed = session
                    .refresh_after_rejection(current.session.expose())
                    .map_err(gateway_failure)?;
                if renewed.session.expose() == current.session.expose() {
                    Ok(response)
                } else {
                    send(&renewed).map_err(transport_failure)
                }
            }
            Ok(response) => Ok(response),
            Err(_) => {
                let renewed = session.refresh("unreachable").map_err(gateway_failure)?;
                send(&renewed).map_err(transport_failure)
            }
        }
    }
}

impl Iterator for AiEventStream {
    type Item = AiResult<StreamEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.finished {
                return None;
            }
            match self.next_frame() {
                Ok(Some((name, data))) if name == "event" => match decode_ai_event(&data) {
                    Ok(Some(event)) => return Some(Ok(event)),
                    Ok(None) => continue,
                    Err(error) => return Some(Err(error)),
                },
                Ok(Some(_)) => {
                    return Some(Err(protocol_failure(
                        "The AI stream contained an unknown event kind.",
                    )));
                }
                Ok(None) => {
                    self.finished = true;
                    return None;
                }
                Err(error) => {
                    self.finished = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

fn decode_ai_event(data: &str) -> AiResult<Option<StreamEvent>> {
    let value: serde_json::Value = serde_json::from_str(data)
        .map_err(|_| protocol_failure("An AI stream event was invalid."))?;
    let event_type = value
        .get("event")
        .and_then(|event| event.get("type"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| protocol_failure("An AI stream event was invalid."))?;
    if !matches!(
        event_type,
        "started"
            | "text_delta"
            | "usage"
            | "tool_activity"
            | "web_search"
            | "citation"
            | "completed"
            | "failed"
            | "cancelled"
    ) {
        return Ok(None);
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|_| protocol_failure("An AI stream event was invalid."))
}

impl AiEventStream {
    fn next_frame(&mut self) -> AiResult<Option<(String, String)>> {
        read_sse_frame(&mut self.reader)
    }
}

impl Iterator for ModelPullEventStream {
    type Item = AiResult<ModelPullEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        match self.next_frame() {
            Ok(Some((name, data))) if name == "event" => Some(
                serde_json::from_str(&data)
                    .map_err(|_| protocol_failure("A model-pull event was invalid.")),
            ),
            Ok(Some(_)) => Some(Err(protocol_failure(
                "The model-pull stream contained an unknown event kind.",
            ))),
            Ok(None) => {
                self.finished = true;
                None
            }
            Err(error) => {
                self.finished = true;
                Some(Err(error))
            }
        }
    }
}

impl ModelPullEventStream {
    fn next_frame(&mut self) -> AiResult<Option<(String, String)>> {
        read_sse_frame(&mut self.reader)
    }
}

fn read_sse_frame(
    reader: &mut BufReader<reqwest::blocking::Response>,
) -> AiResult<Option<(String, String)>> {
    let mut name = String::new();
    let mut data = Vec::new();
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|_| transport_failure_message())?;
        if read == 0 {
            return if name.is_empty() && data.is_empty() {
                Ok(None)
            } else {
                Err(protocol_failure("The AI stream ended mid-event."))
            };
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if name.is_empty() && data.is_empty() {
                continue;
            }
            return Ok(Some((name, data.join("\n"))));
        }
        if let Some(value) = line.strip_prefix("event:") {
            name = value.trim_start().to_owned();
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start().to_owned());
        }
    }
}

/// A tool decision travels with a body only when it carries a choice, so a
/// gateway that predates the body sees the same empty POST it always did.
fn tool_decision_body(choice: Option<String>) -> AiResult<Option<Bytes>> {
    choice
        .map(|choice| {
            json_body(&koru_ai_wire::ToolDecisionRequest {
                choice: Some(choice),
            })
        })
        .transpose()
}

/// A refusal the client makes before anything is sent, with the code the
/// service would answer.
fn local_attachment_failure(code: &str, message: &str) -> AiFailure {
    AiFailure {
        code: code.into(),
        message: message.into(),
        retryable: false,
        details: Default::default(),
    }
}

/// Percent-encode a query value: unreserved characters stay, every other
/// byte of its UTF-8 is `%XX`.
fn percent_encode_query(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn json_body<T: serde::Serialize>(value: &T) -> AiResult<Bytes> {
    serde_json::to_vec(value)
        .map(Bytes::from)
        .map_err(|_| protocol_failure("The AI request could not be encoded."))
}

fn checked(response: reqwest::blocking::Response) -> AiResult<reqwest::blocking::Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    match response.json::<AiFailure>() {
        Ok(failure) => Err(failure),
        Err(_) => Err(AiFailure {
            code: if status == reqwest::StatusCode::UNAUTHORIZED {
                "unauthenticated"
            } else {
                "gateway_failure"
            }
            .into(),
            message: "The Koru API gateway returned an invalid error response.".into(),
            retryable: status.is_server_error(),
            details: Default::default(),
        }),
    }
}

fn protocol_failure(message: &str) -> AiFailure {
    AiFailure {
        code: "invalid_response".into(),
        message: message.into(),
        retryable: true,
        details: Default::default(),
    }
}

fn gateway_failure(error: Error) -> AiFailure {
    AiFailure {
        code: "gateway_unavailable".into(),
        message: error.to_string(),
        retryable: true,
        details: Default::default(),
    }
}

fn ai_to_store_error(error: AiFailure) -> Error {
    let code = match error.code.as_str() {
        "invalid_request" | "invalid_cursor" => Code::InvalidRequest,
        "unauthenticated" => Code::Unauthenticated,
        "forbidden" => Code::Forbidden,
        "ai_unavailable" | "gateway_unavailable" => Code::StoreUnreachable,
        _ => Code::SearchUnavailable,
    };
    Error::new(code, error.message)
}

fn transport_failure(_: reqwest::Error) -> AiFailure {
    transport_failure_message()
}

fn transport_failure_message() -> AiFailure {
    AiFailure {
        code: "gateway_unavailable".into(),
        message: "The Koru API gateway connection ended.".into(),
        retryable: true,
        details: Default::default(),
    }
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn identity(segment: &str, requested_scopes: Vec<String>) -> koru_api_discovery::ClientHello {
    koru_api_discovery::ClientHello {
        product: "koru-api-client".into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        segment: segment.into(),
        api_version: koru_api_wire::API_VERSION,
        requested_scopes,
        // **Never asked for on this path.** A grant is a first-party window's
        // to obtain and to present; `identity` serves every caller of this
        // crate, including MCP servers and scripts, and one that fetched a
        // grant on their behalf would be handing out the thing the grant
        // exists to withhold. A window builds its own hello — see
        // [`grants::request`] — and asks for it deliberately.
        grant: None,
    }
}

/// The same hello, from a window that has asked its supervisor to vouch for it.
///
/// **The one place a grant is fetched.** It is deliberately not reachable from
/// [`identity`]: that serves every caller of this crate, MCP servers and
/// scripts included, and one that fetched a grant on their behalf would be
/// handing out the thing the grant exists to withhold.
fn first_party_identity(
    segment: &str,
    root: Option<&Path>,
    requested_scopes: Vec<String>,
) -> koru_api_discovery::ClientHello {
    // Any failure is no grant. The Agent refusing, no Agent listening, a
    // framing mismatch — each means this process is not vouched for right now,
    // and a hello without a grant is admitted with the ordinary scopes.
    let grant = grants::agent_endpoint(segment, root)
        .and_then(|endpoint| grants::request(&endpoint))
        .ok();
    koru_api_discovery::ClientHello {
        grant,
        ..identity(segment, requested_scopes)
    }
}

fn discovery_error(error: koru_api_discovery::DiscoveryError) -> Error {
    let code = match error {
        koru_api_discovery::DiscoveryError::NotRunning(_) => Code::StoreUnreachable,
        koru_api_discovery::DiscoveryError::Incompatible { .. } => Code::ApiVersionUnsupported,
        koru_api_discovery::DiscoveryError::Ipc(_)
        | koru_api_discovery::DiscoveryError::Handshake(_) => Code::StorageFailed,
    };
    Error::new(code, error.to_string())
}

/// Ask Agent for gateway readiness without receiving any credential from it.
/// This is used by independent product hosts such as MCP; a Koru window already
/// has a long-lived Agent link and normally reaches `connect` after its own
/// readiness request.
fn ensure_gateway_with_agent(segment: &str) -> Result<()> {
    use koru_agent_wire::{
        AgentGreeting, FRAME_VERSION, FromAgent, FromShell, PROTOCOL_VERSION, Request, Response,
        Service, ShellHello,
    };

    let endpoint = koru_ipc::Endpoint::for_service(koru_ipc::AGENT, segment)
        .map_err(|error| boundary_error(error.to_string()))?;
    let mut stream = match koru_ipc::connect(&endpoint) {
        Ok(stream) => stream,
        Err(_) => {
            spawn_agent()?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                match koru_ipc::connect(&endpoint) {
                    Ok(stream) => break stream,
                    Err(error) if std::time::Instant::now() >= deadline => {
                        return Err(boundary_error(format!(
                            "the Agent did not start within three seconds: {error}"
                        )));
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(25)),
                }
            }
        }
    };
    koru_ipc::set_timeouts(&stream, Some(std::time::Duration::from_secs(12)));
    koru_ipc::frame::write_frame(
        &mut stream,
        FRAME_VERSION,
        &ShellHello {
            product: "koru-api-client".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .and_then(|()| stream.flush())
    .map_err(|error| boundary_error(format!("Agent greeting failed: {error}")))?;
    let (version, greeting): (u8, AgentGreeting) = koru_ipc::frame::read_frame(&mut stream)
        .map_err(|error| boundary_error(format!("Agent greeting failed: {error}")))?;
    if version != FRAME_VERSION {
        return Err(boundary_error("the Agent framing version is incompatible"));
    }
    if !matches!(greeting, AgentGreeting::Welcome(_)) {
        return Err(boundary_error("the Agent protocol version is incompatible"));
    }
    koru_ipc::frame::write_frame(
        &mut stream,
        FRAME_VERSION,
        &FromShell::Request {
            id: 1,
            body: Request::EnsureService {
                service: Service::ApiGateway,
            },
        },
    )
    .and_then(|()| stream.flush())
    .map_err(|error| boundary_error(format!("Agent readiness request failed: {error}")))?;
    loop {
        let (version, answer): (u8, FromAgent) = koru_ipc::frame::read_frame(&mut stream)
            .map_err(|error| boundary_error(format!("Agent readiness response failed: {error}")))?;
        if version != FRAME_VERSION {
            return Err(boundary_error("the Agent framing version changed"));
        }
        match answer {
            FromAgent::Response {
                id: 1,
                body: Response::Ok,
            } => return Ok(()),
            FromAgent::Response {
                id: 1,
                body: Response::Failed { message, .. },
            } => return Err(boundary_error(message)),
            _ => {}
        }
    }
}

fn spawn_agent() -> Result<()> {
    let executable = std::env::current_exe()
        .map_err(|error| boundary_error(format!("cannot locate this executable: {error}")))?;
    let agent = executable
        .parent()
        .ok_or_else(|| boundary_error("this executable has no parent directory"))?
        .join(if cfg!(windows) {
            "koru-agent.exe"
        } else {
            "koru-agent"
        });
    if !agent.is_file() {
        return Err(boundary_error(format!(
            "Koru Agent is not beside this build at {}",
            agent.display()
        )));
    }
    let mut command = std::process::Command::new(agent);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    detach(&mut command);
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| boundary_error(format!("Koru Agent could not start: {error}")))
}

#[cfg(windows)]
fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn detach(_command: &mut std::process::Command) {}

fn boundary_error(message: impl Into<String>) -> Error {
    Error::new(Code::StoreUnreachable, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_namespace_stays_disabled_without_the_capability() {
        let client = Client::from_gateway(koru_api_discovery::GatewayHello {
            api_version: koru_api_wire::API_VERSION,
            segment: "internal-debug".into(),
            gateway_version: env!("CARGO_PKG_VERSION").into(),
            api_endpoint: "http://127.0.0.1:9".into(),
            instance_id: "test".into(),
            supervisor_launch_id: None,
            capabilities: vec!["store".into(), "ai-disabled".into()],
            session: koru_api_discovery::SessionCredential::new("secret".into(), i64::MAX),
        })
        .unwrap();
        assert!(!client.ai().available());
        assert!(
            !client.events().available(),
            "events stays unavailable until the gateway hello says otherwise"
        );
    }

    #[test]
    fn the_events_namespace_follows_its_capability() {
        let client = Client::from_gateway(koru_api_discovery::GatewayHello {
            api_version: koru_api_wire::API_VERSION,
            segment: "internal-debug".into(),
            gateway_version: env!("CARGO_PKG_VERSION").into(),
            api_endpoint: "http://127.0.0.1:9".into(),
            instance_id: "test".into(),
            supervisor_launch_id: None,
            capabilities: vec!["store".into(), "events".into()],
            session: koru_api_discovery::SessionCredential::new("secret".into(), i64::MAX),
        })
        .unwrap();
        assert!(client.events().available());
    }

    #[test]
    fn unknown_ai_event_variants_are_skipped_but_malformed_known_events_fail() {
        let unknown = r#"{"version":1,"sequence":2,"event":{"type":"future_signal"}}"#;
        assert!(decode_ai_event(unknown).unwrap().is_none());

        let malformed = r#"{"version":1,"sequence":2,"event":{"type":"text_delta"}}"#;
        assert_eq!(
            decode_ai_event(malformed).unwrap_err().code,
            "invalid_response"
        );
    }
}
