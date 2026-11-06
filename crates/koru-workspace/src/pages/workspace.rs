// SPDX-License-Identifier: MPL-2.0

//! The workspace's half of the page registry: what the shell asks, and what
//! each store turn does for the pages in flight.

use koru_core::system::PageTitle;

use super::*;

impl Workspace {
    // ------------------------------------------------------- the page's id

    /// The page's current generation, or `None` while the system document
    /// is still opening and cannot be asked. A workspace with no system
    /// document has never had a page document either.
    fn page_generation(&mut self, page: PageKey) -> Option<u64> {
        self.ensure_system(false);
        match &self.system {
            None => Some(0),
            Some(system) => Some(
                system
                    .page_generations(page)?
                    .into_iter()
                    .max()
                    .unwrap_or(0),
            ),
        }
    }

    /// The semantic name the page's document is registered under: the list's
    /// name as it stands, or the bucket's token. Never a localized label.
    fn page_title(&mut self, page: PageKey) -> String {
        match page {
            PageKey::List(list) => {
                self.ensure_system(false);
                self.system
                    .as_ref()
                    .and_then(|system| system.list_name(list))
                    .unwrap_or_default()
            }
            PageKey::Bucket(bucket) => bucket.as_str().to_owned(),
        }
    }

    /// Record that `page`'s document of `generation` exists — the grow-only
    /// key the first input adds (§6.2), creating the system document when
    /// the workspace has none.
    fn record_page_generation(&mut self, page: PageKey, generation: u64) {
        self.ensure_system(true);
        if let Some(system) = &mut self.system
            && system.add_page_generation(page, generation)
        {
            self.lists_changed = true;
        }
    }

    /// Which document `page` points at: the current generation's id,
    /// stepping past a trashed one (§6.2: a trashed page document is
    /// replaced). `None` while the generation cannot be read yet.
    pub fn page_document(&mut self, page: PageKey) -> Option<(DocumentId, u64)> {
        let workspace = self.current?;
        let mut generation = self.page_generation(page)?;
        let mut document = page_document_id(workspace, page, generation);
        // Bounded: the manifest can only have trashed documents that exist.
        for _ in 0..64 {
            let trashed = self
                .manifest
                .as_ref()
                .is_some_and(|m| m.is_trashed(explore::store_document(document)));
            if !trashed {
                break;
            }
            generation += 1;
            document = page_document_id(workspace, page, generation);
        }
        // A generation that was permanently deleted (a tombstone, §6.2) is
        // replaced too: the page shows empty and the first input creates
        // id(n+1). A recovered one is live again and is not stepped past.
        // Only the current generation can carry a live tombstone, so this is
        // one step, not a loop.
        if self
            .system
            .as_ref()
            .and_then(|s| s.deletion_record(page, generation))
            .is_some_and(|record| record.is_deleted())
        {
            generation += 1;
            document = page_document_id(workspace, page, generation);
        }
        Some((document, generation))
    }

    /// Every page document this workspace's system document knows: each
    /// list's (tombstones included — a deleted list keeps its document) and
    /// each bucket's, for every generation it has had. Derived, never
    /// stored (plan §6.6's page-document map).
    pub fn page_documents(&mut self) -> Vec<(DocumentId, PageKey, u64)> {
        let Some(workspace) = self.current else {
            return Vec::new();
        };
        self.ensure_system(false);
        let Some(system) = &self.system else {
            return Vec::new();
        };
        let mut pages: Vec<PageKey> = system
            .all_lists()
            .unwrap_or_default()
            .into_iter()
            .map(|list| PageKey::List(list.id))
            .collect();
        pages.extend(
            koru_core::tasks::Bucket::ALL
                .into_iter()
                .map(PageKey::Bucket),
        );
        let mut out = Vec::new();
        for page in pages {
            for generation in system.page_generations(page).unwrap_or_default() {
                out.push((
                    page_document_id(workspace, page, generation),
                    page,
                    generation,
                ));
            }
        }
        out
    }

    /// The resolved title of a page document — `None` for a document that
    /// is nobody's note.
    pub fn page_title_of(&mut self, document: DocumentId) -> Option<PageTitle> {
        let (page, generation) = self
            .page_documents()
            .into_iter()
            .find(|(id, _, _)| *id == document)
            .map(|(_, page, generation)| (page, generation))
            .or_else(|| {
                self.pages
                    .entries
                    .get(&document)
                    .map(|entry| (entry.page, entry.generation))
            })?;
        self.system
            .as_ref()
            .and_then(|system| system.page_title(page, generation))
    }

    /// A direct rename of a page document: its own title from now on, until
    /// the list is renamed (Q5). Answers whether `document` was a note.
    pub(crate) fn rename_page_document(&mut self, document: DocumentId, title: &str) -> bool {
        let Some((_, page, generation)) = self
            .page_documents()
            .into_iter()
            .find(|(id, _, _)| *id == document)
        else {
            return false;
        };
        let title = title.trim();
        let Some(system) = &mut self.system else {
            return false;
        };
        let changed =
            system.set_page_override(page, generation, (!title.is_empty()).then_some(title));
        if changed {
            self.lists_changed = true;
        }
        true
    }

    /// Which page `document` is the note of, if any — the registry's answer
    /// when it is making the document exist, else derived from every live
    /// list's and every bucket's generations. `None` for an ordinary
    /// document.
    pub fn page_of_document(&mut self, document: DocumentId) -> Option<PageKey> {
        if let Some(entry) = self.pages.entries.get(&document) {
            return Some(entry.page);
        }
        let workspace = self.current?;
        let lists: Vec<PageKey> = self
            .lists_snapshot()
            .into_iter()
            .map(|list| PageKey::List(list.id))
            .collect();
        let buckets = koru_core::tasks::Bucket::ALL
            .into_iter()
            .map(PageKey::Bucket);
        for page in lists.into_iter().chain(buckets) {
            let current = self.page_generation(page).unwrap_or(0);
            if (0..=current).any(|g| page_document_id(workspace, page, g) == document) {
                return Some(page);
            }
        }
        None
    }

    // ------------------------------------------------------- ensure, retry

    /// Make sure `page` has a document this process can show, and say where
    /// it stands. Idempotent and cheap once the document exists — the one
    /// call the shell makes for every Tasks page it shows, every turn.
    ///
    /// An empty page gets a genesis document parked under its id with no
    /// store session, so the Tasks tab can activate it and the first
    /// keystroke lands in the real document. A listed page is opened
    /// off-thread. Nothing is registered until something is written.
    pub fn ensure_page(&mut self, page: PageKey) -> Option<PageState> {
        let mut state = self.ensure_page_inner(page)?;
        state.conflict = matches!(
            self.page_resolution(page),
            koru_core::pages::deletion::Resolution::Conflict(_)
        );
        Some(state)
    }

    fn ensure_page_inner(&mut self, page: PageKey) -> Option<PageState> {
        let workspace = self.current?;
        let (document, generation) = self.page_document(page)?;
        if let Some(entry) = self.pages.entries.get_mut(&document) {
            // A document held for this workspace comes back to the parked
            // set, so the tab can activate it like any other.
            if entry.workspace == workspace
                && self.active != Some(document)
                && !self.others.contains_key(&document)
            {
                if let Some(doc) = entry.held.take() {
                    self.others.insert(document, doc);
                }
                if let Some(session) = entry.session.take() {
                    self.doc_sessions.insert(document, session);
                }
            }
            return Some(entry.state());
        }
        if self.active == Some(document) || self.others.contains_key(&document) {
            return Some(PageState {
                page,
                document,
                generation,
                phase: PagePhase::Ready,
                conflict: false,
            });
        }
        let title = self.page_title(page);
        let listed = self
            .manifest
            .as_ref()
            .is_some_and(|m| m.contains_document(explore::store_document(document)));
        let entry = if listed {
            let mut entry =
                PageEntry::new(page, workspace, document, generation, PagePhase::Loading);
            entry.title = title;
            entry.seed = false;
            entry
        } else {
            let mut doc = WorkspaceDoc::page_genesis(workspace, document).ok()?;
            // The id the store will list it under, so a reference minted in
            // it names the document — the same stamp every open makes.
            let _ = doc.stamp_document_id(document);
            doc.enable_undo();
            let baseline = doc.version();
            let replica = doc.peer_id();
            self.others.insert(document, doc);
            let mut entry = PageEntry::new(page, workspace, document, generation, PagePhase::Empty);
            entry.title = title;
            entry.baseline = baseline;
            entry.replica = replica;
            entry
        };
        let state = entry.state();
        self.pages.entries.insert(document, entry);
        Some(state)
    }

    /// Run a failed page's registration or open again — the person's
    /// explicit action on the band's *Retry*.
    pub fn retry_page(&mut self, page: PageKey) {
        let Some((document, _)) = self.page_document(page) else {
            return;
        };
        if let Some(entry) = self.pages.entries.get_mut(&document)
            && matches!(entry.phase, PagePhase::Failed { .. })
        {
            entry.phase = if entry.seed {
                PagePhase::Registering
            } else {
                PagePhase::Loading
            };
        }
    }

    // ---------------------------------------------------------- the turn

    fn page_doc(&self, id: DocumentId) -> Option<&WorkspaceDoc> {
        if self.active == Some(id) {
            return Some(&self.ws);
        }
        if let Some(doc) = self.others.get(&id) {
            return Some(doc);
        }
        self.pages.entries.get(&id).and_then(|e| e.held.as_ref())
    }

    /// One turn for every page in flight: take what the registration
    /// threads answered, start what is owed, write and release revisions.
    /// Answers whether a document changed under a view.
    pub(crate) fn tick_pages(&mut self) -> bool {
        let mut changed = false;
        let ids: Vec<DocumentId> = self.pages.entries.keys().copied().collect();
        for id in ids {
            changed |= self.poll_page(id);
            self.advance_page(id);
            self.write_page_revision(id, false);
            self.release_page_revisions(id);
            if let Some(entry) = self.pages.entries.get(&id)
                && !entry.owes()
                && entry.held.is_none()
                && entry.request.is_none()
            {
                self.pages.entries.remove(&id);
            }
        }
        changed
    }

    /// Take a registration's answer, if one arrived.
    fn poll_page(&mut self, id: DocumentId) -> bool {
        let Some(entry) = self.pages.entries.get_mut(&id) else {
            return false;
        };
        let Some(request) = &entry.request else {
            return false;
        };
        let outcome = match request.try_recv() {
            Ok(outcome) => outcome,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("the registration ended without answering".to_owned())
            }
        };
        entry.request = None;
        match outcome {
            Ok(registration) => self.page_registered(id, registration),
            Err(why) => {
                if let Some(entry) = self.pages.entries.get_mut(&id) {
                    entry.phase = PagePhase::Failed {
                        why,
                        opening: !entry.seed,
                    };
                }
                false
            }
        }
    }

    /// The store holds the document: fold its state into ours, keep the room,
    /// and list the page in Explore until the manifest echo arrives.
    fn page_registered(&mut self, id: DocumentId, registration: Registration) -> bool {
        let Registration {
            mut session,
            snapshot,
        } = registration;
        if let Some(waker) = &self.waker {
            session.set_waker(waker.clone());
        }
        let Some(entry) = self.pages.entries.get_mut(&id) else {
            return false;
        };
        let workspace = entry.workspace;
        let mut changed = false;
        let imported: Result<(), String> = if self.active == Some(id) {
            let r = self.ws.import(&snapshot).map_err(|e| e.to_string());
            if r.is_ok() {
                self.doc_sessions.insert(id, session);
                self.publish_active();
                changed = true;
            }
            r
        } else if let Some(doc) = self.others.get_mut(&id) {
            let r = doc.import(&snapshot).map_err(|e| e.to_string());
            if r.is_ok() {
                let _ = session.publish(doc);
                self.doc_sessions.insert(id, session);
            }
            r
        } else if let Some(doc) = entry.held.as_mut() {
            let r = doc.import(&snapshot).map_err(|e| e.to_string());
            if r.is_ok() {
                let _ = session.publish(doc);
                entry.session = Some(session);
            }
            r
        } else {
            // An existing document, opened: the store's state is the whole
            // of it, stamped and replayed as every open does.
            match WorkspaceDoc::from_snapshot(&snapshot) {
                Ok(mut doc) => {
                    self.replay_trouble
                        .extend(session.replay_buffered(&mut doc));
                    let _ = doc.stamp_workspace_id(workspace);
                    let _ = doc.stamp_document_id(id);
                    doc.enable_undo();
                    entry.baseline = doc.version();
                    entry.replica = doc.peer_id();
                    if self.current == Some(workspace) {
                        self.others.insert(id, doc);
                        self.doc_sessions.insert(id, session);
                    } else {
                        entry.held = Some(doc);
                        entry.session = Some(session);
                    }
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        };
        let Some(entry) = self.pages.entries.get_mut(&id) else {
            return changed;
        };
        match imported {
            Ok(()) => {
                entry.phase = PagePhase::Ready;
                let listed = self
                    .manifest
                    .as_ref()
                    .is_some_and(|m| m.contains_document(explore::store_document(id)));
                if !listed && !self.pending_documents.iter().any(|p| p.id == id) {
                    self.pending_documents.push(Pending {
                        id,
                        title: entry.title.clone(),
                        after: None,
                        parent: None,
                        kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
                    });
                    self.entries_changed = true;
                }
            }
            Err(why) => {
                entry.phase = PagePhase::Failed {
                    why,
                    opening: !entry.seed,
                }
            }
        }
        changed
    }

    /// Start what a page owes: the first write turns an empty page into a
    /// registering one, and a registering or loading page with no request in
    /// flight gets one.
    fn advance_page(&mut self, id: DocumentId) {
        let Some(entry) = self.pages.entries.get(&id) else {
            return;
        };
        if entry.phase == PagePhase::Empty {
            let moved = self
                .page_doc(id)
                .is_some_and(|doc| doc.version() != entry.baseline);
            if !moved {
                return;
            }
            let (page, generation) = (entry.page, entry.generation);
            // The revision first, before anything is sent (§6.5).
            self.write_page_revision(id, true);
            if let Some(entry) = self.pages.entries.get_mut(&id) {
                entry.phase = PagePhase::Registering;
            }
            self.record_page_generation(page, generation);
        }
        let Some(entry) = self.pages.entries.get(&id) else {
            return;
        };
        if !matches!(entry.phase, PagePhase::Registering | PagePhase::Loading)
            || entry.request.is_some()
        {
            return;
        }
        let Some((segment, root)) = self.store.clone() else {
            if let Some(entry) = self.pages.entries.get_mut(&id) {
                entry.phase = PagePhase::Failed {
                    why: "this workspace is not open on a store".into(),
                    opening: !entry.seed,
                };
            }
            return;
        };
        let seed = if entry.seed {
            match self.page_doc(id).map(WorkspaceDoc::export_all_updates) {
                Some(Ok(bytes)) => Some(bytes),
                _ => {
                    if let Some(entry) = self.pages.entries.get_mut(&id) {
                        entry.phase = PagePhase::Failed {
                            why: "the note could not be exported".into(),
                            opening: !entry.seed,
                        };
                    }
                    return;
                }
            }
        } else {
            None
        };
        let (workspace, title) = (entry.workspace, entry.title.clone());
        let waker = self.waker.clone();
        let (send, receive) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("koru-page-register".into())
            .spawn(move || {
                let outcome = register(&segment, root.as_deref(), workspace, id, &title, seed);
                let _ = send.send(outcome);
                if let Some(waker) = waker {
                    waker.wake();
                }
            });
        if let Some(entry) = self.pages.entries.get_mut(&id) {
            match spawned {
                Ok(_) => entry.request = Some(receive),
                Err(e) => {
                    entry.phase = PagePhase::Failed {
                        why: format!("spawn: {e}"),
                        opening: !entry.seed,
                    }
                }
            }
        }
    }

    /// Write the page's pending revision when its writing moved — at most
    /// once a second unless `force`, which is the way out of the process.
    fn write_page_revision(&mut self, id: DocumentId, force: bool) {
        let Some(entry) = self.pages.entries.get(&id) else {
            return;
        };
        if entry.phase == PagePhase::Empty || entry.phase == PagePhase::Loading {
            return;
        }
        let Some(doc) = self.page_doc(id) else {
            return;
        };
        let version = doc.version();
        if version == entry.baseline || entry.written.as_ref() == Some(&version) {
            return;
        }
        if !force
            && entry
                .written_at
                .is_some_and(|at| at.elapsed() < REVISION_INTERVAL)
        {
            return;
        }
        let root = self.store.as_ref().and_then(|(_, root)| root.clone());
        let listed = self
            .manifest
            .as_ref()
            .is_some_and(|m| m.contains_document(explore::store_document(id)));
        let written = (|| -> Result<RevisionName, String> {
            let dir = document_dir(root.as_deref(), entry.workspace, id).ok_or_else(|| {
                "this platform has no local application-data directory".to_owned()
            })?;
            let updates = doc.export_all_updates().map_err(|e| e.to_string())?;
            let name = RevisionName {
                replica: entry.replica,
                rev: entry.next_rev,
            };
            let record = Record {
                workspace: entry.workspace.to_storage(),
                document: id.to_storage(),
                page: entry.page.to_storage(),
                generation: entry.generation,
                state: phase_token(&entry.phase).to_owned(),
                seen_listed: listed,
                blobs: Vec::new(),
                version: version.encode(),
            };
            WriteRevision::new(&dir, name, &part_token(), updates, Vec::new(), &record)
                .run(&mut DiskFs)
                .map_err(|e| e.to_string())?;
            Ok(name)
        })();
        let Some(entry) = self.pages.entries.get_mut(&id) else {
            return;
        };
        entry.written_at = Some(Instant::now());
        match written {
            Ok(name) => {
                entry.next_rev += 1;
                entry.written = Some(version.clone());
                entry.pending.push((name, version));
                entry.revision_failure = None;
            }
            Err(why) => entry.revision_failure = Some(why),
        }
    }

    /// Delete the revisions the store's durable acknowledgement now covers
    /// (§6.3's release rule).
    fn release_page_revisions(&mut self, id: DocumentId) {
        let Some(entry) = self.pages.entries.get(&id) else {
            return;
        };
        if entry.pending.is_empty() {
            return;
        }
        let acked = self
            .doc_sessions
            .get(&id)
            .or(entry.session.as_ref())
            .and_then(|s| s.acked().cloned());
        let Some(acked) = acked else {
            return;
        };
        let root = self.store.as_ref().and_then(|(_, root)| root.clone());
        let Some(dir) = document_dir(root.as_deref(), entry.workspace, id) else {
            return;
        };
        let covered: Vec<RevisionName> = entry
            .pending
            .iter()
            .filter(|(_, version)| acked.includes_vv(version))
            .map(|(name, _)| *name)
            .collect();
        let mut released = Vec::new();
        for name in covered {
            let done = ReleaseRevision::of_name(&DiskFs, &dir, name)
                .and_then(|mut release| release.run(&mut DiskFs));
            if done.is_ok() {
                released.push(name);
            }
        }
        if let Some(entry) = self.pages.entries.get_mut(&id) {
            entry.pending.retain(|(name, _)| !released.contains(name));
        }
    }

    // ------------------------------------------------- start-up discovery

    /// Take up every pending revision on this machine's disk for the open
    /// workspace: each becomes a registering page document, held here
    /// until its page asks for it, and released once the store holds it.
    pub(crate) fn discover_pages(&mut self) {
        let Some(workspace) = self.current else {
            return;
        };
        let root = self.store.as_ref().and_then(|(_, root)| root.clone());
        let base = match root.as_deref() {
            Some(root) => root.to_path_buf(),
            None => match koru_channel::store_root() {
                Some(root) => root,
                None => return,
            },
        };
        let workspace_dir = base
            .join("outbox")
            .join(explore::store_workspace(workspace).to_storage());
        let Ok(entries) = std::fs::read_dir(&workspace_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Some(document) = entry.file_name().to_str().and_then(DocumentId::parse) else {
                continue;
            };
            if self.pages.entries.contains_key(&document) {
                continue;
            }
            let dir = entry.path();
            let Ok(found) = discover(&DiskFs, &dir) else {
                continue;
            };
            let mut kept: Vec<String> = found
                .incomplete
                .iter()
                .map(|i| format!("{}: a note's revision without its record", i.dir.display()))
                .collect();
            kept.extend(
                found
                    .damaged
                    .iter()
                    .map(|d| format!("{}: {}", d.record_path.display(), d.why)),
            );
            let Some(first) = found.complete.first() else {
                if !kept.is_empty() {
                    // Nothing consumable, but something owed: counted under
                    // the id so the shutdown guard can say so.
                    let mut e = PageEntry::new(
                        PageKey::Bucket(koru_core::tasks::Bucket::Today),
                        workspace,
                        document,
                        0,
                        PagePhase::Ready,
                    );
                    e.kept = kept;
                    self.pages.entries.insert(document, e);
                }
                continue;
            };
            let Some(page) = PageKey::parse(&first.record.page) else {
                continue;
            };
            let Ok(mut doc) = WorkspaceDoc::page_genesis(workspace, document) else {
                continue;
            };
            let mut pending = Vec::new();
            for revision in &found.complete {
                let Ok(updates) =
                    std::fs::read(revision.dir.join(crate::store::revisions::UPDATES))
                else {
                    continue;
                };
                if doc.import(&updates).is_err() {
                    continue;
                }
                if let Ok(version) = loro::VersionVector::decode(&revision.record.version) {
                    pending.push((revision.name, version));
                }
            }
            let _ = doc.stamp_document_id(document);
            doc.enable_undo();
            let title = self.page_title(page);
            let replica = doc.peer_id();
            let mut e = PageEntry::new(
                page,
                workspace,
                document,
                first.record.generation,
                PagePhase::Registering,
            );
            e.title = title;
            e.replica = replica;
            e.held = Some(doc);
            e.pending = pending;
            e.kept = kept;
            self.pages.entries.insert(document, e);
        }
    }

    // ------------------------------------------------ what the shell asks

    /// Pages whose writing the store does not hold yet, and why the last
    /// revision could not be written if it could not.
    pub(crate) fn pages_owed(&self) -> (usize, Option<String>) {
        let mut owed = 0;
        let mut not_buffered = None;
        for (id, entry) in &self.pages.entries {
            let moved = self
                .page_doc(*id)
                .is_some_and(|doc| doc.version() != entry.baseline);
            if (moved && entry.phase != PagePhase::Ready)
                || !entry.pending.is_empty()
                || !entry.kept.is_empty()
            {
                owed += 1;
            }
            if not_buffered.is_none() {
                not_buffered = entry.revision_failure.clone();
            }
        }
        (owed, not_buffered)
    }

    /// Write every pending page's revision now, whatever the clock says.
    pub(crate) fn buffer_pages_now(&mut self) {
        let ids: Vec<DocumentId> = self.pages.entries.keys().copied().collect();
        for id in ids {
            self.write_page_revision(id, true);
        }
    }

    /// When the next revision is due, if any page has unwritten changes.
    pub(crate) fn pages_next_due(&self) -> Option<Instant> {
        self.pages
            .entries
            .iter()
            .filter(|(id, entry)| {
                entry.phase != PagePhase::Empty
                    && self.page_doc(**id).is_some_and(|doc| {
                        doc.version() != entry.baseline
                            && entry.written.as_ref() != Some(&doc.version())
                    })
            })
            .map(|(_, entry)| {
                entry
                    .written_at
                    .map(|at| at + REVISION_INTERVAL)
                    .unwrap_or_else(Instant::now)
            })
            .min()
    }

    /// Whether any page still has a request or unconfirmed writing — part of
    /// what keeps the store watchdog awake.
    pub(crate) fn pages_busy(&self) -> bool {
        self.pages.entries.values().any(PageEntry::owes)
    }

    /// A tab let go of `id`: when the registry still owes something for it,
    /// take the document and its room back rather than dropping them.
    /// Answers whether it did.
    pub(crate) fn take_page_back(&mut self, id: DocumentId) -> bool {
        if !self.pages.is_pending(id) || self.active == Some(id) {
            return false;
        }
        let Some(entry) = self.pages.entries.get_mut(&id) else {
            return false;
        };
        if let Some(doc) = self.others.remove(&id) {
            entry.held = Some(doc);
        }
        if let Some(session) = self.doc_sessions.remove(&id) {
            entry.session = Some(session);
        }
        true
    }

    /// Before the workspace switches: every page still owed keeps its
    /// document and room here, so a switch cannot lose a note the store has
    /// not confirmed.
    pub(crate) fn detach_pages(&mut self) {
        let ids: Vec<DocumentId> = self.pages.entries.keys().copied().collect();
        for id in ids {
            if !self.pages.is_pending(id) {
                continue;
            }
            if self.active == Some(id) {
                self.park_active();
            }
            self.take_page_back(id);
        }
    }

    /// The waker, for the rooms the registry holds itself.
    pub(crate) fn wake_pages_with(&mut self, waker: &crate::store::link::Waker) {
        for entry in self.pages.entries.values_mut() {
            if let Some(session) = &mut entry.session {
                session.set_waker(waker.clone());
            }
        }
    }

    /// Drain the rooms the registry holds itself, so a held document keeps
    /// up and its acknowledgements arrive.
    pub(crate) fn tick_held_pages(&mut self) {
        for entry in self.pages.entries.values_mut() {
            if let (Some(session), Some(doc)) = (&mut entry.session, &mut entry.held) {
                let _ = session.publish(doc);
                let _ = session.drain(doc);
            }
        }
    }

    // ----------------------------------------------------- deletion lifecycle

    /// This device's replica id and version of `document`, when it holds a
    /// copy — open, parked or held by the registry.
    fn page_local_version(&self, document: DocumentId) -> Option<(u64, loro::VersionVector)> {
        self.page_doc(document)
            .map(|doc| (doc.peer_id(), doc.version()))
    }

    /// Mint a deletion event for a page document being permanently deleted
    /// (plan §6.2, §6.4). The body stays retained — the manifest only
    /// unregisters — so this records *that* it was deleted, with the coverage
    /// this device can vouch for: its own version of the document when it
    /// holds a copy, else the empty vector (conservative: a peer's concurrent
    /// writing then shows as a conflict until the version route lets a
    /// not-open deleter read coverage, §6.7).
    fn mint_page_deletion(&mut self, document: DocumentId, deleted_ms: i64) {
        let Some((page, generation)) = self
            .page_documents()
            .into_iter()
            .find(|(id, _, _)| *id == document)
            .map(|(_, page, generation)| (page, generation))
        else {
            return;
        };
        let covers = self
            .page_local_version(document)
            .map(|(_, vv)| vv)
            .unwrap_or_default();
        if let Some(system) = &mut self.system {
            system.write_deletion(
                page,
                generation,
                koru_core::pages::deletion::EventId::new(),
                &covers,
                deleted_ms,
            );
            self.lists_changed = true;
        }
    }

    /// Report, for every in-force event of `page`'s current generation, this
    /// device's version of the document when it falls outside the event's
    /// coverage (plan §6.2, *evidence only grows*; the reconnect/start-up
    /// compare that stands in for the commit watch until §6.2's push lands).
    fn report_page_uncovered(&mut self, page: PageKey, generation: u64) {
        let Some(workspace) = self.current else {
            return;
        };
        let document = page_document_id(workspace, page, generation);
        let Some((replica, version)) = self.page_local_version(document) else {
            return;
        };
        let Some(record) = self
            .system
            .as_ref()
            .and_then(|s| s.deletion_record(page, generation))
        else {
            return;
        };
        let owed: Vec<koru_core::pages::deletion::EventId> = record
            .events_in_force()
            .into_iter()
            .filter(|event| {
                record
                    .coverage(*event)
                    .is_none_or(|coverage| !koru_core::pages::covers(&coverage, &version))
            })
            .collect();
        if owed.is_empty() {
            return;
        }
        if let Some(system) = &mut self.system {
            for event in owed {
                system.report_uncovered(page, generation, replica, event, &version);
            }
            self.lists_changed = true;
        }
    }

    /// The resolved state of `page`'s current generation — what the Tasks
    /// page shows: Live, Deleted (a plain deletion, no prompt) or Conflict
    /// (shown on both devices with *Keep deleted* / *Recover*). Reports this
    /// device's own uncovered writing first.
    pub fn page_resolution(&mut self, page: PageKey) -> koru_core::pages::deletion::Resolution {
        use koru_core::pages::deletion::Resolution;
        self.ensure_system(false);
        let Some(generation) = self.page_generation(page) else {
            return Resolution::Live;
        };
        self.report_page_uncovered(page, generation);
        self.system
            .as_ref()
            .and_then(|s| s.deletion_record(page, generation))
            .map(|record| record.resolve())
            .unwrap_or(Resolution::Live)
    }

    /// The replica id this device writes decisions under for `page`'s current
    /// generation — its own copy's peer when it holds one.
    fn page_replica(&self, page: PageKey, generation: u64) -> u64 {
        self.current
            .map(|ws| page_document_id(ws, page, generation))
            .and_then(|document| self.page_local_version(document))
            .map(|(replica, _)| replica)
            .unwrap_or(0)
    }

    /// *Keep deleted* every in-force conflict on `page`'s current generation:
    /// one decision per event, covering the writing the conflict showed
    /// (plan §6.3). The conflict closes; the body stays retained.
    pub fn keep_page_deleted(&mut self, page: PageKey) -> bool {
        use koru_core::pages::deletion::{DecisionId, Keep};
        self.ensure_system(false);
        let Some(generation) = self.page_generation(page) else {
            return false;
        };
        let replica = self.page_replica(page, generation);
        let Some(record) = self
            .system
            .as_ref()
            .and_then(|s| s.deletion_record(page, generation))
        else {
            return false;
        };
        let decisions: Vec<_> = record
            .conflicts()
            .into_iter()
            .filter_map(|event| {
                record
                    .closing_coverage(event)
                    .map(|covers| (event, Keep { replica, covers }))
            })
            .collect();
        if decisions.is_empty() {
            return false;
        }
        if let Some(system) = &mut self.system {
            for (event, keep) in decisions {
                system.write_keep(page, generation, event, DecisionId::new(), &keep);
            }
            self.lists_changed = true;
        }
        true
    }

    /// *Recover* `page`'s current generation: name every event this device
    /// has observed (plan §6.3). Recover beats every Keep. The document's
    /// body is re-listed through the registry's ordinary create-or-join on
    /// the next `ensure_page`, which re-seeds the retained candidate.
    pub fn recover_page(&mut self, page: PageKey) -> bool {
        self.ensure_system(false);
        let Some(generation) = self.page_generation(page) else {
            return false;
        };
        let replica = self.page_replica(page, generation);
        let Some(record) = self
            .system
            .as_ref()
            .and_then(|s| s.deletion_record(page, generation))
        else {
            return false;
        };
        let observed: Vec<_> = record.events().collect();
        if observed.is_empty() {
            return false;
        }
        if let Some(system) = &mut self.system {
            system.write_recover(page, generation, replica, observed);
            self.lists_changed = true;
        }
        true
    }

    /// Restoring a page document from the trash is a *Recover* of every event
    /// this device observed (plan §6.4, owner Q2): with no replacement, the
    /// page reconnects to it.
    pub(crate) fn recover_on_restore(&mut self, document: DocumentId) {
        if let Some((_, page, _)) = self
            .page_documents()
            .into_iter()
            .find(|(id, _, _)| *id == document)
        {
            self.recover_page(page);
        }
    }

    /// Mint a deletion event for every page document a permanent deletion is
    /// about to unregister — called from the trash sweep and the named purge
    /// before the manifest row goes.
    pub(crate) fn mint_page_deletions(&mut self, ids: &[DocumentId], now_ms: i64) {
        let pages: std::collections::HashSet<DocumentId> = self
            .page_documents()
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        for id in ids {
            if pages.contains(id) {
                self.mint_page_deletion(*id, now_ms);
            }
        }
    }
}

fn phase_token(phase: &PagePhase) -> &'static str {
    match phase {
        PagePhase::Empty => "empty",
        PagePhase::Registering => "registering",
        PagePhase::Loading => "loading",
        PagePhase::Ready => "ready",
        PagePhase::Failed { .. } => "failed",
    }
}
