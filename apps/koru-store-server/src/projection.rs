// SPDX-License-Identifier: MPL-2.0

//! Running the projection layer beside the store.
//!
//! **This file is where the lock discipline lives.** `koru-projection` knows
//! nothing about mutexes — it asks a [`Source`] for a fork and walks it — and
//! this is the implementation that takes `AppState`'s lock, forks, and lets go.
//!
//! That matters because one lock covers the whole store: a walk held behind it
//! would block every other workspace's commit, and a rebuild held behind it
//! would block them for as long as the rebuild ran. A `LoroDoc::fork` is a
//! structural share, so what is inside the lock is O(1) and what is outside is
//! the work.
//!
//! ## Why a thread and not a task
//!
//! The daemon is async, so a `tokio::spawn` would have been the obvious home.
//! It is not one: `Store` is synchronous and behind a `std::sync::Mutex`, and
//! projecting a large document is CPU-bound work that would hold a runtime
//! worker off its sockets for as long as it took. An OS thread that blocks is
//! the honest shape — the same reasoning that put the store's tray on its own
//! thread.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use koru_projection::model::ListedDocument;
use koru_projection::worker::{PositionsError, Projector, Source, Sweeps};
use koru_projection::{Projections, consumer::Counting};
use koru_store_api::state::AppState;
use koru_store_core::Committed;
use koru_store_core::errors::Code;
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_core::ports::HistoryReason;
use koru_store_core::reminders::{
    EffectiveState, OccurrenceMoment, ReminderReductionInput, ReminderSettings, TargetState,
};
use koru_store_wire::reminders::{
    ReminderQuery, ReminderQueryResponse, ReminderSnapshot, ReminderSnapshotRow, ReminderTarget,
    ReminderView,
};

/// A read-only projection connection serving the daemon's derived-data
/// queries. The worker keeps its writer connection on another thread; WAL lets
/// this reader answer without taking that thread's mutex or the Store's
/// authoritative lock.
///
/// One connection, one mutex, two ports: the HTTP crate names two traits
/// because they answer different questions, but they read the same file and
/// there is no reason to open it twice.
struct ReadPort {
    projections: Mutex<Projections>,
    sweeps: Sweeps,
}

impl koru_store_api::routes::workspaces::WorkspaceUsageService for ReadPort {
    fn live_file_blob_hashes(&self, workspace: WorkspaceId) -> Result<Vec<String>, String> {
        // A workspace manager can be the first consumer of a cold workspace
        // after Store start-up. Asking for a sweep here makes the derived
        // answer converge even when no edit or search has touched it yet.
        self.sweeps.request(workspace);
        self.projections
            .lock()
            .map_err(|_| "the workspace usage projection lock was poisoned".to_owned())?
            .live_file_blob_hashes(workspace)
            .map_err(|error| error.to_string())
    }
}

impl koru_store_api::routes::workspaces::DocumentOutlineService for ReadPort {
    fn headings(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<(Vec<(String, u8, String)>, bool), String> {
        // Same reasoning as the usage port: a picker can be the first consumer
        // of a cold workspace, and asking for a sweep is what makes a document
        // nobody has touched since start-up converge.
        self.sweeps.request(workspace);
        let projections = self
            .projections
            .lock()
            .map_err(|_| "the document outline projection lock was poisoned".to_owned())?;
        // Asked before the headings, and separately: a document with no
        // headings and a document the read model has never seen both answer
        // with an empty list, and a caller has to be able to tell them apart.
        let projected = projections
            .revision(document)
            .map_err(|error| error.to_string())?
            .is_some();
        let headings = projections
            .headings(document)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|heading| (heading.node, heading.level, heading.text))
            .collect();
        Ok((headings, projected))
    }
}

impl koru_store_api::state::ReminderProjectionService for ReadPort {
    fn snapshot(
        &self,
        reminders: &[ReminderReductionInput],
        settings: &ReminderSettings,
    ) -> koru_store_core::Result<ReminderSnapshot> {
        let mut workspaces: Vec<_> = reminders
            .iter()
            .map(|reminder| reminder.stored().rule.target.workspace())
            .collect();
        workspaces.sort_unstable();
        workspaces.dedup();
        let tickets: Vec<_> = workspaces
            .iter()
            .map(|workspace| (*workspace, self.sweeps.request(*workspace)))
            .collect();
        // **No waiting here.** This runs on the thread that answers the API,
        // and how long a sweep takes is a property of the projection's backlog,
        // not of this call. The sweeps are requested, the projection is read
        // as it stands, and a workspace whose sweep has not yet caught up is
        // named in `updating_workspaces` — the Agent treats those as
        // uncertainty and retries from its cursor loop, which is where the
        // waiting belongs. The Tasks port beside this (ADR-287, ADR-288) was
        // built the same way; this one used to sleep up to 100 ms first.
        let targets: Vec<_> = reminders
            .iter()
            .map(|reminder| reminder.stored().rule.target.clone())
            .collect();
        let joined = self
            .projections
            .lock()
            .map_err(|_| {
                koru_store_core::Error::new(
                    Code::ProjectionUnavailable,
                    "the reminder projection lock was poisoned",
                )
            })?
            .reminder_targets(&targets)
            .map_err(|error| {
                koru_store_core::Error::new(Code::ProjectionUnavailable, error.to_string())
            })?;
        let mut updating_workspaces: Vec<_> = tickets
            .iter()
            .filter(|(workspace, ticket)| self.sweeps.finished(*workspace) < *ticket)
            .map(|(workspace, _)| *workspace)
            .collect();
        let mut rows = Vec::with_capacity(reminders.len());
        for (input, mut target) in reminders.iter().cloned().zip(joined) {
            if !target.listed && !updating_workspaces.contains(&target.target.workspace()) {
                // A completed sweep refreshed this exact workspace's listing.
                // A document absent from it cannot become ready by sweeping
                // again, even if a projection survives in another workspace.
                // Report authoritative absence for the existing target-cleanup
                // path instead of blocking every healthy sibling indefinitely.
                target.projected = true;
                target.state = TargetState::Missing;
                target.due = None;
            }
            if !target.projected && !updating_workspaces.contains(&target.target.workspace()) {
                updating_workspaces.push(target.target.workspace());
            }
            if updating_workspaces.contains(&target.target.workspace()) {
                // Rows from a previous projection generation are useful only
                // as display hints while this request's authoritative sweep is
                // incomplete. They must not produce an actionable occurrence.
                target.projected = false;
                target.state = TargetState::Missing;
                target.due = None;
            }
            let occurrence_source = target.due.as_ref().and_then(|due| {
                koru_projection::reminders::derive_due_source(&input.stored().rule, due)
            });
            let reduced = input.reduce(target.state, occurrence_source.as_ref());
            rows.push(ReminderSnapshotRow {
                target: ReminderTarget {
                    target: target.target,
                    title: target.title,
                    source_location: target.source_location,
                    state: target.state,
                    due: target.due,
                    projected: target.projected,
                },
                stored: input.into_stored(),
                occurrence_source,
                reduced,
            });
        }
        updating_workspaces.sort_unstable();
        updating_workspaces.dedup();
        Ok(ReminderSnapshot {
            reminders: rows,
            settings: settings.clone(),
            updating_workspaces,
        })
    }

    fn query(
        &self,
        reminders: &[ReminderReductionInput],
        settings: &ReminderSettings,
        query: &ReminderQuery,
    ) -> koru_store_core::Result<ReminderQueryResponse> {
        let snapshot = self.snapshot(reminders, settings)?;
        let timing = if matches!(query.view, ReminderView::Missed | ReminderView::Upcoming) {
            let now = query.now_ms.ok_or_else(|| {
                koru_store_core::Error::new(Code::InvalidRequest, "missed/upcoming requires now_ms")
            })?;
            let zone = query.device_zone.as_deref().ok_or_else(|| {
                koru_store_core::Error::new(
                    Code::InvalidRequest,
                    "missed/upcoming requires device_zone",
                )
            })?;
            let rules = koru_time_zone::IanaZoneRules::parse(zone).map_err(|_| {
                koru_store_core::Error::new(
                    Code::InvalidRequest,
                    "device_zone is not a canonical IANA zone",
                )
            })?;
            if rules.canonical_name().as_str() != zone {
                return Err(koru_store_core::Error::new(
                    Code::InvalidRequest,
                    "device_zone is not a canonical IANA zone",
                ));
            }
            Some((now, rules))
        } else {
            None
        };
        let reminders = snapshot
            .reminders
            .into_iter()
            .filter(|row| match query.view {
                ReminderView::Active => matches!(
                    row.reduced.effective,
                    EffectiveState::Scheduled { .. } | EffectiveState::Snoozed { .. }
                ),
                ReminderView::Waiting => {
                    matches!(row.reduced.effective, EffectiveState::WaitingForDue)
                }
                ReminderView::HandledHistory => !row.reduced.handled_history.is_empty(),
                ReminderView::Missed | ReminderView::Upcoming => {
                    let (now, rules) = timing.as_ref().expect("validated above");
                    let instant = match &row.reduced.effective {
                        EffectiveState::Scheduled { moment, .. } => match moment {
                            OccurrenceMoment::Zoned { value } => Some(value.instant_ms()),
                            OccurrenceMoment::Floating { wall } => {
                                let wall = chrono::NaiveDateTime::parse_from_str(
                                    wall.as_str(),
                                    "%Y-%m-%dT%H:%M:%S",
                                )
                                .ok();
                                wall.map(|wall| koru_core::time::ZoneRules::instant_of(rules, wall))
                            }
                        },
                        EffectiveState::Snoozed { until_ms, .. } => Some(*until_ms),
                        _ => None,
                    };
                    instant.is_some_and(|instant| match query.view {
                        ReminderView::Missed => instant <= *now,
                        ReminderView::Upcoming => instant > *now,
                        _ => unreachable!(),
                    })
                }
            })
            .collect();
        Ok(ReminderQueryResponse {
            reminders,
            updating_workspaces: snapshot.updating_workspaces,
        })
    }
}

/// Open the daemon-owned adapter behind the derived-data routes.
///
/// One `Arc`, coerced to each port it serves, so both routes share a single
/// connection to `projection.db`.
pub fn read_ports(
    derived: &crate::derived::Derived,
    sweeps: Sweeps,
) -> Option<(
    Arc<dyn koru_store_api::routes::workspaces::WorkspaceUsageService>,
    Arc<dyn koru_store_api::routes::workspaces::DocumentOutlineService>,
    Arc<dyn koru_store_api::state::ReminderProjectionService>,
)> {
    match derived.projections() {
        Ok(projections) => {
            let port = Arc::new(ReadPort {
                projections: Mutex::new(projections),
                sweeps,
            });
            Some((port.clone(), port.clone(), port))
        }
        Err(error) => {
            tracing::error!("the derived-data projection did not open: {error}");
            None
        }
    }
}

/// How long the worker sleeps when there is nothing to do.
///
/// It is woken by nothing — deliberately. A condvar between the store's commit
/// path and this thread would put a second synchronisation primitive in the
/// commit path to save a quarter of a second on a projection nobody is waiting
/// on, and the debounce is half a second anyway.
const IDLE: std::time::Duration = std::time::Duration::from_millis(250);

/// The store, seen through its lock.
struct StoreSource {
    state: Arc<AppState>,
    history: Mutex<HashMap<(WorkspaceId, DocumentId), AutoHistory>>,
    /// Whether this Store keeps automatic history at all. A device does. An
    /// in-memory composition does not: history is device-local, a replica held
    /// in memory is ephemeral, and markers nobody can ever read would only
    /// grow its memory.
    automatic: bool,
}

#[derive(Clone)]
struct AutoHistory {
    first_change: i64,
    last_change: i64,
    revision: loro::VersionVector,
    captured_revision: Option<loro::VersionVector>,
}

const ACTIVE_HISTORY_MS: i64 = 10 * 60 * 1_000;
const IDLE_HISTORY_MS: i64 = 2 * 60 * 1_000;

fn history_due(state: &AutoHistory, now: i64) -> Option<HistoryReason> {
    if state.captured_revision.as_ref() == Some(&state.revision) {
        None
    } else if now.saturating_sub(state.last_change) >= IDLE_HISTORY_MS {
        Some(HistoryReason::AutomaticIdle)
    } else if now.saturating_sub(state.first_change) >= ACTIVE_HISTORY_MS {
        Some(HistoryReason::AutomaticActive)
    } else {
        None
    }
}

impl StoreSource {
    fn new(state: Arc<AppState>, automatic: bool) -> Self {
        Self {
            state,
            history: Mutex::new(HashMap::new()),
            automatic,
        }
    }

    /// Called by the projection worker, never by the edit/ack path. Snapshot
    /// encoding happens after the O(1) fork and outside the Store lock.
    fn automatic_history(&self, committed: &[Committed], now: i64) {
        if !self.automatic {
            return;
        }
        let mut due = Vec::new();
        let Ok(mut schedule) = self.history.lock() else {
            return;
        };
        for event in committed.iter().filter(|event| !event.is_manifest) {
            // Encoded vectors have no canonical peer order. Decode once at
            // this boundary; history compares causal state, never bytes.
            let Ok(revision) = loro::VersionVector::decode(&event.version_vector) else {
                tracing::warn!("automatic history received an invalid version vector");
                continue;
            };
            let state = schedule
                .entry((event.workspace, event.document))
                .or_insert(AutoHistory {
                    first_change: now,
                    last_change: now,
                    revision: revision.clone(),
                    captured_revision: None,
                });
            state.last_change = now;
            state.revision = revision;
        }
        for (&key, state) in schedule.iter_mut() {
            if let Some(reason) = history_due(state, now) {
                due.push((key, reason));
                state.captured_revision = Some(state.revision.clone());
                state.first_change = now;
            }
        }
        drop(schedule);

        for ((workspace, document), reason) in due {
            let fork = match self
                .state
                .store
                .lock()
                .map_err(|_| "the store lock is poisoned".to_string())
                .and_then(|mut store| {
                    store
                        .fork_document(workspace, document)
                        .map_err(|e| e.to_string())
                }) {
                Ok(fork) => fork,
                Err(error) => {
                    tracing::warn!(%workspace, %document, "automatic history fork failed: {error}");
                    continue;
                }
            };
            let captured_revision = fork.oplog_vv();
            let version = captured_revision.encode();
            let frontiers = fork.oplog_frontiers().encode();
            let snapshot = match fork.export(loro::ExportMode::snapshot()) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    tracing::warn!(%workspace, %document, "automatic history export failed: {error}");
                    continue;
                }
            };
            let result = self
                .state
                .store
                .lock()
                .map_err(|_| "the store lock is poisoned".to_string())
                .and_then(|mut store| {
                    store
                        .record_history_state(
                            workspace,
                            document,
                            &snapshot,
                            &version,
                            &frontiers,
                            reason,
                            koru_store_core::model::Timestamp::from_millis(now),
                        )
                        .map_err(|e| e.to_string())
                });
            if let Ok(mut schedule) = self.history.lock()
                && let Some(state) = schedule.get_mut(&(workspace, document))
            {
                match result {
                    Ok(_) => {
                        // The fork may include an edit committed after this tick's
                        // notification. Remember what was actually captured so the
                        // next pass does not append a duplicate marker for it.
                        state.captured_revision = Some(captured_revision);
                    }
                    Err(error) => {
                        tracing::warn!(%workspace, %document, "automatic history capture failed: {error}");
                        state.captured_revision = None;
                    }
                }
            }
        }
    }
}

impl Source for StoreSource {
    fn fork(&self, workspace: WorkspaceId, document: DocumentId) -> Result<loro::LoroDoc, String> {
        // In, fork, out. Everything expensive happens after this returns.
        let mut store = self
            .state
            .store
            .lock()
            .map_err(|_| "the store lock is poisoned")?;
        store
            .fork_document(workspace, document)
            .map_err(|e| e.to_string())
    }

    fn revision(&self, workspace: WorkspaceId, document: DocumentId) -> Result<Vec<u8>, String> {
        let mut store = self
            .state
            .store
            .lock()
            .map_err(|_| "the store lock is poisoned")?;
        store
            .version(workspace, document)
            .map(|v| v.encode())
            .map_err(|e| e.to_string())
    }

    fn positions(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Vec<(DocumentId, Vec<u8>)>, PositionsError> {
        // **The journal's positions, which open no documents.** Asking each
        // document its version would load every one of them to discover there
        // was nothing to do — and nothing evicts, so the store would stay that
        // way. This reads rows.
        let mut store = self
            .state
            .store
            .lock()
            .map_err(|_| PositionsError::Other("the store lock is poisoned".into()))?;
        store.document_positions(workspace).map_err(|error| {
            if error.code() == Code::WorkspaceNotFound {
                PositionsError::WorkspaceNotFound
            } else {
                PositionsError::Other(error.to_string())
            }
        })
    }

    fn modified_at(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<Option<i64>, String> {
        // One indexed row out of the journal table the fork above already
        // opened. It opens no document and decodes nothing.
        let mut store = self
            .state
            .store
            .lock()
            .map_err(|_| "the store lock is poisoned")?;
        store
            .document_updated_at(workspace, document)
            .map(|at| at.map(|at| at.millis()))
            .map_err(|e| e.to_string())
    }

    fn entries(&self, workspace: WorkspaceId) -> Result<Vec<ListedDocument>, String> {
        // `documents` reads the store's own derived index; it opens the
        // workspace but not one document in it.
        let mut store = self
            .state
            .store
            .lock()
            .map_err(|_| "the store lock is poisoned")?;
        Ok(store
            .documents(workspace)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|e| ListedDocument {
                document: e.id,
                parent: e.parent,
                ordinal: e.ordinal,
                title: e.title,
                kind: e.kind,
                mime: e.mime,
                trashed_at: e.trashed_at.map(|t| t.millis()),
                trashed_by: e.trashed_by,
            })
            .collect())
    }

    fn workspaces(&self) -> Result<Vec<(WorkspaceId, String)>, String> {
        let store = self
            .state
            .store
            .lock()
            .map_err(|_| "the store lock is poisoned")?;
        Ok(store
            .workspaces()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|w| (w.id, w.name))
            .collect())
    }

    fn take_committed(&self) -> Vec<Committed> {
        let committed = match self.state.store.lock() {
            Ok(mut store) => store.take_committed(),
            Err(_) => Vec::new(),
        };
        if !committed.is_empty() {
            let mut activity = self
                .state
                .cloud_activity
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for change in &committed {
                activity.documents_changed(change.workspace.as_uuid());
            }
        }
        committed
    }

    /// Automatic history follows the projection's decision: a commit the
    /// projector does not walk — a delegated run, a schedule
    /// (`koru_projection::model::NOT_PROJECTED`) — is never enrolled here
    /// either, so a four-hour run leaves no captures in `document_versions`.
    fn record_activity(&self, followed: &[Committed], now_millis: i64) {
        self.automatic_history(followed, now_millis);
    }

    fn take_reminder_committed(&self) -> Vec<koru_store_core::ReminderCommitted> {
        let committed = match self.state.store.lock() {
            Ok(mut store) => store.take_reminder_committed(),
            Err(_) => Vec::new(),
        };
        for change in &committed {
            self.state
                .cloud_changes
                .changed(koru_store_api::cloud_changes::Dataset::Profile(
                    change.profile,
                ));
        }
        committed
    }

    fn take_dictionary_committed(&self) -> Vec<koru_store_core::DictionaryCommitted> {
        let committed = match self.state.store.lock() {
            Ok(mut store) => store.take_dictionary_committed(),
            Err(_) => Vec::new(),
        };
        for change in &committed {
            self.state
                .cloud_changes
                .changed(koru_store_api::cloud_changes::Dataset::Profile(
                    change.profile,
                ));
        }
        committed
    }

    fn take_identity_committed(&self) -> Vec<koru_store_core::IdentityCommitted> {
        let committed = match self.state.store.lock() {
            Ok(mut store) => store.take_identity_committed(),
            Err(_) => Vec::new(),
        };
        for change in &committed {
            self.state
                .cloud_changes
                .changed(koru_store_api::cloud_changes::Dataset::Profile(
                    change.profile,
                ));
        }
        committed
    }
}

/// Start the projection worker. Returns its handle so a caller could join it —
/// the daemon does not, because the process ending is how it stops — and the
/// [`Sweeps`] handle a consumer needs to ask for a workspace to be compared
/// against the store.
///
/// `consumers` are registered before the first tick, so nothing is projected
/// with a consumer missing.
///
/// `stop` ends the thread at its next tick — at most [`IDLE`] later, because
/// an idle wait is that long. A device never sets it: the process ending is
/// how its projector stops. An in-memory composition sets it when it is torn
/// down, and joins.
pub fn spawn(
    state: Arc<AppState>,
    derived: &crate::derived::Derived,
    consumers: Vec<Box<dyn koru_projection::Consumer>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> Option<(std::thread::JoinHandle<()>, Sweeps)> {
    let projections = match derived.projections() {
        Ok(p) => p,
        Err(why) => {
            // **Not fatal.** A store that cannot build a read model is a store
            // whose search is stale, not a store that has lost anything — and
            // refusing to start over derived state would be refusing to serve
            // documents because a cache is broken.
            tracing::error!("the projection layer did not start: {why}");
            return None;
        }
    };

    let published = state.projection.clone();
    let mut projector = Projector::new(StoreSource::new(state, !derived.is_memory()), projections);
    // The counting consumer stays: it costs a vector push per projection and it
    // is what proves the seam still works when the real consumer is the thing
    // being debugged.
    projector.add_consumer(Box::new(Counting::default()));
    for consumer in consumers {
        projector.add_consumer(consumer);
    }
    let sweeps = projector.sweeps();
    let worker_sweeps = sweeps.clone();

    let handle = std::thread::Builder::new()
        .name("koru-projection".into())
        .spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                let now = koru_store_core::model::Timestamp::now().millis();
                let busy = projector.tick(now);
                if let Ok(mut slot) = published.lock() {
                    *slot = report(&projector.stats());
                }
                if !busy {
                    worker_sweeps.wait_for_request(IDLE);
                }
            }
        })
        .ok();
    if handle.is_some() {
        tracing::info!("projection: running");
    }
    handle.map(|handle| (handle, sweeps))
}

fn report(stats: &koru_projection::ProjectorStats) -> serde_json::Value {
    serde_json::json!({
        "schema_version": koru_projection::PROJECTION_SCHEMA_VERSION,
        "pending": stats.pending,
        "projected": stats.projected,
        "removed": stats.removed,
        "listings": stats.listings,
        "failed": stats.failed,
        "sweeps": stats.sweeps,
        "sweeping": stats.sweeping,
        "sweep_remaining": stats.sweep_remaining,
        // Documents a sweep reached by *comparing* rather than by being told.
        // **Not a count of missed events**: a workspace's first sweep finds
        // everything it has never projected, which is the mechanism working. It
        // climbing after a workspace is warm is the signal.
        "stale_found": stats.stale_found,
        "incompatible": stats.incompatible,
        // Commits to a run or a schedule, which are neither walked nor
        // captured. Climbing is a run taking steps, not a fault.
        "skipped": stats.skipped,
        // Documents the layer has stopped retrying. Non-zero means the read
        // model is missing content until those documents change again.
        "abandoned": stats.abandoned,
        "last_millis": stats.last_millis,
        "total_millis": stats.total_millis,
        "max_millis": stats.max_millis,
        "fork_millis": stats.fork_millis,
        "walk_millis": stats.walk_millis,
        "write_millis": stats.write_millis,
        "last_error": stats.last_error,
    })
}

#[cfg(test)]
mod automatic_history_tests {
    use super::*;

    fn state(first_change: i64, last_change: i64) -> AutoHistory {
        AutoHistory {
            first_change,
            last_change,
            revision: loro::VersionVector::default(),
            captured_revision: None,
        }
    }

    #[test]
    fn an_idle_document_is_due_after_two_minutes() {
        let state = state(0, 10);
        assert_eq!(history_due(&state, 10 + IDLE_HISTORY_MS - 1), None);
        assert_eq!(
            history_due(&state, 10 + IDLE_HISTORY_MS),
            Some(HistoryReason::AutomaticIdle)
        );
    }

    #[test]
    fn continuous_editing_is_due_after_ten_minutes() {
        let state = state(0, ACTIVE_HISTORY_MS - 1);
        assert_eq!(
            history_due(&state, ACTIVE_HISTORY_MS),
            Some(HistoryReason::AutomaticActive)
        );
    }

    #[test]
    fn equivalent_peer_vectors_with_different_encodings_are_not_captured_again() {
        // A two-peer document and its fork can encode the same vector in
        // opposite peer order. Fixed encodings avoid relying on hash order.
        let committed = [2, 2, 2, 1, 2];
        let captured = [2, 1, 2, 2, 2];
        assert_ne!(committed, captured);
        let mut state = state(0, 0);
        state.revision = loro::VersionVector::decode(&committed).unwrap();
        state.captured_revision = Some(loro::VersionVector::decode(&captured).unwrap());
        assert_eq!(state.captured_revision.as_ref(), Some(&state.revision));
        assert_eq!(history_due(&state, ACTIVE_HISTORY_MS * 2), None);
    }

    #[test]
    fn an_already_captured_revision_is_not_due_again() {
        let mut state = state(0, 0);
        state.captured_revision = Some(state.revision.clone());
        assert_eq!(history_due(&state, ACTIVE_HISTORY_MS * 2), None);
    }
}

#[cfg(test)]
mod reminder_tests;

#[cfg(test)]
mod not_projected_tests;

/// `koru-store-server projection [<document id>]` — look at the read model.
///
/// **The tool that would otherwise be a Python incantation.** Everything about
/// this layer is exercised by tests; this is for the times somebody needs to
/// *look* — a document that will not turn up in search, a counter that seems
/// stuck, a rebuild that seems not to have happened. It reads `projection.db`
/// directly rather than going through the store, because the read model is a
/// local file and a diagnostic that needs the daemon to be healthy is a
/// diagnostic that is missing exactly when it is wanted.
pub fn report_to_console(
    root: &std::path::Path,
    target: Option<&str>,
) -> koru_store_core::errors::Result<()> {
    use koru_store_core::errors::{Code, Error};

    let path = root.join("projection.db");
    if !path.exists() {
        println!("No projection at {}.", path.display());
        println!("Nothing has used a workspace yet, or it has been deleted — either way it");
        println!("is rebuilt the next time a workspace is written to.");
        return Ok(());
    }
    let projections = Projections::open(root)
        .map_err(|e| Error::new(Code::StorageFailed, format!("projection: {e}")))?;

    let Some(target) = target else {
        let revisions = projections
            .revisions()
            .map_err(|e| Error::new(Code::StorageFailed, format!("projection: {e}")))?;
        println!("{}", path.display());
        println!("{} document(s) projected", revisions.len());
        for (document, revision) in revisions {
            // The revision is an encoded version vector — opaque, and long. Its
            // length is the honest thing to show: what a person is checking is
            // whether two of them differ, and for that the id and a short digest
            // are enough.
            println!("  {document}  rev {:>4} bytes", revision.len());
        }
        println!();
        println!("A document id prints that document as the projection has it.");
        return Ok(());
    };

    let document = DocumentId::parse(target).ok_or_else(|| {
        Error::new(
            Code::InvalidRequest,
            format!("{target} is not a document id"),
        )
    })?;
    let Some(projection) = projections
        .read(document)
        .map_err(|e| Error::new(Code::StorageFailed, format!("projection: {e}")))?
    else {
        println!("{document} has no projection.");
        return Ok(());
    };

    println!("workspace {}", projection.workspace);
    println!("document  {}", projection.document);
    println!("{} node(s)", projection.nodes.len());
    println!();
    for node in &projection.nodes {
        let indent = "  ".repeat(node.depth);
        let mut marks = Vec::new();
        if let Some(h) = node.heading {
            marks.push(format!("h{h}"));
        }
        if node.done {
            marks.push("done".into());
        }
        if !node.runs.is_empty() {
            marks.push(format!("{} run(s)", node.runs.len()));
        }
        for attr in &node.attrs {
            marks.push(format!("{}={} ({})", attr.role, attr.value, attr.kind));
        }
        let marks = if marks.is_empty() {
            String::new()
        } else {
            format!("   [{}]", marks.join(", "))
        };
        println!("{indent}{:10} {}{marks}", node.kind, node.text);
    }
    Ok(())
}
