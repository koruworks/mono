// SPDX-License-Identifier: MPL-2.0

//! **The 30-day trash sweep, run by the desktop Store** at start and daily
//! (COLLECTOR.final, rung 2). The sweep itself is shared with Android's
//! embedded Store (`koru_replication::cloud::client_store::expiry`); this
//! decides when, and by whose clock.
//!
//! **Whose clock** (`koru_core::trash_expiry`): a local-only Workspace sweeps
//! by this device's clock. A Cloud Workspace sweeps only by the signed issue
//! time of the credential its last session here was admitted with
//! ([`TrustedTimes`], recorded by the Cloud driver), so a device whose clock
//! runs ahead cannot empty another member's trash; until a session has run in
//! this process it waits. A peer-to-peer Workspace has no time anyone vouched
//! for and its peers may be other people, so it does not empty by itself.
//!
//! Every step is its own Store call — the due documents, one subtree purge,
//! one document's bin — so a save never waits behind a whole Workspace.

use super::StoreWorkerClient;
use koru_replication::cloud::client_store::expiry;
use koru_store_core::{Timestamp, TransportMode, TransportSubject, WorkspaceId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The first look waits until start-up has settled: "at start" is a promise
/// about the day, not about the first second.
const FIRST_LOOK_AFTER: Duration = Duration::from_secs(120);
/// How often the sweep looks for a Workspace that is due a sweep.
const LOOK_EVERY: Duration = Duration::from_secs(60 * 60);
/// How often one Workspace is swept.
const SWEEP_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The Cloud-vouched time per Workspace: the newest credential issue time a
/// session in this process was admitted with. Memory only; a restart waits
/// for the next session.
#[derive(Default)]
pub struct TrustedTimes(Mutex<HashMap<WorkspaceId, i64>>);

impl TrustedTimes {
    pub fn record(&self, workspace: WorkspaceId, issued_at_ms: i64) {
        let mut times = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let time = times.entry(workspace).or_insert(issued_at_ms);
        *time = (*time).max(issued_at_ms);
    }

    fn get(&self, workspace: WorkspaceId) -> Option<i64> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&workspace)
            .copied()
    }
}

/// The clock one Workspace sweeps by, or `None` when it must not sweep now.
fn clock(mode: TransportMode, trusted: Option<i64>, device_now_ms: i64) -> Option<i64> {
    match mode {
        TransportMode::LocalOnly => Some(device_now_ms),
        TransportMode::Cloud => trusted,
        TransportMode::P2p => None,
    }
}

pub(crate) fn install(store: StoreWorkerClient, trusted: Arc<TrustedTimes>) {
    tokio::spawn(async move {
        // The peer this process edits bins with: one per process, as
        // Android's document edits use one per host.
        let peer = loro::LoroDoc::new().peer_id();
        let mut swept: HashMap<WorkspaceId, Instant> = HashMap::new();
        tokio::time::sleep(FIRST_LOOK_AFTER).await;
        loop {
            pass(&store, &trusted, peer, &mut swept).await;
            tokio::time::sleep(LOOK_EVERY).await;
        }
    });
}

async fn pass(
    store: &StoreWorkerClient,
    trusted: &TrustedTimes,
    peer: u64,
    swept: &mut HashMap<WorkspaceId, Instant>,
) {
    let workspaces = store
        .with_store_async(|store| {
            store.workspaces().map(|workspaces| {
                workspaces
                    .into_iter()
                    .filter_map(|workspace| {
                        store
                            .transport_mode(TransportSubject::Workspace(workspace.id))
                            .ok()
                            .map(|state| (workspace.id, state.mode))
                    })
                    .collect::<Vec<_>>()
            })
        })
        .await;
    let workspaces = match workspaces {
        Ok(Ok(workspaces)) => workspaces,
        Ok(Err(error)) => {
            tracing::warn!(target: "store.trash_expiry", code = error.code().as_str(), "could not list Workspaces to sweep");
            return;
        }
        Err(_) => return,
    };
    for (workspace, mode) in workspaces {
        if swept
            .get(&workspace)
            .is_some_and(|at| at.elapsed() < SWEEP_EVERY)
        {
            continue;
        }
        let Some(now) = clock(mode, trusted.get(workspace), Timestamp::now().millis()) else {
            continue;
        };
        match sweep(store, workspace, now, peer).await {
            Ok(result) => {
                swept.insert(workspace, Instant::now());
                if result != expiry::Swept::default() {
                    tracing::info!(target: "store.trash_expiry", workspace = %workspace, mode = mode.as_storage(), documents = result.documents, rows = result.rows, rewritten = result.rewritten, "trash sweep removed what was due");
                }
            }
            Err(code) => {
                tracing::warn!(target: "store.trash_expiry", workspace = %workspace, code, "trash sweep stopped; it retries on the next look");
            }
        }
    }
}

/// One Workspace, a step per Store call.
async fn sweep(
    store: &StoreWorkerClient,
    workspace: WorkspaceId,
    now: i64,
    peer: u64,
) -> Result<expiry::Swept, String> {
    async fn step<R: Send + 'static>(
        store: &StoreWorkerClient,
        task: impl FnOnce(
            &mut crate::WorkerStore,
        ) -> Result<R, koru_replication::cloud::client_store::Failure>
        + Send
        + 'static,
    ) -> Result<R, String> {
        match store.with_store_async(task).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(failure)) => Err(format!("{failure:?}")),
            Err(error) => Err(format!("{error:?}")),
        }
    }
    let mut result = expiry::Swept::default();
    let due = step(store, move |s| expiry::due_documents(s, workspace, now)).await?;
    for root in due {
        result.documents += step(store, move |s| {
            expiry::purge_subtree(s, workspace, root, Timestamp::now())
        })
        .await?;
    }
    let candidates = step(store, move |s| expiry::bin_candidates(s, workspace)).await?;
    for document in candidates {
        let rows = step(store, move |s| {
            expiry::sweep_bin(s, workspace, document, now, peer, Timestamp::now())
        })
        .await?;
        result.rows += rows;
        result.rewritten += usize::from(rows > 0);
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
