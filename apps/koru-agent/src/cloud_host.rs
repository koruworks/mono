// SPDX-License-Identifier: MPL-2.0

//! The desktop's platform half of the Cloud coordinator.
//!
//! The coordinator in `koru-account-runtime` decides what to sync; this
//! adapter is how the Agent does it on a machine where the Store lives in
//! the gateway process. It reaches the Store only through the authenticated
//! control channel the Agent already holds — `read_cloud_bootstrap`,
//! `replace_cloud_context`, `install_cloud_session`, `fence_cloud`,
//! `cloud_status`, and since root-catalog Plan 3.2 the Store half of the
//! account path (the binding, the root inventory, the export boundary and
//! the staged join) — and it owns the network side in this process: the dial
//! on the Cloud ALPN, the four-message handshake, and the bridge that hands
//! the authenticated stream to the gateway under its Cloud principal.
//!
//! The order in `open_dataset` is the order §4 prescribes and the reason the
//! two IPC channels need no ordering between them: the handshake completes
//! first, `install_cloud_session` is acknowledged second, and only then is
//! the stream attached. A stream that reaches the gateway before its install
//! is simply reset there, and this adapter never learns a phantom success
//! from a request it merely sent.
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use koru_account_core::workspace_authorization::Region;
use koru_account_core::{KoruIdentityId, ProfileId};
use koru_account_runtime::account::lifecycle::LocalProfileInventory;
use koru_account_runtime::cloud::{
    self, CloudAccountBindingRef, CloudPermitV2, CloudRootExport, CloudRootJoin, Failure, Host,
    LocalBootstrap, RootInventory, SessionReport, Stage,
};
use koru_account_wire::CloudCatalogBindingV2;
use koru_account_wire::JwksResponseV1;
use koru_account_wire::cloud_sync::SyncCredential;
use koru_account_wire::regional_bootstrap::CloudSyncRouteV1;
use koru_device::cloud::{CloudId, CloudReplicaPrincipalV1};
use koru_device::{DeviceSigningKey, ReplicationEndpointId};
use koru_network_iroh::{CloudRoute, RuntimeHandle, cloud::dial};
use koru_store_core::WorkspaceId;
use koru_store_core::service::JoinUnit;
use koru_sync_control::cloud::CloudStage;
use koru_sync_control::{ControlRequest, ControlResponse, ControlSessionId};
use uuid::Uuid;

use crate::service::SyncControl;

/// How often the adapter asks the gateway where a pass stands while a
/// stream is attached. A poll, not a push: the control channel is
/// request/response and the gateway has no way to call the Agent.
const STATUS_POLL: Duration = Duration::from_millis(500);

/// How long the gateway may use an account bearer token this Agent handed it
/// **without being told again**.
///
/// Derived from [`HEARTBEAT`], not from the token: a pass runs at least once
/// per heartbeat while this process is alive and pushes the current token, so
/// four of them is the window in which an Agent that has stopped — crashed,
/// signed out, been killed — stops the gateway making account-authenticated
/// requests on its behalf. Shorter would refuse during an ordinary slow pass;
/// longer would be this process's liveness expressed as a token's lifetime.
const BLOB_ACCESS_GRACE_MILLIS: i64 = 4 * HEARTBEAT.as_millis() as i64;

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// The longest one dataset session is allowed to run before the adapter
/// gives up on it and reports a timeout. A pass over a large Workspace is
/// many units; this is a ceiling for the first device run, not a target.
const SESSION_CEILING: Duration = Duration::from_secs(15 * 60);

/// The Cloud coordinator's platform effects on the desktop.
pub struct DesktopCloudHost {
    control: SyncControl,
    network: RuntimeHandle,
    tokio: tokio::runtime::Handle,
    region: Region,
    issuer: String,
    device_key: DeviceSigningKey,
    local_endpoint: ReplicationEndpointId,
    local_profiles: Arc<dyn LocalProfileInventory>,
    /// The public API's origin, as this Agent validated it at composition.
    /// The gateway's blob engine reaches the five control routes on it and on
    /// nothing else.
    api_origin: String,
    /// The gateway's one sequence space per control session is shared by
    /// context, fence and policy commands, so this adapter numbers every
    /// sequenced command it sends and ignores the coordinator's own counter
    /// beyond the monotonic check the coordinator already makes.
    sequence: Mutex<u64>,
    /// The account and generation the last acknowledged context named, so
    /// a fence can be addressed to it.
    context: Mutex<Option<(CloudId, u64)>>,
    /// Shared worker connections and their per-dataset handshake generations.
    connections: dial::Pool,
    /// At most the installed permit budget. The gateway owns the retained
    /// stream; this lease keeps its QUIC connection alive between passes.
    sessions: Mutex<BTreeMap<Uuid, dial::Connection>>,
}

impl DesktopCloudHost {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        control: SyncControl,
        network: RuntimeHandle,
        tokio: tokio::runtime::Handle,
        region: Region,
        issuer: String,
        device_key: DeviceSigningKey,
        local_endpoint: ReplicationEndpointId,
        local_profiles: Arc<dyn LocalProfileInventory>,
        api_origin: String,
    ) -> Self {
        Self {
            control,
            network,
            tokio,
            region,
            issuer,
            device_key,
            local_endpoint,
            local_profiles,
            api_origin,
            sequence: Mutex::new(0),
            context: Mutex::new(None),
            connections: dial::Pool::default(),
            sessions: Mutex::new(BTreeMap::new()),
        }
    }

    fn session(&self) -> Result<ControlSessionId, Failure> {
        self.control.control_session().ok_or(Failure::Unavailable)
    }

    fn ask(&self, request: ControlRequest) -> Result<ControlResponse, Failure> {
        match self.control.cloud_request(request) {
            Ok(ControlResponse::Rejected { code }) => {
                // Every refusal here is retried by the coordinator's backoff:
                // a stale session heals on the next control connection, a
                // sequence conflict on the next context, a capacity refusal
                // on the next cohort. The root-catalog codes that mean
                // something to the coordinator keep their word.
                tracing::warn!(code, "cloud host: the gateway refused a control request");
                Err(match code.as_str() {
                    cloud::CATALOG_BINDING_CONFLICT => Failure::CatalogBindingConflict,
                    cloud::PLACEMENT_PENDING => Failure::PlacementPending,
                    _ => Failure::Unavailable,
                })
            }
            Ok(response) => Ok(response),
            Err(error) => {
                tracing::warn!(error, "cloud host: the control channel did not answer");
                Err(Failure::Unavailable)
            }
        }
    }

    fn next_sequence(&self) -> std::sync::MutexGuard<'_, u64> {
        // Keep the guard through acknowledgement: allocating numbers alone
        // would let concurrent callers submit them in reverse order.
        let mut sequence = self.sequence.lock().unwrap_or_else(|e| e.into_inner());
        *sequence = sequence.saturating_add(1);
        sequence
    }

    fn cloud_status(&self) -> Result<koru_sync_control::cloud::CloudStatus, Failure> {
        match self.ask(ControlRequest::CloudStatus {
            control_session_id: self.session()?,
        })? {
            ControlResponse::CloudStatus(status) => Ok(status),
            _ => Err(Failure::Unavailable),
        }
    }
}

fn cloud(value: Uuid) -> CloudId {
    CloudId::from_bytes(*value.as_bytes())
}

fn route_of(route: &CloudSyncRouteV1) -> Result<CloudRoute, Failure> {
    let mut direct_addresses = Vec::with_capacity(route.direct_addresses().len());
    for address in route.direct_addresses() {
        direct_addresses.push(address.parse().map_err(|_| Failure::Unavailable)?);
    }
    Ok(CloudRoute {
        endpoint_id: route.endpoint_id(),
        direct_addresses,
        relay_url: route.relay_url().map(str::to_owned),
    })
}

fn terminal(stage: CloudStage) -> bool {
    !matches!(
        stage,
        CloudStage::Connecting | CloudStage::Reconciling | CloudStage::WaitingForConsent
    )
}

impl Host for DesktopCloudHost {
    fn local_workspace(
        &self,
        account: KoruIdentityId,
        dataset: CloudReplicaPrincipalV1,
    ) -> Result<Option<LocalBootstrap>, Failure> {
        let CloudReplicaPrincipalV1::Workspace { workspace_id, .. } = dataset else {
            return Err(Failure::DatasetMismatch);
        };
        let response = self.ask(ControlRequest::ReadCloudBootstrapV2 {
            control_session_id: self.session()?,
            account_id: cloud(account.as_uuid()),
            dataset,
        })?;
        let ControlResponse::CloudBootstrap {
            manifest_document_id,
            allows_cloud,
            ..
        } = response
        else {
            return Err(Failure::Unavailable);
        };
        Ok(manifest_document_id.map(|manifest| LocalBootstrap {
            workspace_id: WorkspaceId::from_uuid(workspace_id.as_uuid()),
            manifest_document_id: manifest.as_uuid(),
            allows_cloud,
        }))
    }

    /// The Profile whose Identity/Private units are about to sync must be
    /// present here (the selection path's provisioning, TECHNICAL §7.4,
    /// already ran when a Profile was selected on this device). Checked
    /// through the same inventory the account runtime uses; an absent
    /// Profile is a bootstrap refusal the coordinator retries after
    /// selection has landed it. Never asked before a Workspace's content.
    fn ensure_profile_presence(
        &self,
        profile: ProfileId,
        account: KoruIdentityId,
    ) -> Result<(), Failure> {
        match self.local_profiles.authorize(profile, account) {
            Ok(true) => Ok(()),
            Ok(false) => Err(Failure::Bootstrap),
            Err(_) => Err(Failure::Unavailable),
        }
    }

    /// Step 3 runs in the gateway at installation, where the Store is: the
    /// Workspace permit carries the descriptor, and the gateway provisions
    /// the replica under its root placement before it opens the stream.
    /// Nothing to do on this side of the bridge.
    fn ensure_workspace_presence(&self, _permit: &CloudPermitV2) -> Result<(), Failure> {
        Ok(())
    }

    fn replace_context(
        &self,
        _sequence: u64,
        account: KoruIdentityId,
        account_generation: u64,
        permits: &[CloudPermitV2],
    ) -> Result<(), Failure> {
        if permits.len() > koru_sync_control::cloud::MAX_CLOUD_PERMIT_PAGE {
            for page in permits.chunks(koru_sync_control::cloud::MAX_CLOUD_PERMIT_PAGE) {
                self.replace_context(_sequence, account, account_generation, page)?;
            }
            return Ok(());
        }
        let control_session_id = self.session()?;
        let sequence_guard = self.next_sequence();
        let sequence = *sequence_guard;
        let account_id = cloud(account.as_uuid());
        let response = self.ask(ControlRequest::ReplaceCloudContext {
            control_session_id,
            sequence,
            account_id,
            account_generation,
            permits: Vec::new(),
            permits_v2: permits.to_vec(),
        })?;
        match response {
            ControlResponse::CloudContextApplied {
                sequence: acknowledged,
                ..
            } if acknowledged == sequence => {
                *self.context.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((account_id, account_generation));
                for permit in permits {
                    tracing::info!(
                        permit_id = %permit.permit_id,
                        placement = ?permit.placement_profile(),
                        dataset = ?permit.dataset(),
                        account_generation,
                        sequence,
                        "cloud host: gateway acknowledged installed permit"
                    );
                }
                Ok(())
            }
            _ => Err(Failure::Unavailable),
        }
    }

    /// `cloud-blob-object-v1` §3, over control 7.
    ///
    /// **The expiry sent is not the token's.** It is how long the gateway may
    /// keep using this value without the Agent confirming it again — derived
    /// from [`HEARTBEAT`], because a pass runs at least that often while this
    /// process is alive, so a gateway whose Agent has gone quiet stops making
    /// account-authenticated requests within a few heartbeats instead of
    /// holding a bearer token for its full life.
    fn replace_blob_access(
        &self,
        account: KoruIdentityId,
        account_generation: u64,
        access: &koru_account_wire::AccessToken,
    ) -> Result<(), Failure> {
        let control_session_id = self.session()?;
        let sequence_guard = self.next_sequence();
        let sequence = *sequence_guard;
        let expires_at_millis = now_millis().saturating_add(BLOB_ACCESS_GRACE_MILLIS);
        let response = self.ask(ControlRequest::ReplaceCloudBlobAccess {
            control_session_id,
            sequence,
            account_id: cloud(account.as_uuid()),
            account_generation,
            access: Box::new(koru_sync_control::cloud::CloudBlobAccessV1 {
                version: koru_sync_control::cloud::CLOUD_BLOB_ACCESS_VERSION,
                api_origin: self.api_origin.clone(),
                region: match self.region {
                    Region::Us => "us".into(),
                    Region::Eu => "eu".into(),
                },
                access_token: access.0.clone(),
                expires_at_millis,
            }),
        })?;
        match response {
            ControlResponse::CloudBlobAccessApplied {
                sequence: acknowledged,
                ..
            } if acknowledged == sequence => Ok(()),
            _ => Err(Failure::Unavailable),
        }
    }

    fn fence(&self, permit_ids: &[Uuid]) -> Result<(), Failure> {
        if permit_ids.len() > koru_sync_control::cloud::MAX_CLOUD_PERMIT_PAGE {
            for page in permit_ids.chunks(koru_sync_control::cloud::MAX_CLOUD_PERMIT_PAGE) {
                self.fence(page)?;
            }
            return Ok(());
        }
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|id, _| !permit_ids.contains(id));
        if permit_ids.is_empty() {
            return Ok(());
        }
        let Some((account_id, account_generation)) =
            *self.context.lock().unwrap_or_else(|e| e.into_inner())
        else {
            // Nothing was ever installed under this control session, so
            // there is nothing the gateway could still be admitting.
            return Ok(());
        };
        let control_session_id = self.session()?;
        let sequence_guard = self.next_sequence();
        let sequence = *sequence_guard;
        let response = self.ask(ControlRequest::FenceCloud {
            control_session_id,
            sequence,
            account_id,
            account_generation,
            permit_ids: Some(permit_ids.to_vec()),
        })?;
        match response {
            ControlResponse::CloudFenced {
                sequence: acknowledged,
                ..
            } if acknowledged == sequence => {
                tracing::info!(
                    ?permit_ids,
                    account_generation,
                    sequence,
                    "cloud host: gateway acknowledged retired permits"
                );
                Ok(())
            }
            _ => Err(Failure::Unavailable),
        }
    }

    fn open_dataset(
        &self,
        permit: &CloudPermitV2,
        credential: &SyncCredential,
        route: &CloudSyncRouteV1,
        dataset: CloudReplicaPrincipalV1,
        keys: &JwksResponseV1,
        access: &cloud::SessionAccess,
    ) -> Result<SessionReport, Failure> {
        let mut account_access = access.access_token().ok_or(Failure::SessionExpired)?;
        let mut access_renewed = Instant::now();
        if permit
            .expires_at_seconds
            .saturating_sub(now_millis() / 1000)
            <= cloud::SESSION_RENEW_BEFORE_SECONDS
        {
            return Err(Failure::CredentialExpired);
        }
        let account = KoruIdentityId::from_str_lossy(permit.account_id)?;
        let control_session_id = self.session()?;
        let verifier = cloud::capability_verifier(self.region, &self.issuer, keys)?;
        let retained = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&permit.permit_id)
            .is_some_and(|connection| connection.close_reason().is_none());
        if retained {
            let status = self.cloud_status()?;
            if let Some(record) = status
                .records
                .iter()
                .find(|r| r.permit_id == permit.permit_id)
                && matches!(
                    record.stage,
                    CloudStage::Settled
                        | CloudStage::CheckingCoverage
                        | CloudStage::Reconciling
                        | CloudStage::WaitingForConsent
                )
            {
                return Ok(SessionReport {
                    stage: record.stage.into(),
                    bytes_held: record.bytes_held,
                    queued_jobs: record.queued_jobs,
                    pending_consent_ids: record.pending_consent_ids.clone(),
                    error_code: record.error_code.as_deref().map(cloud::static_code),
                    ..SessionReport::default()
                });
            }
        }
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&permit.permit_id);
        let claims = cloud::credential_claims(credential)?;
        let worker_endpoint = route.endpoint_id();

        // 1. Dial and authenticate. A refusal is the worker's or the
        //    credential's answer; a failed dial is the network's.
        let dialed = match self.tokio.block_on(self.connections.open(
            &self.network,
            route_of(route)?,
            credential.compact().to_owned(),
            dataset,
            self.local_endpoint,
            &verifier,
            &self.device_key,
        )) {
            Ok(dialed) => dialed,
            Err(error) => {
                // Which dataset, which worker, and whether a reply came back
                // at all: "malformed" alone hid a full worker refusing a
                // dataset it was never able to lease.
                tracing::warn!(
                    code = error.code(),
                    refusal = error.is_refusal(),
                    dataset = ?dataset,
                    worker = ?worker_endpoint,
                    "cloud host: dataset dial failed"
                );
                return Err(if error.code() == cloud::CREDENTIAL_EXPIRED {
                    Failure::CredentialExpired
                } else if error.is_refusal() {
                    Failure::Denied
                } else {
                    Failure::Offline
                });
            }
        };
        let accepted = cloud::Accepted {
            session_generation: dialed.prepared.generation,
            expires_at_seconds: dialed.prepared.expires_at_seconds,
        };
        let principal = cloud::build_principal(
            permit,
            &claims,
            self.local_endpoint,
            worker_endpoint,
            control_session_id.as_uuid(),
            accepted,
        )?;

        // 2. The install barrier, acknowledged before any byte is forwarded.
        let response = self.ask(ControlRequest::InstallCloudSession {
            principal: Box::new(principal),
            connection_id: dialed.connection.stable_id() as u64,
            // The bridge mints the stream's own correlation id at admission;
            // the session generation is the retry correlation §4 asks for.
            stream_session_id: accepted.session_generation,
        });
        match response {
            Ok(ControlResponse::CloudSessionInstalled {
                permit_id,
                session_generation,
            }) if permit_id == permit.permit_id
                && session_generation == accepted.session_generation => {}
            Ok(_) | Err(_) => {
                let _ = self.fence(&[permit.permit_id]);
                return Err(Failure::Unavailable);
            }
        }

        // 3. Attach: the authenticated stream crosses the bridge under its
        //    principal and the Cloud content token.
        let dial::Dialed {
            connection,
            send,
            recv,
            ..
        } = dialed;
        if let Err(error) = self.tokio.block_on(self.network.admit_cloud_session(
            principal,
            &connection,
            send,
            recv,
        )) {
            tracing::warn!(?error, "cloud host: the bridge refused the dataset stream");
            let _ = self.fence(&[permit.permit_id]);
            return Err(Failure::Unavailable);
        }

        // 4. The gateway drives the pass; this side watches its stage.
        let started = Instant::now();
        let report = loop {
            std::thread::sleep(STATUS_POLL);
            let Some(current_access) = access.access_token() else {
                break SessionReport {
                    stage: Stage::AwaitingAuthority,
                    error_code: Some(cloud::SESSION_EXPIRED),
                    ..SessionReport::default()
                };
            };
            if current_access.0 != account_access.0 || access_renewed.elapsed() >= HEARTBEAT {
                // Keep the file engine's short local grant alive independently
                // of document completion, using the account's newest bearer.
                if let Err(error) =
                    self.replace_blob_access(account, permit.account_generation, &current_access)
                {
                    tracing::warn!(
                        code = error.code(),
                        "cloud session: file access renewal failed"
                    );
                }
                account_access = current_access;
                access_renewed = Instant::now();
            }
            let status = self.cloud_status()?;
            let record = status
                .records
                .iter()
                .find(|record| record.permit_id == permit.permit_id);
            let Some(record) = record else {
                // Fenced or replaced while the stream was attached: the
                // gateway refuses its remaining work and the pass is over.
                break SessionReport {
                    stage: Stage::Unavailable,
                    error_code: Some("replica_fenced"),
                    ..SessionReport::default()
                };
            };
            if terminal(record.stage) {
                break SessionReport {
                    stage: record.stage.into(),
                    // The live gateway handles hints on this same stream.
                    remote_hint: false,
                    bytes_held: record.bytes_held,
                    queued_jobs: record.queued_jobs,
                    pending_consent_ids: record.pending_consent_ids.clone(),
                    error_code: record.error_code.as_deref().map(cloud::static_code),
                };
            }
            if accepted
                .expires_at_seconds
                .saturating_sub(now_millis() / 1000)
                <= cloud::SESSION_RENEW_BEFORE_SECONDS
            {
                break SessionReport {
                    stage: Stage::Connecting,
                    error_code: Some(cloud::CREDENTIAL_EXPIRED),
                    ..SessionReport::default()
                };
            }
            if connection.close_reason().is_some() {
                break SessionReport {
                    stage: Stage::Offline,
                    error_code: Some("cloud_connection_closed"),
                    ..SessionReport::default()
                };
            }
            if started.elapsed() > SESSION_CEILING {
                break SessionReport {
                    stage: Stage::Unavailable,
                    error_code: Some("cloud_session_timeout"),
                    ..SessionReport::default()
                };
            }
        };
        if matches!(report.stage, Stage::Settled | Stage::CheckingCoverage) {
            tracing::info!(permit_id = %permit.permit_id, dataset = ?dataset,
                "cloud subscription: retained after initial reconciliation");
            self.sessions
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(permit.permit_id, connection);
        } else {
            let _ = self.fence(&[permit.permit_id]);
        }
        Ok(report)
    }

    // ----- The Store half of the account path, over control --------------

    fn account_binding(&self) -> Result<Option<CloudAccountBindingRef>, Failure> {
        match self.ask(ControlRequest::ReadCloudAccountBinding {
            control_session_id: self.session()?,
        })? {
            ControlResponse::CloudAccountBinding { binding, .. } => Ok(binding),
            _ => Err(Failure::Unavailable),
        }
    }

    fn bind_account_scope(&self, binding: &CloudCatalogBindingV2) -> Result<(), Failure> {
        match self.ask(ControlRequest::BindCloudAccountScope {
            control_session_id: self.session()?,
            account_id: cloud(binding.account_id.as_uuid()),
            catalog_scope_id: cloud(binding.catalog_scope_id.as_uuid()),
            bound_at_ms: binding.bound_at_ms.max(1),
        })? {
            ControlResponse::CloudAccountScopeBound { .. } => Ok(()),
            _ => Err(Failure::Unavailable),
        }
    }

    fn root_inventory(&self) -> Result<RootInventory, Failure> {
        match self.ask(ControlRequest::ReadCloudRootInventory {
            control_session_id: self.session()?,
        })? {
            ControlResponse::CloudRootInventory { units, held, .. } => {
                Ok(RootInventory { units, held })
            }
            _ => Err(Failure::Unavailable),
        }
    }

    fn root_unit_frontier(&self, unit: JoinUnit) -> Result<Option<Vec<u8>>, Failure> {
        match self.ask(ControlRequest::ReadCloudRootFrontier {
            control_session_id: self.session()?,
            unit,
        })? {
            ControlResponse::CloudRootFrontier { frontier, .. } => Ok(frontier),
            _ => Err(Failure::Unavailable),
        }
    }

    fn export_root_unit(
        &self,
        unit: JoinUnit,
        recipient_frontier: Option<&[u8]>,
    ) -> Result<CloudRootExport, Failure> {
        match self.ask(ControlRequest::ExportCloudRootUnit {
            control_session_id: self.session()?,
            unit,
            recipient_frontier: recipient_frontier.map(<[u8]>::to_vec),
        })? {
            ControlResponse::CloudRootUnitExport { export, .. } => Ok(export),
            _ => Err(Failure::Unavailable),
        }
    }

    fn join_root_unit(
        &self,
        account: KoruIdentityId,
        unit: JoinUnit,
        frames: &[Vec<u8>],
        advertised: Option<&[u8]>,
    ) -> Result<CloudRootJoin, Failure> {
        match self.ask(ControlRequest::JoinCloudRootUnit {
            control_session_id: self.session()?,
            account_id: cloud(account.as_uuid()),
            unit,
            frames: frames.to_vec(),
            advertised: advertised.map(<[u8]>::to_vec),
        })? {
            ControlResponse::CloudRootUnitJoined { join, .. } => Ok(join),
            _ => Err(Failure::Unavailable),
        }
    }
}

/// The permit's account as the runtime's account id. A permit's account is a
/// canonical v4 by validation, so this cannot fail for a permit the
/// coordinator built; a value that does is not a permit to open anything
/// under.
trait AccountFromCloud: Sized {
    fn from_str_lossy(id: CloudId) -> Result<Self, Failure>;
}

impl AccountFromCloud for KoruIdentityId {
    fn from_str_lossy(id: CloudId) -> Result<Self, Failure> {
        id.as_uuid()
            .to_string()
            .parse()
            .map_err(|_| Failure::DatasetMismatch)
    }
}

/// Account discovery, reconnection and proactive credential renewal cadence.
/// Live gateway subscriptions handle local commits and worker hints themselves;
/// an edit must never wait for this heartbeat. See ADR-441.
pub(crate) const HEARTBEAT: Duration = Duration::from_secs(30);

/// Compose the Cloud coordinator for this machine, or `None` when it cannot
/// dial a worker: no regional credential policy (which names the region and
/// issuer the sync credential is verified against), no device network, no
/// gateway control channel, or no device identity.
pub fn compose(
    origin: &str,
    control: Option<SyncControl>,
    signing_secret: Option<[u8; 32]>,
    endpoint_secret: Option<[u8; 32]>,
    local_profiles: Arc<dyn LocalProfileInventory>,
    waker: crate::event::Waker,
) -> Option<Arc<koru_account_runtime::cloud::Control>> {
    let policy = koru_env::value(koru_env::name::KORU_PUBLIC_API_CREDENTIAL_ROLLOUT)
        .filter(|value| !value.trim().is_empty())
        .and_then(|value| koru_account_session::RegionalCredentialPolicy::parse(&value).ok())?;
    let network = crate::network::current()?;
    let control = control?;
    let signing_secret = signing_secret?;
    let endpoint_secret = endpoint_secret?;
    let client = koru_account_client::AccountClient::new(origin)
        .ok()?
        .with_regional_credentials(policy.clone())
        .ok()?;
    let host = DesktopCloudHost::new(
        control,
        network.runtime().clone(),
        network.tokio().clone(),
        policy.region,
        // The sync credential's issuer is the regional one (the API signs
        // it as such), whatever stage the access-token issuance is at.
        policy.regional_issuer.clone(),
        DeviceSigningKey::from_secret_bytes(signing_secret),
        koru_device::ReplicationEndpointSecret::from_secret_bytes(endpoint_secret).endpoint_id(),
        local_profiles,
        // The same origin the account client was built on, canonicalised by
        // the same validation: one fact, one spelling, and never a second
        // value a deployment could set differently.
        koru_account_client::validate_api_origin(origin)
            .ok()?
            .to_string(),
    );
    let coordinator = Arc::new(koru_account_runtime::cloud::Control::start(
        koru_account_runtime::cloud::HostedApi::new(client),
        host,
        move |_snapshot| waker.wake(crate::event::AgentEvent::AccountChanged),
    ));
    let heartbeat = Arc::downgrade(&coordinator);
    if std::thread::Builder::new()
        .name("koru-agent cloud heartbeat".into())
        .spawn(move || {
            loop {
                std::thread::sleep(HEARTBEAT);
                let Some(coordinator) = heartbeat.upgrade() else {
                    return;
                };
                coordinator.wake();
            }
        })
        .is_err()
    {
        tracing::warn!("cloud host: no heartbeat thread; local changes wait for a remote hint");
    }
    Some(coordinator)
}
