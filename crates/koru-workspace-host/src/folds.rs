// SPDX-License-Identifier: MPL-2.0

//! Which toggles this reader closed, kept privately on this device.
//!
//! The editor folds a toggle at once and owes an explicit intent
//! ([`koru_editor::folds::FoldIntent`]); this module saves it through the
//! Store's document-private route on its own thread, never on the one that
//! answers the person, and hands every view the current answer.
//!
//! The rules it keeps, from the collapse/expand plan:
//!
//! - **A request names a value, never "invert".** Repeating one is a no-op.
//! - **A late answer cannot undo a newer click.** Every local change carries a
//!   sequence number; an acknowledgement clears only the pending changes it
//!   covers, and a pending change always wins over what the Store said.
//! - **A failed read is not an empty state.** The document shows open while
//!   unknown, and nothing is ever written back from a read — writes are only
//!   the reader's own explicit changes — so a failure cannot erase saved folds.
//! - **A failed save stays visible.** The fold stays as the reader set it for
//!   the session and [`save_failed`] says so, for the host's nonmodal notice.
//! - **One read per document per session**, plus a re-read when another
//!   window of this reader changed it (`document.private.changed`). Nothing
//!   polls, and nothing here wakes replication: this state is device-local.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock, mpsc};

use koru_store_core::document_private::{COLLAPSED, MAX_BATCH, PrivateChange, PrivateState};
use koru_store_core::ids::{DocumentId, ProfileId};
use winit::event_loop::EventLoopProxy;

use crate::app::Wake;

/// Whether this reader's saved folds for a document have arrived.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Asked for, not answered. Shown open.
    Loading,
    Ready,
    /// The read failed. Shown open, and never saved back as empty.
    Failed,
}

#[derive(Default)]
struct DocFolds {
    status: Option<Status>,
    /// What the Store last said: the toggles it holds closed.
    saved: HashSet<uuid::Uuid>,
    /// Local changes not yet acknowledged: node → (closed, sequence).
    pending: HashMap<uuid::Uuid, (bool, u64)>,
    save_failed: bool,
}

impl DocFolds {
    fn closed(&self) -> HashSet<uuid::Uuid> {
        let mut out = self.saved.clone();
        for (node, (closed, _)) in &self.pending {
            if *closed {
                out.insert(*node);
            } else {
                out.remove(node);
            }
        }
        out
    }

    fn adopt(&mut self, state: &PrivateState) {
        self.saved = closed_in(state);
        self.status = Some(Status::Ready);
    }
}

fn closed_in(state: &PrivateState) -> HashSet<uuid::Uuid> {
    state
        .nodes
        .iter()
        .filter(|(_, attributes)| attributes.get(COLLAPSED).copied().unwrap_or(false))
        .filter_map(|(node, _)| uuid::Uuid::parse_str(node).ok())
        .collect()
}

enum Job {
    SwitchProfile(ProfileId),
    Load(DocumentId),
    Save {
        document: DocumentId,
        changes: Vec<(uuid::Uuid, bool)>,
        seq: u64,
    },
}

static TX: OnceLock<mpsc::Sender<Job>> = OnceLock::new();
static PROFILE: RwLock<Option<ProfileId>> = RwLock::new(None);
static STATE: RwLock<Option<HashMap<DocumentId, DocFolds>>> = RwLock::new(None);
/// Bumped whenever any document's answer moves, so a view compares one
/// integer per frame rather than locking the map.
static VERSION: AtomicU64 = AtomicU64::new(0);
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Grows whenever any document's folds or status move.
pub fn version() -> u64 {
    VERSION.load(Ordering::Acquire)
}

fn bump() {
    VERSION.fetch_add(1, Ordering::AcqRel);
}

fn with_state<T>(f: impl FnOnce(&mut HashMap<DocumentId, DocFolds>) -> T) -> Option<T> {
    STATE
        .write()
        .ok()
        .map(|mut state| f(state.get_or_insert_with(HashMap::new)))
}

/// Make sure this document's saved folds have been asked for. Cheap to call
/// on every observe; the read goes out once per document per session.
pub fn ensure(document: DocumentId) {
    let Some(tx) = TX.get() else {
        return;
    };
    let ask = with_state(|state| {
        let entry = state.entry(document).or_default();
        if entry.status.is_some() {
            return false;
        }
        entry.status = Some(Status::Loading);
        true
    })
    .unwrap_or(false);
    if ask {
        let _ = tx.send(Job::Load(document));
    }
}

/// The toggles shown closed for `document`: what the Store holds with this
/// session's unacknowledged changes laid over it. `None` until asked.
pub fn closed(document: DocumentId) -> Option<(Status, HashSet<uuid::Uuid>)> {
    let state = STATE.read().ok()?;
    let entry = state.as_ref()?.get(&document)?;
    Some((entry.status?, entry.closed()))
}

/// Whether a save for `document` failed this session and has not since
/// succeeded.
pub fn save_failed(document: DocumentId) -> bool {
    STATE.read().is_ok_and(|state| {
        state
            .as_ref()
            .and_then(|state| state.get(&document))
            .is_some_and(|entry| entry.save_failed)
    })
}

/// Record the reader's fold changes now and save them in the background.
/// **Optimistic**: [`closed`] answers with them at once.
pub fn set(document: DocumentId, changes: Vec<(uuid::Uuid, bool)>) {
    if changes.is_empty() {
        return;
    }
    let seq = SEQ.fetch_add(1, Ordering::AcqRel) + 1;
    with_state(|state| {
        let entry = state.entry(document).or_default();
        for (node, closed) in &changes {
            entry.pending.insert(*node, (*closed, seq));
        }
    });
    bump();
    if let Some(tx) = TX.get() {
        let _ = tx.send(Job::Save {
            document,
            changes,
            seq,
        });
    }
}

/// Whether the fold thread is running for exactly this Profile.
pub fn connected_to(profile: ProfileId) -> bool {
    TX.get().is_some()
        && PROFILE
            .read()
            .is_ok_and(|active| active.as_ref() == Some(&profile))
}

/// Start the fold thread once the profile is known, or point it at a new
/// Profile — whose folds are its own, so everything known is dropped.
/// `connect` builds the Store client on the worker thread.
pub fn connect(
    proxy: &EventLoopProxy<Wake>,
    profile: ProfileId,
    connect: impl Fn() -> Result<koru_api_client::Client, koru_store_core::errors::Error>
    + Send
    + 'static,
) {
    let changed = PROFILE.write().is_ok_and(|mut current| {
        if current.as_ref() == Some(&profile) {
            false
        } else {
            *current = Some(profile);
            true
        }
    });
    if !changed {
        return;
    }
    if let Ok(mut state) = STATE.write() {
        *state = None;
    }
    bump();
    if let Some(tx) = TX.get() {
        let _ = tx.send(Job::SwitchProfile(profile));
        return;
    }
    let (tx, jobs) = mpsc::channel::<Job>();
    let reload = tx.clone();
    if TX.set(tx).is_err() {
        return;
    }
    let proxy = proxy.clone();
    let _ = std::thread::Builder::new()
        .name("koru-folds".into())
        .spawn(move || {
            let mut profile = profile;
            let mut client: Option<koru_api_client::Client> = None;
            let mut events = None;
            while let Ok(job) = jobs.recv() {
                if let Job::SwitchProfile(next) = job {
                    profile = next;
                    continue;
                }
                if client.is_none() {
                    client = connect().ok();
                }
                let Some(active) = client.as_ref() else {
                    fail(&job);
                    let _ = proxy.send_event(Wake::Folds);
                    continue;
                };
                if events.is_none() && active.events().available() {
                    let reload = reload.clone();
                    events = active
                        .events()
                        .on(
                            None,
                            koru_api_client::Filter::names("document.private."),
                            move |event| {
                                if let Some(document) = event
                                    .envelope
                                    .subject
                                    .as_ref()
                                    .and_then(|subject| DocumentId::parse(&subject.id))
                                {
                                    let _ = reload.send(Job::Load(document));
                                }
                            },
                        )
                        .ok();
                }
                run(active, profile, job);
                bump();
                let _ = proxy.send_event(Wake::Folds);
            }
        });
}

fn run(client: &koru_api_client::Client, profile: ProfileId, job: Job) {
    match job {
        Job::SwitchProfile(_) => {}
        Job::Load(document) => {
            // Another window's change arrives here too, for documents this
            // process never opened; those are not worth a read.
            let wanted = STATE.read().is_ok_and(|state| {
                state
                    .as_ref()
                    .is_some_and(|state| state.contains_key(&document))
            });
            if !wanted {
                return;
            }
            match client.document_private_state(profile, document) {
                Ok(state) => {
                    with_state(|map| map.entry(document).or_default().adopt(&state));
                }
                Err(error) => {
                    tracing::warn!(
                        target: "koru::folds",
                        code = error.code.as_str(),
                        "reading private fold state failed; toggles show open"
                    );
                    with_state(|map| {
                        let entry = map.entry(document).or_default();
                        if entry.status != Some(Status::Ready) {
                            entry.status = Some(Status::Failed);
                        }
                    });
                }
            }
        }
        Job::Save {
            document,
            changes,
            seq,
        } => {
            let mut outcome = Ok(None);
            for batch in changes.chunks(MAX_BATCH) {
                let batch = batch
                    .iter()
                    .map(|(node, closed)| PrivateChange {
                        node_id: node.to_string(),
                        key: COLLAPSED.to_owned(),
                        value: Some(*closed),
                    })
                    .collect();
                match client.apply_document_private_changes(profile, document, batch) {
                    Ok(state) => outcome = Ok(Some(state)),
                    Err(error) => {
                        outcome = Err(error);
                        break;
                    }
                }
            }
            with_state(|map| {
                let entry = map.entry(document).or_default();
                match &outcome {
                    Ok(state) => {
                        if let Some(state) = state {
                            entry.adopt(state);
                        }
                        // Clear only what this request covered: a newer
                        // click on the same toggle keeps its own entry.
                        entry.pending.retain(|_, (_, pending)| *pending > seq);
                        entry.save_failed = false;
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "koru::folds",
                            code = error.code.as_str(),
                            count = changes.len(),
                            "saving private fold state failed; kept for this session"
                        );
                        entry.save_failed = true;
                    }
                }
            });
        }
    }
}

/// No Store: a read is a failure, and a save stays pending and failed.
fn fail(job: &Job) {
    let document = match job {
        Job::Load(document) | Job::Save { document, .. } => *document,
        Job::SwitchProfile(_) => return,
    };
    with_state(|map| {
        let entry = map.entry(document).or_default();
        match job {
            Job::Load(_) if entry.status != Some(Status::Ready) => {
                entry.status = Some(Status::Failed);
            }
            Job::Save { .. } => entry.save_failed = true,
            _ => {}
        }
    });
}

#[cfg(test)]
mod tests;
