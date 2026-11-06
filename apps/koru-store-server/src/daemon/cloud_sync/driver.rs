// SPDX-License-Identifier: MPL-2.0

//! The Reconciler driver: one dataset kept current from Cloud, as a task its
//! owner can stop and join.
//!
//! The scheduling a device's gateway always ran around the public
//! `Reconciler` — a live session, a pass, a wait for the next change — and the
//! session plumbing around it, made into one reusable owner so a host that is
//! not a device runs exactly the same engine:
//!
//! - **a session source** ([`SessionSource`]) opens each admitted session. It
//!   is where the credential lives: a device's gateway hands over the one
//!   stream its Agent bridged ([`OnceSource`]), and a hosted replica dials with
//!   a connection credential for the stage it is at. The driver never sees a
//!   credential, a route or a key.
//! - **a stage** ([`Stage`]). A Workspace session admitted at the manifest
//!   stage carries the manifest alone; [`Driver::advance`] ends it and opens
//!   the next at the full stage, because the stage is the credential's.
//! - **a unit the manifest does not name**, fetched on request
//!   ([`Driver::fetch`]): a hosted creation's candidate, looked up in Cloud by
//!   id before anything is seeded (TECHNICAL §5.6).
//! - **stop-and-join** ([`Driver::stop`]). It answers only once the driver's
//!   task has exited. By then the session's carrier is dropped — its stream
//!   closed, its reservations released — and every Store job it had already
//!   handed to the executor refuses as fenced when it runs, so no export of the
//!   stopped driver can leave afterwards. What the executor still holds is the
//!   executor's to drain (a fence does that); none of it can reach Cloud.
//!
//! The Store work goes through [`StoreJobs`], so the same driver runs over the
//! Store worker's queue and over a host's per-replica executor.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use koru_replication::cloud::{
    Refusal,
    transfer::{Decision, Error, Start, Unit},
};
use koru_store_core::errors::Code;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot, watch};

use super::{
    Authorize, DeferralLedger, Downloads, Ended, Hooks, Outcome, Plan, Progress, Session, Stage,
    StoreJobs, Uploads, live, run_dataset,
};

/// One admitted dataset session: the stream halves and the facts its admission
/// established.
pub struct Opened<R, W> {
    pub reader: R,
    pub writer: W,
    pub session: Session,
}

/// Where the driver's sessions come from: **the credential seam**.
///
/// `open` is asked for a session at a stage, and answers one admitted at it,
/// or `None` when there will be no more — a device's single bridged stream is
/// spent, a replica's authority has lapsed. It may take as long as it likes:
/// a source that renews its credential only when a call presents fresh
/// authority waits here, and a stopped driver stops waiting.
pub trait SessionSource: Send + 'static {
    type Reader: AsyncRead + Unpin + Send + 'static;
    type Writer: AsyncWrite + Unpin + Send + 'static;

    fn open(
        &mut self,
        stage: Stage,
    ) -> impl Future<Output = Option<Opened<Self::Reader, Self::Writer>>> + Send;
}

/// One session already admitted, and no other: a device's gateway, whose Agent
/// dialled and bridged the stream before the Store ever heard of it.
pub struct OnceSource<R, W>(Option<Opened<R, W>>);

impl<R, W> OnceSource<R, W> {
    pub fn new(reader: R, writer: W, session: Session) -> Self {
        Self(Some(Opened {
            reader,
            writer,
            session,
        }))
    }
}

impl<R, W> SessionSource for OnceSource<R, W>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    type Reader = R;
    type Writer = W;

    fn open(&mut self, _stage: Stage) -> impl Future<Output = Option<Opened<R, W>>> + Send {
        std::future::ready(self.0.take())
    }
}

/// Everything a pass needs besides its sessions.
pub struct Parts<J, P, F> {
    pub plan: Plan,
    /// The stage the first session is asked for. A device's is `Full`.
    pub stage: Stage,
    pub jobs: J,
    pub authorize: Authorize,
    pub downloads: Arc<dyn Downloads>,
    pub deferrals: DeferralLedger,
    /// Cloned into each session's carrier.
    pub permission: P,
    pub progress: F,
    /// Which units may be uploaded at the full stage; `None` is every unit,
    /// as on a device. The manifest stage uploads nothing whatever this says.
    pub uploads: Option<Uploads>,
}

/// A unit fetched on request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fetched {
    /// Cloud holds the unit; its content is now in the Store.
    Present,
    /// Cloud holds no such unit: an empty version, not an error.
    Absent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchError {
    /// Only an ordinary document of this Workspace can be fetched: not the
    /// manifest, not a Profile unit, not another Workspace's document.
    NotADocument,
    /// The session's credential admits no document bodies yet.
    ManifestStage,
    /// A Store job would not serve the unit; its own code.
    Refused(Code),
    /// The carrier failed; the session ended with it.
    Transfer(Error),
    /// The driver stopped, or its source is spent, before the fetch ran.
    Stopped,
}

/// What a caller can know about the driver without asking it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub stage: Stage,
    /// A session is open.
    pub connected: bool,
    /// Passes finished since the driver started, over every session.
    pub passes: u64,
    /// The last pass's documents carried whole, and how many the manifest
    /// names (with the manifest itself), once a pass has counted them.
    pub documents_synced: u32,
    pub documents_required: Option<u32>,
    /// Units the last pass reported as not syncing.
    pub unavailable: usize,
}

pub(crate) enum Command {
    Fetch {
        unit: Unit,
        reply: oneshot::Sender<Result<Fetched, FetchError>>,
    },
    Advance(Stage),
}

/// How many requests may wait for the session to take them. A caller that
/// finds it full is told the driver is busy rather than queued without bound.
const COMMANDS: usize = 16;

/// The running driver. See the module note.
///
/// **Dropping it stops it without joining**: the task is told to stop and is
/// aborted, and every Store job it had submitted refuses as fenced. A caller
/// that needs to know the task has exited — a fence — calls [`Driver::stop`].
pub struct Driver {
    commands: mpsc::Sender<Command>,
    fenced: Arc<AtomicBool>,
    stop: Option<oneshot::Sender<()>>,
    status: watch::Receiver<Status>,
    task: Option<tokio::task::JoinHandle<Result<Outcome, Error>>>,
}

impl Driver {
    /// Start the driver on the current Tokio runtime.
    pub fn spawn<S, J, P, F>(source: S, parts: Parts<J, P, F>) -> Self
    where
        S: SessionSource,
        J: StoreJobs,
        P: FnMut(&Start) -> Decision + Clone + Send + 'static,
        F: FnMut(Progress) + Send + 'static,
    {
        let (commands, receiver) = mpsc::channel(COMMANDS);
        let (stop, stopped) = oneshot::channel();
        let (status_tx, status) = watch::channel(Status {
            stage: parts.stage,
            connected: false,
            passes: 0,
            documents_synced: 0,
            documents_required: None,
            unavailable: 0,
        });
        let fenced = Arc::new(AtomicBool::new(false));
        let inner = parts.authorize;
        let gate = Arc::clone(&fenced);
        // Every Store job and unit start asks this. Once the driver is stopped
        // it refuses, so a job the executor runs after the stop — queued
        // before it — exports nothing and commits nothing.
        let authorize: Authorize = Arc::new(move |unit| {
            if gate.load(Ordering::Acquire) {
                Err(Refusal::ReplicaFenced)
            } else {
                inner(unit)
            }
        });
        let parts = Parts { authorize, ..parts };
        let task = tokio::spawn(run(source, parts, receiver, stopped, status_tx));
        Self {
            commands,
            fenced,
            stop: Some(stop),
            status,
            task: Some(task),
        }
    }

    /// The driver's status as it changes.
    pub fn status(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }

    /// Move to another stage. The current session ends after the pass it is
    /// in, and the next opens at `stage`. Asking for the stage it is at does
    /// nothing.
    pub fn advance(&self, stage: Stage) -> Result<(), FetchError> {
        self.commands
            .try_send(Command::Advance(stage))
            .map_err(|_| FetchError::Stopped)
    }

    /// Reconcile one document unit of this Workspace that the manifest need
    /// not name, between passes, and say whether Cloud held it.
    pub async fn fetch(&self, unit: Unit) -> Result<Fetched, FetchError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .try_send(Command::Fetch { unit, reply })
            .map_err(|_| FetchError::Stopped)?;
        answer.await.unwrap_or(Err(FetchError::Stopped))
    }

    /// Stop, and return **only once the driver's task has exited**. See the
    /// module note for what is left behind: nothing that can reach Cloud.
    ///
    /// It answers nothing about the session it ended. That session may have
    /// failed on the fence this raised — its Store jobs refuse from the moment
    /// `stop` is called — and a fence's own refusal is not news.
    pub async fn stop(mut self) {
        self.fenced.store(true, Ordering::Release);
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = self.join_task().await;
    }

    /// Wait for the driver to end on its own — its source spent — without
    /// asking it to stop.
    pub async fn join(mut self) -> Result<Outcome, Error> {
        self.join_task().await
    }

    async fn join_task(&mut self) -> Result<Outcome, Error> {
        let Some(task) = self.task.take() else {
            return Err(Error::Unavailable);
        };
        match task.await {
            Ok(result) => result,
            Err(join) => {
                tracing::error!(
                    cancelled = join.is_cancelled(),
                    "cloud driver: task did not finish"
                );
                Err(Error::Unavailable)
            }
        }
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.fenced.store(true, Ordering::Release);
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn run<S, J, P, F>(
    mut source: S,
    parts: Parts<J, P, F>,
    mut commands: mpsc::Receiver<Command>,
    mut stopped: oneshot::Receiver<()>,
    status: watch::Sender<Status>,
) -> Result<Outcome, Error>
where
    S: SessionSource,
    J: StoreJobs,
    P: FnMut(&Start) -> Decision + Clone + Send + 'static,
    F: FnMut(Progress) + Send + 'static,
{
    let Parts {
        plan,
        mut stage,
        jobs,
        authorize,
        downloads,
        deferrals,
        permission,
        mut progress,
        uploads,
    } = parts;
    let mut last: Result<Outcome, Error> = Ok(Outcome::default());
    loop {
        let opened = tokio::select! {
            biased;
            _ = &mut stopped => return last,
            opened = source.open(stage) => opened,
        };
        let Some(Opened {
            reader,
            writer,
            session,
        }) = opened
        else {
            return last;
        };
        status.send_modify(|status| {
            status.stage = stage;
            status.connected = true;
        });
        let mut observed = |event: Progress| {
            match &event {
                Progress::Documents { synced, required } => status.send_modify(|status| {
                    status.documents_synced = *synced;
                    if required.is_some() {
                        status.documents_required = *required;
                    }
                }),
                Progress::PassFinished(outcome) => status.send_modify(|status| {
                    status.passes = status.passes.saturating_add(1);
                    status.documents_synced = outcome.documents_synced;
                    status.unavailable = outcome.unavailable.len();
                }),
                _ => {}
            }
            progress(event);
        };
        // Owned or borrowed mutably, so the session is `Send` without asking
        // anything shared of the permission policy.
        let (plan, jobs, authorize, downloads, deferrals, permission, uploads) = (
            plan.clone(),
            jobs.clone(),
            Arc::clone(&authorize),
            Arc::clone(&downloads),
            Arc::clone(&deferrals),
            permission.clone(),
            uploads.clone(),
        );
        let (observed, commands) = (&mut observed, &mut commands);
        let session = async move {
            let watch = live::watch(&jobs, &plan).await?;
            run_dataset(
                reader,
                writer,
                session,
                plan,
                jobs,
                authorize,
                downloads,
                deferrals,
                permission,
                observed,
                Some(watch),
                Hooks {
                    stage,
                    commands: Some(commands),
                    uploads,
                },
            )
            .await
        };
        let ended = tokio::select! {
            biased;
            _ = &mut stopped => return last,
            ended = session => ended,
        };
        status.send_modify(|status| status.connected = false);
        match ended {
            Ok(Ended::Advance(next, outcome)) => {
                tracing::debug!(?next, "cloud driver: moving to another stage");
                stage = next;
                last = Ok(outcome);
            }
            Ok(Ended::Finished(outcome)) => last = Ok(outcome),
            Err(error) => {
                tracing::info!(code = error.code(), "cloud driver: session ended");
                last = Err(error);
            }
        }
    }
}

#[cfg(test)]
mod tests;
