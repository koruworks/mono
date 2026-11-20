// SPDX-License-Identifier: MPL-2.0

//! Device-local decisions for final deletion. A missing manifest row is not
//! evidence that this device has no content: workers inspect its Store and
//! outbox by document id before releasing anything.

use crate::{Workspace, explore};
use koru_core::pages::purge::{Coverage, Event};
use koru_core::{DocumentId, WorkspaceId};
use std::{collections::BTreeMap, path::PathBuf, sync::mpsc};

mod worker;
use worker::{Outcome, Source};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentPurgePrompt {
    pub document: DocumentId,
    pub title: String,
    pub by: String,
    pub unknown: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentPurgeDecision {
    Keep,
    Delete,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocumentPurgeProgress {
    pub checking: usize,
    pub waiting_for_decision: usize,
    pub keeping: usize,
    pub failed: usize,
}

enum Phase {
    Checking(mpsc::Receiver<Result<Outcome, String>>),
    Prompt(Source),
    Working(mpsc::Receiver<Result<Outcome, String>>),
    Settled(Option<loro::VersionVector>),
    Failed(String),
}

struct Record {
    events: Vec<Event>,
    title: String,
    parent: Option<DocumentId>,
    kind: String,
    phase: Phase,
}

#[derive(Default)]
pub(crate) struct Reconciliation {
    workspace: Option<WorkspaceId>,
    records: BTreeMap<DocumentId, Record>,
    replacements: Vec<(DocumentId, DocumentId)>,
    closed: Vec<DocumentId>,
    buffered: BTreeMap<DocumentId, loro::VersionVector>,
}

#[derive(Clone)]
struct Context {
    segment: String,
    root: Option<PathBuf>,
    workspace: WorkspaceId,
    document: DocumentId,
    title: String,
    parent: Option<DocumentId>,
    kind: String,
}

impl Workspace {
    fn frozen_open_documents(&self) -> Vec<DocumentId> {
        let mut ids: std::collections::BTreeSet<_> = self
            .others
            .keys()
            .copied()
            .chain(self.active)
            .chain(self.document_purges.records.keys().copied())
            .collect();
        ids.retain(|id| self.is_document_purged(*id) && self.page_doc(*id).is_some());
        ids.into_iter().collect()
    }

    pub(crate) fn document_purges_unconfirmed(&self) -> (usize, Option<String>) {
        let owed = self
            .frozen_open_documents()
            .into_iter()
            .filter(|id| {
                self.page_doc(*id).is_some_and(|doc| {
                    self.document_purges
                        .buffered
                        .get(id)
                        .is_none_or(|version| !version.includes_vv(&doc.version()))
                })
            })
            .count();
        (
            owed,
            (owed != 0)
                .then(|| "a frozen document is waiting for its local recovery snapshot".into()),
        )
    }

    /// The explicit shutdown flush, alongside StoreSession::buffer_now. Normal
    /// turns use the worker; closing cannot outrun its durable snapshot reply.
    pub(crate) fn buffer_document_purges_now(&mut self) {
        let Some((_, configured)) = &self.store else {
            return;
        };
        let Some(base) = configured.clone().or_else(koru_channel::store_root) else {
            return;
        };
        let Some(workspace) = self.current else {
            return;
        };
        for id in self.frozen_open_documents() {
            let Some(doc) = self.page_doc(id) else {
                continue;
            };
            let version = doc.version();
            if self
                .document_purges
                .buffered
                .get(&id)
                .is_some_and(|buffered| buffered.includes_vv(&version))
            {
                continue;
            }
            let Ok(snapshot) = doc.export_snapshot() else {
                continue;
            };
            let path = base
                .join("purge-copies")
                .join(workspace.to_storage())
                .join(id.to_storage())
                .join(format!("{}.loro", koru_store_core::checksum(&snapshot)));
            if crate::store::purge_intent::write_once(&path, &snapshot).is_ok() {
                self.document_purges.buffered.insert(id, version);
            }
        }
    }
    /// Hosts must reject editing/saving the old id immediately, including
    /// while evidence is waiting for its local durable acknowledgment.
    pub fn document_purge_frozen(&self, document: DocumentId) -> bool {
        self.is_document_purged(document) || self.page_purge_requested(document)
    }

    pub fn document_purge_prompts(&self) -> Vec<DocumentPurgePrompt> {
        self.document_purges
            .records
            .iter()
            .filter_map(|(document, record)| {
                let Phase::Prompt(source) = &record.phase else {
                    return None;
                };
                Some(DocumentPurgePrompt {
                    document: *document,
                    title: source.title.clone(),
                    by: record
                        .events
                        .first()
                        .map(|event| event.by.clone())
                        .unwrap_or_default(),
                    unknown: matches!(Coverage::of(&record.events), Some(Coverage::Unknown)),
                })
            })
            .collect()
    }

    pub fn document_purge_progress(&self) -> DocumentPurgeProgress {
        let mut out = DocumentPurgeProgress::default();
        for record in self.document_purges.records.values() {
            match &record.phase {
                Phase::Checking(_) => out.checking += 1,
                Phase::Prompt(_) => out.waiting_for_decision += 1,
                Phase::Working(_) => out.keeping += 1,
                Phase::Failed(_) => out.failed += 1,
                Phase::Settled(_) => {}
            }
        }
        out
    }

    pub fn document_purge_failures(&self) -> Vec<(DocumentId, String)> {
        self.document_purges
            .records
            .iter()
            .filter_map(|(id, record)| {
                if let Phase::Failed(error) = &record.phase {
                    Some((*id, error.clone()))
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn take_document_purge_replacements(&mut self) -> Vec<(DocumentId, DocumentId)> {
        std::mem::take(&mut self.document_purges.replacements)
    }

    /// Old tabs to close after a covered-copy discard or explicit Delete.
    pub fn take_document_purge_closed(&mut self) -> Vec<DocumentId> {
        std::mem::take(&mut self.document_purges.closed)
    }

    pub fn decide_document_purge(
        &mut self,
        document: DocumentId,
        decision: DocumentPurgeDecision,
    ) -> Result<(), String> {
        let context = self.purge_context(document)?;
        let record = self
            .document_purges
            .records
            .get(&document)
            .ok_or("document has no purge question")?;
        let Phase::Prompt(source) = &record.phase else {
            return Err("document is not waiting for a purge decision".into());
        };
        let source = source.clone();
        let frozen = self.capture_purge_source(document)?;
        let receive =
            self.spawn_purge_worker(move || worker::decide(context, source, frozen, decision))?;
        self.document_purges
            .records
            .get_mut(&document)
            .expect("record retained")
            .phase = Phase::Working(receive);
        Ok(())
    }

    pub fn retry_document_purge(&mut self, document: DocumentId) {
        if self
            .document_purges
            .records
            .get(&document)
            .is_some_and(|record| matches!(record.phase, Phase::Failed(_)))
        {
            self.document_purges.records.remove(&document);
        }
    }

    fn purge_context(&self, document: DocumentId) -> Result<Context, String> {
        let (segment, root) = self
            .store
            .clone()
            .ok_or("workspace is not connected to a Store")?;
        let workspace = self.current.ok_or("workspace identity is unavailable")?;
        let record = self.document_purges.records.get(&document);
        let entry = self.manifest.as_ref().and_then(|manifest| {
            manifest
                .entries()
                .into_iter()
                .find(|entry| entry.id == explore::store_document(document))
        });
        let is_reading = self
            .system
            .as_ref()
            .and_then(|system| system.purge_events().ok().flatten())
            .is_some_and(|events| {
                events.iter().any(|event| {
                    koru_core::ids::reading_document_id(DocumentId::from_uuid(event.document_id))
                        == document
                })
            });
        Ok(Context {
            segment,
            root,
            workspace,
            document,
            title: entry
                .as_ref()
                .map(|entry| entry.title.clone())
                .or_else(|| record.map(|record| record.title.clone()))
                .unwrap_or_default(),
            parent: entry
                .as_ref()
                .and_then(|entry| entry.parent.map(explore::koru_document))
                .or_else(|| record.and_then(|record| record.parent)),
            kind: entry
                .as_ref()
                .map(|entry| entry.kind.clone())
                .or_else(|| record.map(|record| record.kind.clone()))
                .unwrap_or_else(|| {
                    if is_reading {
                        koru_core::explore::KIND_READING.into()
                    } else {
                        koru_core::explore::KIND_DOCUMENT.into()
                    }
                }),
        })
    }

    fn capture_purge_source(&mut self, document: DocumentId) -> Result<Option<Vec<u8>>, String> {
        if let Some(session) = self.doc_sessions.remove(&document) {
            session.leave();
        }
        self.quiesce_purged_page(document);
        // A Loro fork freezes the exact frontier; no later editor mutation is
        // included by accident. Hosts also enforce document_purge_frozen.
        self.page_doc(document)
            .map(|doc| {
                doc.doc()
                    .fork()
                    .export(loro::ExportMode::Snapshot)
                    .map_err(|e| e.to_string())
            })
            .transpose()
    }

    fn spawn_purge_worker(
        &self,
        work: impl FnOnce() -> Result<Outcome, String> + Send + 'static,
    ) -> Result<mpsc::Receiver<Result<Outcome, String>>, String> {
        let (send, receive) = mpsc::channel();
        let waker = self.waker.clone();
        std::thread::Builder::new()
            .name("document-purge".into())
            .spawn(move || {
                let _ = send.send(work());
                if let Some(waker) = waker {
                    waker.wake();
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(receive)
    }

    pub(crate) fn tick_document_purges(&mut self) -> bool {
        if self.document_purges.workspace != self.current {
            self.document_purges = Reconciliation {
                workspace: self.current,
                ..Default::default()
            };
        }
        let Ok(Some(events)) = self.document_purge_events() else {
            return false;
        };
        let mut grouped: BTreeMap<DocumentId, Vec<Event>> = BTreeMap::new();
        for event in events {
            grouped
                .entry(DocumentId::from_uuid(event.document_id))
                .or_default()
                .push(event);
        }
        // Freeze before any ordinary document session can publish this turn.
        for id in grouped.keys() {
            if let Some(session) = self.doc_sessions.remove(id) {
                session.leave();
            }
            self.quiesce_purged_page(*id);
        }
        if !self
            .system
            .as_ref()
            .is_some_and(|system| system.pages_durable())
        {
            return false;
        }
        for (id, events) in grouped {
            let restart = self.document_purges.records.get(&id).is_none_or(|record| {
                (record.events != events && !matches!(record.phase, Phase::Working(_)))
                    || matches!(&record.phase, Phase::Settled(kept) if self.page_doc(id).is_some_and(|doc| kept.as_ref().is_none_or(|kept| !kept.includes_vv(&doc.version()))))
            });
            if restart {
                let Ok(context) = self.purge_context(id) else {
                    continue;
                };
                let title = context.title.clone();
                let parent = context.parent;
                let kind = context.kind.clone();
                let result = self.capture_purge_source(id).and_then(|frozen| {
                    let evidence = events.clone();
                    self.spawn_purge_worker(move || worker::inspect(context, frozen, evidence))
                });
                let phase = match result {
                    Ok(rx) => Phase::Checking(rx),
                    Err(error) => Phase::Failed(error),
                };
                self.document_purges.records.insert(
                    id,
                    Record {
                        events,
                        title,
                        parent,
                        kind,
                        phase,
                    },
                );
            }
        }
        let ids: Vec<_> = self.document_purges.records.keys().copied().collect();
        let mut changed = false;
        for id in ids {
            let reply = match &self.document_purges.records[&id].phase {
                Phase::Checking(rx) | Phase::Working(rx) => match rx.try_recv() {
                    Ok(reply) => Some(reply),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(Err("purge worker disconnected".into()))
                    }
                },
                _ => None,
            };
            let Some(reply) = reply else {
                continue;
            };
            let buffered = match &reply {
                Ok(Outcome::Prompt(source)) | Ok(Outcome::Prepared { source, .. }) => {
                    Some(source.version.clone())
                }
                Ok(Outcome::Released {
                    source: Some(source),
                    ..
                }) => Some(source.version.clone()),
                _ => None,
            };
            if let Some(version) = buffered {
                self.document_purges.buffered.insert(id, version);
            }
            let phase = match reply {
                Ok(Outcome::Prompt(source)) => Phase::Prompt(source),
                Ok(Outcome::Prepared {
                    source,
                    replacement,
                }) => {
                    let changed_since = self
                        .page_doc(id)
                        .is_some_and(|doc| !source.version.includes_vv(&doc.version()));
                    if changed_since {
                        self.document_purges.records.remove(&id);
                        continue;
                    }
                    match self.purge_context(id).and_then(|context| {
                        self.spawn_purge_worker(move || {
                            worker::release(context, source, replacement)
                        })
                    }) {
                        Ok(rx) => Phase::Working(rx),
                        Err(error) => Phase::Failed(error),
                    }
                }
                Ok(Outcome::Released {
                    source,
                    replacement,
                }) => {
                    // Check the live editor again after the seeded receipt. A
                    // host that admitted another edit must keep it for a new
                    // question; the new durable copy is never taken as proof
                    // that those later edits are saved.
                    let changed_since = self.page_doc(id).is_some_and(|doc| {
                        source
                            .as_ref()
                            .is_none_or(|source| !source.version.includes_vv(&doc.version()))
                    });
                    if changed_since {
                        self.document_purges.records.remove(&id);
                        continue;
                    }
                    if let Err(error) = self.suppress_purged_document(id) {
                        Phase::Failed(error)
                    } else {
                        if let Some(new) = replacement {
                            self.document_purges.replacements.push((id, new));
                        } else {
                            self.document_purges.closed.push(id);
                        }
                        self.others.remove(&id);
                        self.forget_reconciled_page(id);
                        changed = true;
                        Phase::Settled(source.map(|source| source.version))
                    }
                }
                Err(error) => Phase::Failed(error),
            };
            if let Some(record) = self.document_purges.records.get_mut(&id) {
                record.phase = phase;
            }
        }
        changed
    }

    fn suppress_purged_document(&mut self, document: DocumentId) -> Result<(), String> {
        let Some(manifest) = &mut self.manifest else {
            return Ok(());
        };
        let id = explore::store_document(document);
        if !manifest.contains(id) {
            return Ok(());
        }
        // Each child is reconciled independently. Removing one parent must
        // not silently destroy an unexamined child's manifest entry.
        let children: Vec<_> = manifest
            .entries()
            .into_iter()
            .filter(|entry| entry.parent == Some(id))
            .map(|entry| entry.id)
            .collect();
        for child in children {
            manifest
                .move_document(child, None, usize::MAX)
                .map_err(|e| e.to_string())?;
        }
        manifest.remove_document(id).map_err(|e| e.to_string())?;
        self.publish_manifest();
        self.entries_changed = true;
        Ok(())
    }
}
