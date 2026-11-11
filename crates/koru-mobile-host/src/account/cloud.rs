// SPDX-License-Identifier: MPL-2.0

//! The Android host's platform half of the Cloud coordinator, in process.
//!
//! On Android there is no gateway process and no bridge: the Store worker,
//! the device network and the coordinator all live in this process, so the
//! same steps the desktop takes over its control channel are taken here as
//! direct calls. The dial and the handshake are the shared
//! [`koru_network_iroh::cloud::dial`]; the pass is the shared
//! [`koru_store_server::cloud_sync::reconcile_dataset`], run on the host's
//! own Tokio runtime against the host's own `StoreWorkerClient`, exactly as
//! the gateway runs it on the desktop. The Store half of the account path
//! (root-catalog Plan 3.2) — the binding, the root inventory, the export
//! boundary and the staged join — is the same `root_sync` API the desktop
//! daemon answers over control, called under this host's own lock.
//!
//! The permit bookkeeping the gateway keeps in its `ControlHost` is kept here
//! as a table of live permits: an installed context admits a stream, a fence
//! or a replaced generation removes it, and every Store job and unit start
//! re-checks the table so work admitted after a fence is refused.
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, Weak};

use koru_account_core::workspace_authorization::Region;
use koru_account_core::{KoruIdentityId, ProfileId};
use koru_account_runtime::cloud::{
    self, CloudAccountBindingRef, CloudPermitScopeV2, CloudPermitV2, CloudRootExport,
    CloudRootJoin, CloudRootUnitState, Failure, Host, LocalBootstrap, RootInventory,
    RootRepairOutcome, SessionReport, Stage,
};
use koru_account_wire::cloud_sync::SyncCredential;
use koru_account_wire::{CloudCatalogBindingV2, CloudSyncRouteV1, JwksResponseV1};
use koru_device::cloud::{CloudId, CloudReplicaPrincipalV1};
use koru_network_iroh::{CloudRoute, cloud::dial};
use koru_replication::cloud::{Refusal, transfer};
use koru_store_core::errors::Code;
use koru_store_core::root_catalog::{CatalogUnitRef, Derived, ExportSubject};
use koru_store_core::service::{JoinUnit, RootGrant, RootJoinOutcome};
use koru_store_core::transport::{TransportPath, TransportSubject};
use koru_store_core::{DocumentId, Timestamp, WorkspaceId};
use koru_store_server::cloud_sync::{self, Progress};
use uuid::Uuid;

use crate::MobileHost;

/// Shared carrier accounting across this phone's Cloud subscriptions. It is a
/// cap over real allocations, not a preallocation: a peer record write
/// reserves twice the 16 MiB payload ceiling and a read reserves thirty-two
/// times its length, so a small number here refuses ordinary records. A
/// first-run bound, not a measured capacity.
const HOST_BUDGET_BYTES: usize = 256 * 1024 * 1024;

const SESSION_QUEUE: usize = 8;

/// How long the blob engine may use an installed account token **without
/// being told again**.
///
/// Derived from the coordinator's own wake interval rather than from the
/// token: the app's account poll wakes Cloud at least that often while a
/// person is looking at it (D244), so a phone that stops running passes stops
/// making account-authenticated blob requests within a few wakes instead of
/// holding a bearer live for its full lifetime. The same shape and the same
/// reasoning as the Agent's `BLOB_ACCESS_GRACE_MILLIS` on the desktop (D257).
const BLOB_ACCESS_GRACE_MILLIS: i64 = 4 * crate::CLOUD_WAKE_INTERVAL.as_millis() as i64;

/// One live permit, with its installed generation scoped to its connection.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Live {
    account_id: CloudId,
    account_generation: u64,
    session_generation: u64,
    session_connection_id: u64,
}

/// The Cloud coordinator's platform effects on Android.
pub struct MobileCloudHost {
    host: Weak<MobileHost>,
    region: Region,
    issuer: String,
    /// The account API's origin, as the trusted build configuration named it.
    /// The blob **control** plane's five routes hang off it; the byte origins
    /// are never learned from here or from any response.
    api_origin: String,
    /// This host's in-process stand-in for a control session id: minted once
    /// per host, so a principal from a previous process is never current.
    control_session_id: Uuid,
    live: Arc<Mutex<BTreeMap<Uuid, Live>>>,
    connections: dial::Pool,
    budget: transfer::MemoryBudget,
    /// Consecutive Store refusals per unit, across passes. Held by the host
    /// rather than the pass for the reason `cloud_sync::Deferrals` states: a
    /// pass visits each unit once, so a bound on consecutive refusals only
    /// means something if it outlives the pass that counted one.
    deferrals: cloud_sync::DeferralLedger,
    sessions: Mutex<BTreeMap<Uuid, Subscription>>,
}

struct Subscription {
    connection: dial::Connection,
    task: tokio::task::JoinHandle<()>,
    report: tokio::sync::watch::Receiver<Option<SessionReport>>,
    live: Arc<Mutex<BTreeMap<Uuid, Live>>>,
    permit_id: Uuid,
    generation: u64,
}

struct OpenDocument(Weak<MobileHost>);
impl cloud_sync::Downloads for OpenDocument {
    fn interests(&self) -> Vec<DocumentId> {
        self.0
            .upgrade()
            .and_then(|host| {
                *host
                    .document_focus
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
            })
            .into_iter()
            .collect()
    }
    fn watching(&self, _unit: transfer::Unit) -> bool {
        false
    }
    fn arrived(&self, _unit: transfer::Unit, _bytes: Vec<u8>) {}
    fn priority(&self, unit: transfer::Unit) -> bool {
        let transfer::Unit::Document { document_id, .. } = unit else {
            return false;
        };
        self.0.upgrade().is_some_and(|host| {
            *host
                .document_focus
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                == Some(DocumentId::from_uuid(document_id.as_uuid()))
        })
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(held) = lock(&self.live).get_mut(&self.permit_id)
            && held.session_generation == self.generation
            && held.session_connection_id == self.connection.stable_id() as u64
        {
            held.session_generation = 0;
        }
        // Accepted Store jobs retain their own ownership. Their authorization
        // checks above reject new work after retirement, even if already queued.
        self.task.abort();
    }
}

impl MobileCloudHost {
    pub fn new(host: Weak<MobileHost>, region: Region, issuer: String, api_origin: String) -> Self {
        Self {
            host,
            region,
            issuer,
            api_origin,
            control_session_id: Uuid::new_v4(),
            live: Arc::new(Mutex::new(BTreeMap::new())),
            connections: dial::Pool::default(),
            budget: transfer::MemoryBudget::new(HOST_BUDGET_BYTES),
            deferrals: Arc::new(Mutex::new(cloud_sync::Deferrals::new())),
            sessions: Mutex::new(BTreeMap::new()),
        }
    }

    fn host(&self) -> Result<Arc<MobileHost>, Failure> {
        self.host.upgrade().ok_or(Failure::Unavailable)
    }

    /// One Store turn for the account path. A Store refusal is carried as
    /// the stable code the Agent's coordinator reads; the worker being gone
    /// is unavailability.
    fn with_store<R: Send + 'static>(
        &self,
        task: impl FnOnce(&mut koru_store_server::WorkerStore) -> Result<R, &'static str>
        + Send
        + 'static,
    ) -> Result<R, Failure> {
        let host = self.host()?;
        host.store()
            .with_store(task)
            .map_err(|_| Failure::Unavailable)?
            .map_err(|code| match code {
                cloud::CATALOG_BINDING_CONFLICT => Failure::CatalogBindingConflict,
                _ => {
                    tracing::info!(code, "cloud host: the Store refused a root operation");
                    Failure::Unavailable
                }
            })
    }
}

fn lock(live: &Mutex<BTreeMap<Uuid, Live>>) -> std::sync::MutexGuard<'_, BTreeMap<Uuid, Live>> {
    live.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn cloud(value: Uuid) -> CloudId {
    CloudId::from_bytes(*value.as_bytes())
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
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

/// A Store refusal of a root or personal operation as the wire names it.
/// The publication job, then the eligible root inventory over the account's
/// Cloud grant. One function because the ordinary read and Q10's repair both
/// end in it, and the two must not drift (STORE.md: the job runs first so a
/// pending overlay has had its chance).
fn read_root_inventory(
    store: &mut koru_store_server::WorkerStore,
) -> Result<RootInventory, &'static str> {
    if let Err(error) = store.publish_pending_overlays() {
        tracing::info!(
            code = store_code(&error),
            "cloud host: publication job did not run"
        );
    }
    let inventory = store
        .root_export_inventory(&RootGrant::account())
        .map_err(|error| store_code(&error))?;
    Ok(RootInventory {
        units: inventory
            .units
            .into_iter()
            .map(|(unit, version)| CloudRootUnitState {
                unit: JoinUnit::catalog(unit),
                frontier: version.encode(),
            })
            .chain(
                inventory
                    .personal
                    .into_iter()
                    .map(|(workspace, version)| CloudRootUnitState {
                        unit: JoinUnit::PersonalV2 { workspace },
                        frontier: version.encode(),
                    }),
            )
            .collect(),
        held: u32::try_from(inventory.held).unwrap_or(u32::MAX),
    })
}

fn store_code(error: &koru_store_core::Error) -> &'static str {
    match error.code() {
        Code::Forbidden => "mode_refuses_transport",
        Code::IdempotencyConflict => "policy_changed",
        Code::WorkspaceNotFound => "root_unit_not_found",
        Code::CorruptState => "store_corrupt",
        _ => "store_unavailable",
    }
}

fn catalog_unit(unit: JoinUnit) -> Option<CatalogUnitRef> {
    match unit {
        JoinUnit::ProfileCatalog { profile } => Some(CatalogUnitRef::Profile { profile }),
        JoinUnit::WorkspaceCatalog { workspace } => Some(CatalogUnitRef::Workspace { workspace }),
        JoinUnit::PersonalV2 { .. } => None,
    }
}

fn subject(unit: JoinUnit) -> ExportSubject {
    match unit {
        JoinUnit::ProfileCatalog { profile } => ExportSubject::ProfileCatalog(profile),
        JoinUnit::WorkspaceCatalog { workspace } => ExportSubject::WorkspaceCatalog(workspace),
        JoinUnit::PersonalV2 { workspace } => ExportSubject::PersonalV2(workspace),
    }
}

fn version(bytes: Option<&[u8]>) -> Option<loro::VersionVector> {
    bytes.map(|bytes| loro::VersionVector::decode(bytes).unwrap_or_default())
}

impl Host for MobileCloudHost {
    fn local_workspace(
        &self,
        _account: KoruIdentityId,
        dataset: CloudReplicaPrincipalV1,
    ) -> Result<Option<LocalBootstrap>, Failure> {
        let CloudReplicaPrincipalV1::Workspace { workspace_id, .. } = dataset else {
            return Err(Failure::DatasetMismatch);
        };
        let workspace = WorkspaceId::from_uuid(workspace_id.as_uuid());
        // The same rule as the gateway's `read_cloud_bootstrap`: a Workspace
        // this Store does not hold is a fresh device; one it holds answers
        // its durable manifest identity, deleted or not, and whether its own
        // transport policy admits the Cloud path; a Store that cannot answer
        // is an error, never a null.
        let found =
            self.with_store(
                move |store| match store.workspace_including_deleted(workspace) {
                    Ok(_) => {
                        let manifest = store
                            .replica_manifest_id_including_deleted(workspace)
                            .map_err(|_| "manifest_unavailable")?;
                        let allows_cloud = store
                            .transport_admission(TransportSubject::Workspace(workspace))
                            .ok()
                            .map(|admission| admission.allows_cloud());
                        Ok(Some((manifest, allows_cloud)))
                    }
                    Err(error) if error.code == Code::WorkspaceNotFound => Ok(None),
                    Err(_) => Err("store_unavailable"),
                },
            )?;
        Ok(found.map(|(manifest, allows_cloud)| LocalBootstrap {
            workspace_id: workspace,
            manifest_document_id: manifest.as_uuid(),
            allows_cloud,
        }))
    }

    /// The Profiles that are on this phone, bound to this account and not
    /// removed from it: the only ones a Cloud pass asks for. The service no
    /// longer requires a device to be bound to a Profile before it issues a
    /// credential (ADR-000-cloudneedsnopeer), so without this a phone would
    /// be handed every Profile the account owns. A Store that cannot answer
    /// gives the empty list, so a pass that cannot tell asks for nothing.
    fn held_profiles(
        &self,
        account: KoruIdentityId,
    ) -> Option<std::collections::BTreeSet<ProfileId>> {
        let Ok(binding) =
            koru_store_core::ProfileAccountBinding::from_identity_uuid(account.as_uuid())
        else {
            return Some(Default::default());
        };
        Some(
            self.with_store(move |store| {
                store
                    .profile_lifecycle_inventory()
                    .map(|inventory| {
                        inventory
                            .iter()
                            .filter(|stored| {
                                stored.account_binding == binding && stored.removed_at.is_none()
                            })
                            .map(|stored| stored.profile)
                            .collect()
                    })
                    .map_err(|_| "store_unavailable")
            })
            .unwrap_or_default(),
        )
    }

    /// The Profile whose Identity/Private units are about to sync must be
    /// present here and bound to this account (the selection path's
    /// provisioning, TECHNICAL §7.4). A Profile bound to another account is
    /// never touched. Never asked before a Workspace's content.
    fn ensure_profile_presence(
        &self,
        profile: ProfileId,
        account: KoruIdentityId,
    ) -> Result<(), Failure> {
        let binding = koru_store_core::ProfileAccountBinding::from_identity_uuid(account.as_uuid())
            .map_err(|_| Failure::Denied)?;
        let present = self.with_store(move |store| {
            store
                .profile_lifecycle_inventory()
                .map(|inventory| {
                    inventory.iter().any(|stored| {
                        stored.profile == profile
                            && stored.account_binding == binding
                            && stored.removed_at.is_none()
                    })
                })
                .map_err(|_| "store_unavailable")
        })?;
        if present {
            Ok(())
        } else {
            Err(Failure::Bootstrap)
        }
    }

    /// Step 3 under the Workspace's **root placement** (root-catalog
    /// TECHNICAL.final §7.2–7.3): the shared identity-only bootstrap the
    /// gateway runs at installation. A placement this Store cannot list the
    /// Workspace under yet is `placement_pending`, which the coordinator
    /// retries after the next root join; the descriptor's placement seeds
    /// nothing — and for a permit of the v1 window, whose service has no root
    /// units to join, it places the row alone and **no Workspace unit is
    /// published** (root-catalog QUESTIONS.md Q7), so the first real unit
    /// from a linked device is the only placement. The phone takes that from
    /// the same `bootstrap_placement` the desktop driver uses rather than
    /// having a second idea of what the window is.
    fn ensure_workspace_presence(&self, permit: &CloudPermitV2) -> Result<(), Failure> {
        let CloudPermitScopeV2::Workspace { descriptor } = &permit.scope else {
            return Err(Failure::DatasetMismatch);
        };
        let host = self.host()?;
        let store = host.store().clone();
        let workspace = WorkspaceId::from_uuid(descriptor.workspace_id.as_uuid());
        let manifest = DocumentId::from_uuid(descriptor.manifest_document_id.as_uuid());
        let placement = koru_store_server::cloud_driver::bootstrap_placement(permit);
        host.tokio()
            .block_on(koru_store_server::cloud_driver::ensure_workspace(
                &store, workspace, manifest, placement,
            ))
            .map_err(|code| {
                tracing::warn!(code, "cloud host: workspace bootstrap refused");
                if code == cloud::PLACEMENT_PENDING {
                    Failure::PlacementPending
                } else {
                    Failure::Bootstrap
                }
            })
    }

    fn replace_context(
        &self,
        _sequence: u64,
        account: KoruIdentityId,
        account_generation: u64,
        permits: &[CloudPermitV2],
    ) -> Result<(), Failure> {
        let account_id = cloud(account.as_uuid());
        let mut live = lock(&self.live);
        // An account-generation replacement fences every older permit of
        // that account without enumerating them; other accounts keep theirs.
        live.retain(|_, held| {
            held.account_id != account_id || held.account_generation >= account_generation
        });
        for permit in permits {
            if permit.account_id != account_id
                || permit.account_generation != account_generation
                || permit.dataset().is_none()
            {
                return Err(Failure::DatasetMismatch);
            }
            live.entry(permit.permit_id)
                .and_modify(|held| held.account_generation = account_generation)
                .or_insert(Live {
                    account_id,
                    account_generation,
                    session_generation: 0,
                    session_connection_id: 0,
                });
        }
        Ok(())
    }

    /// `cloud-blob-object-v1` §3, **in process**.
    ///
    /// The desktop sends this over control 7 because the account lives in
    /// `koru-agent` and the Store lives in the gateway (D257). A phone has
    /// both here, so the same value goes straight into the engine the Store
    /// worker composed — through the same `validate` and under the same
    /// expiry rule, so neither platform has a second idea of what blob access
    /// is. Pushed every pass rather than on change, for D257's reason: the
    /// expiry carried is how long it may be used *without being told again*.
    fn replace_blob_access(
        &self,
        _account: KoruIdentityId,
        _account_generation: u64,
        access: &koru_account_wire::AccessToken,
    ) -> Result<(), Failure> {
        let host = self.host()?;
        let Some(blobs) = host.blobs() else {
            // A device whose read model did not open has no engine to hold
            // this. Not a refusal of the pass: its documents still sync.
            return Ok(());
        };
        blobs
            .install_access(
                &host.tokio(),
                koru_sync_control::cloud::CloudBlobAccessV1 {
                    version: koru_sync_control::cloud::CLOUD_BLOB_ACCESS_VERSION,
                    api_origin: self.api_origin.clone(),
                    region: match self.region {
                        Region::Us => "us".into(),
                        Region::Eu => "eu".into(),
                    },
                    access_token: access.0.clone(),
                    expires_at_millis: now_millis().saturating_add(BLOB_ACCESS_GRACE_MILLIS),
                },
            )
            .map_err(|code| {
                tracing::warn!(code, "cloud host: blob access was refused");
                Failure::Denied
            })
    }

    fn fence(&self, permit_ids: &[Uuid]) -> Result<(), Failure> {
        let mut live = lock(&self.live);
        for permit_id in permit_ids {
            live.remove(permit_id);
        }
        drop(live);
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|id, _| !permit_ids.contains(id));
        Ok(())
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
        access.access_token().ok_or(Failure::SessionExpired)?;
        if permit
            .expires_at_seconds
            .saturating_sub(now_millis() / 1000)
            <= cloud::SESSION_RENEW_BEFORE_SECONDS
        {
            return Err(Failure::CredentialExpired);
        }
        let account: KoruIdentityId = permit
            .account_id
            .as_uuid()
            .to_string()
            .parse()
            .map_err(|_| Failure::DatasetMismatch)?;
        let host = self.host()?;
        if !lock(&self.live).contains_key(&permit.permit_id) {
            return Err(Failure::Unavailable);
        }
        {
            let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(session) = sessions.get(&permit.permit_id)
                && !session.task.is_finished()
                && session.connection.close_reason().is_none()
                && let Some(report) = session.report.borrow().clone()
            {
                return Ok(report);
            }
            sessions.remove(&permit.permit_id);
        }
        let verifier = cloud::capability_verifier(self.region, &self.issuer, keys)?;
        let claims = cloud::credential_claims(credential)?;
        let worker_endpoint = route.endpoint_id();
        let local_endpoint = host.local_endpoint();

        // 1. Dial and authenticate.
        let dialed = match host.tokio().block_on(self.connections.open(
            host.network(),
            route_of(route)?,
            credential.compact().to_owned(),
            dataset,
            local_endpoint,
            &verifier,
            &host.device_key(),
        )) {
            Ok(dialed) => dialed,
            Err(error) => {
                tracing::warn!(code = error.code(), "cloud host: dataset dial failed");
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
        // The principal is formed and validated exactly as on the desktop,
        // even though no control frame carries it here: it is the one
        // cross-check between the permit and the credential.
        let principal = cloud::build_principal(
            permit,
            &claims,
            local_endpoint,
            worker_endpoint,
            self.control_session_id,
            accepted,
        )?;

        // 2. Install: the permit must still be live, and the generation must
        //    advance, or the stream is not admitted.
        {
            let mut live = lock(&self.live);
            let Some(held) = live.get_mut(&permit.permit_id) else {
                return Err(Failure::Unavailable);
            };
            let connection_id = dialed.connection.stable_id() as u64;
            if connection_id == held.session_connection_id
                && principal.session_generation <= held.session_generation
            {
                return Err(Failure::Unavailable);
            }
            held.session_generation = principal.session_generation;
            held.session_connection_id = connection_id;
        }
        // Which Workspace's pictures this pass makes worth walking. A
        // Profile dataset carries no blobs of its own: the personal
        // Workspaces beside it are datasets in their own right and get their
        // own pass. The same rule `cloud_driver` applies on the desktop.
        let blob_workspace = match dataset {
            CloudReplicaPrincipalV1::Workspace { workspace_id, .. } => {
                Some(WorkspaceId::from_uuid(workspace_id.as_uuid()))
            }
            CloudReplicaPrincipalV1::Profile { .. } => None,
        };
        // The dataset incarnation coverage is claimed under, exactly as
        // `cloud_driver` reads it on the desktop.
        let blob_incarnation = match &permit.scope {
            CloudPermitScopeV2::Workspace { descriptor } => {
                Some(descriptor.workspace_incarnation_id)
            }
            _ => None,
        };
        let (plan, scope) = koru_store_server::cloud_driver::plan_for(permit).map_err(|code| {
            tracing::warn!(code, "cloud host: permit cannot be planned");
            Failure::DatasetMismatch
        })?;
        let Some(capacity) = NonZeroUsize::new(SESSION_QUEUE) else {
            return Err(Failure::Unavailable);
        };
        let session = cloud_sync::Session {
            accepted: dialed.prepared,
            scope,
            // TECHNICAL §4.4 on a phone: the network policy operation exists
            // and the metered ceiling is owed with the consent surface; the
            // first device run accepts every payload.
            automatic_limit: transfer::MAX_PAYLOAD,
            capacity,
            budget: self.budget.clone(),
            observed: tokio::time::Instant::now(),
            wall_millis: now_millis(),
        };
        let live = Arc::clone(&self.live);
        let permit_id = permit.permit_id;
        let generation = principal.session_generation;
        let session_connection_id = dialed.connection.stable_id() as u64;
        let authorize: cloud_sync::Authorize = Arc::new(move |_unit| {
            if lock(&live).get(&permit_id).is_some_and(|held| {
                held.session_generation == generation
                    && held.session_connection_id == session_connection_id
            }) {
                Ok(())
            } else {
                Err(Refusal::ReplicaFenced)
            }
        });

        // Reconciliation owns a retained task, not this blocking admission
        // call. Report the first completed pass and let the lane admit siblings.
        let dial::Dialed {
            connection,
            send,
            recv,
            ..
        } = dialed;
        let worker = host.store().clone();
        let downloads: Arc<dyn cloud_sync::Downloads> = Arc::new(OpenDocument(self.host.clone()));
        let blobs = host.blobs().cloned();
        let deferrals = Arc::clone(&self.deferrals);
        let live = Arc::clone(&self.live);
        let (reports, report) = tokio::sync::watch::channel::<Option<SessionReport>>(None);
        let (completed, mut completed_rx) =
            tokio::sync::watch::channel::<Option<cloud_sync::Outcome>>(None);
        let task_reports = reports.clone();
        let task = host.tokio().spawn(async move {
            let blob_work = async {
                while completed_rx.changed().await.is_ok() {
                    let Some(outcome) = completed_rx.borrow_and_update().clone() else {
                        continue;
                    };
                    if let (Some(workspace), Some(blobs)) = (blob_workspace, blobs.as_ref()) {
                        if let Some(incarnation) = blob_incarnation {
                            let live = Arc::clone(&live);
                            let authorization: koru_blob_sync::LiveAuthorization =
                                Arc::new(move |_| {
                                    lock(&live)
                                        .get(&permit_id)
                                        .is_some_and(|held| {
                                            held.session_generation == generation
                                                && held.session_connection_id
                                                    == session_connection_id
                                        })
                                        .then_some(incarnation)
                                });
                            blobs.expect_documents(
                                workspace,
                                incarnation,
                                koru_store_server::cloud_driver::covered_documents(&outcome),
                                authorization,
                            );
                        }
                        blobs.after_document_pass(workspace).await;
                    }
                }
                std::future::pending::<()>().await;
            };
            let work = cloud_sync::subscribe_dataset(
                recv,
                send,
                session,
                plan,
                worker,
                authorize,
                downloads,
                deferrals,
                |_start| transfer::Decision::Accept,
                move |progress| match progress {
                    Progress::PassStarted => {
                        task_reports.send_if_modified(|report| {
                            if let Some(report) = report {
                                report.stage = Stage::Reconciling;
                                true
                            } else {
                                false
                            }
                        });
                    }
                    Progress::PassFinished(outcome) => {
                        task_reports.send_replace(Some(SessionReport {
                            stage: if outcome.unavailable.is_empty() {
                                Stage::Settled
                            } else {
                                Stage::Unavailable
                            },
                            error_code: (!outcome.unavailable.is_empty())
                                .then_some("cloud_unit_unavailable"),
                            ..SessionReport::default()
                        }));
                        completed.send_replace(Some(*outcome));
                    }
                    Progress::Refused { unit, code } | Progress::Unavailable { unit, code } => {
                        tracing::warn!(
                            ?unit,
                            code = code.as_str(),
                            "cloud subscription: unit refused"
                        )
                    }
                    _ => {}
                },
            );
            let result = tokio::select! {
                result = work => result,
                () = blob_work => unreachable!("file follower stays alive with the stream"),
            };
            let error_code = result.err().map(|error| cloud::static_code(error.code()));
            tracing::info!(?dataset, error_code, "cloud subscription: ended");
            reports.send_replace(Some(SessionReport {
                stage: Stage::Unavailable,
                error_code,
                ..SessionReport::default()
            }));
        });
        let subscription = Subscription {
            connection,
            task,
            report,
            live: Arc::clone(&self.live),
            permit_id,
            generation,
        };
        let started = std::time::Instant::now();
        let mut renewed = std::time::Instant::now();
        loop {
            let current_access = access.access_token().ok_or(Failure::SessionExpired)?;
            if renewed.elapsed().as_secs() >= 30 {
                self.replace_blob_access(account, permit.account_generation, &current_access)?;
                renewed = std::time::Instant::now();
            }
            if accepted
                .expires_at_seconds
                .saturating_sub(now_millis() / 1000)
                <= cloud::SESSION_RENEW_BEFORE_SECONDS
            {
                return Err(Failure::CredentialExpired);
            }
            let report = subscription.report.borrow().clone();
            if let Some(report) = report {
                self.sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(permit_id, subscription);
                return Ok(report);
            }
            if subscription.task.is_finished() || started.elapsed().as_secs() > 15 * 60 {
                return Err(Failure::Unavailable);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    // ----- The Store half of the account path, in process ---------------

    fn account_binding(&self) -> Result<Option<CloudAccountBindingRef>, Failure> {
        self.with_store(|store| {
            store
                .account_binding()
                .map(|binding| {
                    binding.map(|binding| CloudAccountBindingRef {
                        account_id: cloud(binding.account_id),
                        catalog_scope_id: cloud(binding.catalog_scope_id),
                    })
                })
                .map_err(|error| store_code(&error))
        })
    }

    fn bind_account_scope(&self, binding: &CloudCatalogBindingV2) -> Result<(), Failure> {
        let account_id = binding.account_id.as_uuid();
        let catalog_scope_id = binding.catalog_scope_id.as_uuid();
        let bound_at_ms = binding.bound_at_ms;
        self.with_store(move |store| {
            store
                .bind_account_scope(account_id, catalog_scope_id, bound_at_ms)
                .map(|_| ())
                .map_err(|error| match error.code() {
                    // §4.1: two bound accounts never join; a binding is never
                    // renamed. Both terminal for the coordinator.
                    Code::Forbidden => cloud::CATALOG_BINDING_CONFLICT,
                    _ => store_code(&error),
                })
        })
    }

    fn root_inventory(&self) -> Result<RootInventory, Failure> {
        self.with_store(|store| read_root_inventory(store))
    }

    fn repair_bootstrap_units(&self) -> Result<RootRepairOutcome, Failure> {
        self.with_store(|store| {
            // Q10's repair, in the same turn as the read that follows it, so
            // the pass pushes what it just created. The phone reaches the same
            // Store port in process that the desktop reaches over control's
            // one additive field; the *decision* is the coordinator's on both.
            let repaired = match store.repair_bootstrap_units(Timestamp::now()) {
                Ok(done) => u32::try_from(done.len()).unwrap_or(u32::MAX),
                Err(error) => {
                    tracing::info!(
                        code = store_code(&error),
                        "cloud host: the withheld-unit repair did not run"
                    );
                    0
                }
            };
            Ok(RootRepairOutcome {
                repaired,
                inventory: read_root_inventory(store)?,
            })
        })
    }

    fn root_unit_frontier(&self, unit: JoinUnit) -> Result<Option<Vec<u8>>, Failure> {
        self.with_store(move |store| {
            let frontier = match (catalog_unit(unit), unit) {
                (Some(catalog), _) => store.root_unit_frontier(catalog, TransportPath::Cloud),
                (None, JoinUnit::PersonalV2 { workspace }) => {
                    store.personal_frontier(workspace, TransportPath::Cloud)
                }
                (None, _) => return Err("root_unit_not_found"),
            }
            .map_err(|error| store_code(&error))?;
            Ok(frontier.map(|version| version.encode()))
        })
    }

    fn export_root_unit(
        &self,
        unit: JoinUnit,
        recipient_frontier: Option<&[u8]>,
    ) -> Result<CloudRootExport, Failure> {
        let recipient = version(recipient_frontier);
        self.with_store(move |store| {
            // §4.3 in three steps on one worker turn: prepare, derive,
            // recheck the policy generation before the bytes are answered.
            let source = match store.root_export_source(&RootGrant::account(), subject(unit)) {
                Ok(Some(source)) => source,
                Ok(None) => return Ok(CloudRootExport::NotFound),
                Err(error) if error.code() == Code::Forbidden => return Ok(CloudRootExport::Held),
                Err(error) => return Err(store_code(&error)),
            };
            let authorization = store
                .account_binding()
                .ok()
                .flatten()
                .map(|binding| binding.account_id.to_string())
                .unwrap_or_default();
            Ok(
                match source.derive(recipient.as_ref(), authorization.clone()) {
                    Ok(Derived::Publish(publication)) => {
                        if store.policy_generation() != publication.capture.policy_generation
                            || publication.capture.authorization != authorization
                        {
                            tracing::info!(
                                ?unit,
                                "cloud host: payload discarded reason=policy_changed"
                            );
                            CloudRootExport::Held
                        } else {
                            CloudRootExport::Published {
                                frames: vec![publication.bytes],
                                redacted_ops: u32::try_from(publication.redacted_ops)
                                    .unwrap_or(u32::MAX),
                            }
                        }
                    }
                    Ok(Derived::Hold { reason }) => {
                        tracing::info!(?unit, reason = %reason, "cloud host: root unit held");
                        CloudRootExport::Held
                    }
                    Err(error) => {
                        tracing::info!(?unit, ?error, "cloud host: root export refused");
                        CloudRootExport::Held
                    }
                },
            )
        })
    }

    fn join_root_unit(
        &self,
        account: KoruIdentityId,
        unit: JoinUnit,
        frames: &[Vec<u8>],
        advertised: Option<&[u8]>,
    ) -> Result<CloudRootJoin, Failure> {
        let account_id = account.as_uuid();
        let frames = frames.to_vec();
        let advertised = version(advertised);
        self.with_store(move |store| {
            let outcome = match (catalog_unit(unit), unit) {
                (Some(catalog), _) => {
                    let staged = store
                        .stage_root_unit(account_id, catalog, &frames)
                        .map_err(|error| store_code(&error))?;
                    store
                        .commit_staged_root_unit(account_id, staged, advertised.as_ref())
                        .map_err(|error| store_code(&error))?
                }
                (None, JoinUnit::PersonalV2 { workspace }) => {
                    let staged = store
                        .stage_personal_unit(account_id, workspace, &frames)
                        .map_err(|error| store_code(&error))?;
                    store
                        .commit_staged_personal_unit(account_id, staged, advertised.as_ref())
                        .map_err(|error| store_code(&error))?
                }
                (None, _) => return Err("root_unit_not_found"),
            };
            Ok(match outcome {
                RootJoinOutcome::Applied { frontier, .. } => CloudRootJoin::Applied {
                    frontier: frontier.encode(),
                },
                RootJoinOutcome::Pending { frontier } => CloudRootJoin::Pending {
                    frontier: frontier.encode(),
                },
            })
        })
    }
}
