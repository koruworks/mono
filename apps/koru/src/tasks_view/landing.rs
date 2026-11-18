// SPDX-License-Identifier: MPL-2.0

//! Opening a task from search: read that one task as it is now, and decide
//! which Tasks page it is on.
//!
//! **Where a task goes is decided from the task, not from the search hit.** A
//! hit is as old as the index; the task may have been completed, pinned,
//! moved to another list or unmarked since. So the landing asks the store for
//! exactly that task, with this device's bucket windows, and applies one
//! function — [`task_destination`] — to the row that comes back.
//!
//! One bounded query and no page walk. It runs off the UI thread like every
//! page query, under the page's own ceiling
//! ([`wait_ceiling`](super::query::wait_ceiling)); when the ceiling passes, or
//! the task is not there, or its bucket is missing, **the landing is retired
//! and the task's own document opens at its line**. Each landing has a
//! generation: a retired one's late answer is dropped, so it cannot replace
//! what the person is looking at by then.

use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

use koru_core::ids::ListId;
use koru_core::tasks::Bucket;
use koru_core::{DocumentId, NodeId, WorkspaceId};
use koru_store_wire::tasks::{TaskQuery, TaskRef, TaskRow};

use super::query::{wait_ceiling, windows_now};
use crate::panes::explore::rail::RailTarget;

/// Which page of Tasks a task is on (nested-lists S9), in this order:
///
/// 1. it is completed → **Completed**;
/// 2. its natural bucket is Today, or it is pinned to Today → **Today**;
/// 3. it is in a list that is live in this workspace → **that list**;
/// 4. otherwise → the queue its natural bucket names.
///
/// `None` when the row carries no bucket and none of the first three rules
/// placed it: the store did not say, and no bucket rule is written here.
pub fn task_destination(row: &TaskRow, is_live_list: impl Fn(ListId) -> bool) -> Option<Placed> {
    use koru_core::attributes::{KEY_LIST, KEY_TODAY, Lifecycle};

    let value = |role: &str| {
        row.attributes
            .iter()
            .find(|attr| attr.role == role)
            .map(|attr| attr.value.as_str())
    };
    if row.lifecycle == Lifecycle::Completed.as_str() {
        return Some(Placed::new(Bucket::Completed, Rule::Completed));
    }
    let bucket = row.bucket.as_deref().and_then(bucket_of);
    if bucket == Some(Bucket::Today) || value(KEY_TODAY) == Some("true") {
        return Some(Placed::new(Bucket::Today, Rule::Today));
    }
    if let Some(list) = value(KEY_LIST).and_then(ListId::parse)
        && is_live_list(list)
    {
        return Some(Placed {
            target: RailTarget::List(list),
            rule: Rule::List,
        });
    }
    bucket.map(|bucket| Placed::new(bucket, Rule::Queue))
}

/// Where a task was placed, and by which rule — the rule is what the debug
/// log names.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Placed {
    pub target: RailTarget,
    pub rule: Rule,
}

impl Placed {
    fn new(bucket: Bucket, rule: Rule) -> Self {
        Self {
            target: RailTarget::Bucket(bucket),
            rule,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Rule {
    Completed,
    Today,
    List,
    Queue,
}

impl Rule {
    /// A closed token for `KORU_TASKS_DEBUG`.
    pub fn token(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Today => "today",
            Self::List => "list",
            Self::Queue => "queue",
        }
    }
}

fn bucket_of(token: &str) -> Option<Bucket> {
    Bucket::ALL
        .into_iter()
        .find(|bucket| bucket.as_str() == token)
}

/// A task somebody chose in search, on its way to a page.
struct Pending {
    generation: u64,
    document: DocumentId,
    node: NodeId,
    match_text: Option<String>,
    started: Instant,
    receiver: Receiver<(u64, Option<TaskRow>)>,
}

/// What a landing came to.
#[derive(Debug)]
pub enum Landed {
    /// The task, as it is now. The container places it with
    /// [`task_destination`].
    Task {
        row: Box<TaskRow>,
        document: DocumentId,
        node: NodeId,
    },
    /// No answer in time, no such task, or a refused request: open the task's
    /// own document at its line instead.
    Document {
        document: DocumentId,
        node: NodeId,
        match_text: Option<String>,
    },
}

/// The one landing in flight, if any.
#[derive(Default)]
pub struct SearchLanding {
    generation: u64,
    pending: Option<Pending>,
}

impl SearchLanding {
    /// Begin landing on a task. **A newer landing retires the older one**:
    /// opening a second result before the first has landed discards the
    /// first.
    ///
    /// With no store client there is nothing to ask, and the answer is the
    /// document straight away.
    pub fn begin(
        &mut self,
        client: Option<koru_api_client::Client>,
        workspace: WorkspaceId,
        document: DocumentId,
        node: NodeId,
        match_text: Option<String>,
        now: Instant,
    ) {
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let (send, receiver) = mpsc::channel();
        if let Some(client) = client {
            let spawned = std::thread::Builder::new()
                .name("koru-task-landing".into())
                .spawn(move || {
                    let _ = send.send((generation, read(&client, workspace, document, node)));
                });
            if spawned.is_err() {
                // The sender went with the closure: `poll` reads a closed
                // channel and falls back at once.
            }
        }
        self.pending = Some(Pending {
            generation,
            document,
            node,
            match_text,
            started: now,
            receiver,
        });
    }

    /// Give up on whatever is in flight. Any navigation does this: the
    /// person has gone somewhere, and a task landing a moment later must not
    /// take them away from it.
    pub fn retire(&mut self) -> bool {
        self.generation = self.generation.wrapping_add(1);
        self.pending.take().is_some()
    }

    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    /// The landing's answer, once: the row, or the document to open instead.
    pub fn poll(&mut self, now: Instant) -> Option<Landed> {
        let pending = self.pending.as_ref()?;
        let fallback = |pending: Pending| Landed::Document {
            document: pending.document,
            node: pending.node,
            match_text: pending.match_text,
        };
        match pending.receiver.try_recv() {
            // A retired generation's answer, arriving late: dropped.
            Ok((generation, _)) if generation != pending.generation => None,
            Ok((_, Some(row))) => {
                let pending = self.pending.take()?;
                Some(Landed::Task {
                    row: Box::new(row),
                    document: pending.document,
                    node: pending.node,
                })
            }
            Ok((_, None)) | Err(mpsc::TryRecvError::Disconnected) => {
                self.pending.take().map(fallback)
            }
            Err(mpsc::TryRecvError::Empty) => {
                if now.duration_since(pending.started) < wait_ceiling() {
                    return None;
                }
                // Past the ceiling. Retired by generation, so the answer that
                // is still on its way is dropped when it arrives.
                self.generation = self.generation.wrapping_add(1);
                self.pending.take().map(fallback)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn begin_with_for_test(
        &mut self,
        document: DocumentId,
        node: NodeId,
        now: Instant,
    ) -> (u64, mpsc::Sender<(u64, Option<TaskRow>)>) {
        self.generation = self.generation.wrapping_add(1);
        let (send, receiver) = mpsc::channel();
        self.pending = Some(Pending {
            generation: self.generation,
            document,
            node,
            match_text: None,
            started: now,
            receiver,
        });
        (self.generation, send)
    }
}

/// The worker: one query for one task. `None` for a task that is not there —
/// deleted or unmarked since it was indexed — and for a request that failed.
fn read(
    client: &koru_api_client::Client,
    workspace: WorkspaceId,
    document: DocumentId,
    node: NodeId,
) -> Option<TaskRow> {
    let (_, _, windows) = windows_now();
    let answered = client.store().query_tasks(&only_query(
        koru_store_core::ids::WorkspaceId::from_uuid(workspace.as_uuid()),
        koru_store_core::ids::DocumentId::from_uuid(document.as_uuid()),
        node,
        windows,
    ));
    let node = node.to_storage();
    answered
        .ok()?
        .rows
        .into_iter()
        .find(|row| row.document.as_uuid() == document.as_uuid() && row.node == node)
}

/// The landing's question: exactly this task, with this device's windows, so
/// the row says which bucket it is naturally in.
pub fn only_query(
    workspace: koru_store_core::ids::WorkspaceId,
    document: koru_store_core::ids::DocumentId,
    node: NodeId,
    windows: koru_store_wire::tasks::BucketWindows,
) -> TaskQuery {
    TaskQuery {
        workspaces: vec![workspace],
        only: Some(vec![TaskRef {
            document,
            node: node.to_storage(),
        }]),
        bucket_windows: Some(windows),
        ..TaskQuery::default()
    }
}

#[cfg(test)]
mod tests;
