// SPDX-License-Identifier: MPL-2.0

//! The owner server's part on the AI writer.
//!
//! The streams of linked devices are tasks on a runtime; `ai.db` has one
//! writer thread and no read lane. What a stream needs of `ai.db` therefore
//! runs here, as short calls in the writer's own loop ([`WriterCall`]), and
//! this is all the state the writer keeps for linked desktops:
//!
//! - this start of the worker, recorded before anything of this run writes
//!   a marker ([`Writer::started`]);
//! - the feed, once its first reconcile has run;
//! - the feed pages waiting for the writer's budget ([`Writer::build_page`]):
//!   a page is built only when no command is queued, or when 250 ms have
//!   passed since the last one, and during a reset at most one per 50 ms;
//! - the feed's head, published for the streams that follow it
//!   ([`Writer::publish_head`]), only while one does;
//! - the watches of the live role ([`super::live::Watches`]).
//!
//! No call here waits on the Store, on a stream or on another thread: an
//! answer is handed to a channel that never blocks.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use koru_ai_sqlite::Repository;

use super::super::feed::{FeedBudget, FeedRefusal, HomeCopies, OwnerFeed, PageOutcome};
use super::Identity;
use super::live::Watches;
use crate::worker::{Envelope, LOOP_INTERVAL, WorkerState};

/// One piece of work on the writer, asked for by a linked device's stream.
pub(in crate::worker) type WriterCall = Box<dyn FnOnce(&mut WorkerState) + Send>;

/// What the writer publishes for the streams: the feed's head, and how many
/// feed streams follow it.
pub(in crate::worker) struct WriterHint {
    pub(in crate::worker) head: tokio::sync::watch::Sender<u64>,
    pub(in crate::worker) feed_streams: Arc<AtomicUsize>,
}

impl WriterHint {
    /// The writer's half, and the two values an owner server reads.
    pub(in crate::worker) fn pair() -> (Self, tokio::sync::watch::Receiver<u64>, Arc<AtomicUsize>) {
        let (head, follow) = tokio::sync::watch::channel(0);
        let feed_streams = Arc::new(AtomicUsize::new(0));
        (
            Self {
                head,
                feed_streams: Arc::clone(&feed_streams),
            },
            follow,
            feed_streams,
        )
    }
}

/// One page a feed stream waits for.
pub(super) struct PageRequest {
    pub(super) cursor: u64,
    pub(super) reset: bool,
    pub(super) copies: HomeCopies,
    pub(super) reply: tokio::sync::oneshot::Sender<Result<PageOutcome, FeedRefusal>>,
}

/// The writer's state for linked desktops.
pub(in crate::worker) struct Writer {
    /// This start of the worker, once it is recorded in `ai.db`. `None` when
    /// it could not be recorded: this run then serves no linked desktop.
    boot_id: Option<String>,
    /// The feed, once the first reconcile has run.
    feed: Option<OwnerFeed>,
    budget: FeedBudget,
    pages: VecDeque<PageRequest>,
    hint: Option<WriterHint>,
    pub(in crate::worker) live: Watches,
}

impl Writer {
    pub(in crate::worker) fn new(hint: Option<WriterHint>) -> Self {
        Self {
            boot_id: None,
            feed: None,
            budget: FeedBudget::default(),
            pages: VecDeque::new(),
            hint,
            live: Watches::default(),
        }
    }

    /// Record this start of the worker with the feed's head as it is now.
    ///
    /// **Called before anything of this run writes a marker**, the restart
    /// sweep included. After `ai.db` was restored from a backup, the first
    /// start that follows begins at the restored head, and that is what lets
    /// the owner refuse a cursor from the history it lost (contract 4.6). A
    /// start recorded later, after this run had written, would accept such a
    /// cursor.
    pub(in crate::worker) fn started(&mut self, repository: &mut Repository) {
        let boot_id = OwnerFeed::new_boot_id();
        match repository.feed_start(&boot_id) {
            Ok(_) => self.boot_id = Some(boot_id),
            Err(error) => tracing::warn!(
                code = error.code(),
                "this start of the AI worker could not be recorded; linked desktops are not served by this run"
            ),
        }
    }

    pub(super) fn boot_id(&self) -> Option<String> {
        self.boot_id.clone()
    }

    pub(super) fn feed(&self) -> Option<&OwnerFeed> {
        self.feed.as_ref()
    }

    pub(super) fn feed_mut(&mut self) -> Option<&mut OwnerFeed> {
        self.feed.as_mut()
    }

    /// The feed's identity, once it exists.
    pub(super) fn identity(&self) -> Option<Identity> {
        self.feed.as_ref().map(|feed| Identity {
            owner: feed.owner(),
            feed_id: feed.feed_id().to_owned(),
            boot_id: feed.boot_id().to_owned(),
        })
    }

    /// Keep the feed whose first reconcile has run. A feed that is already
    /// there stays: two streams cannot build two.
    pub(super) fn install(&mut self, feed: OwnerFeed) -> Identity {
        self.feed.get_or_insert(feed);
        self.identity().expect("the feed was just installed")
    }

    /// Queue one page for the writer's budget.
    pub(super) fn queue_page(&mut self, request: PageRequest) {
        self.pages.push_back(request);
    }

    fn has_pages(&self) -> bool {
        !self.pages.is_empty()
    }

    /// Build at most one waiting page, if the budget allows it with
    /// `queued` commands waiting for the writer. Answers how long until the
    /// next waiting page may be built, when one still waits.
    pub(super) fn build_page(
        &mut self,
        repository: &Repository,
        queued: usize,
        now: Instant,
    ) -> Option<Duration> {
        // A stream that went away while its page waited gets none built.
        self.pages.retain(|request| !request.reply.is_closed());
        let request = self.pages.front()?;
        if let Some(wait) = self.budget.wait(now, queued, request.reset) {
            return Some(wait);
        }
        let request = self.pages.pop_front()?;
        let outcome = match &self.feed {
            Some(feed) => feed.page(repository, request.cursor, request.reset, &request.copies),
            None => Err(FeedRefusal::Unavailable),
        };
        // Only a page that was built spends the budget: an answer that asks
        // for the Store first read a handful of markers.
        if matches!(outcome, Ok(PageOutcome::Page(_))) {
            self.budget.built(now);
        }
        let _ = request.reply.send(outcome);
        let next = self.pages.front()?;
        Some(
            self.budget
                .wait(now, queued, next.reset)
                .unwrap_or(Duration::ZERO),
        )
    }

    /// Publish the feed's head for the streams that follow it. One seek, and
    /// only while a feed stream is open.
    fn publish_head(&self, repository: &Repository) {
        let Some(hint) = &self.hint else {
            return;
        };
        if hint.feed_streams.load(Ordering::Acquire) == 0 {
            return;
        }
        let Ok(head) = repository.feed_head() else {
            return;
        };
        hint.head.send_if_modified(|published| {
            let moved = *published != head;
            *published = head;
            moved
        });
    }
}

impl WorkerState {
    /// The writer's next piece of work. With no feed page waiting this is
    /// the ordinary wait on the inbox. With one waiting, a page is built
    /// between two pieces of work when the budget allows: at once when
    /// nothing else is queued, and otherwise only when a quarter of a second
    /// has passed since the last page, so a reset of a long history yields
    /// to chat writes.
    pub(in crate::worker) fn next_envelope(
        &mut self,
        receiver: &Receiver<Envelope>,
    ) -> Result<Envelope, RecvTimeoutError> {
        self.bot_clients.publish_head(&self.repository);
        if !self.bot_clients.has_pages() {
            return receiver.recv_timeout(LOOP_INTERVAL);
        }
        match receiver.try_recv() {
            Ok(envelope) => {
                self.bot_clients
                    .build_page(&self.repository, 1, Instant::now());
                Ok(envelope)
            }
            Err(TryRecvError::Empty) => {
                let wait = self
                    .bot_clients
                    .build_page(&self.repository, 0, Instant::now())
                    .unwrap_or(LOOP_INTERVAL);
                receiver.recv_timeout(wait.min(LOOP_INTERVAL))
            }
            Err(TryRecvError::Disconnected) => Err(RecvTimeoutError::Disconnected),
        }
    }
}
