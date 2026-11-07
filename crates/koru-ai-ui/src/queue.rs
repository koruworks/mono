// SPDX-License-Identifier: MPL-2.0

//! **Messages queued while a reply runs**, and **Steer**.
//!
//! Sending while a reply runs no longer does nothing: the message — its text
//! with its chips and quotes, its attachments — is set aside whole in the
//! chat's queue and the composer is cleared for the next. The queue shows as
//! a *pending* list above the composer, one elided line each, with Steer,
//! Edit and Remove ×.
//!
//! - **A reply that completes** sends the head of the queue, as if the person
//!   had pressed Send then.
//! - **A reply the person stopped, that failed, or ended in an error**
//!   pauses the queue: *Queue paused · Send next*. A click on *Send next*
//!   sends the head; a message sent by hand resumes it too.
//! - **A queued send the service refused** — a `chat_busy` race, an
//!   attachment it would not take — goes back to the head of the queue,
//!   paused, never into the composer over what is being typed.
//! - **Steer** sends an item into the running reply. An agent that takes
//!   text mid-turn (`AgentProviderView::steers`: Codex, Claude) gets it
//!   natively, through the attempt's steer endpoint; every other agent, and a
//!   message carrying attachments, document chips or skill chips (D121),
//!   gets *stop, then send*:
//!   the reply is cancelled and the item is sent first once the cancel lands.
//!   That stop does not pause the queue — it was a steer, not a Stop.
//!
//! The queue is per chat and in this window's memory only: switching chats
//! shows that chat's queue, and a reload starts with none. Nothing about it
//! crosses the wire except the sends and steers it makes.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use koru_ai_core::{ChatId, InferenceAttemptId};
use koru_draw::geometry::Rect;
use koru_draw::{icons, scene, theme, theme::role};
use koru_i18n::{Key, fill, t};
use koru_ui::button::{ActionButton, ActionStyle};
use koru_ui::icon_button::{IconButton, State as IconButtonState};
use koru_ui::tooltip::TooltipContent;
use vello::Scene;
use vello::kurbo::Affine;

use crate::reader::Command;
use crate::{ChatSurface, attachments, composer, doc_links};

/// One row of the pending list.
pub(crate) const ROW_H: f32 = 28.0;
/// Between the list and what is under it (the document pill, the composer).
const GAP: f32 = 6.0;
/// The items shown; the rest are counted on a line of their own.
const SHOWN: usize = 3;
/// A row's icon buttons: their square and the gap between them.
const ICON: f32 = 22.0;
const ICON_GAP: f32 = 2.0;
/// The tooltip keys of the row icons and the send button.
const TOOLTIP_BASE: u64 = 0x5155_0000;

/// One message set aside whole, as the composer held it.
pub(crate) struct Queued {
    /// The composer's document, quote chips (D88) and all.
    pub(crate) composer: composer::Draft,
    pub(crate) attachments: Vec<attachments::Draft>,
    /// What it says as it would be sent: chips in their `[[koru://doc/…]]`
    /// spelling, each quote a blockquote before the text under it.
    pub(crate) markdown: String,
    /// It names a document or a skill by chip: a document only a send can
    /// pin, a skill only a turn of its own can carry (D121).
    pub(crate) documents: bool,
}

impl Queued {
    /// Whether a native steer can carry it: text only. Attachments and
    /// chips ride a send, never a steer.
    pub(crate) fn text_only(&self) -> bool {
        self.attachments.is_empty() && !self.documents
    }
}

/// One chat's queue: first in, first out.
#[derive(Default)]
pub(crate) struct ChatQueue {
    pub(crate) items: VecDeque<Queued>,
    /// Waiting for *Send next* or a send by hand, after a Stop or a failure.
    pub(crate) paused: bool,
}

/// Every chat's queue, by chat.
#[derive(Default)]
pub(crate) struct Queues {
    by_chat: HashMap<ChatId, ChatQueue>,
}

impl Queues {
    pub(crate) fn get(&self, chat: ChatId) -> Option<&ChatQueue> {
        self.by_chat
            .get(&chat)
            .filter(|queue| !queue.items.is_empty())
    }

    pub(crate) fn len(&self, chat: ChatId) -> usize {
        self.get(chat).map_or(0, |queue| queue.items.len())
    }

    pub(crate) fn push(&mut self, chat: ChatId, item: Queued) {
        self.by_chat.entry(chat).or_default().items.push_back(item);
    }

    pub(crate) fn push_front(&mut self, chat: ChatId, item: Queued) {
        self.by_chat.entry(chat).or_default().items.push_front(item);
    }

    pub(crate) fn pop_front(&mut self, chat: ChatId) -> Option<Queued> {
        let queue = self.by_chat.get_mut(&chat)?;
        let item = queue.items.pop_front();
        self.forget_if_empty(chat);
        item
    }

    pub(crate) fn remove(&mut self, chat: ChatId, index: usize) -> Option<Queued> {
        let queue = self.by_chat.get_mut(&chat)?;
        let item = queue.items.remove(index);
        self.forget_if_empty(chat);
        item
    }

    /// Pause a queue that has something in it.
    pub(crate) fn pause(&mut self, chat: ChatId) {
        if let Some(queue) = self.by_chat.get_mut(&chat)
            && !queue.items.is_empty()
        {
            queue.paused = true;
        }
    }

    pub(crate) fn resume(&mut self, chat: ChatId) {
        if let Some(queue) = self.by_chat.get_mut(&chat) {
            queue.paused = false;
        }
    }

    pub(crate) fn is_paused(&self, chat: ChatId) -> bool {
        self.get(chat).is_some_and(|queue| queue.paused)
    }

    fn forget_if_empty(&mut self, chat: ChatId) {
        if self
            .by_chat
            .get(&chat)
            .is_some_and(|queue| queue.items.is_empty())
        {
            self.by_chat.remove(&chat);
        }
    }
}

/// How the reply that just ended ended, as its terminal event said.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReplyEnd {
    Completed,
    /// Cancelled: by the person's Stop, or by a stop-then-send steer.
    Stopped,
    Failed,
}

/// What the send button is while the composer and the reply are as they are.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum SendMode {
    Send,
    /// A reply runs and the composer has something: a press queues it.
    Queue,
    /// A reply runs and the composer is empty: a press stops the reply.
    Stop,
}

/// A press on the pending list.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum QueueHit {
    SendNext,
    Steer(usize),
    Edit(usize),
    Remove(usize),
}

impl ChatSurface {
    // ----- state ------------------------------------------------------

    /// A reply runs, or a message is on its way to start one: a send now
    /// is queued rather than refused.
    /// Only this chat's: a message still on its way for a chat the surface
    /// has since left does not hold the one on screen.
    pub(crate) fn reply_busy(&self) -> bool {
        let shown = self.shown_chat_id();
        self.attempt.is_some()
            || self
                .sent_message
                .as_ref()
                .is_some_and(|sent| Some(sent.chat) == shown)
            || self
                .staged_embedded_send
                .as_ref()
                .is_some_and(|staged| Some(staged.chat_id) == shown)
    }

    /// The composer holds something to send: text, a chip, a quote or an
    /// attachment.
    pub(crate) fn composer_has_content(&self) -> bool {
        !self.composer.is_blank() || self.has_strip()
    }

    pub(crate) fn send_mode(&self) -> SendMode {
        if !self.reply_busy() {
            SendMode::Send
        } else if self.composer_has_content() || self.attempt.is_none() {
            SendMode::Queue
        } else {
            SendMode::Stop
        }
    }

    /// The shown chat's queue.
    pub(crate) fn shown_queue(&self) -> Option<&ChatQueue> {
        self.queues.get(self.shown_chat_id()?)
    }

    /// Whether the shown chat's agent takes text into a running turn.
    pub(crate) fn chat_steers(&self) -> bool {
        let Some(provider) = self
            .transcript
            .as_ref()
            .map(|transcript| &transcript.chat.current_model.provider)
        else {
            return false;
        };
        self.settings.as_ref().is_some_and(|settings| {
            settings
                .agents
                .iter()
                .any(|agent| &agent.id == provider && agent.steers)
        })
    }

    // ----- queueing ---------------------------------------------------

    /// **Set the composer's message aside in the chat's queue**, whole, and
    /// clear the composer for the next. `false` when there is nothing to
    /// queue; an attachment still being stored keeps the message where it
    /// is, and the status line says why.
    pub(crate) fn queue_composer(&mut self) -> bool {
        if self.composer.composing() {
            return true;
        }
        let Some(chat) = self.shown_chat_id() else {
            return false;
        };
        if !self.composer_has_content() {
            return false;
        }
        let message = self.composer.message();
        let Some(attachments) = self.take_attachments_for_queue(chat) else {
            return true;
        };
        for (document, title) in &message.documents {
            if !title.trim().is_empty() {
                self.document_titles.insert(*document, title.clone());
            }
        }
        let item = Queued {
            composer: self.composer.stash(),
            attachments,
            markdown: message.markdown.trim().to_owned(),
            documents: !message.documents.is_empty() || !message.skills.is_empty(),
        };
        self.forget_input_walk();
        self.queues.push(chat, item);
        true
    }

    /// **Send the head of the shown chat's queue**, resuming it: what a
    /// completed reply, *Send next* and a stop-then-send steer do.
    pub(crate) fn send_queue_head(&mut self) -> bool {
        let Some(chat) = self.shown_chat_id() else {
            return false;
        };
        if self.reply_busy() {
            return false;
        }
        let Some(item) = self.queues.pop_front(chat) else {
            return false;
        };
        self.queues.resume(chat);
        self.send_queued(chat, item)
    }

    /// Send `item` exactly as the composer would have, without disturbing
    /// what the person is typing: the message being typed is set aside
    /// whole, the item takes its place for the send, and it comes back
    /// after. An item the send would not take — an attachment still being
    /// stored — goes back to the head of the queue, paused.
    fn send_queued(&mut self, chat: ChatId, item: Queued) -> bool {
        let typing = self.composer.stash();
        let walk = (self.history_cursor.take(), self.history_draft.take());
        self.composer.unstash(item.composer);
        let strip = self.strips.take(chat);
        for draft in item.attachments {
            self.strips.push(chat, draft);
        }
        self.sending_queued = true;
        self.send_composer();
        self.sending_queued = false;
        if self.composer_has_content() {
            let item = Queued {
                composer: self.composer.stash(),
                attachments: self.strips.take(chat),
                markdown: item.markdown,
                documents: item.documents,
            };
            self.queues.push_front(chat, item);
            self.queues.pause(chat);
        }
        self.composer.unstash(typing);
        for draft in strip {
            self.strips.push(chat, draft);
        }
        (self.history_cursor, self.history_draft) = walk;
        true
    }

    /// A queued send the service refused goes back to the head of its
    /// queue, paused — never into the composer over what is being typed.
    pub(crate) fn requeue_refused(
        &mut self,
        chat: ChatId,
        item: Queued,
        failure: Option<&koru_ai_wire::Failure>,
    ) {
        let mut item = item;
        Self::mark_refused(&mut item.attachments, failure);
        self.queues.push_front(chat, item);
        self.queues.pause(chat);
    }

    /// **The reply's terminal has been read back**: what the queue does next.
    /// A completed reply sends the head; a stop-then-send steer's cancel
    /// sends the head it put there; a Stop or a failure pauses the queue.
    pub(crate) fn after_reply(&mut self, chat: ChatId) {
        let steering = self.steer_after_cancel == Some(chat);
        let Some((ended, end)) = self.reply_end.take() else {
            return;
        };
        if ended != chat {
            return;
        }
        self.steer_after_cancel = None;
        match end {
            ReplyEnd::Completed => {
                if !self.queues.is_paused(chat) {
                    self.send_queue_head();
                }
            }
            ReplyEnd::Stopped if steering => {
                self.send_queue_head();
            }
            ReplyEnd::Stopped | ReplyEnd::Failed => self.queues.pause(chat),
        }
    }

    /// Another chat came on screen: a queue it had waits for the person,
    /// whatever happened to its reply while it was not shown.
    pub(crate) fn queue_shown_again(&mut self, chat: ChatId) {
        self.queues.pause(chat);
    }

    // ----- the list's actions -----------------------------------------

    pub(crate) fn queue_action(&mut self, hit: QueueHit) -> bool {
        let Some(chat) = self.shown_chat_id() else {
            return false;
        };
        match hit {
            QueueHit::SendNext => {
                self.queues.resume(chat);
                self.send_queue_head()
            }
            QueueHit::Remove(index) => {
                if let Some(item) = self.queues.remove(chat, index) {
                    self.discard_drafts(chat, item.attachments);
                }
                self.queue_tooltip.clear();
                true
            }
            QueueHit::Edit(index) => self.edit_queued(chat, index),
            QueueHit::Steer(index) => self.steer_queued(chat, index),
        }
    }

    /// **Edit pulls the item back into the composer** and out of the queue.
    /// Anything already in the composer is not lost: it takes the item's
    /// place in the queue.
    fn edit_queued(&mut self, chat: ChatId, index: usize) -> bool {
        let Some(item) = self.queues.remove(chat, index) else {
            return false;
        };
        if self.composer_has_content() {
            let message = self.composer.message();
            let displaced = Queued {
                markdown: message.markdown.trim().to_owned(),
                documents: !message.documents.is_empty() || !message.skills.is_empty(),
                composer: self.composer.stash(),
                attachments: self.strips.take(chat),
            };
            let at = index.min(self.queues.len(chat));
            if let Some(queue) = self.queues.by_chat.get_mut(&chat) {
                queue.items.insert(at, displaced);
            } else {
                self.queues.push(chat, displaced);
            }
        }
        self.composer.unstash(item.composer);
        for draft in item.attachments {
            self.strips.push(chat, draft);
        }
        self.forget_input_walk();
        self.focus = crate::Focus::Composer;
        self.composer.set_focused(true);
        self.queue_tooltip.clear();
        true
    }

    /// **Steer**: the item goes into the running reply. Natively, as text,
    /// to an agent that takes it; otherwise the reply is stopped and the item
    /// sent first. With no reply running it is simply sent now.
    fn steer_queued(&mut self, chat: ChatId, index: usize) -> bool {
        let Some(item) = self.queues.remove(chat, index) else {
            return false;
        };
        self.queue_tooltip.clear();
        let Some(attempt) = self.attempt.filter(|_| self.ended_attempt != self.attempt) else {
            // Nothing running to steer (or it is ending): first in line.
            self.queues.push_front(chat, item);
            if !self.reply_busy() {
                return self.send_queue_head();
            }
            return true;
        };
        if self.chat_steers() && item.text_only() {
            return self.steer_natively(chat, attempt, item);
        }
        self.stop_then_send(chat, attempt, item)
    }

    /// Stop the reply, and send `item` first once the cancel lands — without
    /// pausing the queue.
    fn stop_then_send(&mut self, chat: ChatId, attempt: InferenceAttemptId, item: Queued) -> bool {
        self.queues.push_front(chat, item);
        self.queues.resume(chat);
        self.steer_after_cancel = Some(chat);
        self.send_command(Command::Cancel(attempt))
    }

    /// Hand the item's text to the running turn. The item waits beside the
    /// request until the service answers: accepted, it shows as a sent
    /// message under the question, above the reply (D99); refused, it goes
    /// back to the head of the queue, unpaused, and is sent when the reply
    /// ends.
    fn steer_natively(&mut self, chat: ChatId, attempt: InferenceAttemptId, item: Queued) -> bool {
        let request = koru_ai_wire::SteerAttemptRequest {
            operation_id: uuid::Uuid::new_v4(),
            markdown: item.markdown.clone(),
        };
        if !self.queue_command(Command::Steer(attempt, request)) {
            self.queues.push_front(chat, item);
            return true;
        }
        self.steering = Some((chat, item));
        true
    }

    /// The service took the steer: the message shows under the question,
    /// above the running reply, until the transcript read at its end
    /// carries it there (D99).
    pub(crate) fn steer_accepted(&mut self, message_id: koru_ai_core::MessageId) {
        if let Some((chat, item)) = self.steering.take() {
            // Sent now, so Up walks back to it (D100), as to any message
            // sent; while it waited in the queue it was not.
            self.sent_inputs.push(super::LocalInput {
                chat,
                message_id: Some(message_id),
                markdown: item.markdown.clone(),
                attachments: Vec::new(),
            });
            self.steered.push((chat, item.markdown));
            self.messages_dirty = true;
        }
    }

    /// The service would not take the steer — the turn ended first, or its
    /// agent took no text just then: the item is first in line, sent when
    /// the reply ends.
    pub(crate) fn steer_refused(&mut self, failure: &koru_ai_wire::Failure) {
        if let Some((chat, item)) = self.steering.take() {
            self.queues.push_front(chat, item);
        }
        tracing::info!(
            target: "koru_ai::ui",
            code = failure.code,
            "AI UI steer was not taken; the message is first in the queue"
        );
    }

    /// The steered messages to draw under the question, in order.
    pub(crate) fn steered_shown(&self) -> Vec<String> {
        let Some(chat) = self.shown_chat_id() else {
            return Vec::new();
        };
        self.steered
            .iter()
            .filter(|(of, _)| *of == chat)
            .map(|(_, markdown)| markdown.clone())
            .collect()
    }

    // ----- geometry ---------------------------------------------------

    /// The list's rows: the paused line, the items shown and the count of
    /// the rest. Zero when the shown chat has no queue.
    fn queue_rows(&self) -> usize {
        let Some(queue) = self.shown_queue() else {
            return 0;
        };
        let items = queue.items.len();
        usize::from(queue.paused) + items.min(SHOWN) + usize::from(items > SHOWN)
    }

    /// The height the pending list takes above the composer, its gap
    /// included.
    pub(crate) fn queue_band_h(&self) -> f32 {
        match self.queue_rows() {
            0 => 0.0,
            rows => rows as f32 * ROW_H + GAP,
        }
    }

    /// The pending list, as wide as the composer, above its box.
    pub(crate) fn queue_band_rect(&mut self) -> Option<Rect> {
        let rows = self.queue_rows();
        if rows == 0 {
            return None;
        }
        let composer = self.composer_rect();
        let h = rows as f32 * ROW_H;
        Some(Rect::new(composer.x, composer.y - GAP - h, composer.w, h))
    }

    /// Each shown item's row, with its index in the queue.
    fn item_rows(&mut self) -> Vec<(usize, Rect)> {
        let Some(band) = self.queue_band_rect() else {
            return Vec::new();
        };
        let Some(queue) = self.shown_queue() else {
            return Vec::new();
        };
        let first = usize::from(queue.paused);
        (0..queue.items.len().min(SHOWN))
            .map(|index| {
                let y = band.y + (first + index) as f32 * ROW_H;
                (index, Rect::new(band.x, y, band.w, ROW_H))
            })
            .collect()
    }

    /// A row's three icons, right to left: Remove, Edit, Steer.
    fn row_icons(row: Rect) -> [Rect; 3] {
        let y = row.y + (row.h - ICON) * 0.5;
        let remove = Rect::new(row.x + row.w - ICON - 4.0, y, ICON, ICON);
        let edit = Rect::new(remove.x - ICON_GAP - ICON, y, ICON, ICON);
        let steer = Rect::new(edit.x - ICON_GAP - ICON, y, ICON, ICON);
        [steer, edit, remove]
    }

    /// *Send next*, at the paused line's end.
    fn send_next_rect(&mut self) -> Option<Rect> {
        let band = self.queue_band_rect()?;
        if !self.shown_queue().is_some_and(|queue| queue.paused) {
            return None;
        }
        let label = t(self.lang, Key::AiQueueSendNext);
        let w = self.engine.simple_layout(label, role::meta()).width() + 20.0;
        Some(Rect::new(
            band.x + band.w - w - 4.0,
            band.y + 2.0,
            w,
            ROW_H - 4.0,
        ))
    }

    pub(crate) fn queue_hit_at(&mut self, x: f32, y: f32) -> Option<QueueHit> {
        let band = self.queue_band_rect()?;
        if !band.contains(x, y) {
            return None;
        }
        if self
            .send_next_rect()
            .is_some_and(|rect| rect.contains(x, y))
        {
            return Some(QueueHit::SendNext);
        }
        for (index, row) in self.item_rows() {
            let [steer, edit, remove] = Self::row_icons(row);
            if steer.contains(x, y) {
                return Some(QueueHit::Steer(index));
            }
            if edit.contains(x, y) {
                return Some(QueueHit::Edit(index));
            }
            if remove.contains(x, y) {
                return Some(QueueHit::Remove(index));
            }
        }
        None
    }

    /// What a hit is called, for its tooltip and a screen reader.
    fn hit_name(&self, hit: QueueHit) -> &'static str {
        let steer_key = if !self.reply_busy() {
            Key::AiQueueSendNow
        } else if self.chat_steers()
            && self.shown_queue().is_some_and(|queue| match hit {
                QueueHit::Steer(index) => queue.items.get(index).is_some_and(Queued::text_only),
                _ => true,
            })
        {
            Key::AiQueueSteer
        } else {
            Key::AiQueueSteerStops
        };
        t(
            self.lang,
            match hit {
                QueueHit::SendNext => Key::AiQueueSendNext,
                QueueHit::Steer(_) => steer_key,
                QueueHit::Edit(_) => Key::AiQueueEdit,
                QueueHit::Remove(_) => Key::AiQueueRemove,
            },
        )
    }

    /// The send button's name as it stands: *Send*, *Queue* or *Stop*.
    pub(crate) fn send_button_name(&self) -> &'static str {
        t(
            self.lang,
            match self.send_mode() {
                SendMode::Send => Key::AiComposerSend,
                SendMode::Queue => Key::AiComposerQueue,
                SendMode::Stop => Key::AiComposerStop,
            },
        )
    }

    /// Hover over the list's icons and the send button raises their names.
    pub(crate) fn queue_hover(&mut self, x: f32, y: f32) -> bool {
        let content = match self.queue_hit_at(x, y) {
            Some(hit @ (QueueHit::Steer(_) | QueueHit::Edit(_) | QueueHit::Remove(_))) => {
                let rows = self.item_rows();
                let (index, slot) = match hit {
                    QueueHit::Steer(index) => (index, 0),
                    QueueHit::Edit(index) => (index, 1),
                    QueueHit::Remove(index) => (index, 2),
                    QueueHit::SendNext => unreachable!("matched above"),
                };
                rows.iter()
                    .find(|(row_index, _)| *row_index == index)
                    .map(|(_, row)| {
                        TooltipContent::new(
                            TOOLTIP_BASE + (index as u64) * 4 + slot,
                            Self::row_icons(*row)[slot as usize],
                            self.hit_name(hit),
                        )
                    })
            }
            _ if self.send_button().contains(x, y) && !self.showing_setup() => {
                Some(TooltipContent::new(
                    TOOLTIP_BASE - 1,
                    self.send_button(),
                    self.send_button_name(),
                ))
            }
            _ => None,
        };
        self.queue_tooltip.set_hover(content, Instant::now())
    }

    // ----- drawing ----------------------------------------------------

    pub(crate) fn draw_queue(&mut self, canvas: &mut Scene, root: Affine) {
        let Some(band) = self.queue_band_rect() else {
            return;
        };
        let p = theme::p();
        let _list = koru_draw::inspect::scope(
            "ComposerQueue",
            "component",
            root,
            scene::rect(band.x, band.y, band.w, band.h),
            |r| r.property("rows", self.queue_rows() as f32, "paused line, items, more"),
        );
        let Some(chat) = self.shown_chat_id() else {
            return;
        };
        let paused = self.queues.is_paused(chat);
        if paused {
            let line = format!("{} \u{00B7}", t(self.lang, Key::AiQueuePaused));
            let layout = self.engine.simple_layout(&line, role::meta());
            scene::draw_layout(
                canvas,
                root,
                &layout,
                band.x + 6.0,
                band.y + (ROW_H - layout.height()) * 0.5,
                Some(p.text_dim),
            );
            if let Some(rect) = self.send_next_rect() {
                let label = t(self.lang, Key::AiQueueSendNext);
                let _button = koru_draw::inspect::scope(
                    label,
                    "button",
                    root,
                    scene::rect(rect.x, rect.y, rect.w, rect.h),
                    |r| r.property("accessible name", label, "caller label"),
                );
                ActionButton::new(label, ActionStyle::Ghost, None).draw(
                    canvas,
                    root,
                    &mut self.engine,
                    rect,
                    rect.contains(self.pointer.0, self.pointer.1),
                );
            }
        }
        let untitled = t(self.lang, Key::AiDocumentLinkUntitled);
        let lines: Vec<String> = self
            .shown_queue()
            .map(|queue| {
                queue
                    .items
                    .iter()
                    .take(SHOWN)
                    .map(|item| {
                        let (read, _) = doc_links::titled(
                            &crate::quote_pills::quote_bars(&item.markdown),
                            &self.document_titles,
                            untitled,
                        );
                        one_line(&read)
                    })
                    .collect()
            })
            .unwrap_or_default();
        for ((index, row), line) in self.item_rows().into_iter().zip(lines) {
            let hovered = row.contains(self.pointer.0, self.pointer.1);
            scene::fill(
                canvas,
                root,
                if hovered { p.hover_tint } else { p.panel_bg },
                &scene::rrect(row.x, row.y + 1.0, row.w, row.h - 2.0, 6.0),
            );
            scene::stroke(
                canvas,
                root,
                p.panel_border,
                theme::BORDER_W,
                &scene::rrect(row.x, row.y + 1.0, row.w, row.h - 2.0, 6.0),
            );
            let icons_at = Self::row_icons(row);
            let room = (icons_at[0].x - row.x - 16.0).max(1.0);
            let shown = self.engine.elided(&line, role::meta(), room);
            let layout = self.engine.simple_layout(&shown, role::meta());
            scene::draw_layout(
                canvas,
                root,
                &layout,
                row.x + 10.0,
                row.y + (row.h - layout.height()) * 0.5,
                Some(p.text),
            );
            for (slot, (rect, glyph)) in icons_at
                .into_iter()
                .zip([
                    icons::CARET_DOUBLE_RIGHT,
                    icons::PENCIL,
                    icons::WINDOW_CLOSE,
                ])
                .enumerate()
            {
                let hit = match slot {
                    0 => QueueHit::Steer(index),
                    1 => QueueHit::Edit(index),
                    _ => QueueHit::Remove(index),
                };
                let name = self.hit_name(hit);
                let _icon = koru_draw::inspect::scope(
                    name,
                    "button",
                    root,
                    scene::rect(rect.x, rect.y, rect.w, rect.h),
                    |r| r.property("accessible name", name, "caller label"),
                );
                IconButton::new(glyph, rect)
                    .with_state(IconButtonState {
                        hovered: rect.contains(self.pointer.0, self.pointer.1),
                        active: false,
                        disabled: false,
                    })
                    .draw(canvas, root, &mut self.engine);
            }
        }
        let more = self.queues.len(chat).saturating_sub(SHOWN);
        if more > 0 {
            let line = fill(t(self.lang, Key::AiQueueMore), &[&more.to_string()]);
            let layout = self.engine.simple_layout(&line, role::meta());
            scene::draw_layout(
                canvas,
                root,
                &layout,
                band.x + 10.0,
                band.y + band.h - ROW_H + (ROW_H - layout.height()) * 0.5,
                Some(p.text_dim),
            );
        }
    }
}

/// A message's first line, for its row; the row elides it to fit.
fn one_line(markdown: &str) -> String {
    markdown
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests;
