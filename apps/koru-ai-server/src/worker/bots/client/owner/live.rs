// SPDX-License-Identifier: MPL-2.0

//! The live role: the events of a running reply, forwarded while a linked
//! desktop watches the conversation, and never saved (contract 2.5, 4.7).
//!
//! **A preview, not the record.** Live events carry no cursor and are not
//! written anywhere: nothing here writes `ai.db`, and no feed marker moves.
//! What a conversation holds is what the feed delivers.
//!
//! **Only a Bot conversation a copy shows can be watched.** A watch names a
//! conversation; it is checked like a command's target, so an ordinary chat
//! of the person, a hidden conversation and a withheld Bot's are all
//! `not_found`.
//!
//! **What is forwarded** is the public event of AI stream version 1, with
//! two things left out, as in a copied message: a tool call's arguments and
//! results, and the session's command listing, which names what is installed
//! on this computer.
//!
//! **A watch is checked again while it stands**, as often as the feed's
//! markers are reconciled: a conversation whose Bot is no longer copied
//! (its workspace is now kept on this computer) ends `not_found`.
//!
//! **Bounded.** The writer hands an event to a watch without waiting. A
//! watch holds at most 64 events its stream has not written yet; one more
//! ends the watch with `live_end { dropped }`. A stream holds at most four
//! watches.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use koru_ai_core::{AiPersistence, ChatId, InferenceAttemptId};
use koru_ai_sqlite::Repository;
use koru_ai_wire::{AI_STREAM_VERSION, PublicAiEvent, StreamEvent};
use koru_bot_client_wire::{
    Live, LiveEnd, LiveEndReason, Refused, WireMessage, limits, refusal, scope,
};
use tokio::sync::mpsc;

use super::command::{Target, resolve};
use super::{Read, Served, blocking};
use crate::worker::bots::client::feed::{command_home_copied, public_tool_activity};

/// What the writer hands a live stream.
pub(super) enum LiveItem {
    Event {
        chat: ChatId,
        attempt: InferenceAttemptId,
        event: Box<StreamEvent>,
        /// The watch's count of events not written yet; the stream takes
        /// one off when it has written this one.
        queued: Arc<AtomicUsize>,
    },
    End {
        chat: ChatId,
        attempt: Option<InferenceAttemptId>,
        reason: LiveEndReason,
    },
}

/// The room a live stream's channel has: every watch's events, each watch's
/// end, and as much again for watches that ended and were opened anew
/// before the stream wrote what they left.
const STREAM_CHANNEL: usize =
    2 * (limits::MAX_WATCHES_PER_STREAM as usize) * (limits::LIVE_BUFFER_EVENTS as usize + 2);

struct Watcher {
    id: u64,
    sender: mpsc::Sender<LiveItem>,
    queued: Arc<AtomicUsize>,
    /// Set when an item could not be handed over at all: the stream closes.
    overflowed: Arc<AtomicBool>,
}

/// The watches of every live stream, on the writer.
#[derive(Default)]
pub(in crate::worker) struct Watches {
    by_chat: HashMap<ChatId, Vec<Watcher>>,
    /// The conversation of each attempt seen while somebody watched.
    chats: HashMap<InferenceAttemptId, Option<ChatId>>,
    /// The last sequence forwarded for an attempt that has no local stream.
    sequences: HashMap<InferenceAttemptId, u64>,
    next_id: u64,
}

impl Watches {
    /// Whether anybody watches anything: the writer's event path asks this
    /// first and does nothing more when nobody does.
    pub(in crate::worker) fn any(&self) -> bool {
        !self.by_chat.is_empty()
    }

    fn watch(
        &mut self,
        chat: ChatId,
        sender: mpsc::Sender<LiveItem>,
        overflowed: Arc<AtomicBool>,
    ) -> u64 {
        self.next_id += 1;
        self.by_chat.entry(chat).or_default().push(Watcher {
            id: self.next_id,
            sender,
            queued: Arc::new(AtomicUsize::new(0)),
            overflowed,
        });
        self.next_id
    }

    fn unwatch(&mut self, chat: ChatId, id: u64) {
        if let Some(watchers) = self.by_chat.get_mut(&chat) {
            watchers.retain(|watcher| watcher.id != id);
            if watchers.is_empty() {
                self.by_chat.remove(&chat);
            }
        }
        if self.by_chat.is_empty() {
            self.chats.clear();
            self.sequences.clear();
        }
    }

    /// Hand one public event of `attempt` to the watches of its
    /// conversation. `local` is the sequence the event has on the local
    /// response stream, when there is one; it is passed through. Reads one
    /// row of `ai.db` the first time an attempt is seen, and writes nothing.
    pub(in crate::worker) fn forward(
        &mut self,
        repository: &Repository,
        attempt: InferenceAttemptId,
        local: Option<u64>,
        event: &PublicAiEvent,
        terminal: bool,
    ) {
        if self.by_chat.is_empty() {
            return;
        }
        let chat = *self.chats.entry(attempt).or_insert_with(|| {
            repository
                .attempt(attempt)
                .ok()
                .flatten()
                .and_then(|attempt| attempt.chat_id)
        });
        let sequence = local.unwrap_or_else(|| {
            self.sequences
                .get(&attempt)
                .map_or(1, |last| last.saturating_add(1))
        });
        if terminal {
            self.chats.remove(&attempt);
            self.sequences.remove(&attempt);
        } else {
            self.sequences.insert(attempt, sequence);
        }
        let Some(chat) = chat else {
            return;
        };
        let Some(watchers) = self.by_chat.get_mut(&chat) else {
            return;
        };
        let Some(event) = forwarded(event) else {
            return;
        };
        watchers.retain(|watcher| {
            let give = |item| match watcher.sender.try_send(item) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    watcher.overflowed.store(true, Ordering::Release);
                    false
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            };
            if watcher.queued.load(Ordering::Acquire) >= limits::LIVE_BUFFER_EVENTS as usize {
                give(LiveItem::End {
                    chat,
                    attempt: Some(attempt),
                    reason: LiveEndReason::Dropped,
                });
                return false;
            }
            watcher.queued.fetch_add(1, Ordering::AcqRel);
            if !give(LiveItem::Event {
                chat,
                attempt,
                event: Box::new(StreamEvent {
                    version: AI_STREAM_VERSION,
                    sequence,
                    event: event.clone(),
                }),
                queued: Arc::clone(&watcher.queued),
            }) {
                return false;
            }
            if terminal {
                give(LiveItem::End {
                    chat,
                    attempt: Some(attempt),
                    reason: LiveEndReason::Terminal,
                });
                return false;
            }
            true
        });
        if watchers.is_empty() {
            self.by_chat.remove(&chat);
        }
    }
}

/// The event as a linked desktop is sent it, or `None` for one it is not
/// sent at all.
fn forwarded(event: &PublicAiEvent) -> Option<PublicAiEvent> {
    match event {
        // What a local agent can be told to do by name in a folder of this
        // computer. Nothing a linked desktop sends uses it.
        PublicAiEvent::AgentCommands { .. } => None,
        PublicAiEvent::ToolActivity {
            attempt_id,
            activity,
        } => {
            let mut activity = activity.clone();
            public_tool_activity(&mut activity);
            Some(PublicAiEvent::ToolActivity {
                attempt_id: *attempt_id,
                activity,
            })
        }
        other => Some(other.clone()),
    }
}

/// Whether `chat` may be watched now: a conversation a copy shows, of a
/// Bot of this person on this computer whose workspace is copied. The
/// identity on the writer, the Store on a blocking thread.
async fn watchable(served: &Served, chat: ChatId) -> Result<bool, super::Gone> {
    let shared = served.shared();
    let home = shared
        .link
        .on_writer(move |state| resolve(state, Target::Chat(chat)))
        .await?;
    let Ok(Some(home)) = home else {
        return Ok(false);
    };
    let authority = Arc::clone(&shared.authority);
    Ok(matches!(
        blocking(move || command_home_copied(authority.as_ref(), home)).await?,
        Ok(())
    ))
}

/// Serve one live stream until it ends.
pub(super) async fn serve(mut served: Served) {
    let (sender, mut items) = mpsc::channel(STREAM_CHANNEL);
    let overflowed = Arc::new(AtomicBool::new(false));
    let mut watching: HashMap<ChatId, u64> = HashMap::new();
    let link = served.shared().link.clone();
    let recheck = served.shared().timing.reconcile;
    let mut recheck_at = tokio::time::Instant::now() + recheck;
    let refusal = loop {
        tokio::select! {
            read = served.stream.read() => {
                let message = match read {
                    Read::Message(message) => message,
                    Read::Closed => break None,
                    Read::Malformed => break Some(Refused::new(refusal::PROTOCOL_ERROR)),
                };
                if let Err(refusal) = served.check(&message, scope::READ) {
                    break Some(refusal);
                }
                match message {
                    WireMessage::Watch(watch) => {
                        if let Err(refusal) = served.names_this_owner(&watch.owner_device) {
                            break Some(refusal);
                        }
                        let chat = watch.chat_id;
                        if watching.contains_key(&chat) {
                            continue;
                        }
                        if watching.len() >= limits::MAX_WATCHES_PER_STREAM as usize {
                            break Some(Refused::new(refusal::BUSY));
                        }
                        // The conversation is checked like a command's
                        // target: its Bot is this person's on this computer
                        // and is copied.
                        let Ok(found) = watchable(&served, chat).await else {
                            break None;
                        };
                        if !found {
                            let end = WireMessage::LiveEnd(LiveEnd {
                                chat_id: chat,
                                attempt_id: None,
                                reason: LiveEndReason::NotFound,
                            });
                            if served.stream.send(end).await.is_err() {
                                break None;
                            }
                            continue;
                        }
                        let (sender, overflowed) = (sender.clone(), Arc::clone(&overflowed));
                        let Ok(id) = link
                            .on_writer(move |state| {
                                state.bot_clients.live.watch(chat, sender, overflowed)
                            })
                            .await
                        else {
                            break None;
                        };
                        watching.insert(chat, id);
                    }
                    WireMessage::Unwatch(unwatch) => {
                        let chat = unwatch.chat_id;
                        let Some(id) = watching.remove(&chat) else {
                            continue;
                        };
                        if link
                            .on_writer(move |state| state.bot_clients.live.unwatch(chat, id))
                            .await
                            .is_err()
                        {
                            break None;
                        }
                        let end = WireMessage::LiveEnd(LiveEnd {
                            chat_id: chat,
                            attempt_id: None,
                            reason: LiveEndReason::Closed,
                        });
                        if served.stream.send(end).await.is_err() {
                            break None;
                        }
                    }
                    // `check` let only this role's two messages through.
                    _ => break Some(Refused::new(refusal::PROTOCOL_ERROR)),
                }
            }
            item = items.recv() => {
                let Some(item) = item else { break None };
                let message = match item {
                    LiveItem::Event { chat, attempt, event, queued } => {
                        queued.fetch_sub(1, Ordering::AcqRel);
                        // An event of a watch this stream already ended.
                        if !watching.contains_key(&chat) {
                            continue;
                        }
                        WireMessage::Live(Live {
                            chat_id: chat,
                            attempt_id: attempt,
                            event: *event,
                        })
                    }
                    LiveItem::End { chat, attempt, reason } => {
                        if watching.remove(&chat).is_none() {
                            continue;
                        }
                        WireMessage::LiveEnd(LiveEnd {
                            chat_id: chat,
                            attempt_id: attempt,
                            reason,
                        })
                    }
                };
                if served.stream.send(message).await.is_err() {
                    break None;
                }
            }
            // An item the writer could not hand over at all: the stream's
            // own channel was full, which the per-watch bound should make
            // impossible. Nothing can be said in order any more. Looked at
            // once a second, since nothing else wakes the stream for it.
            () = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                if overflowed.load(Ordering::Acquire) {
                    break Some(Refused::new(refusal::BUSY));
                }
                if link.stopping() {
                    break None;
                }
            }
            // Every watch that stands is checked again: a Bot that stopped
            // being copied is no longer forwarded.
            () = tokio::time::sleep_until(recheck_at), if !watching.is_empty() => {
                recheck_at = tokio::time::Instant::now() + recheck;
                let mut gone = Vec::new();
                for chat in watching.keys().copied().collect::<Vec<_>>() {
                    match watchable(&served, chat).await {
                        Ok(true) => {}
                        Ok(false) => gone.push(chat),
                        Err(_) => break,
                    }
                }
                let mut closed = false;
                for chat in gone {
                    let Some(id) = watching.remove(&chat) else {
                        continue;
                    };
                    let forgotten = link
                        .on_writer(move |state| state.bot_clients.live.unwatch(chat, id))
                        .await;
                    let end = WireMessage::LiveEnd(LiveEnd {
                        chat_id: chat,
                        attempt_id: None,
                        reason: LiveEndReason::NotFound,
                    });
                    if forgotten.is_err() || served.stream.send(end).await.is_err() {
                        closed = true;
                        break;
                    }
                }
                if closed {
                    break None;
                }
            }
        }
    };
    // Whatever ended the stream, the writer forgets its watches.
    if !watching.is_empty() {
        let watching: Vec<_> = watching.into_iter().collect();
        let _ = link
            .on_writer(move |state| {
                for (chat, id) in watching {
                    state.bot_clients.live.unwatch(chat, id);
                }
            })
            .await;
    }
    if let Some(refusal) = refusal {
        served.stream.refuse(refusal).await;
    }
}
