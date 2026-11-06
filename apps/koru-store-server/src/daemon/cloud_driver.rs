// SPDX-License-Identifier: MPL-2.0

//! The gateway's owner of bridged Cloud content streams.
//!
//! The Agent dials the worker, completes the handshake, has
//! `install_cloud_session` acknowledged and only then forwards the stream
//! over the private bridge under its Cloud principal. That stream lands here.
//! The two IPC channels are not ordered, so attachment proves the stream
//! against the *installed* context — the live permit, the account and dataset
//! it names, the exact session generation — instead of trusting that the
//! install arrived first. A stream that cannot prove itself is reset; nothing
//! is opened for it and nothing is derived from it.
//!
//! What runs under an admitted stream is the existing dataset pass,
//! [`super::cloud_sync::reconcile_dataset`], over the existing Store ports.
//! Every Store job and every unit start re-checks that the permit is still
//! live at its installed generation, so a fence or a replacement refuses work
//! admitted after it. Progress is reported back through the control host's
//! in-memory status only: `Settled` here is not coverage, and nothing is
//! written down about a pass.
use std::{
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use koru_device::cloud::CloudReplicaPrincipalV1;
use koru_network_edge::TransportSession;
use koru_network_iroh::SessionRuntimeHandle;
use koru_replication::cloud::{
    Bytes, HandshakeV1, PreparedSession, Refusal, Version1,
    transfer::{self, Decision, MemoryBudget, Scope, Unit},
};
use koru_store_api::cloud_activity::{CloudActivity, PassCounts, UnitStep};
use koru_store_core::{
    DocumentId, ProfileId, Timestamp, WorkspaceId, profile_units::ProfileUnitRef,
};
use koru_sync_control::{
    ControlHost,
    cloud::{CloudPermitV1, CloudStage},
};
use tokio::time::Instant;

use super::{
    StoreWorkerClient,
    blob_sync::BlobSync,
    cloud_sync::{self, Plan, Progress},
    session_io,
};

/// The blob engine, once the read model it walks has opened.
///
/// A `OnceLock` rather than an argument because the two are installed at
/// different moments and in that order: the Cloud acceptor must exist before
/// the first bridged session can arrive, and the engine cannot exist before
/// `projection.db` is open. A session that lands in between syncs its
/// documents and leaves its pictures to the next pass, which is the safe
/// direction.
pub type BlobEngineSlot = Arc<std::sync::OnceLock<BlobSync>>;

/// The carrier's accounting cap across gateway subscriptions: decoded records and
/// payload fragments in flight, charged against the same accounting the
/// standard carrier uses. It is a cap over real allocations, not a
/// preallocation: a peer record write reserves twice the 16 MiB payload
/// ceiling and a read reserves thirty-two times its length. A test bound for
/// the first device run, not a measured capacity.
const HOST_BUDGET_BYTES: usize = 512 * 1024 * 1024;

/// Control messages the carrier may queue before it stops reading.
const SESSION_QUEUE: usize = 8;

/// What a dataset's stage says when a unit has stopped syncing while its
/// neighbours carried on.
///
/// Its own token, not one of the transfer codes and not the Store code that
/// caused it. A unit the pass deferred, a unit that ran out of deferrals and a
/// pass that could not continue are three different things a person acts on
/// differently, and `sync_transfer_invalid` meaning two things at once sent
/// two investigations into the wrong crate tonight. The Store's own code is in
/// the pass record and in the log beside this.
const UNIT_UNAVAILABLE: &str = "cloud_unit_unavailable";

/// Route every Cloud-principal stream the bridge admits to [`run_session`].
/// Installed once at startup, before the first session can arrive.
pub(crate) fn install(
    replication: &SessionRuntimeHandle,
    host: Arc<Mutex<ControlHost>>,
    store: StoreWorkerClient,
    rooms: Arc<koru_store_api::rooms::Rooms>,
    activity: Arc<Mutex<CloudActivity>>,
    blobs: BlobEngineSlot,
) {
    // One ledger for the whole gateway, minted here rather than inside a
    // session: a bound on consecutive refusals only means something if it
    // outlives the pass that counted one.
    let deferrals: cloud_sync::DeferralLedger = Arc::new(Mutex::new(cloud_sync::Deferrals::new()));
    let budget = MemoryBudget::new(HOST_BUDGET_BYTES);
    replication.set_cloud_acceptor(Arc::new(move |session| {
        let host = Arc::clone(&host);
        let store = store.clone();
        let rooms = Arc::clone(&rooms);
        let activity = Arc::clone(&activity);
        let deferrals = Arc::clone(&deferrals);
        let blobs = Arc::clone(&blobs);
        let budget = budget.clone();
        tokio::spawn(async move {
            run_session(
                session, host, store, rooms, activity, deferrals, blobs, budget,
            )
            .await;
        });
    }));
}

/// The dataset key one pass records under — the Workspace's id for a Workspace
/// plan, the Profile's for a Profile one. A pass is one or the other
/// ([`plan_for`]), so one key answers for both.
fn dataset_key(plan: &Plan) -> uuid::Uuid {
    match plan {
        Plan::Workspace { workspace_id } => workspace_id.as_uuid(),
        Plan::Profile { profile_id, .. } => profile_id.as_uuid(),
    }
}

/// **The document half of coverage**, out of the pass that established it.
///
/// One entry per document unit the pass settled, carrying the exact state it
/// read — so the reference sample can be checked against the same state rather
/// than against a second, later read. A unit the pass did not carry whole is
/// present with `None`: it is required, and Cloud does not have it.
///
/// A unit whose read never happened carries an empty state, which decodes to
/// the empty version vector and is included by everything — so it is reported
/// as `None` here rather than as a state nobody read.
pub fn covered_documents(outcome: &cloud_sync::Outcome) -> koru_blob_sync::CoveredDocuments {
    outcome
        .documents
        .iter()
        .filter_map(|document| {
            let Unit::Document { document_id, .. } = document.unit else {
                return None;
            };
            let id = DocumentId::from_uuid(document_id.as_uuid());
            let carried = document.carried && !document.state.is_empty();
            Some((
                id,
                carried.then(|| koru_blob_sync::DocumentState::from_bytes(document.state.clone())),
            ))
        })
        .collect()
}

/// The `(workspace, document)` a unit is about, or `None` for a Profile unit —
/// Identity, Private and Personal are facts about the dataset and about no
/// document at all.
fn unit_document(unit: Unit) -> Option<(uuid::Uuid, uuid::Uuid)> {
    match unit {
        Unit::Document {
            workspace_id,
            document_id,
        } => Some((workspace_id.as_uuid(), document_id.as_uuid())),
        _ => None,
    }
}

/// The local half of a Cloud download: the same room broadcast the peer path
/// performs after it commits remote updates, so a document already open sees
/// what the Cloud just delivered instead of waiting for a reopen (D242).
struct CloudDownloads {
    rooms: Arc<koru_store_api::rooms::Rooms>,
    /// The manifest's document id for this dataset; its room is the
    /// workspace's, not a document's.
    manifest: Option<(uuid::Uuid, uuid::Uuid)>,
}

impl CloudDownloads {
    fn room(&self, unit: Unit) -> Option<koru_store_api::rooms::Room> {
        use koru_store_api::rooms::Room;
        let Unit::Document {
            workspace_id,
            document_id,
        } = unit
        else {
            // A Profile unit has no document room; the surfaces that show one
            // re-read it through their own path.
            return None;
        };
        let workspace = WorkspaceId::from_uuid(workspace_id.as_uuid());
        let document = DocumentId::from_uuid(document_id.as_uuid());
        match self.manifest {
            Some((manifest_workspace, manifest_document))
                if manifest_document == document_id.as_uuid()
                    && manifest_workspace == workspace_id.as_uuid() =>
            {
                Some(Room::Manifest(workspace))
            }
            _ => Some(Room::Document(document)),
        }
    }
}

impl cloud_sync::Downloads for CloudDownloads {
    fn interests(&self) -> Vec<DocumentId> {
        self.rooms.documents()
    }
    fn watching(&self, unit: Unit) -> bool {
        self.room(unit)
            .is_some_and(|room| self.rooms.subscribers(&room) > 0)
    }

    fn arrived(&self, unit: Unit, bytes: Vec<u8>) {
        let Some(room) = self.room(unit) else {
            return;
        };
        let message = loro_protocol::protocol::ProtocolMessage::DocUpdate {
            crdt: loro_protocol::protocol::CrdtType::Loro,
            room_id: room.to_id(),
            updates: vec![bytes],
            batch_id: loro_protocol::protocol::BatchId([0; 8]),
        };
        let Ok(frame) = loro_protocol::encoding::encode(&message) else {
            return;
        };
        // `u64::MAX` is nobody: every subscriber of the room is told, because
        // none of them is the author of a Cloud download.
        let told =
            self.rooms
                .broadcast(&room, koru_store_api::rooms::ConnectionId(u64::MAX), frame);
        tracing::debug!(
            ?unit,
            told,
            "cloud session: told open views about a download"
        );
    }
}

fn lock(host: &Mutex<ControlHost>) -> std::sync::MutexGuard<'_, ControlHost> {
    host.lock().unwrap_or_else(|error| error.into_inner())
}

fn set_stage(
    host: &Mutex<ControlHost>,
    permit_id: uuid::Uuid,
    stage: CloudStage,
    error_code: Option<&str>,
) {
    lock(host).set_cloud_stage(permit_id, stage, error_code);
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// One admitted subscription's lifetime, including idle listening between
/// passes. Returns on a fence, refusal or lost stream; dropping the halves
/// closes this bridged dataset stream.
#[allow(clippy::too_many_arguments)]
async fn run_session(
    session: TransportSession,
    host: Arc<Mutex<ControlHost>>,
    store: StoreWorkerClient,
    rooms: Arc<koru_store_api::rooms::Rooms>,
    activity: Arc<Mutex<CloudActivity>>,
    deferrals: cloud_sync::DeferralLedger,
    blobs: BlobEngineSlot,
    budget: MemoryBudget,
) {
    let Some(principal) = session.principal.cloud().copied() else {
        session.reset(1);
        return;
    };
    let permit = match lock(&host).installed_cloud_permit(&principal) {
        Ok(permit) => permit,
        Err(error) => {
            tracing::warn!(
                code = error.code(),
                "cloud session: a stream arrived without an installed context; reset"
            );
            session.reset(1);
            return;
        }
    };
    let permit_id = permit.permit_id;
    let generation = principal.session_generation;
    let blob_workspace = match permit.dataset {
        CloudReplicaPrincipalV1::Workspace { workspace_id, .. } => {
            Some(WorkspaceId::from_uuid(workspace_id.as_uuid()))
        }
        // A Profile dataset carries no blobs of its own: the Personal
        // Workspaces beside it are datasets in their own right and get their
        // own pass.
        CloudReplicaPrincipalV1::Profile { .. } => None,
    };
    // **The dataset incarnation coverage is claimed under.** A Workspace that
    // was dropped and re-provisioned is a different dataset, and a mapping
    // under the old incarnation says nothing about the new one, so the
    // boundary check below compares this exact value.
    let blob_incarnation = permit
        .workspace_descriptor
        .as_ref()
        .map(|descriptor| descriptor.workspace_incarnation_id);
    let (plan, scope) = match plan_for(&permit) {
        Ok(value) => value,
        Err(code) => {
            set_stage(&host, permit_id, CloudStage::Unavailable, Some(code));
            session.reset(1);
            return;
        }
    };
    // HOST-CONTROL §3.1 step 3, at installation: the identity-only
    // bootstrap for a Workspace this Store does not hold yet. An existing
    // Workspace whose manifest is not the descriptor's is a refusal, never a
    // replacement.
    if let Err(code) = bootstrap(&store, &permit).await {
        tracing::warn!(code, "cloud session: workspace bootstrap refused");
        set_stage(&host, permit_id, CloudStage::Unavailable, Some(code));
        session.reset(1);
        return;
    }
    set_stage(&host, permit_id, CloudStage::Reconciling, None);

    let (reader, writer) = session_io::split(session);
    // The gateway never saw the worker's `SessionAccepted` message: the Agent
    // completed the handshake and installed its facts as the principal. The
    // carrier reads only the principal, the generation and the expiry from
    // this value; the session id field is the worker's and is not carried
    // across the bridge, so it is left zero here rather than invented.
    let accepted = PreparedSession {
        accepted: HandshakeV1::SessionAccepted {
            version: Version1,
            session_id: Bytes([0; 32]),
            generation,
            expires_at_seconds: principal.expires_at_seconds,
        },
        principal: permit.dataset,
        expires_at_seconds: principal.expires_at_seconds,
        generation,
    };
    let Some(capacity) = NonZeroUsize::new(SESSION_QUEUE) else {
        return;
    };
    let dataset_session = cloud_sync::Session {
        accepted,
        scope,
        // TECHNICAL §4.4: the desktop is unmetered, so every payload up to
        // the carrier's maximum is accepted without asking. The network
        // policy operation exists on control 7; wiring it to this ceiling is
        // owed with the consent surface.
        automatic_limit: transfer::MAX_PAYLOAD,
        capacity,
        budget,
        observed: Instant::now(),
        wall_millis: now_millis(),
    };
    let authorize_host = Arc::clone(&host);
    let authorize: cloud_sync::Authorize = Arc::new(move |_unit| {
        if lock(&authorize_host).cloud_permit_live(permit_id, generation) {
            Ok(())
        } else {
            Err(Refusal::ReplicaFenced)
        }
    });
    // What the pass actually moved, not just how many units it visited. A pass
    // that settles everything and carries nothing is the ordinary case once two
    // devices agree — and is indistinguishable from a broken one without this.
    let uploaded = Arc::new(AtomicUsize::new(0));
    let downloaded = Arc::new(AtomicUsize::new(0));
    let counted = (Arc::clone(&uploaded), Arc::clone(&downloaded));
    // **The same facts the log line carries, kept where a query can answer
    // them.** Memory only and coalesced per document; see
    // `koru_store_api::cloud_activity` for why nothing about a pass is written
    // down. The documents this pass started are remembered locally too, so a
    // refusal can say which documents it left waiting — that list is bounded
    // by the units one pass runs.
    let dataset = dataset_key(&plan);

    let started_documents: Arc<Mutex<Vec<(uuid::Uuid, uuid::Uuid)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let progress_activity = Arc::clone(&activity);
    let progress_started = Arc::clone(&started_documents);
    let downloads: Arc<dyn cloud_sync::Downloads> = Arc::new(CloudDownloads {
        rooms,
        manifest: Some((
            scope.eligibility_workspace.as_uuid(),
            scope.manifest.as_uuid(),
        )),
    });

    let progress_host = Arc::clone(&host);
    let (completed, mut completed_rx) =
        tokio::sync::watch::channel::<Option<cloud_sync::Outcome>>(None);
    // Coalesced file work runs beside document reception, with no task backlog.
    let blob_work = async {
        while completed_rx.changed().await.is_ok() {
            let Some(outcome) = completed_rx.borrow_and_update().clone() else {
                continue;
            };
            // **The documents are current; now the pictures they name.**
            // This is §10's Cloud-mapping trigger and its startup recovery in
            // one place: a pass has just established what this Workspace
            // holds, which is exactly when the referenced set is worth
            // walking. It runs after the stage is set, so a blob transfer
            // never delays what the document pass has to report.
            if let (Some(workspace), Some(blobs)) = (blob_workspace, blobs.get()) {
                // **The documents this pass carried, before the pictures
                // move.** Installed rather than computed: the reconciliation
                // below finishes by computing the claim, and doing it here as
                // well would be two samples and two rounds of control requests
                // for one pass.
                if let Some(incarnation) = blob_incarnation {
                    let live = Arc::clone(&host);
                    let authorization: koru_blob_sync::LiveAuthorization = Arc::new(move |_| {
                        // The same liveness question every Store job of the
                        // pass asked, asked once more at the boundary: a
                        // revocation that landed while coverage was being
                        // computed must supersede it rather than be published
                        // through it.
                        lock(&live)
                            .cloud_permit_live(permit_id, generation)
                            .then_some(incarnation)
                    });
                    blobs.expect_documents(
                        workspace,
                        incarnation,
                        covered_documents(&outcome),
                        authorization,
                    );
                }
                blobs.recover(workspace).await;
                // **And then the whole truth**, which `reconcile` computes as
                // the last thing it does: the documents this pass carried, the
                // pictures those exact states name, and whether Cloud holds
                // both. Before it, Cloud status said "the documents synced"
                // and a person had to be told separately that this did not
                // include their pictures.
                blobs
                    .reconcile(workspace, koru_blob_sync::Trigger::MappingChanged)
                    .await;
            }
        }
        std::future::pending::<()>().await;
    };
    // The extracted driver, over the one stream this session bridged: it ends
    // when that stream does, with the session's result.
    let driver = cloud_sync::driver::Driver::spawn(
        cloud_sync::driver::OnceSource::new(reader, writer, dataset_session),
        cloud_sync::driver::Parts {
            plan,
            stage: cloud_sync::Stage::Full,
            jobs: store,
            authorize,
            downloads,
            deferrals,
            permission: |_start: &transfer::Start| Decision::Accept,
            uploads: None,
            progress: move |progress| {
                let step = match progress {
                    Progress::PassStarted => {
                        counted.0.store(0, Ordering::Relaxed);
                        counted.1.store(0, Ordering::Relaxed);
                        progress_started
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clear();
                        record(&progress_activity, |a| {
                            a.pass_started(dataset, now_millis())
                        });
                        set_stage(&progress_host, permit_id, CloudStage::Reconciling, None);
                        None
                    }
                    Progress::PassFinished(outcome) => {
                        report_pass(
                            &progress_host,
                            &progress_activity,
                            dataset,
                            permit_id,
                            &outcome,
                            counted.0.load(Ordering::Relaxed),
                            counted.1.load(Ordering::Relaxed),
                        );
                        completed.send_replace(Some(*outcome));
                        None
                    }
                    Progress::Documents { synced, required } => {
                        record(&progress_activity, |a| {
                            a.document_progress(dataset, synced, required, now_millis())
                        });
                        None
                    }
                    Progress::Started(unit) => {
                        tracing::debug!(?unit, "cloud session: unit started");
                        if let Some(key) = unit_document(unit)
                            && let Ok(mut started) = progress_started.lock()
                            && !started.contains(&key)
                        {
                            started.push(key);
                        }
                        Some((unit, UnitStep::Started))
                    }
                    Progress::Uploaded(unit) => {
                        counted.0.fetch_add(1, Ordering::Relaxed);
                        tracing::debug!(?unit, "cloud session: unit uploaded");
                        Some((unit, UnitStep::Uploaded))
                    }
                    Progress::Downloaded(unit) => {
                        counted.1.fetch_add(1, Ordering::Relaxed);
                        tracing::debug!(?unit, "cloud session: unit downloaded");
                        Some((unit, UnitStep::Downloaded))
                    }
                    Progress::Deferred { unit, upload } => {
                        tracing::debug!(?unit, upload, "cloud session: unit deferred");
                        Some((unit, UnitStep::Deferred))
                    }
                    // A Store refusal is recorded as the deferral it now is. The
                    // Store's own code is already at `warn` from the pass; this
                    // keeps the step where a query can answer it.
                    Progress::Refused { unit, code } => {
                        tracing::debug!(
                            ?unit,
                            code = code.as_str(),
                            "cloud session: unit refused by a Store job"
                        );
                        Some((unit, UnitStep::Deferred))
                    }
                    // Not a step: the unit did not move, and saying so is the
                    // point. It is handled beside the outcome below, where the
                    // dataset's stage is set.
                    Progress::Unavailable { unit, code } => {
                        tracing::warn!(
                            ?unit,
                            code = code.as_str(),
                            "cloud session: unit has stopped syncing"
                        );
                        None
                    }
                    Progress::Settled { unit, .. } => {
                        tracing::debug!(?unit, "cloud session: unit settled");
                        Some((unit, UnitStep::Settled))
                    }
                    Progress::RemoteHint => {
                        tracing::debug!("cloud session: remote hint");
                        None
                    }
                };
                if let Some((unit, step)) = step {
                    let at = now_millis();
                    record(&progress_activity, |a| {
                        a.unit(dataset, unit_document(unit), step, at)
                    });
                }
            },
        },
    );
    let outcome = tokio::select! {
        result = driver.join() => result,
        () = blob_work => unreachable!("file follower stays alive with the stream"),
    };

    if let Err(error) = outcome {
        let code = error.code();
        tracing::warn!(code, "cloud session: pass ended with a refusal");
        let touched = started_documents
            .lock()
            .map(|started| started.clone())
            .unwrap_or_default();
        let at = now_millis();
        record(&activity, |a| a.pass_refused(dataset, code, at, &touched));
        set_stage(&host, permit_id, CloudStage::Unavailable, Some(code));
    }
}

#[allow(clippy::too_many_arguments)]
fn report_pass(
    host: &Mutex<ControlHost>,
    activity: &Mutex<CloudActivity>,
    dataset: uuid::Uuid,
    permit_id: uuid::Uuid,
    outcome: &cloud_sync::Outcome,
    uploaded: usize,
    downloaded: usize,
) {
    let changed = activity
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .dataset(dataset)
        .is_some_and(|pass| pass.documents_changed);
    let remote_hint = outcome.remote_hint || changed;
    // A remote hint means the worker has more to exchange: the pass
    // settled every unit it ran, and the Agent reschedules. That is
    // reported as `checking_coverage` rather than `settled`, which is
    // the one stage on the list that says "not finished, not failed".
    //
    // Unless a unit has run out of deferrals. Then the pass really did
    // finish — its counts below are true — and the dataset is still in
    // trouble, because one of its units has not moved in three passes
    // and will not start on its own. Reporting `settled` here is the
    // failure this whole change exists to avoid: a pass that looks
    // like success forever while a document silently never syncs.
    let stage = if !outcome.unavailable.is_empty() {
        CloudStage::Unavailable
    } else if remote_hint {
        CloudStage::CheckingCoverage
    } else {
        CloudStage::Settled
    };
    tracing::info!(
        %dataset,
        %permit_id,
        settled = outcome.settled.len(),
        uploaded = uploaded,
        downloaded = downloaded,
        deferred = outcome.deferred.len(),
        refused = outcome.refused.len(),
        unavailable = outcome.unavailable.len(),
        remote_hint = outcome.remote_hint,
        "cloud session: pass finished"
    );
    let counts = PassCounts {
        settled: outcome.settled.len().try_into().unwrap_or(u32::MAX),
        uploaded: uploaded.try_into().unwrap_or(u32::MAX),
        downloaded: downloaded.try_into().unwrap_or(u32::MAX),
        deferred: outcome.deferred.len().try_into().unwrap_or(u32::MAX),
        remote_hint,
    };
    let at = now_millis();
    record(&activity, |a| a.pass_finished(dataset, counts, at));
    // After `pass_finished`, which clears the previous refusal: a unit
    // that is not syncing is this pass's fact, not the last one's.
    for (unit, code) in &outcome.unavailable {
        let (unit, code) = (*unit, code.as_str());
        record(&activity, |a| {
            a.unit_unavailable(dataset, unit_document(unit), code, at)
        });
    }
    let error_code = (!outcome.unavailable.is_empty()).then_some(UNIT_UNAVAILABLE);
    set_stage(&host, permit_id, stage, error_code);
}

/// Write one fact into the pass record.
///
/// A poisoned lock is recovered from rather than propagated, the same way
/// [`lock`] treats the control host's: this is a diagnostic, and a panic
/// somewhere else must not be able to take a Cloud pass with it.
fn record(activity: &Mutex<CloudActivity>, write: impl FnOnce(&mut CloudActivity)) {
    write(&mut activity.lock().unwrap_or_else(|error| error.into_inner()));
}

/// Which units this permit may carry, and the carrier scope for them. The
/// plan is bounded by the permit, never by what the remote advertises: a
/// Profile permit that does not name Private does not sync Private. Shared
/// with the Android host, which runs the same pass in process.
pub fn plan_for(permit: &CloudPermitV1) -> Result<(Plan, Scope), &'static str> {
    let profile = ProfileId::from_uuid(permit.profile_id.as_uuid());
    let eligibility_workspace = WorkspaceId::from_uuid(permit.authority_workspace_id.as_uuid());
    match permit.dataset {
        CloudReplicaPrincipalV1::Workspace { workspace_id, .. } => {
            let descriptor = permit
                .workspace_descriptor
                .as_ref()
                .ok_or("cloud_permit_descriptor_required")?;
            if descriptor.workspace_id != workspace_id {
                return Err("cloud_permit_descriptor_mismatch");
            }
            let manifest = DocumentId::from_uuid(descriptor.manifest_document_id.as_uuid());
            Ok((
                Plan::Workspace {
                    workspace_id: WorkspaceId::from_uuid(workspace_id.as_uuid()),
                },
                Scope {
                    principal: permit.dataset,
                    eligibility_workspace,
                    profile: permit.profile_id,
                    manifest,
                },
            ))
        }
        CloudReplicaPrincipalV1::Profile { profile_id, .. } => {
            if profile_id != permit.profile_id {
                return Err("cloud_permit_profile_mismatch");
            }
            let mut private = false;
            let mut personal_workspaces = Vec::new();
            for unit in &permit.allowed_profile_units {
                match unit {
                    ProfileUnitRef::Identity { profile: owner } if *owner == profile => {}
                    ProfileUnitRef::Private { profile: owner } if *owner == profile => {
                        private = true;
                    }
                    ProfileUnitRef::Personal { owner, workspace }
                        if *owner == profile && *workspace == eligibility_workspace =>
                    {
                        personal_workspaces.push(*workspace);
                    }
                    _ => return Err("cloud_permit_profile_units"),
                }
            }
            Ok((
                Plan::Profile {
                    profile_id: profile,
                    private,
                    personal_workspaces,
                },
                Scope {
                    principal: permit.dataset,
                    eligibility_workspace,
                    profile: permit.profile_id,
                    // A Profile dataset carries no manifest; the scope's
                    // manifest field is consulted for Document units only.
                    manifest: DocumentId::from_uuid(uuid::Uuid::nil()),
                },
            ))
        }
    }
}

/// HOST-CONTROL §3.1 step 3 for a Workspace dataset: provision the replica
/// with the Profile read back from the Store and the descriptor's manifest
/// id. The Profile's presence is the selection path's job (step 1) and is not
/// invented here; a missing Profile is a refusal the coordinator retries
/// after selection has landed it.
async fn bootstrap(store: &StoreWorkerClient, permit: &CloudPermitV1) -> Result<(), &'static str> {
    let Some(descriptor) = permit.workspace_descriptor.as_ref().copied() else {
        return Ok(());
    };
    ensure_workspace(
        store,
        ProfileId::from_uuid(permit.profile_id.as_uuid()),
        WorkspaceId::from_uuid(descriptor.workspace_id.as_uuid()),
        DocumentId::from_uuid(descriptor.manifest_document_id.as_uuid()),
    )
    .await
}

/// The identity-only Workspace bootstrap itself, on whichever host owns the
/// Store worker: present and mapped to this manifest is fine, absent is
/// provisioned with the stored Profile, and present under another manifest
/// is refused. Names and appearance are left for the manifest's first
/// import.
pub async fn ensure_workspace(
    store: &StoreWorkerClient,
    profile: ProfileId,
    workspace: WorkspaceId,
    manifest: DocumentId,
) -> Result<(), &'static str> {
    store
        .with_store_async(move |store| -> Result<(), &'static str> {
            match store.workspace_including_deleted(workspace) {
                Ok(_) => {
                    let existing = store
                        .replica_manifest_id_including_deleted(workspace)
                        .map_err(|_| "manifest_unavailable")?;
                    if existing != manifest {
                        // A same-incarnation mapping mismatch is refusal,
                        // not replacement (§3.1).
                        return Err("manifest_mismatch");
                    }
                    Ok(())
                }
                Err(error) if error.code == koru_store_core::errors::Code::WorkspaceNotFound => {
                    let profile = store.profile(profile).map_err(|_| "profile_absent")?;
                    let now = Timestamp::now();
                    // Bootstrap fields only: the manifest CRDT overrides the
                    // name and appearance on its first import.
                    let workspace = koru_store_core::model::Workspace {
                        id: workspace,
                        owner: profile.id,
                        name: String::new(),
                        created_at: now,
                        last_opened_at: None,
                        icon: None,
                        color: None,
                        pinned: false,
                        archived: false,
                        deleted_at: None,
                        position: 0,
                    };
                    store
                        .provision_replica(&profile, &workspace, manifest, now)
                        .map_err(|_| "workspace_provisioning_failed")
                }
                Err(_) => Err("store_unavailable"),
            }
        })
        .await
        .map_err(|_| "store_worker_unavailable")?
}

#[cfg(test)]
mod tests;
