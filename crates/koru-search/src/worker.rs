// SPDX-License-Identifier: MPL-2.0

//! Keeping the index level with the read model.
//!
//! ## The consumer only enqueues
//!
//! [`Consumer::projected`] is called on the *projector's* thread. [`Feed`]
//! records `(workspace, document)` and returns; it does not read, tokenize or
//! write. The trait's own contract is that a slow consumer must not hold the
//! projection back, and an index rebuild after a schema bump has to be batched
//! the way a sweep is — which is a state machine of its own.
//!
//! ## It re-reads rather than keeping what it was handed
//!
//! `projected` is given the whole `DocumentProjection` and throws it away. That
//! costs a re-read of rows still warm in the page cache, and it buys one code
//! path: **the trigger and the reconcile do the same thing**. The path that
//! runs after a restart is the path that runs every day, so it cannot rot.
//!
//! ## Reconciling is the mechanism, not a safety net
//!
//! Everything the index learns has a comparison behind it — its
//! `indexed_revision` against the projection's `source_revision`, and its
//! consumed listing digest against the projection's. A missed event is a delay.
//! An event that never arrives because the index was stopped, or because its
//! own schema moved and it threw everything away, is the same delay.
//!
//! ## A document nothing lists is deferred, not retried forever
//!
//! Two rules meet badly. A document with no listing row must not be indexed —
//! the hit would open nothing — and it is given no cursor either, because
//! nothing was consumed. So the reconcile found it behind again, and again.
//! On a real installation that was 5,746 of 6,754 projected documents: the
//! indexer never idled, the queue never emptied, and every search reported
//! every workspace as *updating* forever. A miss is now remembered against the
//! revision it happened at, and retried when the document's revision moves or
//! its workspace's listing changes — the only two things that could make it
//! indexable.
//!
//! ## The tag table rides the same seam
//!
//! A tag is indexed by its **name**, which lives in the workspace's
//! `koru-system` document rather than beside the id on the node
//! ([`crate::tags`], ADR-182). That document is a document: it is projected,
//! and [`Consumer::projected`] queues it like any other. So a rename needs no
//! second channel — it arrives as *the system document changed*, and this
//! module turns that into "re-read the table, and re-index the documents
//! carrying an id whose **name** moved". A colour, a new attribute definition
//! and a merge onto the same spelling all reproject the same document and
//! enqueue nothing, because none of them changes a byte of any fragment.
//!
//! ## Readings do not ride it, and say so through the queue
//!
//! The words read out of a picture or a file come through the [`Readings`]
//! seam, asked whenever a document holding one is indexed. But a reading lives
//! in a document the projector never walks (TECHNICAL D14), so a reading that
//! *lands* reaches nobody here by itself. The daemon's readings mirror says so
//! through two public doors on [`Queue`]: [`Queue::enqueue_document`] for the
//! reading's parent, and [`Queue::enqueue_hashes`] for every other document in
//! the workspace carrying a changed hash, which this module turns into
//! documents with one read of the projection.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use koru_projection::model::DocumentProjection;
use koru_projection::{Consumer, Projections};
use koru_store_core::ids::{DocumentId, WorkspaceId};

use crate::lists::ListNames;
use crate::model::{IndexStats, SearchResult};
use crate::readings::{ReadingText, Readings};
use crate::storage::{SEARCH_SCHEMA_VERSION, SearchIndex};
use crate::tags::{KIND_SYSTEM, TAG_ROLE, TagNames, TagTable};

/// How long the indexer waits, with nothing queued, before comparing itself
/// against the projection again.
///
/// Measurable rather than settled. Short enough that a missed event is a
/// hiccup; long enough that an idle store is not scanning two tables every
/// quarter of a second.
pub const RECONCILE_MILLIS: i64 = 2_000;

/// What one tick will index before yielding.
pub const BATCH: usize = 8;

/// Work waiting for the indexer, and what it has been doing.
///
/// **Shared, in memory, and never on disk.** The query side reads
/// [`pending_workspaces`](Self::pending_workspaces) to fill
/// `SearchPage.updating_workspaces`; writing that to SQLite would put a write
/// in the path of every keystroke to answer a question that is only about right
/// now.
#[derive(Clone, Default)]
pub struct Queue(Arc<Mutex<Inner>>);

#[derive(Default)]
struct Inner {
    documents: Vec<(WorkspaceId, DocumentId)>,
    removed: Vec<DocumentId>,
    listings: Vec<WorkspaceId>,
    /// Blob hashes whose reading changed, per workspace, not yet turned into
    /// the documents that carry them. That turn is a read of `projection.db`,
    /// so it happens on the indexer's thread rather than the caller's.
    hashes: HashMap<WorkspaceId, HashSet<String>>,
    stats: IndexStats,
}

impl Queue {
    pub fn stats(&self) -> IndexStats {
        let inner = self.lock();
        let mut stats = inner.stats.clone();
        stats.schema_version = SEARCH_SCHEMA_VERSION;
        stats.pending =
            inner.documents.len() + inner.removed.len() + inner.listings.len() + inner.hashes.len();
        stats
    }

    /// Workspaces with work outstanding — what a query reports as updating.
    pub fn pending_workspaces(&self) -> Vec<WorkspaceId> {
        let inner = self.lock();
        let mut all: Vec<_> = inner
            .documents
            .iter()
            .map(|(w, _)| *w)
            .chain(inner.listings.iter().copied())
            .chain(inner.hashes.keys().copied())
            .collect();
        all.sort_by_key(|w| w.to_storage());
        all.dedup();
        all
    }

    pub fn is_empty(&self) -> bool {
        let inner = self.lock();
        inner.documents.is_empty()
            && inner.removed.is_empty()
            && inner.listings.is_empty()
            && inner.hashes.is_empty()
    }

    /// **Re-index one document**, as though the projector had announced it.
    ///
    /// For a caller outside the projector that knows a document's *search
    /// text* moved while its projection did not — the Store daemon's readings
    /// mirror, when a reading lands for a file or a page (TECHNICAL §7 "Never
    /// walked, never versioned, and still heard"). The reading document is
    /// never projected, so nothing else would ever tell the index.
    ///
    /// Cheap and safe from any thread: it takes this queue's mutex for a push
    /// and does no I/O. A document already queued is not queued twice. A
    /// document no listing names is dropped by the indexer, as any other is.
    pub fn enqueue_document(&self, workspace: WorkspaceId, document: DocumentId) {
        self.push_document(workspace, document);
    }

    /// **Re-index every document in `workspace` that carries one of
    /// `hashes`** — a picture or a file whose reading changed.
    ///
    /// The hashes are recorded and the call returns. The indexer turns them
    /// into documents on its own thread, with one read of `projection.db`
    /// ([`Projections::documents_carrying`]), and queues exactly those: one
    /// picture pasted into ten pages re-indexes ten documents, and nothing
    /// else in the workspace.
    ///
    /// Cheap and safe from any thread, as [`enqueue_document`](Self::enqueue_document)
    /// is: a mutex and a set insert. Hashes for one workspace accumulate until
    /// the indexer takes them, so a burst of readings landing costs one query.
    /// `b3:<hex>`, as `ProjectedNode::blob` spells them.
    pub fn enqueue_hashes<I, S>(&self, workspace: WorkspaceId, hashes: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let fresh: Vec<String> = hashes.into_iter().map(Into::into).collect();
        if fresh.is_empty() {
            return;
        }
        self.lock()
            .hashes
            .entry(workspace)
            .or_default()
            .extend(fresh);
    }

    fn push_document(&self, workspace: WorkspaceId, document: DocumentId) {
        let mut inner = self.lock();
        if !inner.documents.iter().any(|(_, d)| *d == document) {
            inner.documents.push((workspace, document));
        }
    }

    fn push_listing(&self, workspace: WorkspaceId) {
        let mut inner = self.lock();
        if !inner.listings.contains(&workspace) {
            inner.listings.push(workspace);
        }
    }

    fn push_removed(&self, document: DocumentId) {
        let mut inner = self.lock();
        inner.documents.retain(|(_, d)| *d != document);
        if !inner.removed.contains(&document) {
            inner.removed.push(document);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What the projector tells. Registered with `Projector::add_consumer`.
pub struct Feed {
    queue: Queue,
}

impl Feed {
    pub fn new(queue: Queue) -> Self {
        Self { queue }
    }
}

impl Consumer for Feed {
    fn name(&self) -> &str {
        "search"
    }

    fn projected(&mut self, projection: &DocumentProjection) {
        self.queue
            .push_document(projection.workspace, projection.document);
    }

    fn removed(&mut self, _workspace: WorkspaceId, document: DocumentId) {
        self.queue.push_removed(document);
    }

    fn listing_changed(&mut self, workspace: WorkspaceId) {
        self.queue.push_listing(workspace);
    }

    fn source_incompatible(&mut self, _workspace: WorkspaceId, document: DocumentId) {
        self.queue.push_removed(document);
    }
}

/// Turns the read model into an index, one document at a time.
pub struct Indexer {
    index: SearchIndex,
    projections: Projections,
    queue: Queue,
    /// When the last reconcile ran, so an idle store is not comparing
    /// constantly. `None` means one is owed — which it is at construction,
    /// because that is when this may have missed the most.
    reconciled_at: Option<i64>,
    /// Where a workspace's tag names come from. `None` in a host that wired
    /// none — tags are then indexed as the ids they were before this existed,
    /// rather than disappearing.
    tags: Option<Box<dyn TagNames>>,
    /// One table per workspace, read on first need and replaced when that
    /// workspace's system document reprojects.
    ///
    /// **In memory and never on disk.** It is a projection of a projection: the
    /// cost of losing it is one fork per workspace, and persisting it would be
    /// a third copy of a table whose whole point is that there is one.
    /// `Arc` so a document's fragments can be built without cloning it or
    /// holding a borrow of `self` across the write.
    tag_tables: HashMap<WorkspaceId, Arc<TagTable>>,
    /// How many documents the last reconcile found that no listing names, and
    /// what was last said about them.
    ///
    /// Kept so the count can be published beside `behind`, and so the warning
    /// is said when the number *changes* rather than every two seconds - the
    /// same reason `failed` dedupes by message.
    unlisted: usize,
    said_unlisted: Option<usize>,
    /// Where the words read out of a workspace's pictures and files come from.
    /// `None` in a host that wired none — a picture and a file are then found
    /// by their names alone, which is what the index did before readings.
    readings: Option<Box<dyn Readings>>,
    /// Documents indexed while the readings source could not answer, with the
    /// hashes they asked about, by workspace.
    ///
    /// **The `TagNames` lesson, applied before it had to be learned again.**
    /// `None` means *not now*, so such a document is indexed without words —
    /// and nothing else would ever ask again, because no reading changed and
    /// the document did not either. The reconcile asks the source once per
    /// workspace whether it can answer yet, and re-queues these when it can.
    /// In memory, bounded by the documents that hold a picture or a file.
    unread: HashMap<WorkspaceId, HashMap<DocumentId, Vec<String>>>,
    /// Where a workspace's task lists come from. `None` indexes no list.
    lists: Option<Box<dyn ListNames>>,
    /// Workspaces whose lists and page-task markers this process has brought
    /// up to date once ([`bootstrap`](Self::bootstrap)).
    bootstrapped: HashSet<WorkspaceId>,
}

impl Indexer {
    pub fn new(index: SearchIndex, projections: Projections, queue: Queue) -> Self {
        Self {
            index,
            projections,
            queue,
            reconciled_at: None,
            tags: None,
            tag_tables: HashMap::new(),
            unlisted: 0,
            said_unlisted: None,
            readings: None,
            unread: HashMap::new(),
            lists: None,
            bootstrapped: HashSet::new(),
        }
    }

    /// Install the task-list source, as [`with_tag_names`](Self::with_tag_names)
    /// installs the tag table's. Without one no list is indexed and nothing
    /// else changes.
    pub fn with_list_names(mut self, lists: Box<dyn ListNames>) -> Self {
        self.lists = Some(lists);
        self
    }

    /// Install the tag-name source. The daemon does this in `search::spawn`,
    /// because reading the table needs the store's lock and `prepare` runs
    /// before there is one.
    pub fn with_tag_names(mut self, tags: Box<dyn TagNames>) -> Self {
        self.tags = Some(tags);
        self
    }

    /// Install the readings source, as [`with_tag_names`](Self::with_tag_names)
    /// installs the tag table's: the daemon's readings mirror implements
    /// [`Readings`], and this is the only door it comes in by.
    ///
    /// With one installed, indexing a document asks it for the hashes of that
    /// document's own live pictures and files, so a picture pasted into a page
    /// is findable by words read elsewhere the moment the page is indexed.
    pub fn with_readings(mut self, readings: Box<dyn Readings>) -> Self {
        self.readings = Some(readings);
        self
    }

    pub fn queue(&self) -> Queue {
        self.queue.clone()
    }

    pub fn index(&self) -> &SearchIndex {
        &self.index
    }

    /// Do one unit of work. Returns whether there is more to do now.
    ///
    /// Order matters: removals first, because indexing a document the store has
    /// dropped is work whose only result is a hit nobody can open; then
    /// listings, because a title a document is about to be indexed under should
    /// be the current one; then changed readings, because they only ever turn
    /// into documents; then documents.
    pub fn tick(&mut self, now_millis: i64) -> bool {
        if self.remove_due() {
            return true;
        }
        if self.listing_due(now_millis) {
            return true;
        }
        if self.hashes_due() {
            return true;
        }
        if self.index_due(now_millis) {
            return true;
        }
        self.reconcile(now_millis)
    }

    fn remove_due(&mut self) -> bool {
        let Some(document) = self.queue.lock().removed.pop() else {
            return false;
        };
        for documents in self.unread.values_mut() {
            documents.remove(&document);
        }
        match self.index.remove_document(&document.to_storage()) {
            Ok(()) => {
                self.queue.lock().stats.removed += 1;
            }
            Err(why) => self.failed(why.0),
        }
        true
    }

    fn listing_due(&mut self, now_millis: i64) -> bool {
        let Some(workspace) = self.queue.lock().listings.pop() else {
            return false;
        };
        self.consume_listing(workspace, now_millis);
        true
    }

    fn consume_listing(&mut self, workspace: WorkspaceId, _now_millis: i64) {
        match self.projections.listing(workspace) {
            Ok(Some(listing)) => {
                if let Err(why) = self.index.set_listing(
                    &workspace.to_storage(),
                    &listing.name,
                    &listing.digest,
                    &listing.documents,
                ) {
                    return self.failed(why.0);
                }
                // **A document deferred for want of a row this listing now has
                self.queue.lock().stats.listings += 1;
            }
            // The projection has nothing for this workspace: it was dropped, or
            // it was never mirrored. Either way the index must not keep results
            // for it.
            Ok(None) => {
                if let Err(why) = self.index.remove_workspace(&workspace.to_storage()) {
                    self.failed(why.0);
                }
            }
            Err(why) => self.failed(why.0),
        }
    }

    /// Turn one workspace's changed hashes into the documents that carry them,
    /// and queue those.
    ///
    /// **One query, and only the documents it names.** A reading landing must
    /// not cost a workspace walk; the read model already knows which nodes
    /// hold which blob. A failed read is counted and the hashes are dropped
    /// rather than retried in a loop: the parent document was enqueued by
    /// name beside them, and the next change to any carrier re-indexes it.
    fn hashes_due(&mut self) -> bool {
        let taken = {
            let mut inner = self.queue.lock();
            let Some(workspace) = inner.hashes.keys().next().copied() else {
                return false;
            };
            inner
                .hashes
                .remove(&workspace)
                .map(|hashes| (workspace, hashes))
        };
        let Some((workspace, hashes)) = taken else {
            return false;
        };
        let hashes: Vec<String> = hashes.into_iter().collect();
        match self.projections.documents_carrying(workspace, &hashes) {
            Ok(documents) => {
                // **Counts and ids only.** A hash is not a word anybody wrote,
                // but it is the identity of somebody's file, and the count is
                // all a reader of the log needs.
                tracing::debug!(
                    target: "koru_search",
                    %workspace,
                    hashes = hashes.len(),
                    documents = documents.len(),
                    "readings changed; re-indexing the documents that carry them"
                );
                for document in documents {
                    self.queue.push_document(workspace, document);
                }
            }
            Err(why) => self.failed(why.0),
        }
        true
    }

    fn index_due(&mut self, now_millis: i64) -> bool {
        let batch: Vec<_> = {
            let mut inner = self.queue.lock();
            let taken = inner.documents.len().min(BATCH);
            inner.documents.drain(..taken).collect()
        };
        if batch.is_empty() {
            return false;
        }
        for (workspace, document) in batch {
            self.index_one(workspace, document, now_millis);
        }
        true
    }

    /// Read the projection, build fragments, replace that document's rows.
    fn index_one(&mut self, workspace: WorkspaceId, document: DocumentId, now_millis: i64) {
        let started = std::time::Instant::now();
        let reading = std::time::Instant::now();
        let projection = match self.projections.read(document) {
            Ok(Some(projection)) => projection,
            // Nothing to index. Not a failure: a document dropped between the
            // event and this tick is exactly this, and the removal is already
            // queued behind it.
            Ok(None) => return,
            Err(why) => return self.failed(why.0),
        };
        let listed = match self.index.listed(&document.to_storage()) {
            Ok(Some(listed)) => listed,
            // **No listing row means the index does not know this document
            // exists**, and a fragment for it would be a result nobody can
            // open. Mirror the workspace's listing and come back to it.
            Ok(None) => {
                // The ordinary absence is a race — the document was projected
                // before its workspace's listing was mirrored — and mirroring
                // it is the repair. A document no listing will *ever* name is
                // handled a level up, where `reconcile` consults the read
                // model's own set of listed documents and never queues it.
                self.consume_listing(workspace, now_millis);
                match self.index.listed(&document.to_storage()) {
                    Ok(Some(listed)) => listed,
                    Ok(None) => return,
                    Err(why) => return self.failed(why.0),
                }
            }
            Err(why) => return self.failed(why.0),
        };

        // **The tag table is a document, and this is it arriving.** A rename
        // reaches the index as its system document reprojecting; nothing else
        // is subscribed to, and nothing else needs to be.
        if listed.kind == KIND_SYSTEM {
            self.refresh_tags(workspace);
            // The list table is in the same document, and this is it changing.
            self.refresh_lists(workspace);
        }
        let tags = self.tag_table(workspace);
        // **The `TagNames` shape, for the words read out of this document's
        // own pictures and files.** Asked on every index of the document, so a
        // picture pasted into a page that was already read elsewhere is
        // findable the moment the page is.
        let read = self.readings_for(workspace, document, &projection);

        // Timed in three. Reading the read model, turning it into fragments and
        // writing them are three different things to be slow at, and
        // `last_millis` alone could only say that one of them was.
        let read_millis = reading.elapsed().as_millis() as u64;

        let building = std::time::Instant::now();
        let mut fragments =
            crate::fragments::fragments(&projection, &listed.title, tags.as_deref());
        fragments.extend(crate::fragments::reading_fragments(&projection, &read));
        // The typed half of the same walk. Counted inside `build_millis` on
        // purpose: it is one pass over the projection either way, and a second
        // number would suggest a second read that does not happen.
        let values = crate::fragments::attr_values(
            &workspace.to_storage(),
            &document.to_storage(),
            &projection,
        );
        let build_millis = building.elapsed().as_millis() as u64;

        let writing = std::time::Instant::now();
        if let Err(why) = self.index.replace_document(
            &workspace.to_storage(),
            &document.to_storage(),
            &fragments,
            &values,
            &projection.source_revision,
            now_millis,
        ) {
            return self.failed(why.0);
        }
        let write_millis = writing.elapsed().as_millis() as u64;

        let took = started.elapsed().as_millis() as u64;
        let mut inner = self.queue.lock();
        inner.stats.indexed += 1;
        inner.stats.last_millis = took;
        inner.stats.total_millis += took;
        inner.stats.max_millis = inner.stats.max_millis.max(took);
        inner.stats.read_millis = read_millis;
        inner.stats.build_millis = build_millis;
        inner.stats.write_millis = write_millis;
    }

    // ---------------------------------------------------------- readings

    /// The readings of one document's own live pictures and files, by hash.
    ///
    /// Empty when there is nothing to ask about, when no source is wired, and
    /// when the source answers `None` — *not now*. In that last case the
    /// document is indexed with its names and titles and no words, exactly as
    /// before readings existed, and remembered in [`unread`](Self::unread) so
    /// the words are not lost for want of somebody asking again.
    ///
    /// A hash missing from a `Some` answer has no reading, so a reading that
    /// disappears takes its fragments with it on this document's next index.
    fn readings_for(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        projection: &DocumentProjection,
    ) -> HashMap<String, ReadingText> {
        if let Some(documents) = self.unread.get_mut(&workspace) {
            documents.remove(&document);
        }
        let Some(source) = self.readings.as_ref() else {
            return HashMap::new();
        };
        let hashes = crate::fragments::read_hashes(projection);
        if hashes.is_empty() {
            return HashMap::new();
        }
        match source.readings(workspace, &hashes) {
            Some(found) => {
                let asked: HashSet<&str> = hashes.iter().map(String::as_str).collect();
                found
                    .into_iter()
                    .filter(|reading| asked.contains(reading.hash.as_str()))
                    .map(|reading| (reading.hash.clone(), reading))
                    .collect()
            }
            None => {
                self.unread
                    .entry(workspace)
                    .or_default()
                    .insert(document, hashes);
                HashMap::new()
            }
        }
    }

    /// Ask the readings source, once per workspace, whether it can answer yet
    /// for the documents indexed while it could not; re-queue those it now
    /// can. Returns how many were re-queued.
    ///
    /// **One probe per workspace, not one per document**: the question is
    /// whether the source is answering at all, and one document's hashes ask
    /// it. Each re-queued document then asks properly when it is indexed.
    fn retry_unread(&mut self) -> usize {
        let Some(source) = self.readings.as_ref() else {
            return 0;
        };
        let mut ready = Vec::new();
        for (workspace, documents) in &self.unread {
            let Some(hashes) = documents.values().next() else {
                continue;
            };
            if source.readings(*workspace, hashes).is_some() {
                ready.push(*workspace);
            }
        }
        let mut requeued = 0;
        for workspace in ready {
            for document in self
                .unread
                .remove(&workspace)
                .unwrap_or_default()
                .into_keys()
            {
                self.queue.push_document(workspace, document);
                requeued += 1;
            }
        }
        self.unread.retain(|_, documents| !documents.is_empty());
        requeued
    }

    // ------------------------------------------------------------- tags

    /// One workspace's tag table, read on first need.
    ///
    /// `None` means *not now*: no source wired, no system document yet, a fork
    /// that failed. The fragment builder then indexes the id it was
    /// given, which is what the index held before any of this — a tag must not
    /// vanish from search while its table is being waited for. **Nothing is
    /// cached on a failure**, so the next document tries again.
    fn tag_table(&mut self, workspace: WorkspaceId) -> Option<Arc<TagTable>> {
        if !self.tag_tables.contains_key(&workspace) {
            // **Through `refresh_tags`, so the first successful read is a
            // change like any other.** It used to insert the table quietly
            // here, which left the rule that function documents — *with nothing
            // to compare against, everything the table names is due* — honoured
            // only when the system document happened to be the document that
            // triggered the read. Every other order was a silent hole: a
            // content document indexed while the table could not be read
            // carried raw ids, and nothing ever asked again unless somebody
            // edited the system document.
            self.refresh_tags(workspace);
        }
        self.tag_tables.get(&workspace).cloned()
    }

    /// The workspace's system document changed. Re-read the table, and re-index
    /// exactly the documents carrying an id whose **name** moved.
    ///
    /// **Not the whole workspace.** A system document reprojects for a new
    /// attribute definition, a tag's colour and a merge onto the same spelling
    /// as well, and none of those changes one byte of one fragment; re-indexing
    /// everything on each would make defining an attribute cost a workspace
    /// walk. What is compared is the id-to-name map, and what is enqueued is
    /// the documents the read model says carry one of the ids that moved — one
    /// query, over `attrs`, and only when something actually moved.
    ///
    /// **With nothing to compare against, everything the table names is due.**
    /// That is the first time this process reads the workspace's table, and the
    /// fragments already in the index were built by some earlier process
    /// against some earlier table. Guessing that they agree is how a rename
    /// made before a restart stays invisible forever.
    fn refresh_tags(&mut self, workspace: WorkspaceId) {
        let Some(source) = self.tags.as_ref() else {
            return;
        };
        let Some(pairs) = source.tag_names(workspace) else {
            return;
        };
        let fresh = TagTable::from_pairs(pairs);
        let previous = self.tag_tables.insert(workspace, Arc::new(fresh.clone()));
        let moved = fresh.renamed_since(previous.as_deref().unwrap_or(&TagTable::default()));
        if moved.is_empty() {
            return;
        }

        let attrs = match self.projections.attrs_in_workspace(workspace) {
            Ok(attrs) => attrs,
            Err(why) => return self.failed(why.0),
        };
        let mut due = 0;
        for (document, rows) in attrs {
            if rows
                .iter()
                .any(|(_, attr)| attr.role == TAG_ROLE && moved.contains(&attr.value))
            {
                self.queue.push_document(workspace, document);
                due += 1;
            }
        }
        // **Counts and ids only.** A tag's name is what somebody wrote
        // (LOGGING.md), and this is the one place in the indexer that has one
        // in a local variable.
        tracing::debug!(
            target: "koru_search",
            %workspace,
            tags = moved.len(),
            documents = due,
            "a tag table changed; re-indexing what carries the renamed ids"
        );
    }

    /// Re-read one workspace's task lists and replace what the index holds
    /// for them. Answers whether the source answered.
    ///
    /// **A read that fails is not an empty table.** With no source, or one
    /// that says *not now*, nothing is written: the list rows already indexed
    /// stay, and the next reconcile asks again. No other document is touched
    /// — a list is not in any document's fragments.
    fn refresh_lists(&mut self, workspace: WorkspaceId) -> bool {
        let Some(lists) = self
            .lists
            .as_ref()
            .and_then(|source| source.list_names(workspace))
        else {
            return false;
        };
        match self.index.refresh_lists(&workspace.to_storage(), &lists) {
            Ok(changed) => {
                if changed {
                    // **Counts and ids only**, the tag refresh's rule: a
                    // list's name is something a person wrote.
                    tracing::debug!(
                        target: "koru_search",
                        %workspace,
                        lists = lists.len(),
                        "the task lists changed; their names were re-indexed"
                    );
                }
                true
            }
            Err(why) => {
                self.failed(why.0);
                false
            }
        }
    }

    /// **What an index built before lists and task results existed is owed**,
    /// once per workspace per process, whether or not any document is due.
    ///
    /// An index caught up to a revision is not re-read, so an upgrade alone
    /// would leave it with no list rows and no root-task markers until
    /// somebody edited a list or a page. So the first reconcile that sees a
    /// workspace reads its lists, and queues **only** the pages that are
    /// tasks and carry no marker yet — bounded by the number of such pages,
    /// never a workspace walk. A read that fails does nothing and is tried
    /// again at the next reconcile.
    fn bootstrap(&mut self, workspace: WorkspaceId) -> usize {
        if self.bootstrapped.contains(&workspace) {
            return 0;
        }
        // No source wired at all: nothing will ever answer, and asking every
        // two seconds for ever is the loop `compare` warns about.
        let lists_done = self.lists.is_none() || self.refresh_lists(workspace);
        let roots = match self.projections.task_root_documents(workspace) {
            Ok(roots) => roots,
            Err(why) => {
                self.failed(why.0);
                return 0;
            }
        };
        let mut queued = 0;
        for document in roots {
            match self.index.has_root_task_marker(&document.to_storage()) {
                Ok(true) => {}
                Ok(false) => {
                    self.queue.push_document(workspace, document);
                    queued += 1;
                }
                Err(why) => {
                    self.failed(why.0);
                    return queued;
                }
            }
        }
        if lists_done {
            self.bootstrapped.insert(workspace);
            tracing::debug!(
                target: "koru_search",
                %workspace,
                pages = queued,
                "bootstrapped task lists and page-task markers"
            );
        }
        queued
    }

    /// Compare what the index consumed against what the projection holds, and
    /// enqueue the difference.
    ///
    /// **Equal is current; everything else is work.** A recovered document's
    /// revision can go backwards, so "newer replaces older" would leave it
    /// stale for good.
    fn reconcile(&mut self, now_millis: i64) -> bool {
        if let Some(at) = self.reconciled_at
            && now_millis - at < RECONCILE_MILLIS
        {
            return false;
        }
        self.reconciled_at = Some(now_millis);
        // Not counted in `behind`: these are not the index trailing the read
        // model, they are the readings source catching up.
        let requeued = self.retry_unread();
        let Ok(found) = self.compare() else {
            return requeued > 0;
        };
        // **This is the number that says search is stale.** It is the same
        // comparison the reconcile was already doing, so it costs nothing to
        // publish — and nothing else reports it: `pending` says how much work is
        // queued, which is zero both when the index is current and when it has
        // not noticed it is behind.
        //
        // As of the last reconcile, which is throttled to `RECONCILE_MILLIS` —
        // so it is a recent truth rather than a live one, and a caller reading
        // it while a burst is being consumed will see it fall behind and catch
        // up rather than sit at zero.
        let mut inner = self.queue.lock();
        inner.stats.behind = found;
        inner.stats.unlisted = self.unlisted;
        if found > 0 {
            inner.stats.caught_up += 1;
        }
        drop(inner);
        // Said when it changes, not every reconcile. Zero is worth saying once
        // too: it is how somebody watching a repair knows it finished.
        if self.said_unlisted != Some(self.unlisted) {
            self.said_unlisted = Some(self.unlisted);
            if self.unlisted > 0 {
                tracing::warn!(
                    documents = self.unlisted,
                    "search index: documents exist that no workspace listing names; they cannot be indexed and will not be found by searching"
                );
            } else {
                tracing::info!("search index: every projected document is listed");
            }
        }
        found > 0 || requeued > 0
    }

    fn compare(&mut self) -> SearchResult<usize> {
        let mut found = 0;
        let mut unlisted = 0;

        let ours = self.index.digests()?;
        let mut mirrored = Vec::new();
        for (workspace, digest) in self.projections.listings()? {
            if ours.get(&workspace.to_storage()).map(Vec::as_slice) != Some(digest.as_slice()) {
                self.queue.push_listing(workspace);
                found += 1;
            } else {
                mirrored.push(workspace);
            }
        }
        // Only a workspace whose listing the index already holds: a page
        // queued before its listing row exists would be abandoned, and the
        // listing refresh that is queued above comes round again.
        for workspace in mirrored {
            found += self.bootstrap(workspace);
        }

        let cursors = self.index.cursors()?;
        let theirs = self.projections.revisions()?;
        // **A document no listing names is not work.** `index_one` requires a
        // listing row - a fragment for a document the listing does not name
        // would be a result nobody can open - so asking it to index one is
        // asking for a job it must abandon. It abandoned it silently and
        // recorded nothing, so the next reconcile found the same document and
        // queued it again, forever: a loop that rewrote the workspace's whole
        // listing on every attempt and never logged, because giving up there
        // was not a failure. Worse, `reconcile` answering `found > 0` every
        // time is what kept `busy` true, and the driver only sleeps when it is
        // false - so the thread never rested either.
        //
        // The check is against the *projection's* listing, not the index's.
        // A document missing from the index's listing is the ordinary case and
        // is handled by mirroring the listing and carrying on; only one the
        // projection cannot name is impossible, and only those are skipped.
        //
        // **Not by recording a cursor for them**, which is the tempting fix and
        // the wrong one: a cursor is keyed on `source_revision`, so a document
        // marked done at revision R would stay unindexed if its listing arrived
        // later without its content changing. That trades a loud loop for
        // documents that are quietly unsearchable for good.
        let listed = self.projections.listed_documents()?;
        for (document, revision) in &theirs {
            if cursors.get(&document.to_storage()).map(Vec::as_slice) == Some(revision.as_slice()) {
                continue;
            }
            if !listed.contains(document) {
                unlisted += 1;
                continue;
            }
            // The workspace comes from the projection's own row rather than
            // being guessed, so a document is never indexed under the wrong one.
            if let Ok(Some(projected)) = self.projections.read(*document) {
                self.queue.push_document(projected.workspace, *document);
                found += 1;
            }
        }
        self.unlisted = unlisted;

        // Anything the index holds that the projection no longer does was
        // dropped while this was not listening.
        let held: HashSet<String> = theirs.keys().map(DocumentId::to_storage).collect();
        for document in cursors.keys() {
            if !held.contains(document)
                && let Some(id) = DocumentId::parse(document)
            {
                self.queue.push_removed(id);
                found += 1;
            }
        }
        Ok(found)
    }

    fn failed(&mut self, why: String) {
        let mut inner = self.queue.lock();
        let changed = inner.stats.last_error.as_deref() != Some(why.as_str());
        inner.stats.failed += 1;
        inner.stats.last_error = Some(why.clone());
        drop(inner);
        // A broken derived schema can make every queued workspace fail with
        // the same SQLite error. Count every failure for diagnostics, but say
        // an identical cause once rather than turning a tight recovery loop
        // into hundreds of megabytes of duplicate logs.
        if changed {
            tracing::warn!("search index: {why}");
        }
    }
}

#[cfg(test)]
mod tests;
