// SPDX-License-Identifier: MPL-2.0

//! Viewer rooms bypass inventory passes. Reuse the same scoped Loro carrier
//! and durable Store ports; never infer permission from a viewer subscription.
use super::*;
use koru_store_api::cloud_changes::{Dataset, Subscription};
use std::collections::{HashMap, HashSet};

pub(super) struct Rooms<J> {
    workspace: WorkspaceId,
    known: HashSet<DocumentId>,
    watchers: HashMap<DocumentId, Subscription>,
    viewers: Arc<dyn Downloads>,
    worker: J,
}

impl<J: StoreJobs> Rooms<J> {
    pub fn new(workspace: WorkspaceId, viewers: Arc<dyn Downloads>, worker: J) -> Self {
        Self {
            workspace,
            known: HashSet::new(),
            watchers: HashMap::new(),
            viewers,
            worker,
        }
    }
    pub fn inventory(&mut self, documents: &[DocumentId]) {
        self.known = documents.iter().copied().collect();
    }
    fn unit(&self, document: DocumentId) -> Unit {
        Unit::Document {
            workspace_id: cloud(self.workspace.as_uuid()),
            document_id: cloud(document.as_uuid()),
        }
    }

    pub async fn drain<'a, R, W, F>(
        &mut self,
        sync: &mut Reconciler<'a, R, W>,
        deferrals: &DeferralLedger,
        versions: &Versions,
        progress: &mut F,
    ) -> Result<bool, Error>
    where
        R: AsyncRead + Unpin + Send + 'a,
        W: AsyncWrite + Unpin + Send + 'a,
        F: FnMut(Progress),
    {
        let mut desired = self.viewers.interests();
        desired.retain(|document| self.known.contains(document));
        desired.sort();
        desired.dedup();
        desired.truncate(koru_replication::cloud::transfer::metadata::MAX_LIVE_ROOMS);
        self.watchers.retain(|id, _| desired.contains(id));
        sync.rooms(desired.iter().map(|id| self.unit(*id)))?;
        let mut pending = Vec::new();
        for document in desired {
            if !self.watchers.contains_key(&document) {
                // Watch before reading the version: no lost local commit at join.
                let watch = self
                    .worker
                    .watch_cloud(Dataset::Document(self.workspace, document))
                    .await
                    .map_err(|_| Error::Unavailable)?;
                self.watchers.insert(document, watch);
                pending.push(document);
            } else if self.watchers[&document].pending() {
                pending.push(document);
            }
        }
        let mut hinted = false;
        for document in pending {
            self.watchers
                .get_mut(&document)
                .expect("watched room")
                .begin_pass();
            let unit = self.unit(document);
            let started = Instant::now();
            let mut outcome = Outcome::default();
            // This is not a new workspace pass: do not reset its coverage or
            // report one document as the total inventory. Arrival already uses
            // the same view broadcast as peer sync, after durable commit.
            drive_unit(
                sync,
                unit,
                deferrals,
                versions,
                &mut outcome,
                &mut |event| {
                    if matches!(
                        event,
                        Progress::Uploaded(_)
                            | Progress::Downloaded(_)
                            | Progress::Deferred { .. }
                            | Progress::Refused { .. }
                            | Progress::Unavailable { .. }
                            | Progress::Confirmed { .. }
                    ) {
                        progress(event);
                    }
                },
            )
            .await?;
            // An unrelated remote hint (or a racing local edit) can arrive
            // during this exchange. Preserve it for the background owner.
            hinted |= outcome.remote_hint;
            tracing::info!(
                ?unit,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "cloud room: local reconciliation finished"
            );
        }
        Ok(hinted)
    }
}
