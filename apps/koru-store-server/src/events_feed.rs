// SPDX-License-Identifier: MPL-2.0

//! The commit feed's exit into the events worker.
//!
//! One more [`koru_projection::Consumer`] beside the search feed — told after
//! a projection is durable, off the store lock, on the projector's thread.
//! It *forwards* (ids, revisions, trash flags — never content) and the events
//! worker derives `document.created`/`updated`/`trashed`/… against its own
//! `doc_state` cursor. A transient journal failure leaves a coalesced note in
//! this consumer and an idle projector tick retries it. A process crash may
//! lose that in-memory retry buffer; Piece 4's startup/reconnect reconciliation
//! repairs it from a current authoritative snapshot.
//!
//! **It also says when a workspace's tasks changed** (`projection.tasks.changed`). The
//! Tasks surfaces read the read model, and until this they learned it had
//! moved only from their own writes and a half-minute timer, so a task moved
//! on another device sat in its old place until the timer or a change of
//! view. The feed is told of every projection whatever its source, so it is
//! where *the answer moved* can be said once for all of them: it keeps a
//! digest of each document's task rows, and a document projected with a
//! different one marks its workspace. One note per marked workspace leaves
//! on the next projector tick — a renumbered list of forty tasks is one
//! event, not forty.
//!
//! Before the consumer is registered, [`prepare`] seeds the worker's baseline
//! from the projection's own listings — an upgrade on an existing store must
//! not announce every document as born today. Seeding is idempotent on the
//! worker's side, so a crash between the seed and the mark re-runs safely.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::path::Path;

use koru_events_server::{BaselineEntry, EventsHandle, FeedNote, IdentityWake, ReminderWake};
use koru_projection::model::apply_inherited_trash;
use koru_projection::{Consumer, DocumentProjection, Projections};
use koru_store_core::ids::{DocumentId, WorkspaceId};

/// Seed the baseline and build the consumer. **Not fatal**, like the search
/// feed's `prepare`: a bus that will not open is a store whose events are
/// missing, not a store that has lost anything.
pub fn prepare(root: &Path, handle: EventsHandle) -> Option<Box<dyn Consumer>> {
    let projections = match Projections::open(root) {
        Ok(projections) => projections,
        Err(why) => {
            tracing::error!("the events feed has no read model to consume: {why}");
            return None;
        }
    };
    match handle.status() {
        Ok(status) if !status.disabled && !status.baseline_seeded => {
            match baseline(&projections) {
                Ok(entries) => {
                    if let Err(why) = handle.seed_baseline(&entries) {
                        tracing::error!("the events baseline did not seed: {why}");
                        // Feeding an unseeded journal records without
                        // announcing — safe, just quiet — so the feed still
                        // registers and a later run's seed completes it.
                    }
                }
                Err(why) => {
                    tracing::error!("the events baseline could not be read: {why}");
                }
            }
        }
        Ok(_) => {}
        Err(why) => {
            tracing::error!("the events worker did not answer at wiring: {why}");
            return None;
        }
    }
    Some(Box::new(EventsFeed {
        sink: Box::new(handle),
        projections,
        pending: BTreeMap::new(),
        task_digests: HashMap::new(),
        tasks_dirty: BTreeSet::new(),
    }))
}

/// What already exists, from the projection's listings and revisions —
/// recorded in the worker without emitting.
fn baseline(projections: &Projections) -> Result<Vec<BaselineEntry>, String> {
    let revisions = projections.revisions().map_err(|error| error.to_string())?;
    let mut entries = Vec::new();
    for (workspace, _digest) in projections.listings().map_err(|error| error.to_string())? {
        let Some(listing) = projections
            .listing(workspace)
            .map_err(|error| error.to_string())?
        else {
            continue;
        };
        let mut documents = listing.documents;
        apply_inherited_trash(&mut documents);
        for document in documents {
            entries.push(BaselineEntry {
                workspace: workspace.to_string(),
                document: document.document.to_string(),
                revision: revisions
                    .get(&document.document)
                    .cloned()
                    .unwrap_or_default(),
                trashed: document.trashed_at.is_some(),
            });
        }
    }
    Ok(entries)
}

/// The other half of this seam: **the Store's own committed catalog facts**.
///
/// The feed above *derives* document events from the projection, one layer
/// removed and after a sweep. A catalog mutation has no projection to be
/// derived from — a workspace created and never written to produces no commit
/// note at all — so `koru-store-api` publishes its own fact through this port,
/// in the handler, after the store call that persisted it returned and released
/// the lock. Same journal, same worker, opposite direction.
///
/// The actor is the worker's own `Actor::system("store")`, because the Store is
/// what committed the change; which client asked is not a fact about the
/// catalog.
///
/// **A failed record is a delay, never a failed request.** The store must not
/// refuse to rename a workspace because an observation could not be written
/// down — the ADR-101 rule the feed obeys, held on this side too.
pub struct CatalogEvents {
    handle: EventsHandle,
}

impl CatalogEvents {
    pub fn new(handle: EventsHandle) -> Self {
        Self { handle }
    }
}

impl koru_store_api::CatalogEventSink for CatalogEvents {
    fn publish(&self, event: koru_events::Draft) {
        let envelope = event.into_envelope(&self.handle.state.ctx);
        if let Err(why) = self.handle.state.append(std::slice::from_ref(&envelope)) {
            tracing::warn!(
                name = %envelope.name,
                "a committed catalog fact was not recorded: {why}"
            );
        }
    }
}

struct EventsFeed {
    sink: Box<dyn FeedSink>,
    projections: Projections,
    /// Latest undelivered fact per content-free identity. Repeated writes to
    /// one document/reminder collapse while the journal is unavailable.
    pending: BTreeMap<String, FeedNote>,
    /// What each document's task rows came to when it was last projected in
    /// this process ([`task_digest`]). In memory only: a consumer owns its
    /// cursor, and losing this one costs a single extra `projection.tasks.changed` per
    /// document after a restart, never a missed one.
    task_digests: HashMap<(WorkspaceId, DocumentId), u64>,
    /// The workspaces whose task rows moved since the last projector tick.
    tasks_dirty: BTreeSet<WorkspaceId>,
}

/// The digest of a document with no task rows at all.
const NO_TASKS: u64 = 0;

/// What a document's task rows hash to.
///
/// **Every node that carries an attribute**, whole. A task is a node with
/// `koru.task` or a due, and everything a task row shows beside its words —
/// its date, level, colour, list and place in the list — is an attribute
/// too, so the attributed nodes are a superset of the rows the task query
/// can answer with. A superset is the right side to err on: an attributed
/// node that is not a task costs one query nobody needed, and a narrower
/// rule would have to track the read model's own derivation to stay right.
/// Plain text — the paragraph somebody is typing — has no attributes and
/// moves nothing.
fn task_digest(projection: &DocumentProjection) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut any = false;
    for node in projection
        .nodes
        .iter()
        .filter(|node| !node.attrs.is_empty())
    {
        any = true;
        node.hash(&mut hasher);
    }
    match (any, hasher.finish()) {
        (false, _) => NO_TASKS,
        (true, NO_TASKS) => 1,
        (true, digest) => digest,
    }
}

trait FeedSink: Send {
    fn feed(&self, notes: &[FeedNote]) -> Result<u32, String>;
}

impl FeedSink for EventsHandle {
    fn feed(&self, notes: &[FeedNote]) -> Result<u32, String> {
        EventsHandle::feed(self, notes).map_err(|error| error.to_string())
    }
}

impl EventsFeed {
    fn deliver(&mut self, notes: Vec<FeedNote>) {
        for note in notes {
            self.pending.insert(note_key(&note), note);
        }
        self.flush();
    }

    /// One `projection.tasks.changed` per workspace marked since the last tick.
    fn announce_tasks(&mut self) {
        for workspace in std::mem::take(&mut self.tasks_dirty) {
            let note = FeedNote::TasksChanged {
                workspace: workspace.to_string(),
            };
            self.pending.insert(note_key(&note), note);
        }
    }

    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let notes: Vec<_> = self.pending.values().cloned().collect();
        if let Err(why) = self.sink.feed(&notes) {
            tracing::warn!("the events worker missed a feed note: {why}");
            return;
        }
        self.pending.clear();
    }
}

fn note_key(note: &FeedNote) -> String {
    match note {
        FeedNote::Projected {
            workspace,
            document,
            ..
        }
        | FeedNote::Removed {
            workspace,
            document,
        } => format!("document:{workspace}:{document}"),
        FeedNote::Listing { workspace, .. } => format!("listing:{workspace}"),
        FeedNote::TasksChanged { workspace } => format!("tasks:{workspace}"),
        FeedNote::ReminderChanged { profile, change } => {
            format!("reminder:{profile}:{change:?}")
        }
        FeedNote::DictionaryChanged { profile } => format!("dictionary:{profile}"),
        FeedNote::ProfileIdentityChanged { profile, change } => {
            format!("identity:{profile}:{change:?}")
        }
        FeedNote::DueChanged {
            workspace,
            document,
            entity,
        } => format!("due:{workspace}:{document}:{entity}"),
        FeedNote::EntityRemoved {
            workspace,
            document,
            entity,
        } => format!("entity:{workspace}:{document}:{entity}"),
    }
}

impl Consumer for EventsFeed {
    fn name(&self) -> &str {
        "events"
    }

    fn flush_pending(&mut self) {
        // Once per projector tick, idle ticks included: everything that
        // marked a workspace during the last one leaves as a single note.
        self.announce_tasks();
        self.flush();
    }

    fn projected(&mut self, projection: &DocumentProjection) {
        self.deliver(vec![FeedNote::Projected {
            workspace: projection.workspace.to_string(),
            document: projection.document.to_string(),
            revision: projection.source_revision.clone(),
        }]);
        let digest = task_digest(projection);
        let moved = match self
            .task_digests
            .insert((projection.workspace, projection.document), digest)
        {
            Some(previous) => previous != digest,
            // First sight since this process started. A document that holds
            // tasks may have just changed one; one that holds none either
            // never did or lost its last, and that is said by the attribute
            // and removal diffs below.
            None => digest != NO_TASKS,
        };
        if moved {
            self.tasks_dirty.insert(projection.workspace);
        }
    }

    fn removed(&mut self, workspace: WorkspaceId, document: DocumentId) {
        self.deliver(vec![FeedNote::Removed {
            workspace: workspace.to_string(),
            document: document.to_string(),
        }]);
        // Gone with whatever tasks it held; unknown counts as held.
        if self.task_digests.remove(&(workspace, document)) != Some(NO_TASKS) {
            self.tasks_dirty.insert(workspace);
        }
    }

    fn listing_changed(&mut self, workspace: WorkspaceId) {
        // A document binned, restored, renamed or moved changes which tasks
        // the query lists and the path each row shows.
        self.tasks_dirty.insert(workspace);
        let listing = match self.projections.listing(workspace) {
            Ok(Some(listing)) => listing,
            Ok(None) => return,
            Err(why) => {
                tracing::warn!("the events feed could not read a listing: {why}");
                return;
            }
        };
        let mut documents = listing.documents;
        apply_inherited_trash(&mut documents);
        self.deliver(vec![FeedNote::Listing {
            workspace: workspace.to_string(),
            documents: documents
                .into_iter()
                .map(|document| (document.document.to_string(), document.trashed_at.is_some()))
                .collect(),
        }]);
    }

    fn attribute_events(&mut self, events: &[koru_projection::AttributeEvent]) {
        self.deliver(due_notes(events));
        // An attribute set, moved or cleared is a task row changing — and
        // the one way a document seen for the first time says it lost one.
        self.tasks_dirty
            .extend(events.iter().map(|event| event.workspace));
    }

    fn reminder_changes(&mut self, events: &[koru_store_core::ReminderCommitted]) {
        self.deliver(reminder_notes(events));
    }

    fn dictionary_changes(&mut self, events: &[koru_store_core::DictionaryCommitted]) {
        self.deliver(
            events
                .iter()
                .map(|event| FeedNote::DictionaryChanged {
                    profile: event.profile.to_storage(),
                })
                .collect(),
        );
    }

    fn identity_changes(&mut self, events: &[koru_store_core::IdentityCommitted]) {
        self.deliver(
            events
                .iter()
                .map(|event| FeedNote::ProfileIdentityChanged {
                    profile: event.profile.to_storage(),
                    change: match event.change {
                        koru_store_core::IdentityChange::Name => IdentityWake::Name,
                        koru_store_core::IdentityChange::Appearance => IdentityWake::Appearance,
                    },
                })
                .collect(),
        );
    }

    fn entities_removed(&mut self, workspace: WorkspaceId, document: DocumentId, nodes: &[String]) {
        self.deliver(entity_removed_notes(workspace, document, nodes));
        if !nodes.is_empty() {
            self.tasks_dirty.insert(workspace);
        }
    }
}

fn due_notes(events: &[koru_projection::AttributeEvent]) -> Vec<FeedNote> {
    events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                koru_projection::AttributeEventKind::DueChanged { .. }
            )
        })
        .map(|event| FeedNote::DueChanged {
            workspace: event.workspace.to_storage(),
            document: event.document.to_storage(),
            entity: event.node.clone(),
        })
        .collect()
}

fn reminder_notes(events: &[koru_store_core::ReminderCommitted]) -> Vec<FeedNote> {
    events
        .iter()
        .map(|event| FeedNote::ReminderChanged {
            profile: event.profile.to_storage(),
            change: match event.change {
                koru_store_core::reminders::ReminderChange::Rule => ReminderWake::Rule,
                koru_store_core::reminders::ReminderChange::Action => ReminderWake::Action,
                koru_store_core::reminders::ReminderChange::History => ReminderWake::History,
                koru_store_core::reminders::ReminderChange::Settings => ReminderWake::Settings,
            },
        })
        .collect()
}

fn entity_removed_notes(
    workspace: WorkspaceId,
    document: DocumentId,
    nodes: &[String],
) -> Vec<FeedNote> {
    nodes
        .iter()
        .map(|entity| FeedNote::EntityRemoved {
            workspace: workspace.to_storage(),
            document: document.to_storage(),
            entity: entity.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests;
