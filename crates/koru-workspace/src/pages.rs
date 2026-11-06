// SPDX-License-Identifier: MPL-2.0

//! Page documents on Tasks pages: the registry that makes one exist
//! (page-document plan §6.9).
//!
//! A Tasks page's document is written before it exists anywhere: the first
//! keystroke lands in a document this process built from the page's
//! deterministic genesis (`WorkspaceDoc::page_genesis`), and only then is the
//! store asked to hold it — create-or-join, off the thread that answers the
//! user, through the store's candidate-seed route, which answers an id that
//! already exists with its stored bytes and so is safe to repeat.
//!
//! The registry keeps no second editing path beside the one every tab has.
//! A page's document is parked in the workspace under its derived id the
//! moment the page is shown, with no store session yet; selecting the Tasks
//! tab activates it like any document, so the page body is a leaf over the
//! active document and every dispatch, catch-up and release rule that holds
//! for a document tab holds for it unchanged. What the registry owns is the
//! state of that id, the request in flight for it, and the pending revisions
//! on disk that let unsent writing survive a crash
//! ([`revisions`](crate::store::revisions)). A document the workspace lets
//! go of while its registration is still owed comes back here rather than
//! being dropped.
//!
//! The first input writes a revision before anything is sent; later edits
//! are written again at most once a second, and unconditionally on the way
//! out. A revision is released only once the store's durable acknowledgement
//! covers its version (§6.3), never on a timer. An image reaches the store
//! before its node can exist (the host's image import puts the bytes first),
//! so a record lists the hashes the document references and holds no copy.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use koru_core::ids::{DocumentId, PageKey, WorkspaceId, page_document_id};
use koru_core::schema::WorkspaceDoc;

use crate::explore;
use crate::store::revisions::{
    DiskFs, Record, ReleaseRevision, RevisionName, WriteRevision, discover,
};
use crate::store::{StoreSession, outbox};
use crate::{Pending, Workspace};

/// How long typing is left to settle before the pending revision is
/// rewritten — the outbox's own interval, for the same reason.
const REVISION_INTERVAL: Duration = Duration::from_secs(1);

/// Where a page's document stands, as the Tasks page shows it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PagePhase {
    /// The id is known and no document exists anywhere; a document built
    /// from the genesis waits in the workspace for the first input.
    Empty,
    /// Something was written and create-or-join is running.
    Registering,
    /// An existing document is being opened off-thread.
    Loading,
    /// Store-backed, like any open document.
    Ready,
    /// Create-or-join or the open failed; the document and its writing are
    /// kept, and *Retry* runs it again. `opening` says which it was: a
    /// document the store already listed would not open, or a note written
    /// here could not be registered.
    Failed { why: String, opening: bool },
}

/// A page's document and its phase, answered to the Tasks page every turn.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PageState {
    pub page: PageKey,
    pub document: DocumentId,
    pub generation: u64,
    pub phase: PagePhase,
}

/// What the registration thread hands back: the room joined, and the
/// store's state to fold in.
pub(crate) struct Registration {
    session: StoreSession,
    snapshot: Vec<u8>,
}

type Outcome = Result<Registration, String>;

/// One page document the registry is responsible for.
pub(crate) struct PageEntry {
    pub page: PageKey,
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    pub generation: u64,
    pub phase: PagePhase,
    /// The document while no workspace holds it: discovered at start-up, or
    /// handed back when its tab closed before the registration landed.
    pub held: Option<WorkspaceDoc>,
    /// Its room, while the document is held rather than parked.
    session: Option<StoreSession>,
    /// The version right after genesis; moving past it is "something was
    /// written", which is what triggers registration.
    baseline: loro::VersionVector,
    request: Option<mpsc::Receiver<Outcome>>,
    /// Whether the next request seeds the candidate (a document this
    /// process started) or only joins (one the store already lists).
    seed: bool,
    /// The semantic title the seed carries into the manifest: a list's name
    /// or a bucket's token, never a localized label.
    title: String,
    /// The replica the revisions are named under, and the next number —
    /// `<replica>.<rev>` names are never reused.
    replica: u64,
    next_rev: u64,
    /// What the last written revision covered, and when.
    written: Option<loro::VersionVector>,
    written_at: Option<Instant>,
    /// Why the last revision could not be written, if it could not — the
    /// difference between *saved on this computer* and *saved nowhere*.
    pub revision_failure: Option<String>,
    /// Revisions on disk awaiting the store's acknowledgement.
    pending: Vec<(RevisionName, loro::VersionVector)>,
    /// What discovery found and could not consume — an incomplete directory
    /// or a damaged record — kept and counted.
    pub kept: Vec<String>,
}

impl PageEntry {
    /// An entry with nothing written, no title and no replica yet; the
    /// caller sets what it knows.
    fn new(
        page: PageKey,
        workspace: WorkspaceId,
        document: DocumentId,
        generation: u64,
        phase: PagePhase,
    ) -> Self {
        Self {
            page,
            workspace,
            document,
            generation,
            phase,
            held: None,
            session: None,
            baseline: loro::VersionVector::new(),
            request: None,
            seed: true,
            title: String::new(),
            replica: 0,
            next_rev: 1,
            written: None,
            written_at: None,
            revision_failure: None,
            pending: Vec::new(),
            kept: Vec::new(),
        }
    }

    fn state(&self) -> PageState {
        PageState {
            page: self.page,
            document: self.document,
            generation: self.generation,
            phase: self.phase.clone(),
        }
    }

    /// Whether this entry still has anything to do — a request to run or
    /// await, or writing the store has not confirmed.
    fn owes(&self) -> bool {
        self.phase != PagePhase::Ready || !self.pending.is_empty() || !self.kept.is_empty()
    }
}

/// Every page document this process is responsible for, by id.
#[derive(Default)]
pub struct PageRegistry {
    entries: HashMap<DocumentId, PageEntry>,
}

impl PageRegistry {
    /// Whether the registry still owns `id`'s fate — a document the
    /// workspace hands back rather than drops when a tab lets go of it.
    pub fn is_pending(&self, id: DocumentId) -> bool {
        self.entries.get(&id).is_some_and(PageEntry::owes)
    }

    /// The phase of `id`, if the registry knows it.
    pub fn phase(&self, id: DocumentId) -> Option<PagePhase> {
        self.entries.get(&id).map(|e| e.phase.clone())
    }

    /// The document the registry holds for `id` while no tab has it.
    #[cfg(test)]
    pub(crate) fn get_held(&self, id: DocumentId) -> Option<&WorkspaceDoc> {
        self.entries.get(&id).and_then(|e| e.held.as_ref())
    }
}

/// Where a page document's revisions live: the document's outbox directory.
fn document_dir(
    root: Option<&std::path::Path>,
    workspace: WorkspaceId,
    document: DocumentId,
) -> Option<PathBuf> {
    outbox::dir(
        root,
        explore::store_workspace(workspace),
        explore::store_document(document),
    )
}

/// A fresh token for the directory a revision is filled under.
fn part_token() -> String {
    DocumentId::new().to_storage()
}

/// The create-or-join a registration thread runs: seed the candidate when
/// this process started the document, then join its room. Every step is
/// safe to repeat — the seed answers an existing candidate with its bytes,
/// and a join is a join.
fn register(
    segment: &str,
    root: Option<&std::path::Path>,
    workspace: WorkspaceId,
    document: DocumentId,
    title: &str,
    seed: Option<Vec<u8>>,
) -> Outcome {
    let store_workspace = explore::store_workspace(workspace);
    let store_document = explore::store_document(document);
    if let Some(initial_update) = seed {
        let client = match root {
            Some(root) => koru_api_client::Client::open_at(segment, root),
            None => koru_api_client::Client::open(segment),
        }
        .map_err(|e| e.to_string())?;
        let request_hash = koru_store_core::checksum(&initial_update);
        client
            .seed_document_with_candidate(
                store_workspace,
                title,
                koru_core::explore::KIND_DOCUMENT,
                None,
                None,
                store_document,
                &format!("page:{document}"),
                &request_hash,
                initial_update,
            )
            .map_err(|e| e.to_string())?;
    }
    let (session, snapshot) = StoreSession::join(segment, root, store_workspace, store_document)
        .map_err(|e| e.to_string())?;
    Ok(Registration { session, snapshot })
}

mod workspace;

#[cfg(test)]
mod tests;
