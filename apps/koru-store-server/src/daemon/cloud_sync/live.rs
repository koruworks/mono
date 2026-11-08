// SPDX-License-Identifier: MPL-2.0

//! A live dataset retains the same Reconciler, including partial frames,
//! outstanding writes and original deadlines, between passes. Never rebuild
//! the Client around the same stream or cancel a raw framed read on a wake.
use super::*;
use koru_store_api::cloud_changes::{Dataset, Subscription};
use std::time::Duration;

const RECOVERY: Duration = Duration::from_secs(60);
const DEBOUNCE: Duration = Duration::from_millis(500);

#[allow(clippy::too_many_arguments)]
pub async fn subscribe_dataset<R, W, J, P, F>(
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
    let watch = watch(&worker, &plan).await?;
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
        Some(watch),
        Hooks::device(),
    )
    .await
    .map(Ended::into_outcome)
}

/// The dataset's commit subscription. Taken before the session's first read:
/// a commit racing that read remains pending, rather than disappearing into
/// the gap before idle listening.
pub(super) async fn watch<J: StoreJobs>(worker: &J, plan: &Plan) -> Result<Subscription, Error> {
    let dataset = match plan {
        Plan::Workspace { workspace_id } => Dataset::Workspace(*workspace_id),
        Plan::Profile { profile_id, .. } => Dataset::Profile(*profile_id),
    };
    worker
        .watch_cloud(dataset)
        .await
        .map_err(|_| Error::Unavailable)
}

/// Why an idle session woke.
pub(super) enum Wake {
    /// A local change, a remote hint or the recovery timer: run a pass.
    Changed,
    /// The driver's caller asked for something.
    Command(driver::Command),
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn wait_for_change<'a, R, W, J, F>(
    sync: &mut Reconciler<'a, R, W>,
    changes: &mut Subscription,
    hinted: bool,
    authorize: &Authorize,
    scope: Scope,
    rooms: &mut Option<rooms::Rooms<J>>,
    deferrals: &DeferralLedger,
    versions: &Versions,
    progress: &mut F,
    mut commands: Option<&mut tokio::sync::mpsc::Receiver<driver::Command>>,
) -> Result<Wake, Error>
where
    R: AsyncRead + Unpin + Send + 'a,
    W: AsyncWrite + Unpin + Send + 'a,
    J: StoreJobs,
    F: FnMut(Progress),
{
    let gate = match scope.principal {
        koru_device::cloud::CloudReplicaPrincipalV1::Workspace { .. } => Unit::Document {
            workspace_id: cloud(scope.eligibility_workspace.as_uuid()),
            document_id: cloud(scope.manifest.as_uuid()),
        },
        _ => Unit::Identity {
            profile_id: scope.profile,
        },
    };
    let mut due = if hinted || changes.pending() {
        Some(Instant::now() + DEBOUNCE)
    } else {
        None
    };
    let recovery = Instant::now() + RECOVERY;
    // Only bounded in-memory viewer interest is sampled. Document reads run on
    // join or a durable commit wake, never on this timer.
    let mut fence = tokio::time::interval(Duration::from_millis(100));
    loop {
        authorize(gate).map_err(Error::Authority)?;
        if let Some(rooms) = rooms
            && rooms.drain(sync, deferrals, versions, progress).await?
        {
            due.get_or_insert(Instant::now() + DEBOUNCE);
        }
        tokio::select! {
            event = sync.next() => match event? {
                Event::RemoteHint => { due.get_or_insert(Instant::now() + DEBOUNCE); }
                Event::Progress => {},
                Event::Downloaded(unit) => {
                    tracing::info!(?unit, "cloud room: pushed update committed");
                    progress(Progress::Downloaded(unit));
                }
                Event::Deferred { start, upload } => progress(Progress::Deferred { unit: start.unit, upload }),
                _ => return Err(Error::Invalid),
            },
            _ = changes.changed() => { due.get_or_insert(Instant::now() + DEBOUNCE); },
            command = next_command(&mut commands) => {
                if let Some(command) = command {
                    return Ok(Wake::Command(command));
                }
                // Every handle is gone; nobody is left to ask.
                commands = None;
            },
            _ = tokio::time::sleep_until(due.unwrap_or(recovery)) => {
                tracing::debug!(reason = if due.is_some() { "change" } else { "recovery" },
                    dataset = ?scope.principal,
                    "cloud subscription: scheduling reconciliation");
                return Ok(Wake::Changed);
            },
            _ = fence.tick() => {},
        }
    }
}

/// The next command, or never when this session takes none.
async fn next_command(
    commands: &mut Option<&mut tokio::sync::mpsc::Receiver<driver::Command>>,
) -> Option<driver::Command> {
    match commands {
        Some(commands) => commands.recv().await,
        None => std::future::pending().await,
    }
}
