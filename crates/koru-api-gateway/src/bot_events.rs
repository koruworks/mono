// SPDX-License-Identifier: MPL-2.0

//! Where the AI worker's Bot events go in this process: the events worker's
//! journal (M8, the six `bots.*` rows of `reference/EVENTS.md`).
//!
//! The AI worker publishes on its writer, after a commit, and on the tasks
//! that serve linked devices' streams; neither may wait for the journal,
//! which is a database behind a lock its readers also take. So a row is put
//! in a bounded queue and written by one thread of its own, in the order it
//! was published.
//!
//! **A row that cannot be queued or written is dropped, with a warning that
//! names the event and nothing else.** These rows are diagnostics: the team
//! pause, a command's receipt and a copy are the truth, and they are read
//! from the AI worker. Nothing is retried and nothing is kept for later.

use std::sync::Arc;
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};

use koru_ai_server::{BotEventDraft, BotEventSink};

/// How many rows wait for the journal at once. A row is published per
/// durable transition (a team pause, a closed receipt, a stream admitted or
/// ended, a copy changing state), so the queue is empty in ordinary use.
const QUEUE: usize = 256;

/// The sink the AI worker and the Bot ingress adapter publish to.
pub(crate) struct BotEventJournal {
    queue: SyncSender<BotEventDraft>,
}

impl BotEventJournal {
    /// Write every published row to `events`, on a thread that ends when the
    /// last holder of the sink is gone. `None` when that thread could not be
    /// started: nothing is published then, and nothing else is affected.
    pub(crate) fn start(events: koru_events_server::EventsHandle) -> Option<Arc<Self>> {
        Self::start_with(move |draft| {
            let envelope = draft.into_envelope(&events.state.ctx);
            if let Err(why) = events.state.append(std::slice::from_ref(&envelope)) {
                tracing::warn!(name = %envelope.name, "a Bot event was not recorded: {why}");
            }
        })
    }

    /// [`Self::start`] with what is done with a row given.
    pub(crate) fn start_with(record: impl Fn(BotEventDraft) + Send + 'static) -> Option<Arc<Self>> {
        let (queue, rows) = sync_channel::<BotEventDraft>(QUEUE);
        std::thread::Builder::new()
            .name("koru-bot-events".into())
            .spawn(move || {
                while let Ok(draft) = rows.recv() {
                    record(draft);
                }
            })
            .map_err(|error| {
                tracing::warn!(%error, "Bot events are not recorded: no thread for them");
            })
            .ok()?;
        Some(Arc::new(Self { queue }))
    }
}

impl BotEventSink for BotEventJournal {
    fn publish(&self, event: BotEventDraft) {
        match self.queue.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                tracing::warn!("a Bot event was dropped: the journal is not keeping up");
            }
            // The writing thread is gone; there is nowhere to record it.
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

#[cfg(test)]
mod tests;
