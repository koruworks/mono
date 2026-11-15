// SPDX-License-Identifier: MPL-2.0

//! One background reader per owner, shared by blob and document upload holds.
//! Registration comes from a live installed permit. The account read must also
//! return that exact Workspace/incarnation in its owner-only usage page.

use super::*;
use koru_account_core::cloud_admission::Binding;
use koru_network_iroh::cloud::client_io::reconcile::UploadAdmissionHolds;
use std::{collections::BTreeMap, sync::Weak, time::Duration};
use uuid::Uuid;

#[derive(Clone)]
struct Entry {
    binding: Binding,
    expires_at_millis: i64,
    live: LiveAuthorization,
    documents: UploadAdmissionHolds,
}

pub(super) struct Observer {
    engine: Weak<BlobEngine>,
    access: Arc<ControlAccess>,
    control: Arc<HttpControlPlane>,
    owners: Mutex<BTreeMap<Uuid, BTreeMap<WorkspaceId, Entry>>>,
}

impl Observer {
    pub(super) fn new(
        engine: &Arc<BlobEngine>,
        access: Arc<ControlAccess>,
        control: Arc<HttpControlPlane>,
    ) -> Arc<Self> {
        Arc::new(Self {
            engine: Arc::downgrade(engine),
            access,
            control,
            owners: Mutex::new(BTreeMap::new()),
        })
    }

    pub(super) fn register(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
        permit: &koru_sync_control::cloud::CloudPermitV2,
        documents: UploadAdmissionHolds,
        live: LiveAuthorization,
    ) {
        let koru_sync_control::cloud::CloudPermitScopeV2::Workspace { descriptor } = &permit.scope else {
            return;
        };
        let workspace = WorkspaceId::from_uuid(descriptor.workspace_id.as_uuid());
        let binding = Binding {
            owner: permit.account_id.as_uuid(),
            workspace: workspace.as_uuid(),
            incarnation: descriptor.workspace_incarnation_id.as_uuid(),
            account_generation: permit.account_generation,
            authority_generation: permit.authority_generation,
        };
        let start = {
            let mut owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
            let start = !owners.contains_key(&binding.owner);
            owners.entry(binding.owner).or_default().insert(
                workspace,
                Entry {
                    binding,
                    expires_at_millis: permit.expires_at_seconds.saturating_mul(1000),
                    live,
                    documents,
                },
            );
            start
        };
        if start {
            let weak = Arc::downgrade(self);
            runtime.spawn(async move {
                Self::run(weak, binding.owner).await;
            });
        }
    }

    fn current(&self, owner: Uuid) -> Vec<(WorkspaceId, Entry)> {
        let mut owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entries) = owners.get_mut(&owner) else {
            return Vec::new();
        };
        entries.retain(|workspace, entry| {
            now_millis() < entry.expires_at_millis
                && (entry.live)(*workspace)
                    .is_some_and(|incarnation| incarnation.as_uuid() == entry.binding.incarnation)
        });
        entries
            .iter()
            .map(|(workspace, entry)| (*workspace, entry.clone()))
            .collect()
    }

    async fn run(weak: Weak<Self>, owner: Uuid) {
        let mut due = tokio::time::Instant::now();
        let mut failures = 0u32;
        loop {
            // Only in-memory authority and active hold sets are sampled here.
            // No Store walk or SQL query is performed to discover work.
            tokio::time::sleep(Duration::from_secs(1)).await;
            let Some(observer) = weak.upgrade() else {
                return;
            };
            let Some(engine) = observer.engine.upgrade() else {
                return;
            };
            let entries = observer.current(owner);
            if entries.is_empty() {
                let mut owners = observer.owners.lock().unwrap_or_else(|e| e.into_inner());
                if owners.get(&owner).is_some_and(|entries| entries.is_empty()) {
                    owners.remove(&owner);
                    return;
                }
                continue;
            }
            let held = engine.admission_workspaces();
            let active: Vec<_> = entries
                .into_iter()
                .filter(|(workspace, entry)| held.contains(workspace) || entry.documents.active())
                .collect();
            if active.is_empty() || tokio::time::Instant::now() < due {
                continue;
            }
            let started = tokio::time::Instant::now();
            let bindings: Vec<_> = active.iter().map(|(_, entry)| entry.binding).collect();
            let result = match observer.access.access(active[0].0) {
                Some(access) => tokio::time::timeout(
                    Duration::from_secs(20),
                    observer.control.admission_observations(&access, &bindings),
                )
                .await
                .ok()
                .and_then(Result::ok),
                None => None,
            };
            // Leave one second for the lightweight active-hold sampling tick.
            let mut interval = 59u64;
            if let Some(observations) = result {
                failures = 0;
                // Re-read current generation/authority after the bounded HTTP
                // operation. A cancelled old account can never install facts.
                let current = observer.current(owner);
                for observation in observations {
                    let workspace = WorkspaceId::from_uuid(observation.binding.workspace);
                    let Some((_, entry)) = current
                        .iter()
                        .find(|(w, entry)| *w == workspace && entry.binding == observation.binding)
                    else {
                        continue;
                    };
                    let boundary = observation.valid_until_millis.saturating_sub(now_millis());
                    interval = interval.min((boundary.max(1) as u64).div_ceil(1000));
                    entry.documents.observe(observation);
                    if engine.observe_admission(observation) {
                        let engine = engine.clone();
                        tokio::spawn(async move {
                            engine.reconcile(workspace, Trigger::Recovery).await;
                        });
                    }
                }
            } else {
                failures = failures.saturating_add(1);
                // Deterministic bounded jitter avoids a file-level polling
                // fan-out and keeps every retry start within sixty seconds.
                let jitter = (owner.as_u128() as u64 ^ failures as u64) % 5;
                interval = (5u64.saturating_mul(1 << failures.min(4)) + jitter).min(59);
            }
            // Resource recovery may not change logical usage. Blob attempts
            // are still bounded by allocation/session metadata and one chunk.
            for (workspace, _) in observer.current(owner) {
                if engine.admission_resource_due(workspace) {
                    let engine = engine.clone();
                    tokio::spawn(async move {
                        engine.reconcile(workspace, Trigger::Recovery).await;
                    });
                }
            }
            due = started + Duration::from_secs(interval.max(1));
        }
    }
}
