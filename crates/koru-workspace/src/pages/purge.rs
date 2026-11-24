// SPDX-License-Identifier: MPL-2.0

//! Final purge is separate from recoverable page deletion. Local intent is
//! durable before events, and events are durable before manifest removal.

use super::*;
use koru_core::pages::purge::{Coverage, Event};

#[derive(Default)]
pub(super) struct PendingPurge {
    events: Vec<Event>,
    writing: Option<mpsc::Receiver<Result<Vec<Event>, String>>>,
}

pub(crate) struct TombstoneDurable {
    root: DocumentId,
}

impl Workspace {
    pub(crate) fn quiesce_purged_page(&mut self, document: DocumentId) {
        if let Some(entry) = self.pages.entries.get_mut(&document) {
            if let Some(session) = entry.session.take() {
                session.leave();
            }
            // A late result must not reattach the old-id room to this window.
            entry.request = None;
        }
    }

    pub(crate) fn forget_reconciled_page(&mut self, document: DocumentId) {
        self.pages.entries.remove(&document);
    }
    /// Immutable events for a sender to compare with the Cloud removal list.
    /// None means system-document identity is still being resolved.
    pub fn document_purge_events(&mut self) -> Result<Option<Vec<Event>>, String> {
        self.ensure_system(false);
        match &self.system {
            Some(system) => system.purge_events(),
            None => Ok(Some(Vec::new())),
        }
    }

    /// Poll with the same batch until true before advancing a remote cursor.
    /// This records evidence only; changed local content requires a decision.
    pub fn ingest_document_purge_events(&mut self, events: &[Event]) -> Result<bool, String> {
        if events.is_empty() {
            return Ok(true);
        }
        self.ensure_system(true);
        self.system
            .as_mut()
            .ok_or("system document is not open")?
            .ingest_purge_events(events)
    }

    pub fn is_document_purged(&self, document: DocumentId) -> bool {
        self.system
            .as_ref()
            .is_some_and(|system| system.is_document_purged(document))
    }

    pub fn document_purge_coverage(&self, document: DocumentId) -> Option<Coverage> {
        self.system.as_ref()?.purge_coverage(document)
    }

    /// Quiesce the initiating device's editors while intent is being saved.
    /// This is not deletion evidence and never permits releasing source bytes.
    pub(crate) fn page_purge_requested(&self, document: DocumentId) -> bool {
        self.pages.pending_purges.keys().any(|root| {
            *root == document
                || self
                    .purge_subtree(*root)
                    .is_some_and(|ids| ids.contains(&document))
        })
    }

    pub(crate) fn request_page_purge(&mut self, root: DocumentId) {
        self.pages.pending_purges.entry(root).or_default();
    }

    fn purge_intent_dir(&self) -> Option<PathBuf> {
        // A detached, in-memory workspace must never write the real user's
        // default Store. Tests and embedded hosts supply their actual root.
        let (_, root) = self.store.as_ref()?;
        let base = root.clone().or_else(koru_channel::store_root)?;
        Some(base.join("purges").join(self.current?.to_storage()))
    }

    fn purge_subtree(&self, root: DocumentId) -> Option<Vec<DocumentId>> {
        let entries = self.manifest.as_ref()?.entries();
        let mut subtree = std::collections::HashSet::from([explore::store_document(root)]);
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
        let mut ids: Vec<_> = subtree.into_iter().map(explore::koru_document).collect();
        ids.sort_unstable();
        Some(ids)
    }

    /// None inside Some asks the intent worker to read the closed local source.
    /// Outer None waits until open writing is persisted before fixing a VV.
    fn purge_source_version(&self, id: DocumentId) -> Option<Option<loro::VersionVector>> {
        let Some((_, version)) = self.page_local_version(id) else {
            return Some(None);
        };
        let session = self
            .doc_sessions
            .get(&id)
            .or_else(|| self.pages.entries.get(&id)?.session.as_ref());
        let acknowledged = session
            .and_then(StoreSession::acked)
            .is_some_and(|acked| acked.includes_vv(&version));
        let revision = self
            .pages
            .entries
            .get(&id)
            .and_then(|entry| entry.written.as_ref())
            .is_some_and(|written| written.includes_vv(&version));
        (acknowledged || revision).then_some(Some(version))
    }

    pub(crate) fn prove_page_purge(&mut self, root: DocumentId) -> Option<TombstoneDurable> {
        self.discover_purge_intents();
        if !self.pages.purges_discovered {
            return None;
        }
        self.ensure_system(true);
        if !self.system.as_ref()?.pages_loaded() {
            return None;
        }
        let result = self
            .pages
            .pending_purges
            .get(&root)?
            .writing
            .as_ref()
            .map(|rx| rx.try_recv());
        match result {
            Some(Ok(Ok(events))) => {
                for event in &events {
                    tracing::info!(target: "cloud.document.removal", event_id = %event.event_id, document_id = %event.document_id, stage = "event", code = "intent_durable", age_ms = 0_u64, age_scope = "intent");
                }
                let pending = self.pages.pending_purges.get_mut(&root)?;
                pending.events.extend(events);
                pending.writing = None;
            }
            Some(Ok(Err(error))) => {
                self.pages.pending_purges.get_mut(&root)?.writing = None;
                tracing::warn!(%error, "could not prepare final-purge intent");
                return None;
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                // The disconnected case is reported below without inventing
                // successful durability; retry can rebuild the same intent.
                self.pages.pending_purges.get_mut(&root)?.writing = None;
                tracing::warn!("could not persist final-purge intent");
                return None;
            }
            Some(Err(mpsc::TryRecvError::Empty)) => return None,
            None => {}
        }
        let subtree = self.purge_subtree(root)?;
        let previous = &self.pages.pending_purges.get(&root)?.events;
        let missing: Vec<_> = subtree
            .iter()
            .copied()
            .filter(|id| {
                !previous
                    .iter()
                    .any(|event| event.document_id == id.as_uuid())
            })
            .collect();
        if !missing.is_empty() {
            let mut sources = Vec::new();
            for id in missing {
                let version = self.purge_source_version(id)?;
                sources.push((id, version));
            }
            let dir = self.purge_intent_dir()?;
            let workspace = self.current?;
            let actor = self.actor.clone().unwrap_or_default();
            let (segment, store_root) = self.store.clone()?;
            let mut client = self.store_client();
            let (send, receive) = mpsc::channel();
            let waker = self.waker.clone();
            std::thread::Builder::new()
                .name("purge-intent".into())
                .spawn(move || {
                    let result = (|| {
                        let events = crate::store::purge_intent::prepare(
                            workspace,
                            root,
                            &actor,
                            sources,
                            store_root.as_deref(),
                            |document| {
                                if client.is_none() {
                                    client = Some(
                                        match store_root.as_deref() {
                                            Some(path) => {
                                                koru_api_client::Client::open_at(&segment, path)
                                            }
                                            None => koru_api_client::Client::open(&segment),
                                        }
                                        .map_err(|error| error.to_string())?,
                                    );
                                }
                                match client.as_ref().expect("opened Store client").snapshot(
                                    explore::store_workspace(workspace),
                                    explore::store_document(document),
                                ) {
                                    Ok(snapshot) => Ok(Some(snapshot)),
                                    Err(error)
                                        if error.code()
                                            == koru_store_core::errors::Code::DocumentNotFound =>
                                    {
                                        Ok(None)
                                    }
                                    Err(error) => Err(error.to_string()),
                                }
                            },
                        )?;
                        crate::store::purge_intent::write(&dir, &workspace.to_storage(), &events)?;
                        Ok(events)
                    })();
                    let _ = send.send(result);
                    if let Some(waker) = waker {
                        waker.wake();
                    }
                })
                .ok()?;
            self.pages.pending_purges.get_mut(&root)?.writing = Some(receive);
            return None;
        }
        let events = self.pages.pending_purges.get(&root)?.events.clone();
        match self.ingest_document_purge_events(&events) {
            Ok(true) => Some(TombstoneDurable { root }),
            Ok(false) => None,
            Err(error) => {
                tracing::warn!(%error, "could not persist final-purge events");
                None
            }
        }
    }

    pub(crate) fn remove_durable_page_tree(
        &mut self,
        proof: TombstoneDurable,
    ) -> Result<bool, koru_store_core::errors::Error> {
        let Some(manifest) = &mut self.manifest else {
            return Ok(false);
        };
        let removed = manifest.remove_document(explore::store_document(proof.root))?;
        for id in removed {
            self.pages
                .pending_purges
                .remove(&explore::koru_document(id));
        }
        self.publish_manifest();
        self.entries_changed = true;
        Ok(true)
    }

    fn discover_purge_intents(&mut self) {
        if self.pages.purges_discovered {
            return;
        }
        if let Some(receive) = &self.pages.purge_discovery {
            match receive.try_recv() {
                Ok(Ok(batches)) => {
                    for events in batches {
                        if let Some(first) = events.first() {
                            let root = DocumentId::from_uuid(first.root_id);
                            let pending = self.pages.pending_purges.entry(root).or_default();
                            for event in events {
                                if !pending.events.contains(&event) {
                                    pending.events.push(event);
                                }
                            }
                        }
                    }
                    self.pages.purges_discovered = true;
                    self.pages.purge_discovery = None;
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "could not read final-purge intents");
                    self.pages.purge_discovery = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.pages.purge_discovery = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            return;
        }
        let Some(dir) = self.purge_intent_dir() else {
            return;
        };
        let Some(workspace) = self.current else {
            return;
        };
        let (send, receive) = mpsc::channel();
        let waker = self.waker.clone();
        if std::thread::Builder::new()
            .name("purge-recovery".into())
            .spawn(move || {
                let _ = send.send(crate::store::purge_intent::read(
                    &dir,
                    &workspace.to_storage(),
                ));
                if let Some(waker) = waker {
                    waker.wake();
                }
            })
            .is_ok()
        {
            self.pages.purge_discovery = Some(receive);
        }
    }

    pub(super) fn tick_page_purges(&mut self) -> bool {
        self.discover_purge_intents();
        let roots: Vec<_> = self.pages.pending_purges.keys().copied().collect();
        let mut changed = false;
        for root in roots {
            if self
                .manifest
                .as_ref()
                .is_some_and(|m| !m.contains(explore::store_document(root)))
            {
                self.pages.pending_purges.remove(&root);
                continue;
            }
            if let Some(proof) = self.prove_page_purge(root) {
                match self.remove_durable_page_tree(proof) {
                    Ok(removed) => changed |= removed,
                    Err(error) => tracing::warn!(%error, "could not finish final document purge"),
                }
            }
        }
        changed
    }
}
