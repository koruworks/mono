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
        if let Some(system) = &self.system {
            state.note_shown = system.page_note_shown(page).unwrap_or(false);
            state.attachments = system.page_attachments(page).unwrap_or_default();
        }
        Some(state)
    }

    /// Show or hide the page's note inline (the `+` menu's *Inline
    /// description*), on the page's record so every device agrees.
    pub fn set_page_note_shown(&mut self, page: PageKey, shown: bool) -> bool {
        match &mut self.system {
            Some(system) => system.set_page_note_shown(page, shown),
            None => false,
        }
    }

    /// Attach workspace files to the page as context. The files are already
    /// the workspace's; the page holds references.
    pub fn attach_files_to_page(&mut self, page: PageKey, files: &[DocumentId]) -> bool {
        let now = crate::open::now_ms();
        let Some(system) = &mut self.system else {
            return false;
        };
        let mut changed = false;
        for (i, file) in files.iter().enumerate() {
            // Stamped a millisecond apart so a batch keeps the order it
            // was chosen in on every replica.
            changed |= system.attach_to_page(page, *file, now + i as i64);
        }
        changed
    }

    /// Take a file off the page: the reference goes, the file stays.
    pub fn detach_file_from_page(&mut self, page: PageKey, file: DocumentId) -> bool {
        match &mut self.system {
            Some(system) => system.detach_from_page(page, file),
            None => false,
        }
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
                note_shown: false,
                attachments: Vec::new(),
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
        let mut changed = self.tick_page_purges();
        let ids: Vec<DocumentId> = self.pages.entries.keys().copied().collect();
        for id in ids {
            changed |= self.poll_page(id);
            self.write_page_revision(id, false);
            self.advance_page(id);
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
            self.write_page_revision(id, false);
            if self
                .pages
                .entries
                .get(&id)
                .is_none_or(|entry| entry.written.is_none())
            {
                return;
            }
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
            let Some(doc) = self.page_doc(id) else {
                return;
            };
            Some((doc.doc().clone(), doc.doc().oplog_frontiers()))
        } else {
            None
        };
        let (workspace, title) = (entry.workspace, entry.title.clone());
        let waker = self.waker.clone();
        let (send, receive) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("koru-page-register".into())
            .spawn(move || {
                let outcome = (|| {
                    let seed = seed
                        .map(|(doc, frontier)| {
                            doc.fork_at(&frontier)
                                .map_err(|e| e.to_string())?
                                .export(loro::ExportMode::all_updates())
                                .map_err(|e| e.to_string())
                        })
                        .transpose()?;
                    register(&segment, root.as_deref(), workspace, id, &title, seed)
                })();
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

    /// Poll a worker without waiting. Only the explicit shutdown flush waits
    /// for its outstanding write; typing and maintenance never wait on disk.
    fn poll_page_revision(&mut self, id: DocumentId, wait: bool) {
        let Some(entry) = self.pages.entries.get_mut(&id) else {
            return;
        };
        let Some(reply) = &entry.revision_write else {
            return;
        };
        let outcome = if wait {
            Some(
                reply
                    .recv()
                    .unwrap_or_else(|_| Err("revision worker disconnected".into())),
            )
        } else {
            match reply.try_recv() {
                Ok(outcome) => Some(outcome),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("revision worker disconnected".into()))
                }
            }
        };
        let Some(outcome) = outcome else {
            return;
        };
        entry.revision_write = None;
        match outcome {
            Ok((name, version)) => {
                entry.written = Some(version.clone());
                entry.pending.push((name, version));
                entry.revision_failure = None;
            }
            Err(why) => entry.revision_failure = Some(why),
        }
    }

    fn write_page_revision(&mut self, id: DocumentId, force: bool) {
        self.poll_page_revision(id, force);
        let Some(entry) = self.pages.entries.get(&id) else {
            return;
        };
        if entry.phase == PagePhase::Loading || entry.revision_write.is_some() {
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
        // Clone only the shared handle and capture an exact frontier. The fork
        // and export happen on the worker and cannot include later keystrokes.
        let doc = doc.doc().clone();
        let frontier = doc.oplog_frontiers();
        let root = self.store.as_ref().and_then(|(_, root)| root.clone());
        let listed = self
            .manifest
            .as_ref()
            .is_some_and(|m| m.contains_document(explore::store_document(id)));
        let name = RevisionName {
            replica: entry.replica,
            rev: entry.next_rev,
        };
        let workspace = entry.workspace;
        let record = Record {
            workspace: workspace.to_storage(),
            document: id.to_storage(),
            page: entry.page.to_storage(),
            generation: entry.generation,
            state: phase_token(&entry.phase).to_owned(),
            seen_listed: listed,
            blobs: Vec::new(),
            version: version.encode(),
        };
        let waker = self.waker.clone();
        let (send, receive) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("koru-page-revision".into())
            .spawn(move || {
                let outcome = (|| -> Result<_, String> {
                    let dir = document_dir(root.as_deref(), workspace, id)
                        .ok_or_else(|| "no local application-data directory".to_owned())?;
                    let frozen = doc.fork_at(&frontier).map_err(|e| e.to_string())?;
                    let updates = frozen
                        .export(loro::ExportMode::all_updates())
                        .map_err(|e| e.to_string())?;
                    WriteRevision::new(&dir, name, &part_token(), updates, Vec::new(), &record)
                        .run(&mut DiskFs)
                        .map_err(|e| e.to_string())?;
                    Ok((name, version))
                })();
                let _ = send.send(outcome);
                if let Some(waker) = waker {
                    waker.wake();
                }
            });
        let entry = self.pages.entries.get_mut(&id).expect("page still present");
        entry.written_at = Some(Instant::now());
        match spawned {
            Ok(_) => {
                entry.next_rev += 1;
                entry.revision_write = Some(receive);
            }
            Err(error) => entry.revision_failure = Some(error.to_string()),
        }
        if force {
            self.poll_page_revision(id, true);
        }
    }

    /// Only acknowledged immutable revisions can enter the cleanup worker.
    fn release_page_revisions(&mut self, id: DocumentId) {
        let Some(entry) = self.pages.entries.get_mut(&id) else {
            return;
        };
        if let Some(reply) = &entry.revision_release {
            match reply.try_recv() {
                Ok(released) => {
                    entry.pending.retain(|(name, _)| !released.contains(name));
                    entry.revision_release = None;
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    entry.revision_release = None;
                }
            }
        }
        if entry
            .release_started
            .is_some_and(|at| at.elapsed() < REVISION_INTERVAL)
        {
            return;
        }
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
        let covered: Vec<_> = entry
            .pending
            .iter()
            .filter(|(_, vv)| acked.includes_vv(vv))
            .map(|(name, _)| *name)
            .collect();
        if covered.is_empty() {
            return;
        }
        entry.release_started = Some(Instant::now());
        let workspace = entry.workspace;
        let root = self.store.as_ref().and_then(|(_, root)| root.clone());
        let waker = self.waker.clone();
        let (send, receive) = mpsc::channel();
        match std::thread::Builder::new()
            .name("koru-page-release".into())
            .spawn(move || {
                let mut released = Vec::new();
                if let Some(dir) = document_dir(root.as_deref(), workspace, id) {
                    for name in covered {
                        if ReleaseRevision::of_name(&DiskFs, &dir, name)
                            .and_then(|mut release| release.run(&mut DiskFs))
                            .is_ok()
                        {
                            released.push(name);
                        }
                    }
                }
                let _ = send.send(released);
                if let Some(waker) = waker {
                    waker.wake();
                }
            }) {
            Ok(_) => entry.revision_release = Some(receive),
            Err(error) => tracing::warn!(%error, "could not start page revision cleanup"),
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
                || entry.revision_write.is_some()
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
                entry.revision_write.is_none()
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
        !self.pages.pending_purges.is_empty() || self.pages.entries.values().any(PageEntry::owes)
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
        let mut restored = std::collections::HashSet::from([explore::store_document(document)]);
        if let Some(manifest) = &self.manifest {
            let entries = manifest.entries();
            loop {
                let before = restored.len();
                for entry in &entries {
                    if entry
                        .parent
                        .is_some_and(|parent| restored.contains(&parent))
                    {
                        restored.insert(entry.id);
                    }
                }
                if before == restored.len() {
                    break;
                }
            }
        }
        for id in &restored {
            self.pages
                .pending_purges
                .remove(&explore::koru_document(*id));
        }
        for (id, page, generation) in self.page_documents() {
            if !restored.contains(&explore::store_document(id)) {
                continue;
            }
            let replica = self.page_replica(page, generation);
            if let Some(system) = &mut self.system
                && let Some(record) = system.deletion_record(page, generation)
            {
                let observed = record.events().collect();
                system.write_recover(page, generation, replica, observed);
                self.lists_changed = true;
            }
        }
    }

    pub(crate) fn request_page_purge(&mut self, root: DocumentId) {
        self.pages.pending_purges.entry(root).or_default();
    }

    /// Classify the entire subtree before removing anything. The guard stays
    /// closed while system-document identity or durability is unknown.
    pub(crate) fn prove_page_purge(&mut self, root: DocumentId) -> Option<TombstoneDurable> {
        self.ensure_system(false);
        if self.system.as_ref().is_some_and(|s| !s.pages_loaded()) {
            return None;
        }
        let entries = self.manifest.as_ref()?.entries();
        let store_root = explore::store_document(root);
        let mut subtree = std::collections::HashSet::from([store_root]);
        loop {
            let before = subtree.len();
            for entry in &entries {
                if entry.parent.is_some_and(|parent| subtree.contains(&parent)) {
                    subtree.insert(entry.id);
                }
            }
            if before == subtree.len() {
                break;
            }
        }
        // Recovery of an event this request observed cancels that request.
        // Never mint a replacement event as part of retrying it.
        let previous = self.pages.pending_purges.get(&root)?.clone();
        for (page, generation, event) in &previous {
            let record = self.system.as_ref()?.deletion_record(*page, *generation)?;
            if !record.events_in_force().contains(event) {
                self.pages.pending_purges.remove(&root);
                return None;
            }
        }
        let mut events = previous;
        for (document, page, generation) in self.page_documents() {
            if !subtree.contains(&explore::store_document(document)) {
                continue;
            }
            let record = self.system.as_ref()?.deletion_record(page, generation)?;
            let mut in_force = record.events_in_force();
            if in_force.is_empty() {
                let event = koru_core::pages::deletion::EventId::new();
                let covers = self
                    .page_local_version(document)
                    .map(|(_, v)| v)
                    .unwrap_or_default();
                if !self.system.as_mut()?.write_deletion(
                    page,
                    generation,
                    event,
                    &covers,
                    crate::open::now_ms(),
                ) {
                    return None;
                }
                in_force.push(event);
                self.lists_changed = true;
            }
            for event in in_force {
                let item = (page, generation, event);
                if !events.contains(&item) {
                    events.push(item);
                }
            }
        }
        self.pages.pending_purges.insert(root, events.clone());
        if !events.is_empty() {
            let system = self.system.as_ref()?;
            if !system.pages_durable() {
                return None;
            }
            for (page, generation, _) in &events {
                if system.deletion_record(*page, *generation)?.resolve()
                    != koru_core::pages::deletion::Resolution::Deleted
                {
                    return None;
                }
            }
        }
        Some(TombstoneDurable { root })
    }

    /// The only manifest-removal path: proof and removal occur in the same
    /// event-loop turn, with no opportunity to apply a recovery between them.
    pub(crate) fn remove_durable_page_tree(
        &mut self,
        proof: TombstoneDurable,
    ) -> Result<bool, koru_store_core::errors::Error> {
        let root = proof.root;
        let Some(manifest) = &mut self.manifest else {
            return Ok(false);
        };
        let removed = manifest.remove_document(explore::store_document(root))?;
        for id in removed {
            self.pages
                .pending_purges
                .remove(&explore::koru_document(id));
        }
        self.publish_manifest();
        self.entries_changed = true;
        Ok(true)
    }

    fn tick_page_purges(&mut self) -> bool {
        let version = (
            self.current,
            self.manifest.as_ref().map(|m| m.version()),
            self.system.as_ref().and_then(|s| s.pages_version()),
        );
        if self.pages.last_deletion_scan.as_ref() != Some(&version) {
            self.pages.last_deletion_scan = Some(version);
            // The persisted deletion events also carry intent after a restart.
            // Re-publishing and acknowledging the system document is still required.
            for (document, page, generation) in self.page_documents() {
                if self
                    .manifest
                    .as_ref()
                    .is_some_and(|m| m.is_trashed(explore::store_document(document)))
                    && let Some(record) = self
                        .system
                        .as_ref()
                        .and_then(|s| s.deletion_record(page, generation))
                    && !record.events_in_force().is_empty()
                {
                    self.pages
                        .pending_purges
                        .entry(document)
                        .or_insert_with(|| {
                            record
                                .events_in_force()
                                .into_iter()
                                .map(|event| (page, generation, event))
                                .collect()
                        });
                }
            }
        }
        let roots: Vec<_> = self.pages.pending_purges.keys().copied().collect();
        let mut changed = false;
        for root in roots {
            if self.manifest.as_ref().is_none_or(|m| {
                !m.contains(explore::store_document(root))
                    || !m.is_trashed(explore::store_document(root))
            }) {
                self.pages.pending_purges.remove(&root);
                continue;
            }
            if let Some(proof) = self.prove_page_purge(root) {
                match self.remove_durable_page_tree(proof) {
                    Ok(removed) => changed |= removed,
                    Err(error) => tracing::warn!(%error, "could not finish durable page deletion"),
                }
            }
        }
        changed
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

/// Constructed only after every page event in a removal subtree is durable.
pub(crate) struct TombstoneDurable {
    root: DocumentId,
}
