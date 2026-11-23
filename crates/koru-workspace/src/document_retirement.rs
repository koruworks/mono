// SPDX-License-Identifier: MPL-2.0

//! Closing a tab transfers its only live authority to a worker. A new join
//! cannot race the final outbox write or link disposal. Buffer failure returns
//! that same authority, including its undo history, to the ordinary sync loop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};

use koru_core::{DocumentId, WorkspaceDoc};

use crate::{Workspace, document_load::Permit, store::StoreSession};

type Owned = (WorkspaceDoc, StoreSession);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentReplacementError {
    Pending,
    Unsafe,
}
enum State {
    Waiting(Option<Owned>),
    Working,
    Done,
    Failed(Option<Owned>),
}

pub(crate) struct Retirement {
    state: Arc<Mutex<State>>,
    _release: mpsc::Sender<()>,
    owed: bool,
    buffered: bool,
}

impl Workspace {
    /// True until the old authority has either been restored or fully disposed.
    pub fn document_retiring(&self, id: DocumentId) -> bool {
        self.retirements.contains_key(&id)
    }

    /// A profile replacement must not abandon worker-owned accepted edits.
    /// The caller retries after the normal worker wake and error recovery.
    pub fn document_retirements_pending(&mut self) -> bool {
        self.poll_document_retirements();
        !self.retirements.is_empty()
    }

    /// The ownership boundary used immediately before replacing a Workspace.
    /// Profile preparation may have started earlier; accepted edits since then
    /// must be secured too. This retains the existing explicit-switch save.
    pub fn prepare_document_replacement(&mut self) -> Result<(), DocumentReplacementError> {
        if self.document_retirements_pending() {
            return Err(DocumentReplacementError::Pending);
        }
        self.save();
        if self.unconfirmed().would_lose_work() {
            return Err(DocumentReplacementError::Unsafe);
        }
        Ok(())
    }

    pub(crate) fn retire_document(&mut self, id: DocumentId) -> bool {
        if self.document_retiring(id) {
            return true;
        }
        let Some(doc) = self.others.get(&id) else {
            return false;
        };
        let Some(session) = self.doc_sessions.get(&id) else {
            self.others.remove(&id);
            return true;
        };
        let Ok(permit) = Permit::acquire() else {
            return false;
        };
        let owed = session.owes(doc);
        let buffered = session.buffered_covers(doc);
        let owned = (
            self.others.remove(&id).unwrap(),
            self.doc_sessions.remove(&id).unwrap(),
        );
        let state = Arc::new(Mutex::new(State::Waiting(Some(owned))));
        let output = Arc::clone(&state);
        let (release, released) = mpsc::channel();
        let waker = self.waker.clone();
        let spawned = std::thread::Builder::new()
            .name("koru-document-retire".into())
            .spawn(move || {
                let _permit = permit;
                let owned = match std::mem::replace(&mut *output.lock().unwrap(), State::Working) {
                    State::Waiting(Some(owned)) => owned,
                    _ => return,
                };
                let (doc, mut session) = owned;
                session.buffer_now(&doc);
                if session.owes(&doc) && !session.buffered_covers(&doc) {
                    *output.lock().unwrap() = State::Failed(Some((doc, session)));
                } else {
                    session.leave();
                    // Includes the socket thread join. Publish Done only afterwards.
                    drop(session);
                    drop(doc);
                    *output.lock().unwrap() = State::Done;
                }
                if let Some(waker) = &waker {
                    waker.wake();
                }
                // Failure disposal, including abandonment on process close, stays
                // on this worker. A live Workspace normally takes the pair back.
                let _ = released.recv();
                *output.lock().unwrap() = State::Done;
                drop(_permit);
                if let Some(waker) = waker {
                    waker.wake();
                }
            });
        if spawned.is_err() {
            if let State::Waiting(Some((doc, session))) =
                std::mem::replace(&mut *state.lock().unwrap(), State::Done)
            {
                self.others.insert(id, doc);
                self.doc_sessions.insert(id, session);
            }
            return false;
        }
        self.retirements.insert(
            id,
            Retirement {
                state,
                _release: release,
                owed,
                buffered,
            },
        );
        self.retirement_retries.remove(&id);
        let epoch = self.document_epochs.entry(id).or_default();
        *epoch = epoch.wrapping_add(1);
        true
    }

    /// Called by the normal woken maintenance turn; never waits for a worker.
    pub fn poll_document_retirements(&mut self) {
        let mut restored = Vec::new();
        self.retirements.retain(|id, retirement| {
            let mut state = retirement.state.lock().unwrap();
            match &mut *state {
                State::Done => false,
                State::Failed(owned) => {
                    if let Some(owned) = owned.take() {
                        restored.push((*id, owned));
                    }
                    false
                }
                _ => true,
            }
        });
        for (id, (doc, session)) in restored {
            // These maps cannot acquire another authority while the fence is set.
            debug_assert!(self.document(id).is_none());
            self.others.insert(id, doc);
            self.doc_sessions.insert(id, session);
            self.retirement_retries.insert(id);
        }
        // Failed buffering stays visible in the ordinary session diagnostics.
        // Retry disposal only after its ordinary sync loop has secured the
        // changes, not by writing again on every window event.
        let secured: Vec<_> = self
            .retirement_retries
            .iter()
            .copied()
            .filter(|id| {
                self.others
                    .get(id)
                    .zip(self.doc_sessions.get(id))
                    .is_some_and(|(doc, session)| {
                        !session.owes(doc) || session.buffered_covers(doc)
                    })
            })
            .collect();
        for id in secured {
            self.retire_document(id);
        }
    }
}

#[cfg(test)]
mod tests;

pub(crate) fn unconfirmed(retirements: &HashMap<DocumentId, Retirement>) -> (usize, bool) {
    let mut count = 0;
    let mut unsafe_to_close = false;
    for retirement in retirements.values() {
        if retirement.owed && !matches!(*retirement.state.lock().unwrap(), State::Done) {
            count += 1;
            unsafe_to_close |= !retirement.buffered;
        }
    }
    (count, unsafe_to_close)
}
