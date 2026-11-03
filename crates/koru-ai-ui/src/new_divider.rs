// SPDX-License-Identifier: MPL-2.0

//! **Open at the first message the person has not seen** (koru-ai D84).
//!
//! When a chat comes into view holding something unseen, its host says
//! which message the person saw last ([`ChatSurface::show_new_from`]). The
//! surface then opens there rather than at the end, under a divider — a
//! small **NEW** in the accent colour and a hairline across the column —
//! above the first message after it. The divider stays while the chat stays
//! open; the host clears it the next time the chat comes into view with
//! nothing new.
//!
//! The divider is a band of the conversation's own layout, placed by
//! `position_conversation_items` in front of that message's work, so it
//! scrolls, clips and hit-tests with the messages and never covers one.
//! *Jump to the newest message* (D77) shows as it always does, because the
//! chat did not open at its end.

use koru_ai_core::MessageId;
use koru_draw::geometry::Rect;
use koru_draw::scene;
use koru_draw::theme::{self, FontWeight, role};
use vello::Scene;
use vello::kurbo::Affine;

use crate::{ChatId, ChatSurface};

/// The band the divider takes in the conversation.
pub(crate) const NEW_DIVIDER_H: f32 = 22.0;
/// Air between the **NEW** and its line.
const LABEL_GAP: f32 = 8.0;
/// How far below the column's top the divider lands when the chat opens on
/// it, so the line is not flush with the edge.
const OPEN_MARGIN: f32 = 12.0;

/// A NEW divider over one chat.
pub(crate) struct NewDivider {
    pub chat: ChatId,
    /// The last message the person saw; the divider goes above the next.
    pub after: MessageId,
    /// Where the band starts in the conversation, once a layout has found
    /// the message it goes above.
    pub top: Option<f32>,
    /// The chat opens on the divider, once, rather than at its end.
    pub scroll_owed: bool,
}

impl ChatSurface {
    /// **Show `chat` from what the person has not seen** (D84): a NEW
    /// divider above the first message after `after`, and the chat opened
    /// there. `None` clears the divider — the chat came into view with
    /// nothing new. `true` when something changed.
    pub fn show_new_from(&mut self, chat: ChatId, after: Option<MessageId>) -> bool {
        match after {
            Some(after) => {
                self.new_divider = Some(NewDivider {
                    chat,
                    after,
                    top: None,
                    scroll_owed: true,
                });
                self.messages_dirty = true;
                true
            }
            None => {
                if self.new_divider.is_none() {
                    return false;
                }
                self.new_divider = None;
                self.messages_dirty = true;
                true
            }
        }
    }

    /// The message layout the divider goes above: the first message after
    /// the one seen last, in the chat on screen. `None` when there is no
    /// divider, it is another chat's, or nothing came after.
    pub(crate) fn new_divider_index(&self) -> Option<usize> {
        let divider = self.new_divider.as_ref()?;
        if self.transcript.as_ref()?.chat.id != divider.chat {
            return None;
        }
        let seen = self
            .message_layouts
            .iter()
            .position(|message| message.id == Some(divider.after))?;
        self.message_layouts
            .iter()
            .enumerate()
            .skip(seen + 1)
            .find(|(_, message)| message.id.is_some())
            .map(|(index, _)| index)
    }

    /// Where the chat should open, once, when it opens on its divider: the
    /// scroll that puts the divider near the column's top. Taking it spends
    /// it.
    pub(crate) fn take_new_divider_scroll(&mut self) -> Option<f32> {
        let divider = self.new_divider.as_mut()?;
        let top = divider.top?;
        if !divider.scroll_owed {
            return None;
        }
        divider.scroll_owed = false;
        Some((top - OPEN_MARGIN).max(0.0))
    }

    /// The divider, where the layout put it: **NEW** at the column's start,
    /// then a 1 px line across the rest, both in the accent colour.
    pub(crate) fn draw_new_divider(&mut self, canvas: &mut Scene, root: Affine, area: Rect) {
        let Some(top) = self.new_divider.as_ref().and_then(|divider| divider.top) else {
            return;
        };
        let y = area.y - self.conversation_scroll + top;
        if y + NEW_DIVIDER_H < area.y || y > area.y + area.h {
            return;
        }
        let p = theme::p();
        let label = self.engine.simple_layout(
            koru_i18n::t(self.lang, koru_i18n::Key::AiNewMessages),
            role::hint().with_weight(FontWeight::SemiBold),
        );
        let centre = y + NEW_DIVIDER_H * 0.5;
        let _scope = koru_draw::inspect::scope(
            "NewDivider",
            "container",
            root,
            scene::rect(area.x, y, area.w, NEW_DIVIDER_H),
            |record| record.property("role", "first unseen message", "koru-ai D84"),
        );
        scene::draw_layout_first_line(
            canvas,
            root,
            &label,
            area.x,
            centre - scene::first_line_height(&label) * 0.5,
            Some(p.accent),
        );
        let line_x = area.x + label.width() + LABEL_GAP;
        let line_w = area.x + area.w - line_x;
        if line_w > 0.0 {
            scene::fill(
                canvas,
                root,
                p.accent,
                &scene::rect(line_x, centre.round() - 0.5, line_w, 1.0),
            );
        }
    }
}
