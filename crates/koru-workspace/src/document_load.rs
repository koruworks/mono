// SPDX-License-Identifier: MPL-2.0

//! Preparing an existing document without blocking the window thread.
//!
//! A canceled preparation stays owned by its worker. In particular, dropping
//! its StoreSession can join the link thread and must not happen in a view's
//! close callback. The window takes ownership only after validating its live
//! destination and checking that another opening did not already install it.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};

use koru_core::{DocumentId, WorkspaceId, schema::WorkspaceDoc};

use crate::{Workspace, explore, store::StoreSession};

static RUNNING: AtomicUsize = AtomicUsize::new(0);
pub(crate) struct Permit;
impl Permit {
    pub(crate) fn acquire() -> Result<Self, String> {
        RUNNING
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < 8).then_some(count + 1)
            })
            .map(|_| Self)
            .map_err(|_| "document load capacity reached".into())
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        RUNNING.fetch_sub(1, Ordering::AcqRel);
    }
}

struct PreparedDocument {
    workspace: WorkspaceId,
    document: DocumentId,
    doc: WorkspaceDoc,
    session: StoreSession,
    replay_trouble: Vec<String>,
    seeded: bool,
}

/// A single speculative load. Drop cancels ownership without waiting for I/O.
pub struct DocumentLoad {
    value: Arc<Mutex<Option<Result<PreparedDocument, String>>>>,
    ready: mpsc::Receiver<()>,
    release: Option<mpsc::Sender<()>>,
    epoch: u64,
}

/// Capacity includes canceled workers that are still disposing their sessions.
pub fn capacity_available() -> bool {
    RUNNING.load(Ordering::Acquire) < 8
}

impl DocumentLoad {
    fn launch(
        epoch: u64,
        waker: Option<crate::store::link::Waker>,
        prepare: impl FnOnce(&mpsc::Receiver<()>) -> Result<PreparedDocument, String> + Send + 'static,
    ) -> Result<Self, String> {
        let permit = Permit::acquire()?;
        let value = Arc::new(Mutex::new(None));
        let output = Arc::clone(&value);
        let (ready_tx, ready) = mpsc::sync_channel(1);
        let (release, released) = mpsc::channel();
        std::thread::Builder::new()
            .name("koru-document-load".into())
            .spawn(move || {
                let result = prepare(&released);
                if let Ok(mut output) = output.lock() {
                    *output = Some(result);
                }
                let _ = ready_tx.send(());
                if let Some(waker) = &waker {
                    waker.wake();
                }
                // Acceptance takes the value; cancellation leaves its disposal on
                // this worker. Capacity is held through the session destructor.
                let _ = released.recv();
                if let Ok(mut output) = output.lock() {
                    output.take();
                }
                drop(permit);
                if let Some(waker) = waker {
                    waker.wake();
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            value,
            ready,
            release: Some(release),
            epoch,
        })
    }

    pub fn inspect<T>(&self, read: impl FnOnce(Result<&WorkspaceDoc, &str>) -> T) -> Option<T> {
        let value = self.value.lock().ok()?;
        Some(match value.as_ref()? {
            Ok(prepared) => read(Ok(&prepared.doc)),
            Err(error) => read(Err(error)),
        })
    }
    /// Nonblocking readiness check. The value is still owned by the worker.
    pub fn ready(&self) -> bool {
        match self.ready.try_recv() {
            Ok(()) => true,
            Err(mpsc::TryRecvError::Empty) => self.value.lock().is_ok_and(|value| value.is_some()),
            Err(mpsc::TryRecvError::Disconnected) => true,
        }
    }

    /// Accept only after checking the destination and the workspace's warm
    /// document map. Dropping an unaccepted handle disposes on its worker.
    fn take(mut self) -> Result<PreparedDocument, String> {
        let result = self
            .value
            .lock()
            .map_err(|_| "document load poisoned".to_owned())?
            .take()
            .unwrap_or_else(|| Err("document load disconnected".to_owned()));
        self.release.take();
        result
    }
}

impl Workspace {
    /// Start a read against this Workspace's already-resolved Store. No
    /// discovery, locate, join, replay or snapshot decoding occurs here.
    pub fn prepare_document(
        &self,
        workspace: WorkspaceId,
        document: DocumentId,
    ) -> Result<DocumentLoad, String> {
        if self.document_retiring(document) {
            return Err("document is retiring".into());
        }
        let (segment, root) = self
            .store
            .clone()
            .ok_or_else(|| "workspace has no Store".to_owned())?;
        let client = self
            .store_client()
            .ok_or_else(|| "workspace has no Store client".to_owned())?;
        let seed = self
            .manifest
            .as_ref()
            .and_then(|manifest| {
                manifest
                    .entries()
                    .into_iter()
                    .find(|entry| explore::koru_document(entry.id) == document)
            })
            .map(|entry| (entry.title, entry.kind));
        DocumentLoad::launch(
            self.document_epochs.get(&document).copied().unwrap_or(0),
            self.waker.clone(),
            move |released| {
                let canceled =
                    || matches!(released.try_recv(), Err(mpsc::TryRecvError::Disconnected));
                (|| {
                    if canceled() {
                        return Err("document load canceled".into());
                    }
                    let location = client
                        .locate_document(explore::store_document(document))
                        .map_err(|error| error.to_string())?;
                    if location.workspace.as_uuid() != workspace.as_uuid() {
                        return Err("document is outside the requested workspace".to_owned());
                    }
                    if canceled() {
                        return Err("document load canceled".into());
                    }
                    let (mut session, snapshot) = StoreSession::join(
                        &segment,
                        root.as_deref(),
                        explore::store_workspace(workspace),
                        explore::store_document(document),
                    )
                    .map_err(|error| error.to_string())?;
                    if canceled() {
                        return Err("document load canceled".into());
                    }
                    let mut doc = WorkspaceDoc::from_snapshot(&snapshot)
                        .map_err(|error| error.to_string())?;
                    validate(&doc, workspace, document)?;
                    let replay_trouble = session.replay_buffered(&mut doc);
                    let seeded = initialize(
                        &mut doc,
                        workspace,
                        document,
                        seed.as_ref()
                            .map(|(title, kind)| (title.as_str(), kind.as_str())),
                    )?;
                    Ok(PreparedDocument {
                        workspace,
                        document,
                        doc,
                        session,
                        replay_trouble,
                        seeded,
                    })
                })()
            },
        )
    }

    /// Install a preparation into the ordinary parked-document/session maps.
    /// The caller must check `document()` before taking it from DocumentLoad;
    /// an already-live document and its UndoManager always win that race.
    pub fn install_prepared_document(&mut self, load: DocumentLoad) -> Result<DocumentId, String> {
        // Recheck at the ownership boundary in release builds too. Until
        // these checks pass, cancellation/disposal still belongs to the worker.
        let (workspace, document) = {
            let value = load
                .value
                .lock()
                .map_err(|_| "document load poisoned".to_owned())?;
            let prepared = value
                .as_ref()
                .ok_or_else(|| "document is not ready".to_owned())?
                .as_ref()
                .map_err(Clone::clone)?;
            validate(&prepared.doc, prepared.workspace, prepared.document)?;
            (prepared.workspace, prepared.document)
        };
        if self.current != Some(workspace) {
            return Err("workspace changed during document load".into());
        }
        if self.document_retiring(document) {
            return Err("document is retiring".into());
        }
        if let Some(doc) = self.document(document) {
            validate(doc, workspace, document)?;
            return Ok(document);
        }
        if self.document_epochs.get(&document).copied().unwrap_or(0) != load.epoch {
            return Err("document retired during preparation".into());
        }
        let PreparedDocument {
            workspace: _,
            document,
            doc,
            session,
            replay_trouble,
            seeded,
        } = load.take()?;
        self.replay_trouble.extend(replay_trouble);
        if let Some(waker) = &self.waker {
            session.set_waker(waker.clone());
        }
        self.doc_sessions.insert(document, session);
        if seeded && let Some(session) = self.doc_sessions.get_mut(&document) {
            let _ = session.publish(&doc);
        }
        self.others.insert(document, doc);
        Ok(document)
    }
}

fn validate(
    doc: &WorkspaceDoc,
    workspace: WorkspaceId,
    document: DocumentId,
) -> Result<(), String> {
    if !doc.compatibility().writable() {
        return Err("document requires a newer Koru version".into());
    }
    if doc.id().is_some_and(|id| id != workspace)
        || doc.stamped_document_id().is_some_and(|id| id != document)
    {
        return Err("document identity does not match its destination".into());
    }
    Ok(())
}

/// Repairs precede undo and view acceptance. Legacy root-derived ids are not
/// Store identity; an explicit conflicting stamp is never overwritten.
fn initialize(
    doc: &mut WorkspaceDoc,
    workspace: WorkspaceId,
    document: DocumentId,
    seed: Option<(&str, &str)>,
) -> Result<bool, String> {
    validate(doc, workspace, document)?;
    let before = doc.doc().oplog_vv();
    doc.stamp_workspace_id(workspace)
        .map_err(|error| error.to_string())?;
    doc.stamp_document_id(document)
        .map_err(|error| error.to_string())?;
    if doc.stamped_document_id() != Some(document) {
        return Err("document identity is invalid".into());
    }
    doc.unify_document_roots()
        .map_err(|error| error.to_string())?;
    if doc.walk().map_err(|error| error.to_string())?.is_empty() {
        let (title, kind) =
            seed.ok_or_else(|| "empty document is absent from the manifest".to_owned())?;
        crate::seed_document(doc, title, kind)
            .ok_or_else(|| "document initialization failed".to_owned())?;
        doc.stamp_schema_version()
            .map_err(|error| error.to_string())?;
    }
    validate(doc, workspace, document)?;
    let changed = before != doc.doc().oplog_vv();
    doc.enable_undo();
    Ok(changed)
}

#[cfg(test)]
pub(crate) mod tests;
