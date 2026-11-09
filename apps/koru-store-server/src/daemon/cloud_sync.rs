// SPDX-License-Identifier: MPL-2.0

//! Catalog traversal around the retained one-unit Cloud reconciliation.
//!
//! The gateway (and the Android host, in process) owns the Reconciler and its
//! eager Store jobs. This module turns one admitted dataset session into an
//! ordered pass over that dataset's units, using the existing Store ports and
//! nothing else: no progress database, no fake peer, no second sync engine. A Workspace pass
//! reconciles the manifest first, then compares durable document digests in
//! bounded batches using the peer inventory's shared leaf definition. Only
//! differing documents enter Loro reconciliation. An unchanged manifest is NOT
//! evidence about document bodies. An absent document uses the existing bounded
//! bootstrap path before its exchange. A Profile pass reconciles Identity,
//! Private and the granted Personal units.
//!
//! Settling every unit is not `DocumentCoverageV1`; the caller keeps the
//! outcome in memory and reschedules on a remote hint or a local change.
//!
//! **One unit a Store job will not serve does not end the pass.** It is
//! carried into the deferral this module already has, and the units beside it
//! are reconciled as usual — see [`deferrable`] for where that line is drawn
//! and [`Deferrals`] for the bound that stops a deferral from becoming a
//! silent permanent one.
use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use koru_device::cloud::CloudId;
use koru_network_iroh::cloud::client_io::{
    self,
    reconcile::{Answer, Event, Reconciler},
};
use koru_replication::cloud::{
    PreparedSession, Refusal,
    client_store::{self, Failure},
    transfer::{Decision, Error, MemoryBudget, Scope, Start, Unit},
};
use koru_store_core::{
    DocumentId, ProfileId, Timestamp, WorkspaceId,
    errors::{Class, Code},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::Instant,
};

use super::StoreJobs;
mod comparison;
pub mod driver;
mod live;
mod rooms;
pub use live::subscribe_dataset;

/// Which units a Workspace session carries.
///
/// The stage is a fact of the session's credential, not of the pass: a
/// connection's replica is admitted at the manifest stage first and at the
/// full stage once a call names its documents (TECHNICAL §5.3), and the worker
/// refuses a unit its credential's stage does not admit. A device is always
/// `Full`. A Profile plan carries its own units at either stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The Workspace manifest alone: what the Workspace is called and which
    /// documents it holds, and none of their bodies.
    Manifest,
    /// The manifest, then every document: digest comparison, then exchange, in
    /// the bounded order a device uses.
    Full,
}

/// One admitted dataset session's facts, as the host established them at
/// installation. `accepted` carries the handshake result the transport owner
/// completed; nothing here is a data-plane permit by itself.
pub struct Session {
    pub accepted: PreparedSession,
    pub scope: Scope,
    /// Receiver-set automatic ceiling (TECHNICAL §4.4): payloads at or below
    /// it are accepted without a person; larger ones ask `permission`.
    pub automatic_limit: usize,
    pub capacity: NonZeroUsize,
    pub budget: MemoryBudget,
    pub observed: Instant,
    pub wall_millis: i64,
}

/// Which units this dataset session may carry. The list is bounded by the
/// installed permit's allowed units, never by what the remote advertises.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    Workspace {
        workspace_id: WorkspaceId,
    },
    Profile {
        profile_id: ProfileId,
        private: bool,
        personal_workspaces: Vec<WorkspaceId>,
    },
}

#[derive(Clone, Debug)]
pub enum Progress {
    PassStarted,
    PassFinished(Box<Outcome>),
    Documents {
        synced: u32,
        required: Option<u32>,
    },
    Started(Unit),
    Uploaded(Unit),
    Downloaded(Unit),
    Deferred {
        unit: Unit,
        upload: bool,
    },
    /// A Store job would not serve this unit, and the pass carried on without
    /// it. Its own variant rather than a `Deferred` with a reason attached,
    /// because the two are answered differently: a deferral is the remote
    /// holding a direction and clears itself, a refusal is local and is
    /// counted against [`MAX_CONSECUTIVE_REFUSALS`].
    Refused {
        unit: Unit,
        code: Code,
    },
    /// This unit has now been refused on [`MAX_CONSECUTIVE_REFUSALS`] passes
    /// in a row. It is not syncing and will not start on its own; the host
    /// must say so where a person can see it.
    Unavailable {
        unit: Unit,
        code: Code,
    },
    Settled {
        unit: Unit,
        upload_deferred: bool,
        download_deferred: bool,
    },
    /// Authenticated, content-free: the caller should schedule another pass.
    RemoteHint,
    /// **Cloud holds at least `version` of this unit**, the encoded version
    /// vector the pass last read for it: the unit settled with its upload
    /// acknowledged, or with nothing to upload, and no local change since.
    /// It is what a host that waits for an edit's acknowledgement reads
    /// (Cloud MCP TECHNICAL §5.5): an edit is saved once a confirmed version
    /// includes the frontier its local write produced.
    Confirmed {
        unit: Unit,
        version: Vec<u8>,
    },
    /// The session ended on this carrier error, and the driver opens the next
    /// one. A host with a submission in flight reads it: a refusal defined to
    /// leave the unit unmoved (`sync_unit_exists`) proves the submission had
    /// no effect, any other end leaves it unknown. Emitted by the
    /// [`driver`] only; a single pass returns its error instead.
    SessionEnded(Error),
}

/// One document unit's half of the coverage sample.
///
/// **The state the pass actually read**, and whether it carried that exact
/// state to Cloud. Coverage is computed against this rather than against a
/// second read: a document check over state A and a reference walk over state
/// B is a guarantee about a Workspace that never existed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentPass {
    pub unit: Unit,
    /// The encoded version vector this pass read for the unit. Empty when the
    /// read never happened — a refusal before it, or a pass that ended first.
    pub state: Vec<u8>,
    /// **The pass carried this exact state to Cloud.** It settled, no Store
    /// job refused it, and no upload was held: Cloud is now at or ahead of
    /// `state`. Anything short of all three is `false`, because each of them
    /// on its own is a document that did not arrive.
    pub carried: bool,
}

/// The pass's in-memory result. Not coverage and not persisted.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub documents_synced: u32,
    pub settled: Vec<Unit>,
    pub deferred: Vec<Unit>,
    /// Units a Store job refused on this pass, with the Store's own code. A
    /// refused unit is also in `deferred`: the pass did not carry it, and
    /// `deferred` is the set a caller counts. This list adds the reason.
    pub refused: Vec<(Unit, Code)>,
    /// Units that have now been refused on enough consecutive passes to be
    /// called unavailable. **A caller that ignores this reports a pass that
    /// finished while a unit silently never synced**, which is worse than the
    /// loud stop this replaced.
    pub unavailable: Vec<(Unit, Code)>,
    pub remote_hint: bool,
    /// **What coverage is computed against**: one entry per document unit this
    /// pass settled, with the exact state it read and whether it carried it. A
    /// Profile unit contributes nothing — Identity, Private and Personal are
    /// facts about the dataset and about no document at all.
    pub documents: Vec<DocumentPass>,
}

/// How many consecutive passes may refuse one unit before it is called
/// unavailable out loud.
///
/// Three, because of what a pass is: the caller reschedules on a remote hint
/// or a local change, so three of them is seconds to minutes — inside the
/// sitting the person is in, which is the requirement. One would call a single
/// unlucky pass a permanent fault, and the transients are real: a version read
/// that lost a race with a concurrent commit, an export taken while a
/// checkpoint was running. Ten would let a structural conflict — two Profile
/// identity lineages, a document written by a newer schema — hide for an
/// afternoon behind passes that all reported themselves finished.
pub const MAX_CONSECUTIVE_REFUSALS: u32 = 3;

/// How many refusing units the ledger tracks at once. Only units that are
/// currently refusing hold an entry, so this is not a document count: a
/// healthy dataset of fifty thousand documents keeps nothing here at all.
pub const MAX_TRACKED_UNITS: usize = 256;

/// Consecutive Store refusals per unit, across passes.
///
/// **It has to outlive one pass or it cannot bound anything.** A pass visits
/// each unit once, so "kept deferring" is only observable from one pass to the
/// next; the host owns this and hands the same ledger to every pass it runs.
/// Memory only, for the same reason `cloud_activity` is: a count that survived
/// a restart would start to look like a durable verdict about a document.
#[derive(Debug, Default)]
pub struct Deferrals {
    units: HashMap<Unit, u32>,
}

impl Deferrals {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a refusal and answer with how many in a row this unit now has.
    ///
    /// A full ledger answers [`MAX_CONSECUTIVE_REFUSALS`] straight away rather
    /// than quietly dropping the unit on the floor: [`MAX_TRACKED_UNITS`]
    /// units refusing at once *is* the loud condition, and the one thing this
    /// must never do is go quiet.
    pub fn refused(&mut self, unit: Unit) -> u32 {
        if !self.units.contains_key(&unit) && self.units.len() >= MAX_TRACKED_UNITS {
            return MAX_CONSECUTIVE_REFUSALS;
        }
        let count = self.units.entry(unit).or_insert(0);
        *count = count.saturating_add(1);
        *count
    }

    /// The unit was served. Consecutive means consecutive.
    pub fn cleared(&mut self, unit: Unit) {
        self.units.remove(&unit);
    }

    pub fn tracked(&self) -> usize {
        self.units.len()
    }
}

/// The host's ledger, shared by every pass it runs over every dataset. Units
/// carry their own dataset ids, so one ledger answers for all of them.
pub type DeferralLedger = Arc<Mutex<Deferrals>>;

/// Per unit, the encoded version vector this pass read for it.
///
/// One pass's, not the host's: a state read on the previous pass says nothing
/// about what this one carried, and a coverage claim built on it would be
/// about a Workspace two passes ago.
type Versions = Arc<Mutex<HashMap<Unit, Vec<u8>>>>;

/// Whether a Store refusal is about **this unit** or about the session.
///
/// The distinction is already in [`Class`], which is why nothing new is
/// invented here:
///
/// - `NotFound`, `Invalid` and `Conflict` are statements about the thing being
///   carried — this document does not exist here, this record will not decode,
///   these two identity lineages disagree. The unit beside it is untouched, so
///   the pass carries on and the unit waits. This is the case that froze whole
///   datasets.
/// - `Unavailable`, `Unauthenticated`, `Forbidden` and `Internal` are
///   statements about the Store or the session — the store is gone, the disk
///   failed, this context may not read at all. Deferring those would walk
///   every remaining unit into the same wall and then report a pass that
///   "finished", so the pass ends, loudly, as it does today.
fn deferrable(code: Code) -> bool {
    match code.class() {
        Class::NotFound | Class::Invalid | Class::Conflict => true,
        Class::Unavailable | Class::Unauthenticated | Class::Forbidden | Class::Internal => false,
    }
}

/// Current-authorization check evaluated both at unit start and inside every
/// Store job, so a fenced or replaced context refuses work admitted after it.
pub type Authorize = Arc<dyn Fn(Unit) -> Result<(), Refusal> + Send + Sync>;

fn cloud(uuid: uuid::Uuid) -> CloudId {
    CloudId::from_bytes(*uuid.as_bytes())
}

/// A Store refusal has no room on the carrier: the wire carries the transfer
/// codes and nothing else, so the `Code` dies here. Say it before it does —
/// this is the only place the reason exists, on either end. Without it a pass
/// a local Store job refused is indistinguishable, from the worker's side, from
/// a device that finished its pass and went away: both are a closed stream.
///
/// This is the **session** half: a refusal [`deferrable`] says is about the
/// Store rather than the unit, and the two transport failures around it. It
/// ends the pass, which is right — the next unit would meet the same Store.
fn map_failure(error: Failure) -> Error {
    match error {
        Failure::Transfer(error) => error,
        Failure::Store(code) => {
            tracing::warn!(?code, "cloud session: a Store job refused the pass");
            Error::Unavailable
        }
    }
}

/// The **unit** half: a port's result, turned into the answer the Reconciler
/// reads. A refusal `deferrable` calls local to the unit becomes
/// [`Answer::Refused`], which the pass carries past; anything else keeps the
/// old ending path through [`map_failure`].
fn store_answer<T>(result: Result<T, Failure>) -> Result<Answer<T>, Error> {
    match result {
        Ok(value) => Ok(Answer::Ready(value)),
        Err(Failure::Store(code)) if deferrable(code) => Ok(Answer::Refused(code)),
        Err(error) => Err(map_failure(error)),
    }
}

async fn store_result<T: Send + 'static>(
    reply: Result<super::store_jobs::StoreReply<T>, super::WorkerError>,
) -> Result<T, Error> {
    reply
        .map_err(|error| {
            tracing::warn!(?error, "cloud session: the Store worker refused a job");
            Error::Unavailable
        })?
        .await
        .map_err(|error| {
            tracing::warn!(?error, "cloud session: a Store job did not answer");
            Error::Unavailable
        })
}

/// What a host does with a unit the Cloud just delivered.
///
/// A download is durable in the Store before anyone hears about it, but
/// durable is not the same as *shown*: whatever is already displaying the
/// document holds it in memory and learns of new bytes only when something
/// tells it. The peer path has always told it; this is the same seam for the
/// Cloud path, and without it a Cloud-delivered edit sits correctly on disk
/// behind a view that never changes (D242).
///
/// `watching` is asked **before** the payload is moved into the Store, so a
/// host with nothing displaying the unit costs no copy at all.
pub trait Downloads: Send + Sync {
    /// Current visible document rooms. The caller filters against its Workspace
    /// inventory before subscribing. No document body or database read here.
    fn interests(&self) -> Vec<DocumentId> {
        Vec::new()
    }
    /// Whether anything on this device is showing the unit right now.
    fn watching(&self, unit: Unit) -> bool;
    /// Exactly the bytes that were committed, after durability.
    fn arrived(&self, unit: Unit, bytes: Vec<u8>);
    /// Platforms that re-read views can prioritize without copying downloads.
    fn priority(&self, unit: Unit) -> bool {
        self.watching(unit)
    }
}

/// A host that shows nothing, for a run with no local viewer.
pub struct NoDownloads;

impl Downloads for NoDownloads {
    fn watching(&self, _unit: Unit) -> bool {
        false
    }
    fn arrived(&self, _unit: Unit, _bytes: Vec<u8>) {}
}

/// Run one bounded pass over the plan's units on an already admitted stream.
/// The caller encloses the whole call in the transport owner's fence; on
/// Android the same call runs on the host's own Tokio runtime.
///
/// `deferrals` is the host's, not the pass's: it is how a unit that keeps
/// being refused stops being a private fact of one pass and becomes something
/// the host says out loud. See [`Deferrals`].
#[allow(clippy::too_many_arguments)]
pub async fn reconcile_dataset<R, W, J, P, F>(
    reader: R,
    writer: W,
    session: Session,
    plan: Plan,
    worker: J,
    authorize: Authorize,
    downloads: Arc<dyn Downloads>,
    deferrals: DeferralLedger,
    permission: P,
    progress: F,
) -> Result<Outcome, Error>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    J: StoreJobs,
    P: FnMut(&Start) -> Decision + Send + 'static,
    F: FnMut(Progress),
{
    run_dataset(
        reader,
        writer,
        session,
        plan,
        worker,
        authorize,
        downloads,
        deferrals,
        permission,
        progress,
        None,
        Hooks::device(),
    )
    .await
    .map(Ended::into_outcome)
}

/// What the driver adds to a session: the stage it was admitted at, and the
/// channel its caller asks through. A device's session has neither — it is
/// `Full` and nobody asks it anything.
pub(super) struct Hooks<'h> {
    pub stage: Stage,
    pub commands: Option<&'h mut tokio::sync::mpsc::Receiver<driver::Command>>,
    /// Which units may be uploaded at the full stage; `None` is every unit.
    pub uploads: Option<Uploads>,
    pub interest: Option<std::sync::Arc<std::sync::RwLock<std::collections::HashSet<DocumentId>>>>,
}

impl Hooks<'_> {
    fn device() -> Self {
        Self {
            stage: Stage::Full,
            commands: None,
            uploads: None,
            interest: None,
        }
    }
}

/// Which units a session may upload. A device uploads every unit it holds; a
/// connection's replica never uploads Identity, and nothing at all under read
/// access, and says so here so not even a refused upload is attempted.
pub type Uploads = Arc<dyn Fn(Unit) -> bool + Send + Sync>;

/// The export port's answer: a Store job's, or nothing to send.
enum Either<F> {
    Export(F),
    Held,
}

impl<F, T> std::future::Future for Either<std::pin::Pin<Box<F>>>
where
    F: std::future::Future<Output = Result<Answer<Option<T>>, Error>> + ?Sized,
{
    type Output = Result<Answer<Option<T>>, Error>;
    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match self.get_mut() {
            Either::Export(future) => future.as_mut().poll(cx),
            Either::Held => std::task::Poll::Ready(Ok(Answer::Ready(None))),
        }
    }
}

/// How one session ended without an error.
pub(super) enum Ended {
    /// A single pass, with no live subscription: what `reconcile_dataset`
    /// answers.
    Finished(Outcome),
    /// The driver was asked for another stage. The session's credential
    /// admits only its own, so the session ends and the driver opens the next
    /// one at the new stage.
    Advance(Stage, Outcome),
}

impl Ended {
    fn into_outcome(self) -> Outcome {
        match self {
            Self::Finished(outcome) | Self::Advance(_, outcome) => outcome,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_dataset<R, W, J, P, F>(
    reader: R,
    writer: W,
    session: Session,
    plan: Plan,
    worker: J,
    authorize: Authorize,
    downloads: Arc<dyn Downloads>,
    deferrals: DeferralLedger,
    permission: P,
    mut progress: F,
    mut live: Option<koru_store_api::cloud_changes::Subscription>,
    mut hooks: Hooks<'_>,
) -> Result<Ended, Error>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    J: StoreJobs,
    P: FnMut(&Start) -> Decision + Send + 'static,
    F: FnMut(Progress),
{
    let scope = session.scope;
    let priority = Arc::clone(&downloads);
    let receive_worker = worker.clone();
    let receive_authorize = Arc::clone(&authorize);
    let client_authorize = Arc::clone(&authorize);
    let client = client_io::Client::new(
        reader,
        writer,
        client_io::Config {
            accepted: session.accepted,
            scope,
            observed: session.observed,
            wall_millis: session.wall_millis,
            automatic_limit: session.automatic_limit,
            capacity: session.capacity,
            budget: session.budget.clone(),
        },
        move |complete| {
            let authorize = Arc::clone(&receive_authorize);
            let unit = complete.unit;
            // Copied only when something local is showing this unit, and only
            // before the payload is moved into the Store's commit window.
            let showing = downloads
                .watching(unit)
                .then(|| complete.bytes.clone())
                .map(|bytes| (Arc::clone(&downloads), bytes));
            let reply = receive_worker
                .submit_cloud(scope, complete, Timestamp::now(), move |_, _, unit| {
                    authorize(unit)
                })
                .map_err(|_| Error::Unavailable)?;
            Ok(async move {
                // **The one Store port that still ends the pass on a refusal,
                // whatever the code says.** The other two are read-only and
                // nothing is owed to the peer when they refuse. This one runs
                // after the carrier handed the incoming transfer off, and the
                // peer is owed an acknowledgement for it. The only
                // acknowledgement this client can encode is
                // `Scope::committed_ack`, which says the bytes are durable;
                // there is no encoder for a refused one, and adding one is a
                // producer mapping in the frozen `cloud-sync-admission` and
                // `document-sync-denials-v1` contracts rather than a change
                // here. Ending the pass at least leaves the transfer
                // unacknowledged rather than falsely acknowledged. PLAN19
                // records what it would take to defer this one too.
                let receipt = store_result(Ok(reply)).await?.map_err(map_failure)?;
                // After durability, never before: a view woken by bytes a
                // power cut could still erase would be showing something that
                // never happened.
                if let Some((downloads, bytes)) = showing {
                    downloads.arrived(unit, bytes);
                }
                Ok(receipt)
            })
        },
        move |unit| client_authorize(unit),
        permission,
    )?;
    let read_worker = worker.clone();
    let read_authorize = Arc::clone(&authorize);
    // **The coverage sample, captured where the read happens.** The version
    // the reconciler asks for *is* the per-document state the document check
    // uses; remembering it here is what lets coverage be about that same
    // sample instead of a second, later one.
    let versions: Versions = Arc::new(Mutex::new(HashMap::new()));
    let read_versions = Arc::clone(&versions);
    let export_worker = worker.clone();
    let export_authorize = Arc::clone(&authorize);
    // At the manifest stage nothing is uploaded: the stage admits downloads
    // only (TECHNICAL §5.3). At the full stage the driver's caller decides.
    let export_gate: Option<Uploads> = match hooks.stage {
        Stage::Manifest => Some(Arc::new(|_| false)),
        Stage::Full => hooks.uploads.clone(),
    };
    let mut sync = Reconciler::new(
        client,
        move |unit| {
            let authorize = Arc::clone(&read_authorize);
            let reply = read_worker
                .submit_store(move |store| {
                    client_store::version(
                        store,
                        scope,
                        unit,
                        Timestamp::now(),
                        move |_, _, unit| authorize(unit),
                    )
                })
                .map_err(|_| Error::Unavailable)?;
            let versions = Arc::clone(&read_versions);
            Ok(async move {
                let answer = store_answer(store_result(Ok(reply)).await?)?;
                if let Answer::Ready(version) = &answer {
                    versions
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .insert(unit, version.encode());
                }
                Ok(answer)
            })
        },
        move |request| {
            // A unit this session may not upload answers that there is nothing
            // to send, before any Store work: the request — and the
            // reservation it holds — is dropped here.
            if export_gate.as_ref().is_some_and(|may| !may(request.unit())) {
                return Ok(Either::Held);
            }
            let authorize = Arc::clone(&export_authorize);
            let reply = export_worker
                .submit_cloud_export(request, Timestamp::now(), move |_, _, unit| authorize(unit))
                .map_err(|_| Error::Unavailable)?;
            Ok(Either::Export(Box::pin(async move {
                store_answer(store_result(Ok(reply)).await?)
            })))
        },
    );

    // Viewer rooms carry document bodies, which a manifest-stage credential
    // does not admit.
    let mut rooms = match (&plan, live.is_some(), hooks.stage) {
        (Plan::Workspace { workspace_id }, true, Stage::Full) => Some(rooms::Rooms::new(
            *workspace_id,
            priority.clone(),
            worker.clone(),
        )),
        _ => None,
    };
    loop {
        if let Some(watch) = &mut live {
            watch.begin_pass();
        }
        versions.lock().unwrap_or_else(|e| e.into_inner()).clear();
        progress(Progress::PassStarted);
        let mut outcome = Outcome::default();
        match plan.clone() {
            Plan::Workspace { workspace_id } => 'workspace: {
                let workspace = cloud(workspace_id.as_uuid());
                let manifest = Unit::Document {
                    workspace_id: workspace,
                    document_id: cloud(scope.manifest.as_uuid()),
                };
                drive_unit(
                    &mut sync,
                    manifest,
                    &deferrals,
                    &versions,
                    &mut outcome,
                    &mut progress,
                )
                .await?;
                if hooks.stage == Stage::Manifest {
                    // The manifest names the documents; their bodies wait for
                    // the full stage's credential.
                    break 'workspace;
                }
                // The committed manifest is the authority for which documents
                // exist. Read it back through the Store, never from the remote.
                let mut entries = store_result(worker.submit_store(move |store| {
                    store.documents(workspace_id).map(|entries| {
                        entries
                            .into_iter()
                            .map(|entry| entry.id)
                            .collect::<Vec<_>>()
                    })
                }))
                .await?
                .map_err(|_| Error::Unavailable)?;
                if let Some(interest) = &hooks.interest {
                    let resident = interest.read().unwrap_or_else(|e| e.into_inner());
                    entries.retain(|id| *id == scope.manifest || resident.contains(id));
                }
                // Room interest is a scheduling hint, never authority. The manifest
                // still supplies membership and every unit rechecks its permit.
                entries.sort_by_key(|id| {
                    !priority.priority(Unit::Document {
                        workspace_id: workspace,
                        document_id: cloud(id.as_uuid()),
                    })
                });
                let required = entries
                    .iter()
                    .filter(|id| **id != scope.manifest)
                    .count()
                    .saturating_add(1);
                if let Some(rooms) = &mut rooms {
                    rooms.inventory(&entries);
                    outcome.remote_hint |= rooms
                        .drain(&mut sync, &deferrals, &versions, &mut progress)
                        .await?;
                }
                progress(Progress::Documents {
                    synced: outcome.documents_synced,
                    required: Some(required.try_into().unwrap_or(u32::MAX)),
                });
                comparison::documents(
                    &mut sync,
                    workspace_id,
                    scope.manifest,
                    entries,
                    &worker,
                    &authorize,
                    &deferrals,
                    &versions,
                    &mut outcome,
                    &mut progress,
                    &mut rooms,
                )
                .await?;
            }
            Plan::Profile {
                profile_id,
                private,
                personal_workspaces,
            } => {
                let profile = cloud(profile_id.as_uuid());
                let mut units = vec![Unit::Identity {
                    profile_id: profile,
                }];
                if private {
                    units.push(Unit::Private {
                        profile_id: profile,
                    });
                }
                units.extend(
                    personal_workspaces
                        .into_iter()
                        .map(|workspace| Unit::Personal {
                            profile_id: profile,
                            workspace_id: cloud(workspace.as_uuid()),
                        }),
                );
                for unit in units {
                    drive_unit(
                        &mut sync,
                        unit,
                        &deferrals,
                        &versions,
                        &mut outcome,
                        &mut progress,
                    )
                    .await?;
                }
            }
        }
        progress(Progress::PassFinished(Box::new(outcome.clone())));
        let workspace = match &plan {
            Plan::Workspace { workspace_id } => Some(*workspace_id),
            Plan::Profile { .. } => None,
        };
        match wait_or_serve(
            &mut sync,
            &mut live,
            &outcome,
            &authorize,
            scope,
            &mut rooms,
            &deferrals,
            &versions,
            &mut progress,
            &mut hooks,
            &worker,
            workspace,
        )
        .await?
        {
            Next::Pass => {}
            Next::Done => return Ok(Ended::Finished(outcome)),
            Next::Advance(stage) => return Ok(Ended::Advance(stage, outcome)),
        }
    }
}

/// What a session does after a pass.
enum Next {
    Pass,
    Done,
    Advance(Stage),
}

/// Between passes: wait for a change, and answer what the driver is asked
/// meanwhile — a unit to fetch now, or another stage.
///
/// **A fetch runs here, on the session's own carrier, between passes**, so it
/// never interleaves with a unit already in flight. A request made during a
/// long pass waits for that pass to finish.
#[allow(clippy::too_many_arguments)]
async fn wait_or_serve<'a, R, W, J, F>(
    sync: &mut Reconciler<'a, R, W>,
    live: &mut Option<koru_store_api::cloud_changes::Subscription>,
    outcome: &Outcome,
    authorize: &Authorize,
    scope: Scope,
    rooms: &mut Option<rooms::Rooms<J>>,
    deferrals: &DeferralLedger,
    versions: &Versions,
    progress: &mut F,
    hooks: &mut Hooks<'_>,
    worker: &J,
    workspace: Option<WorkspaceId>,
) -> Result<Next, Error>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
    J: StoreJobs,
    F: FnMut(Progress),
{
    let Some(watch) = live else {
        return Ok(Next::Done);
    };
    let mut hinted = outcome.remote_hint;
    loop {
        let command = match live::wait_for_change(
            sync,
            watch,
            hinted,
            authorize,
            scope,
            rooms,
            deferrals,
            versions,
            progress,
            hooks.commands.as_deref_mut(),
        )
        .await?
        {
            live::Wake::Changed => return Ok(Next::Pass),
            live::Wake::Command(command) => command,
        };
        // Whatever was pending before the command still is.
        hinted = true;
        match command {
            driver::Command::Advance(stage) if stage != hooks.stage => {
                return Ok(Next::Advance(stage));
            }
            driver::Command::Advance(_) => {}
            driver::Command::Fetch { unit, reply } => {
                let answer = fetch(
                    sync,
                    worker,
                    workspace,
                    scope,
                    hooks.stage,
                    unit,
                    deferrals,
                    versions,
                    progress,
                )
                .await;
                // A carrier failure ends the session as any other unit's
                // would; the caller hears why before it does.
                let ended = match &answer {
                    Err(driver::FetchError::Transfer(error)) => Some(*error),
                    _ => None,
                };
                let _ = reply.send(answer);
                if let Some(error) = ended {
                    return Err(error);
                }
            }
        }
    }
}

/// Reconcile one document unit **the manifest does not name** (TECHNICAL
/// §5.6): a hosted creation's candidate, looked up in Cloud by id before it is
/// seeded.
///
/// The unit is provisioned as any document a pass meets, then driven through
/// the same carrier. Cloud resolves a scoped download without a manifest check,
/// and an absent unit is an empty version rather than an error, so the answer
/// is read back from the Store afterwards: content arrived, or nothing did.
#[allow(clippy::too_many_arguments)]
async fn fetch<'a, R, W, J, F>(
    sync: &mut Reconciler<'a, R, W>,
    worker: &J,
    workspace: Option<WorkspaceId>,
    scope: Scope,
    stage: Stage,
    unit: Unit,
    deferrals: &DeferralLedger,
    versions: &Versions,
    progress: &mut F,
) -> Result<driver::Fetched, driver::FetchError>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
    J: StoreJobs,
    F: FnMut(Progress),
{
    let (Some(workspace_id), Unit::Document { document_id, .. }) = (workspace, unit) else {
        return Err(driver::FetchError::NotADocument);
    };
    if scope.room(unit).is_err() || document_id.as_bytes() == scope.manifest.as_uuid().as_bytes() {
        return Err(driver::FetchError::NotADocument);
    }
    if stage != Stage::Full {
        return Err(driver::FetchError::ManifestStage);
    }
    let document = DocumentId::from_uuid(document_id.as_uuid());
    ensure_document(worker, workspace_id, document)
        .await
        .map_err(driver::FetchError::Transfer)?;
    let mut outcome = Outcome::default();
    drive_unit(sync, unit, deferrals, versions, &mut outcome, progress)
        .await
        .map_err(driver::FetchError::Transfer)?;
    if let Some((_, code)) = outcome.refused.first() {
        return Err(driver::FetchError::Refused(*code));
    }
    let empty = store_result(worker.submit_store(move |store| {
        store
            .version(workspace_id, document)
            .map(|version| version.is_empty())
    }))
    .await
    .map_err(driver::FetchError::Transfer)?
    .map_err(|error| driver::FetchError::Refused(error.code()))?;
    if !empty {
        return Ok(driver::Fetched::Present);
    }
    // Absent: take the empty container back out, so the lookup leaves the
    // Store as it found it and a seed under this id is not a duplicate.
    store_result(
        worker.submit_store(move |store| {
            store.discard_empty_replica_document(workspace_id, document)
        }),
    )
    .await
    .map_err(driver::FetchError::Transfer)?
    .map_err(|error| driver::FetchError::Refused(error.code()))?;
    Ok(driver::Fetched::Absent)
}

/// A document the manifest names but this Store does not hold yet gets its
/// empty bootstrap container through the existing bounded path, exactly as
/// the peer path provisions an advertised document before pulling it.
async fn ensure_document<J: StoreJobs>(
    worker: &J,
    workspace: WorkspaceId,
    document: DocumentId,
) -> Result<(), Error> {
    store_result(
        worker.submit_store(move |store| match store.locate(document)? {
            Some(owner) if owner == workspace => Ok(()),
            Some(_) => Err(koru_store_core::Error::bare(
                koru_store_core::errors::Code::DocumentExists,
            )),
            None => store.provision_replica_document(workspace, document, Timestamp::now()),
        }),
    )
    .await?
    .map_err(|_| Error::Unavailable)
}

#[allow(clippy::too_many_arguments)]
async fn drive_unit<'a, R, W, F>(
    sync: &mut Reconciler<'a, R, W>,
    unit: Unit,
    deferrals: &DeferralLedger,
    versions: &Versions,
    outcome: &mut Outcome,
    progress: &mut F,
) -> Result<(), Error>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
    F: FnMut(Progress),
{
    sync.start(unit).map_err(|error| {
        tracing::warn!(
            ?unit,
            code = error.code(),
            status = ?sync.status(),
            "cloud session: unit admission ended the pass"
        );
        error
    })?;
    progress(Progress::Started(unit));
    // Named on the way out, because the carrier cannot carry it: which unit
    // ended a pass is a fact the worker will never see, and a pass that ends
    // here closes its stream without writing a refusal frame at all.
    //
    // What reaches here is now only what `deferrable` calls a statement about
    // the session — a Store that is gone, a context that may not read. A
    // refusal about one unit arrives as `Event::Refused` below and the pass
    // keeps going.
    let named = |error: Error| {
        tracing::warn!(
            ?unit,
            code = error.code(),
            "cloud session: a unit ended the pass"
        );
        error
    };
    let mut settled = false;
    let mut refused = false;
    loop {
        let status = sync.status();
        if settled
            && !status.transport.committing
            && !status.transport.writing
            && status.transport.queued_controls == 0
        {
            return Ok(());
        }
        match sync.next().await.map_err(named)? {
            Event::Settled {
                unit,
                upload_deferred,
                download_deferred,
                local_changed,
            } => {
                if local_changed {
                    outcome.remote_hint = true;
                    progress(Progress::RemoteHint);
                }
                settled = true;
                outcome.settled.push(unit);
                // The document half of the coverage sample, recorded at the
                // one moment all three facts are known: which state was read,
                // and whether it was carried whole.
                if matches!(unit, Unit::Document { .. }) {
                    let carried =
                        !refused && !upload_deferred && !download_deferred && !local_changed;
                    if carried {
                        outcome.documents_synced = outcome.documents_synced.saturating_add(1);
                    }
                    outcome.documents.push(DocumentPass {
                        unit,
                        state: versions
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .get(&unit)
                            .cloned()
                            .unwrap_or_default(),
                        carried,
                    });
                    progress(Progress::Documents {
                        synced: outcome.documents_synced,
                        required: None,
                    });
                }
                // A unit that settled without being refused has broken its
                // run, whatever it did on the passes before. Only an
                // uninterrupted run counts towards the bound.
                if !refused {
                    ledger(deferrals, |held| held.cleared(unit));
                }
                // The version read last is the verify read, and with no local
                // change Cloud's confirmed version includes it.
                if !refused && !upload_deferred && !local_changed {
                    let version = versions
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .get(&unit)
                        .cloned();
                    if let Some(version) = version {
                        progress(Progress::Confirmed { unit, version });
                    }
                }
                progress(Progress::Settled {
                    unit,
                    upload_deferred,
                    download_deferred,
                });
            }
            Event::Refused { unit, code } => {
                refused = true;
                outcome.deferred.push(unit);
                outcome.refused.push((unit, code));
                let run = ledger(deferrals, |held| held.refused(unit));
                tracing::warn!(
                    ?unit,
                    code = code.as_str(),
                    run,
                    "cloud session: a Store job refused a unit; the pass carries on"
                );
                progress(Progress::Refused { unit, code });
                if run >= MAX_CONSECUTIVE_REFUSALS {
                    // Said at `warn` and handed to the caller: a unit that has
                    // not moved in three passes is not a pass that finished.
                    tracing::warn!(
                        ?unit,
                        code = code.as_str(),
                        run,
                        "cloud session: a unit is not syncing and is now unavailable"
                    );
                    outcome.unavailable.push((unit, code));
                    progress(Progress::Unavailable { unit, code });
                }
                // The version read leaves no pass behind, so the unit is over;
                // the export holds its upload and still settles below.
                if sync.status().unit.is_none() {
                    settled = true;
                }
            }
            Event::RemoteHint => {
                outcome.remote_hint = true;
                progress(Progress::RemoteHint);
            }
            Event::Uploaded { unit, .. } => progress(Progress::Uploaded(unit)),
            Event::Downloaded(unit) => progress(Progress::Downloaded(unit)),
            Event::Deferred { start, upload } => {
                outcome.deferred.push(start.unit);
                progress(Progress::Deferred {
                    unit: start.unit,
                    upload,
                });
            }
            Event::Progress => {}
        }
    }
}

/// Read or write the host's ledger, recovering a poisoned lock the way
/// `cloud_driver` recovers the control host's: this is bookkeeping, and a panic
/// somewhere else must not be able to take a Cloud pass with it.
fn ledger<T>(deferrals: &DeferralLedger, write: impl FnOnce(&mut Deferrals) -> T) -> T {
    write(&mut deferrals.lock().unwrap_or_else(|error| error.into_inner()))
}

#[cfg(test)]
mod tests;
