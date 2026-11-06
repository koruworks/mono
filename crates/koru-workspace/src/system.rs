// SPDX-License-Identifier: MPL-2.0

//! The host side of the workspace system document.
//!
//! [`SystemDoc`] owns the content. [`SystemSession`] owns where that content
//! lives: it opens every listed `"koru-system"` document, replays its outbox,
//! and converges on the lowest listed document id. Duplicate rows deliberately
//! remain in the manifest. They are recovery anchors until Koru has a protocol
//! that can prove every offline replica has moved away from them.
//!
//! Opening is background work. The answering thread only creates local CRDT
//! operations, polls a channel, merges ready replicas, and hands updates to an
//! already-running link. A tag created while canonical adoption is in flight is
//! therefore included in the final in-memory merge rather than delayed behind
//! Store I/O.

use std::collections::BTreeSet;
use std::sync::{Arc, mpsc};

use koru_core::ids::{DocumentId, ListId, PageKey, TagId};
use koru_core::system::{ListEntry, PageTitle, SystemDoc, TagEntry};

use crate::{Workspace, explore, open, store};

struct LiveSystem {
    id: DocumentId,
    doc: SystemDoc,
    session: store::StoreSession,
}

fn absorb_and_publish(canonical: &mut LiveSystem, source: &SystemDoc) -> Result<(), String> {
    let snapshot = source.export_snapshot().map_err(|e| e.to_string())?;
    canonical.doc.absorb(&snapshot).map_err(|e| e.to_string())?;
    canonical
        .session
        .publish(&canonical.doc)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Blocking boundary used to prepare system documents. Production opens HTTP
/// and socket sessions on a worker; tests inject a fake.
pub(crate) trait SystemStore: Send + Sync {
    fn list(&self) -> Result<Vec<DocumentId>, String>;
    fn create(&self) -> Result<DocumentId, String>;
    fn join(&self, id: DocumentId) -> Result<(store::StoreSession, Vec<u8>), String>;
}

struct StoreSystemStore {
    segment: String,
    root: Option<std::path::PathBuf>,
    workspace: koru_store_core::ids::WorkspaceId,
}

impl SystemStore for StoreSystemStore {
    fn list(&self) -> Result<Vec<DocumentId>, String> {
        let client = match &self.root {
            Some(root) => koru_api_client::Client::open_at(&self.segment, root),
            None => koru_api_client::Client::open(&self.segment),
        }
        .map_err(|e| e.to_string())?;
        let mut ids: Vec<_> = client
            .documents(self.workspace)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|entry| entry.kind == koru_core::explore::KIND_SYSTEM)
            .map(|entry| explore::koru_document(entry.id))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    fn create(&self) -> Result<DocumentId, String> {
        let client = match &self.root {
            Some(root) => koru_api_client::Client::open_at(&self.segment, root),
            None => koru_api_client::Client::open(&self.segment),
        }
        .map_err(|e| e.to_string())?;
        client
            .create_document_at(
                self.workspace,
                "",
                koru_core::explore::KIND_SYSTEM,
                None,
                None,
            )
            .map(explore::koru_document)
            .map_err(|e| e.to_string())
    }

    fn join(&self, id: DocumentId) -> Result<(store::StoreSession, Vec<u8>), String> {
        store::StoreSession::join(
            &self.segment,
            self.root.as_deref(),
            self.workspace,
            explore::store_document(id),
        )
        .map_err(|e| e.to_string())
    }
}

/// Blocking system-document session used by an import worker.
///
/// This is the worker counterpart to [`SystemSession`]: it reuses the same
/// join/replay/canonicalization path, but has no polling state because the
/// caller is already off the answering thread. It deliberately exposes only
/// opaque snapshots so format crates remain independent of the Store and UI.
pub struct ImportSystemSession {
    store: Arc<dyn SystemStore>,
    active: Option<LiveSystem>,
    losers: Vec<LiveSystem>,
}

impl ImportSystemSession {
    pub fn open(
        segment: &str,
        root: Option<&std::path::Path>,
        workspace: koru_store_core::ids::WorkspaceId,
    ) -> Result<Self, String> {
        let store: Arc<dyn SystemStore> = Arc::new(StoreSystemStore {
            segment: segment.to_owned(),
            root: root.map(std::path::Path::to_path_buf),
            workspace,
        });
        Self::open_with_store(store)
    }

    fn open_with_store(store: Arc<dyn SystemStore>) -> Result<Self, String> {
        let ids = store.list()?;
        if ids.is_empty() {
            return Ok(Self {
                store,
                active: None,
                losers: Vec::new(),
            });
        }
        let mut prepared = prepare(
            store.as_ref(),
            ReconcileRequest {
                ids,
                skip: None,
                create: false,
            },
            None,
        )?;
        let active = prepared
            .active
            .take()
            .ok_or_else(|| "canonical system document was not opened".to_owned())?;
        Ok(Self {
            store,
            active: Some(active),
            losers: prepared.losers,
        })
    }

    /// Current canonical bytes for a fresh per-source stage.
    pub fn snapshot(&self) -> Result<Option<Vec<u8>>, String> {
        self.active
            .as_ref()
            .map(|active| active.doc.export_snapshot().map_err(|e| e.to_string()))
            .transpose()
    }

    /// Absorb and publish one accepted source. A manifest row is created only
    /// when a final Store listing still has none, closing the race between
    /// inspection and commit without deleting a concurrently-created loser.
    pub fn publish_staged(&mut self, snapshot: &[u8], register_system: bool) -> Result<(), String> {
        if self.active.is_none() {
            let mut ids = self.store.list()?;
            if ids.is_empty() {
                if !register_system {
                    return Err("a staged system update has no system document".into());
                }
                ids.push(self.store.create()?);
            }
            let mut prepared = prepare(
                self.store.as_ref(),
                ReconcileRequest {
                    ids,
                    skip: None,
                    create: false,
                },
                None,
            )?;
            self.active = prepared.active.take();
            self.losers.append(&mut prepared.losers);
        }
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| "canonical system document was not opened".to_owned())?;
        active.doc.absorb(snapshot).map_err(|e| e.to_string())?;
        active
            .session
            .publish(&active.doc)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[cfg(test)]
    fn with_store(store: Arc<dyn SystemStore>) -> Result<Self, String> {
        Self::open_with_store(store)
    }
}

impl Drop for ImportSystemSession {
    fn drop(&mut self) {
        // The import never waits for acknowledgement. Persist anything still
        // owed before the worker session leaves so a later ordinary open can
        // replay it through the Phase 2 recovery path.
        if let Some(active) = &mut self.active {
            active.session.buffer_now(&active.doc);
            active.session.leave();
        }
        for loser in &mut self.losers {
            loser.session.buffer_now(&loser.doc);
            loser.session.leave();
        }
    }
}

struct ReconcileRequest {
    ids: Vec<DocumentId>,
    skip: Option<DocumentId>,
    create: bool,
}

struct Prepared {
    ids: BTreeSet<DocumentId>,
    canonical: DocumentId,
    active: Option<LiveSystem>,
    losers: Vec<LiveSystem>,
    /// The worker had C and already absorbed every L it opened.
    losers_absorbed: bool,
    trouble: Vec<String>,
    failures: Vec<String>,
    failed_ids: BTreeSet<DocumentId>,
}

/// Join, replay, and publish on the worker.
///
/// The recovery ordering lives in one place: replay L, publish L, absorb L into
/// C, publish C. Nothing here drains for acknowledgement.
fn prepare(
    store: &dyn SystemStore,
    mut request: ReconcileRequest,
    waker: Option<&store::link::Waker>,
) -> Result<Prepared, String> {
    request.ids.sort_unstable();
    request.ids.dedup();
    if request.ids.is_empty() && request.create {
        request.ids.push(store.create()?);
    }
    let canonical = SystemDoc::canonical_document(&request.ids)
        .ok_or_else(|| "no system document was available".to_owned())?;

    let mut opened = Vec::new();
    let mut trouble = Vec::new();
    let mut failures = Vec::new();
    let mut failed_ids = BTreeSet::new();
    for id in request
        .ids
        .iter()
        .copied()
        .filter(|id| Some(*id) != request.skip)
    {
        let (mut session, snapshot) = match store.join(id) {
            Ok(joined) => joined,
            Err(why) if id != canonical => {
                failures.push(format!("system document {id}: {why}"));
                failed_ids.insert(id);
                continue;
            }
            Err(why) => return Err(format!("canonical system document {id}: {why}")),
        };
        let mut doc = SystemDoc::open_or_create(Some(&snapshot)).map_err(|e| e.to_string())?;
        trouble.extend(session.replay_buffered(&mut doc));
        session.publish(&doc).map_err(|e| e.to_string())?;
        if let Some(waker) = waker {
            session.set_waker(waker.clone());
        }
        opened.push(LiveSystem { id, doc, session });
    }

    let mut active = opened
        .iter()
        .position(|opened| opened.id == canonical)
        .map(|at| opened.remove(at));
    let losers_absorbed = active.is_some();
    if let Some(canonical_doc) = &mut active {
        for loser in &opened {
            absorb_and_publish(canonical_doc, &loser.doc)?;
        }
    }

    Ok(Prepared {
        ids: request.ids.into_iter().collect(),
        canonical,
        active,
        losers: opened,
        losers_absorbed,
        trouble,
        failures,
        failed_ids,
    })
}

/// One lifecycle for ordinary `/tag` writes, duplicate recovery, and adoption.
pub struct SystemSession {
    store: Arc<dyn SystemStore>,
    active: Option<LiveSystem>,
    /// Local edits made while the first open/create is in flight.
    local: Option<SystemDoc>,
    losers: Vec<LiveSystem>,
    pending: Option<mpsc::Receiver<Result<Prepared, String>>>,
    reconciled_ids: BTreeSet<DocumentId>,
    /// A finished open that [`reconcile`](Self::reconcile) adopted, held until
    /// [`tick`](Self::tick) can report it.
    ///
    /// `tick` is the only thing that tells the host the table moved, and
    /// `reconcile` polls too — it must, or a manifest change would queue a
    /// second open behind a finished one. The host's turn calls `reconcile`
    /// *before* `tick`, so whenever the background open lands between two
    /// turns it is `reconcile` that adopts it and `tick` that finds nothing to
    /// report. Dropping the answer there left the tag table empty — every tag
    /// a peer made drawing the unresolved placeholder — until something else
    /// happened to change the system document.
    adopted: bool,
    waker: Option<store::link::Waker>,
    trouble: Vec<String>,
    open_failure: Option<String>,
    retry_after: Option<std::time::Instant>,
}

impl SystemSession {
    fn connected(
        segment: &str,
        root: Option<&std::path::Path>,
        workspace: koru_store_core::ids::WorkspaceId,
        waker: Option<store::link::Waker>,
    ) -> Self {
        Self {
            store: Arc::new(StoreSystemStore {
                segment: segment.to_owned(),
                root: root.map(std::path::Path::to_path_buf),
                workspace,
            }),
            active: None,
            local: None,
            losers: Vec::new(),
            pending: None,
            reconciled_ids: BTreeSet::new(),
            adopted: false,
            waker,
            trouble: Vec::new(),
            open_failure: None,
            retry_after: None,
        }
    }

    #[cfg(test)]
    fn with_store(store: Arc<dyn SystemStore>) -> Self {
        Self {
            store,
            active: None,
            local: None,
            losers: Vec::new(),
            pending: None,
            reconciled_ids: BTreeSet::new(),
            adopted: false,
            waker: None,
            trouble: Vec::new(),
            open_failure: None,
            retry_after: None,
        }
    }

    fn tags(&self) -> Vec<TagEntry> {
        self.active
            .as_ref()
            .map(|live| live.doc.tags())
            .or_else(|| self.local.as_ref().map(SystemDoc::tags))
            .unwrap_or_default()
    }

    fn create_tag(&mut self, name: &str) -> Option<TagId> {
        let doc = match &mut self.active {
            Some(live) => &mut live.doc,
            None => {
                if self.local.is_none() {
                    self.local = SystemDoc::create().ok();
                }
                self.local.as_mut()?
            }
        };
        let id = doc.create_tag(name, None, open::now_ms()).ok()?;
        if let Some(active) = &mut self.active {
            let _ = active.session.publish(&active.doc);
        }
        Some(id)
    }

    // ------------------------------------------------------------------ lists
    //
    // The tag counterparts, one for one, over the same system document and
    // through the same lazy open. A list is a *filter* rather than a place
    // (SPEC §8): what makes a task a member is the `koru.list` value on the
    // task, and everything here is the small registry that gives that id a
    // name, a swatch and a place in the rail.

    fn lists(&self) -> Vec<ListEntry> {
        self.active
            .as_ref()
            .map(|live| live.doc.lists())
            .or_else(|| self.local.as_ref().map(SystemDoc::lists))
            .unwrap_or_default()
    }

    fn create_list(&mut self, name: &str, color: Option<&str>) -> Option<ListId> {
        let doc = match &mut self.active {
            Some(live) => &mut live.doc,
            None => {
                if self.local.is_none() {
                    self.local = SystemDoc::create().ok();
                }
                self.local.as_mut()?
            }
        };
        let id = doc.create_list(name, color, open::now_ms()).ok()?;
        if let Some(active) = &mut self.active {
            let _ = active.session.publish(&active.doc);
        }
        Some(id)
    }

    /// The shared shape of the three list edits: run `edit` over the live (or
    /// still-local) document, and publish only when it answered `true` — a
    /// guard refusal writes nothing worth sending. No edit creates a system
    /// document: an id this workspace has never held has no table to edit.
    fn edit_list(&mut self, edit: impl FnOnce(&mut SystemDoc) -> bool) -> bool {
        let doc = match (&mut self.active, &mut self.local) {
            (Some(live), _) => &mut live.doc,
            (None, Some(local)) => local,
            (None, None) => return false,
        };
        let changed = edit(doc);
        if changed && let Some(active) = &mut self.active {
            let _ = active.session.publish(&active.doc);
        }
        changed
    }

    fn rename_list(&mut self, list: ListId, name: &str) -> bool {
        self.edit_list(|doc| doc.rename_list(list, name).unwrap_or(false))
    }

    fn set_list_color(&mut self, list: ListId, color: Option<&str>) -> bool {
        self.edit_list(|doc| doc.set_list_color(list, color).unwrap_or(false))
    }

    fn set_list_order(&mut self, list: ListId, order: Option<i64>) -> bool {
        self.edit_list(|doc| doc.set_list_order(list, order).unwrap_or(false))
    }

    fn delete_list(&mut self, list: ListId) -> bool {
        self.edit_list(|doc| doc.delete_list(list, open::now_ms()).unwrap_or(false))
    }

    // ------------------------------------------------------------------ pages

    /// A live list's name, as it stands.
    pub(crate) fn list_name(&self, list: ListId) -> Option<String> {
        let doc = self
            .active
            .as_ref()
            .map(|live| &live.doc)
            .or(self.local.as_ref())?;
        doc.list_entry(list).map(|entry| entry.name)
    }

    /// The generations a page has had, or `None` while the open is in flight
    /// and nothing can be asked yet.
    pub(crate) fn page_generations(&self, page: PageKey) -> Option<Vec<u64>> {
        let doc = self
            .active
            .as_ref()
            .map(|live| &live.doc)
            .or(self.local.as_ref())?;
        Some(doc.page_generations(page))
    }

    /// The resolved title of a page document's generation, or `None` while
    /// the open is in flight.
    pub(crate) fn page_title(&self, page: PageKey, generation: u64) -> Option<PageTitle> {
        let doc = self
            .active
            .as_ref()
            .map(|live| &live.doc)
            .or(self.local.as_ref())?;
        Some(doc.page_title(page, generation))
    }

    /// Every list record, tombstones included, or none while the open is in
    /// flight.
    pub(crate) fn all_lists(&self) -> Option<Vec<ListEntry>> {
        let doc = self
            .active
            .as_ref()
            .map(|live| &live.doc)
            .or(self.local.as_ref())?;
        Some(doc.all_lists())
    }

    /// The deletion protocol of a page generation, or `None` while the open
    /// is in flight.
    pub(crate) fn deletion_record(
        &self,
        page: PageKey,
        generation: u64,
    ) -> Option<koru_core::pages::deletion::DeletionRecord> {
        let doc = self
            .active
            .as_ref()
            .map(|live| &live.doc)
            .or(self.local.as_ref())?;
        Some(doc.deletion_record(page, generation))
    }

    pub(crate) fn write_deletion(
        &mut self,
        page: PageKey,
        generation: u64,
        event: koru_core::pages::deletion::EventId,
        covers: &loro::VersionVector,
        deleted_ms: i64,
    ) -> bool {
        self.edit_list(|doc| {
            doc.write_deletion(page, generation, event, covers, deleted_ms)
                .unwrap_or(false)
        })
    }

    pub(crate) fn write_recover(
        &mut self,
        page: PageKey,
        generation: u64,
        replica: u64,
        observed: Vec<koru_core::pages::deletion::EventId>,
    ) -> bool {
        self.edit_list(|doc| {
            doc.write_recover(page, generation, replica, observed)
                .unwrap_or(false)
        })
    }

    pub(crate) fn write_keep(
        &mut self,
        page: PageKey,
        generation: u64,
        event: koru_core::pages::deletion::EventId,
        decision: koru_core::pages::deletion::DecisionId,
        keep: &koru_core::pages::deletion::Keep,
    ) -> bool {
        self.edit_list(|doc| {
            doc.write_keep(page, generation, event, decision, keep)
                .unwrap_or(false)
        })
    }

    pub(crate) fn report_uncovered(
        &mut self,
        page: PageKey,
        generation: u64,
        replica: u64,
        event: koru_core::pages::deletion::EventId,
        version: &loro::VersionVector,
    ) -> bool {
        self.edit_list(|doc| {
            doc.report_uncovered(page, generation, replica, event, version)
                .unwrap_or(false)
        })
    }

    /// A direct rename of a page document's generation.
    pub(crate) fn set_page_override(
        &mut self,
        page: PageKey,
        generation: u64,
        title: Option<&str>,
    ) -> bool {
        self.edit_list(|doc| {
            doc.set_page_override(page, generation, title)
                .unwrap_or(false)
        })
    }

    /// Whether the page shows its note inline, or `None` while the open is
    /// in flight.
    pub(crate) fn page_note_shown(&self, page: PageKey) -> Option<bool> {
        let doc = self
            .active
            .as_ref()
            .map(|live| &live.doc)
            .or(self.local.as_ref())?;
        Some(doc.page_note_shown(page))
    }

    pub(crate) fn set_page_note_shown(&mut self, page: PageKey, shown: bool) -> bool {
        self.edit_list(|doc| doc.set_page_note_shown(page, shown).unwrap_or(false))
    }

    /// The files attached to the page, in attached order, or `None` while
    /// the open is in flight.
    pub(crate) fn page_attachments(&self, page: PageKey) -> Option<Vec<DocumentId>> {
        let doc = self
            .active
            .as_ref()
            .map(|live| &live.doc)
            .or(self.local.as_ref())?;
        Some(doc.page_attachments(page))
    }

    pub(crate) fn attach_to_page(&mut self, page: PageKey, file: DocumentId, now_ms: i64) -> bool {
        self.edit_list(|doc| doc.attach_to_page(page, file, now_ms).unwrap_or(false))
    }

    pub(crate) fn detach_from_page(&mut self, page: PageKey, file: DocumentId) -> bool {
        self.edit_list(|doc| doc.detach_from_page(page, file).unwrap_or(false))
    }

    /// Record a page's generation, creating the local document if the
    /// workspace has none yet — a bucket's page may be the first thing a
    /// workspace ever puts in its system document.
    pub(crate) fn add_page_generation(&mut self, page: PageKey, generation: u64) -> bool {
        let doc = match &mut self.active {
            Some(live) => &mut live.doc,
            None => {
                if self.local.is_none() {
                    self.local = SystemDoc::create().ok();
                }
                match self.local.as_mut() {
                    Some(local) => local,
                    None => return false,
                }
            }
        };
        let added = doc.add_page_generation(page, generation).unwrap_or(false);
        if added && let Some(active) = &mut self.active {
            let _ = active.session.publish(&active.doc);
        }
        added
    }

    /// The workspace spelling dictionary, per language.
    fn dictionary(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        self.active
            .as_ref()
            .map(|live| live.doc.dictionary())
            .or_else(|| self.local.as_ref().map(SystemDoc::dictionary))
            .unwrap_or_default()
    }

    /// Add one workspace-dictionary word — the `create_tag` shape: write to
    /// whichever document is live, publish so peers learn the word.
    fn add_dictionary_word(&mut self, language: &str, word: &str) -> bool {
        let doc = match &mut self.active {
            Some(live) => &mut live.doc,
            None => {
                if self.local.is_none() {
                    self.local = SystemDoc::create().ok();
                }
                match self.local.as_mut() {
                    Some(doc) => doc,
                    None => return false,
                }
            }
        };
        let added = doc.add_dictionary_word(language, word).unwrap_or(false);
        if added && let Some(active) = &mut self.active {
            let _ = active.session.publish(&active.doc);
        }
        added
    }

    pub(super) fn reconcile(&mut self, ids: Vec<DocumentId>, create: bool) {
        // Held for `tick` rather than dropped — see [`Self::adopted`]. A turn
        // that adopts the open here and reports nothing is a tag table that
        // stays empty.
        self.adopted |= self.poll_ready();
        let ids: BTreeSet<_> = ids.into_iter().collect();
        if self
            .retry_after
            .is_some_and(|retry| std::time::Instant::now() < retry)
        {
            return;
        }
        if self.pending.is_some() || (ids == self.reconciled_ids && !ids.is_empty()) {
            return;
        }
        let create = create || (ids.is_empty() && self.local.is_some());
        if ids.is_empty() && !create {
            return;
        }

        let request = ReconcileRequest {
            ids: ids.iter().copied().collect(),
            skip: self.active.as_ref().map(|live| live.id),
            create,
        };
        let store = self.store.clone();
        let waker = self.waker.clone();
        let (send, receive) = mpsc::channel();
        match std::thread::Builder::new()
            .name("koru-system-open".to_owned())
            .spawn(move || {
                let result = prepare(store.as_ref(), request, waker.as_ref());
                let _ = send.send(result);
                if let Some(waker) = waker {
                    waker.wake();
                }
            }) {
            Ok(_) => {
                self.pending = Some(receive);
            }
            Err(why) => {
                self.open_failure = Some(format!("could not start system document open: {why}"));
                self.retry_after =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
            }
        }
    }

    /// Take a completed background open and swap it in without Store I/O.
    fn poll_ready(&mut self) -> bool {
        let result = match self.pending.as_ref().map(mpsc::Receiver::try_recv) {
            Some(Ok(result)) => result,
            Some(Err(mpsc::TryRecvError::Empty)) | None => return false,
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.pending = None;
                self.open_failure =
                    Some("system document open stopped without an answer".to_owned());
                self.retry_after =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
                return false;
            }
        };
        self.pending = None;
        let mut prepared = match result {
            Ok(prepared) => prepared,
            Err(why) => {
                self.open_failure = Some(why);
                self.retry_after =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
                return false;
            }
        };
        self.open_failure = None;

        let mut old = self.active.take();
        let mut canonical = match prepared.active.take() {
            Some(active) => active,
            None => match old.as_ref() {
                Some(active) if active.id == prepared.canonical => {
                    old.take().expect("the canonical was just present")
                }
                Some(_) => {
                    self.active = old.take();
                    self.open_failure = Some(format!(
                        "canonical system document {} was not opened",
                        prepared.canonical
                    ));
                    self.retry_after =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
                    return false;
                }
                None => {
                    self.open_failure = Some(format!(
                        "canonical system document {} was not opened",
                        prepared.canonical
                    ));
                    self.retry_after =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(1));
                    return false;
                }
            },
        };

        if !prepared.losers_absorbed {
            for loser in &prepared.losers {
                if let Err(why) = absorb_and_publish(&mut canonical, &loser.doc) {
                    self.open_failure = Some(format!(
                        "could not absorb system document {} into {}: {why}",
                        loser.id, canonical.id
                    ));
                }
            }
        }

        if let Some(old) = old {
            // Merge the final live replica before retaining its room as a loser.
            if old.id != canonical.id {
                if let Err(why) = absorb_and_publish(&mut canonical, &old.doc) {
                    self.open_failure = Some(format!(
                        "could not adopt system document {} into {}: {why}",
                        old.id, canonical.id
                    ));
                }
                self.keep_loser(old);
            }
        }
        if let Some(local) = self.local.as_ref() {
            match absorb_and_publish(&mut canonical, local) {
                Ok(()) => self.local = None,
                Err(why) => {
                    self.open_failure = Some(format!(
                        "could not adopt local system changes into {}: {why}",
                        canonical.id
                    ));
                }
            }
        }
        for loser in prepared.losers.drain(..) {
            self.keep_loser(loser);
        }

        self.reconciled_ids = prepared
            .ids
            .difference(&prepared.failed_ids)
            .copied()
            .collect();
        if self.open_failure.is_none() {
            self.open_failure = prepared.failures.first().cloned();
        }
        self.retry_after = (!prepared.failed_ids.is_empty())
            .then(|| std::time::Instant::now() + std::time::Duration::from_secs(1));
        self.trouble.append(&mut prepared.trouble);
        self.active = Some(canonical);
        true
    }

    fn keep_loser(&mut self, loser: LiveSystem) {
        if let Some(at) = self.losers.iter().position(|live| live.id == loser.id) {
            let previous = self.losers.swap_remove(at);
            previous.session.leave();
        }
        self.losers.push(loser);
    }

    /// Service every room. A late loser update is forwarded into C immediately;
    /// an acknowledgement only prunes its outbox.
    pub(super) fn tick(&mut self) -> bool {
        let mut changed = self.poll_ready();
        changed |= std::mem::take(&mut self.adopted);
        if let (Some(active), Some(local)) = (&mut self.active, self.local.as_ref()) {
            match absorb_and_publish(active, local) {
                Ok(()) => {
                    self.local = None;
                    self.open_failure = None;
                    changed = true;
                }
                Err(why) => {
                    self.open_failure = Some(format!(
                        "could not adopt local system changes into {}: {why}",
                        active.id
                    ));
                }
            }
        }
        if let Some(active) = &mut self.active {
            let _ = active.session.publish(&active.doc);
            changed |= active.session.drain(&mut active.doc).unwrap_or(false);
        }

        let mut recovered = Vec::new();
        for loser in &mut self.losers {
            let _ = loser.session.publish(&loser.doc);
            if loser.session.drain(&mut loser.doc).unwrap_or(false)
                && let Ok(snapshot) = loser.doc.export_snapshot()
            {
                recovered.push(snapshot);
            }
        }
        if let Some(active) = &mut self.active {
            for snapshot in recovered {
                if active.doc.absorb(&snapshot).is_ok() {
                    let _ = active.session.publish(&active.doc);
                    changed = true;
                }
            }
        }
        changed
    }

    pub(super) fn set_waker(&mut self, waker: store::link::Waker) {
        if let Some(active) = &self.active {
            active.session.set_waker(waker.clone());
        }
        for loser in &self.losers {
            loser.session.set_waker(waker.clone());
        }
        self.waker = Some(waker);
    }

    pub(super) fn leave(&self) {
        if let Some(active) = &self.active {
            active.session.leave();
        }
        for loser in &self.losers {
            loser.session.leave();
        }
    }

    pub(super) fn buffer_if_due(&mut self) {
        if let Some(active) = &mut self.active
            && !active.session.link_state().is_live()
        {
            active.session.buffer_if_due(&active.doc);
        }
        for loser in &mut self.losers {
            if !loser.session.link_state().is_live() {
                loser.session.buffer_if_due(&loser.doc);
            }
        }
    }

    pub(super) fn buffer_now(&mut self) {
        if let Some(active) = &mut self.active {
            active.session.buffer_now(&active.doc);
        }
        for loser in &mut self.losers {
            loser.session.buffer_now(&loser.doc);
        }
    }

    pub(super) fn unacknowledged(&self) -> usize {
        self.sessions()
            .map(store::StoreSession::unacknowledged)
            .sum()
    }

    pub(super) fn busy(&self) -> bool {
        self.pending.is_some()
            || self.local.is_some()
            || self.open_failure.is_some()
            || self.retry_after.is_some()
            || self.sessions().any(|session| {
                session.unacknowledged() > 0
                    || !session.link_state().is_live()
                    || session.stopped().is_some()
            })
    }

    pub(super) fn sessions(&self) -> impl Iterator<Item = &store::StoreSession> {
        self.active
            .as_ref()
            .map(|active| &active.session)
            .into_iter()
            .chain(self.losers.iter().map(|loser| &loser.session))
    }

    pub(super) fn session_count(&self) -> usize {
        self.sessions().count()
    }

    pub(super) fn take_trouble(&mut self) -> Vec<String> {
        std::mem::take(&mut self.trouble)
    }

    pub(super) fn open_failure(&self) -> Option<&str> {
        self.open_failure.as_deref()
    }

    #[cfg(test)]
    fn active_id(&self) -> Option<DocumentId> {
        self.active.as_ref().map(|active| active.id)
    }
}

impl Workspace {
    pub fn tags_snapshot(&mut self) -> Vec<TagEntry> {
        self.ensure_system(false);
        self.system
            .as_ref()
            .map(SystemSession::tags)
            .unwrap_or_default()
    }

    pub fn create_tag(&mut self, name: &str) -> Option<TagId> {
        if !self.ensure_system(true) {
            return None;
        }
        let id = self.system.as_mut()?.create_tag(name)?;
        self.tags_changed = true;
        Some(id)
    }

    pub fn take_tags_changed(&mut self) -> bool {
        std::mem::take(&mut self.tags_changed)
    }

    /// The live list table, opening the system document if the manifest lists
    /// one. Empty both for "no lists yet" and for "no system document yet" —
    /// [`tags_snapshot`](Self::tags_snapshot)'s rule, and for its reason.
    pub fn lists_snapshot(&mut self) -> Vec<ListEntry> {
        self.ensure_system(false);
        self.system
            .as_ref()
            .map(SystemSession::lists)
            .unwrap_or_default()
    }

    /// Create a list — creating and registering the system document first if
    /// this workspace has none — and answer its id for the caller to apply to
    /// tasks.
    ///
    /// `color` is an `AccentColor` token from the swatch set the rest of the
    /// app already offers, never an RGB value: a colour picked on one theme is
    /// read on the other, and on another machine.
    pub fn create_list(&mut self, name: &str, color: Option<&str>) -> Option<ListId> {
        if !self.ensure_system(true) {
            return None;
        }
        let id = self.system.as_mut()?.create_list(name, color)?;
        self.lists_changed = true;
        Some(id)
    }

    /// Rename a list. `false` for an id this table has never held, or one that
    /// has been deleted — the guard lives in `koru-core`, so a surface that
    /// forgot to check cannot rename a tombstone back into the rail.
    pub fn rename_list(&mut self, list: ListId, name: &str) -> bool {
        if !self.ensure_system(false) {
            return false;
        }
        let Some(system) = &mut self.system else {
            return false;
        };
        let renamed = system.rename_list(list, name);
        if renamed {
            self.lists_changed = true;
        }
        renamed
    }

    /// Set or clear a list's swatch. `false` for an id this table has never
    /// held, or one that has been deleted —
    /// [`rename_list`](Self::rename_list)'s guard, in `koru-core` for its
    /// reason.
    ///
    /// The token is an `AccentColor` from the shared swatch set, never an RGB
    /// value: `create_list`'s rule, and the one that makes a colour chosen on
    /// one theme readable on the other.
    /// Where a list sits in the rail. `None` puts it after every ranked one,
    /// which is where a list a peer made arrives.
    pub fn set_list_order(&mut self, list: ListId, order: Option<i64>) -> bool {
        if !self.ensure_system(false) {
            return false;
        }
        let Some(system) = &mut self.system else {
            return false;
        };
        let set = system.set_list_order(list, order);
        if set {
            self.lists_changed = true;
        }
        set
    }

    pub fn set_list_color(&mut self, list: ListId, color: Option<&str>) -> bool {
        if !self.ensure_system(false) {
            return false;
        }
        let Some(system) = &mut self.system else {
            return false;
        };
        let set = system.set_list_color(list, color);
        if set {
            self.lists_changed = true;
        }
        set
    }

    /// Delete a list — a tombstone in the registry, and **no task touched**.
    ///
    /// Memberships clear document by document afterwards
    /// ([`Attributes::clear_list_membership`]); a stored id the table no longer
    /// names reads as *gone* rather than as *unknown*, which is why the record
    /// survives.
    ///
    /// [`Attributes::clear_list_membership`]: koru_core::attributes
    pub fn delete_list(&mut self, list: ListId) -> bool {
        if !self.ensure_system(false) {
            return false;
        }
        let Some(system) = &mut self.system else {
            return false;
        };
        let deleted = system.delete_list(list);
        if deleted {
            self.lists_changed = true;
        }
        deleted
    }

    /// Whether the list table changed since the shell last asked — the tag
    /// flag's counterpart, and the signal behind re-pushing the table into the
    /// tasks rail.
    pub fn take_lists_changed(&mut self) -> bool {
        std::mem::take(&mut self.lists_changed)
    }

    /// The workspace spelling dictionary, language tag → sorted words.
    pub fn workspace_dictionary(&mut self) -> std::collections::BTreeMap<String, Vec<String>> {
        self.ensure_system(false);
        self.system
            .as_ref()
            .map(SystemSession::dictionary)
            .unwrap_or_default()
    }

    /// Add one word every collaborator in this workspace accepts (D3).
    pub fn add_workspace_dictionary_word(&mut self, language: &str, word: &str) -> bool {
        if !self.ensure_system(true) {
            return false;
        }
        self.system
            .as_mut()
            .is_some_and(|system| system.add_dictionary_word(language, word))
    }

    /// A manifest change may have introduced the workspace's first
    /// `"koru-system"` row — look again.
    ///
    /// **This is [`ensure_system`](Self::ensure_system) and nothing else.** It
    /// used to be a weaker copy of it that reconciled an *already open*
    /// session and did nothing at all when there was none — which is exactly
    /// the case its call site names. A workspace opened before any tag existed
    /// has `system: None`, so a peer's first tag arrived in the manifest, its
    /// document replicated into the Store, and this replica never opened it:
    /// every chip carrying that `TagId` drew the unresolved placeholder until
    /// the app was restarted and the open happened from the top.
    pub(crate) fn recheck_system_on_entries_change(&mut self) {
        self.ensure_system(false);
    }

    pub(super) fn system_ids(&self) -> Vec<DocumentId> {
        let mut ids: Vec<_> = self
            .manifest
            .as_ref()
            .map(|manifest| {
                manifest
                    .entries()
                    .into_iter()
                    .filter(|entry| entry.kind == koru_core::explore::KIND_SYSTEM)
                    .map(|entry| explore::koru_document(entry.id))
                    .collect()
            })
            .unwrap_or_default();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// A session bound to this workspace's Store, or `None` for a workspace
    /// with no store behind it. Constructing one performs no I/O — the join
    /// and the listing happen on `reconcile`'s worker.
    ///
    /// The one place the production `SystemStore` is built, so a test can put
    /// its fake behind the same lazy open the app uses rather than reaching
    /// past it.
    fn new_system_session(&self) -> Option<SystemSession> {
        #[cfg(test)]
        if let Some(store) = &self.system_store_for_tests {
            return Some(SystemSession::with_store(Arc::clone(store)));
        }
        let (segment, root) = self.store.clone()?;
        let workspace = self.current?;
        Some(SystemSession::connected(
            &segment,
            root.as_deref(),
            explore::store_workspace(workspace),
            self.waker.clone(),
        ))
    }

    /// Schedule an open/create if necessary. This performs no HTTP, join,
    /// replay, or wait on the caller's thread.
    pub(crate) fn ensure_system(&mut self, create: bool) -> bool {
        let ids = self.system_ids();
        if self.system.is_none() {
            if ids.is_empty() && !create {
                return false;
            }
            let Some(session) = self.new_system_session() else {
                return false;
            };
            self.system = Some(session);
        }
        if let Some(system) = &mut self.system {
            system.reconcile(ids, create);
            if create && system.active.is_none() && system.local.is_none() {
                system.local = SystemDoc::create().ok();
            }
            return system.active.is_some() || system.local.is_some();
        }
        false
    }
}

#[cfg(test)]
pub(crate) mod tests;
