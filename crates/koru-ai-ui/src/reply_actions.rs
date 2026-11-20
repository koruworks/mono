// SPDX-License-Identifier: MPL-2.0

//! One reply-action list owns availability, placement, glyphs and tooltips.
use super::*;
use koru_draw::theme::{Profile, Space};
use koru_i18n::{Key as Words, t};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ReplyAction {
    Copy,
    Branch,
    Store,
    Link,
    Retry,
    Details,
}

impl ReplyAction {
    fn glyph(self) -> char {
        match self {
            Self::Copy => icons::COPY,
            Self::Branch => icons::GIT_BRANCH,
            Self::Store => icons::FLOPPY_DISK,
            Self::Link => icons::LINK,
            Self::Retry => icons::REDO,
            Self::Details => icons::ANCHOR,
        }
    }

    pub(crate) fn words(self) -> Words {
        match self {
            Self::Copy => Words::AiReplyCopy,
            Self::Branch => Words::AiReplyBranch,
            Self::Store => Words::AiReplySave,
            Self::Link => Words::AiReplyLink,
            Self::Retry => Words::AiReplyRetry,
            Self::Details => Words::AiReplyDetails,
        }
    }
}

impl ChatSurface {
    pub(crate) fn reply_actions(
        &self,
        message: &MessageLayout,
    ) -> impl Iterator<Item = ReplyAction> {
        use ReplyAction::*;
        let assistant = message.role == ChatRole::Assistant && message.id.is_some();
        let normal = assistant && !self.observing_bot_thread() && !message.folded_empty_answer();
        let complete = normal && message.state == MessageState::Complete;
        let branch = !self.bot_chat_flow();
        // Engine block IDs are layout keys, not durable Store/Link targets.
        let durable_blocks = self.managed.is_none();
        let retry = normal
            && matches!(
                message.state,
                MessageState::Failed | MessageState::Interrupted
            )
            && message.id.is_some_and(|id| self.offers_retry(id));
        let details = assistant
            && self.bot_chat_flow()
            && message
                .id
                .is_some_and(|id| self.bot_work_replies.contains(&id));
        [Copy, Branch, Store, Link, Retry, Details]
            .into_iter()
            .filter(move |action| match action {
                Copy => complete,
                Branch => complete && branch,
                Store | Link => complete && durable_blocks,
                Retry => retry,
                Details => details,
            })
    }

    pub(crate) fn reply_action_rect(
        &self,
        message: &MessageLayout,
        card: Rect,
        index: usize,
    ) -> Rect {
        let rect = action_rect(card, index);
        if self.bot_chat_flow() {
            Rect::new(
                card.x
                    + message.padding()
                    + index as f32 * (ACTION_H + Space::S0_5.px(Profile::Desktop)),
                if message.folded_empty_answer() {
                    card.y
                } else {
                    rect.y
                },
                rect.w,
                rect.h,
            )
        } else {
            rect
        }
    }

    fn reply_actions_visible(&self, index: usize, message: &MessageLayout) -> bool {
        self.response_message_hover == Some(index)
            || message
                .id
                .is_some_and(|id| self.work_focus() == Some(work_view::WorkHit::Header(id)))
    }

    pub(crate) fn draw_reply_actions(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        index: usize,
        card: Rect,
    ) {
        let message = &self.message_layouts[index];
        if !self.reply_actions_visible(index, message) {
            return;
        }
        let actions: Vec<_> = self
            .reply_actions(message)
            .enumerate()
            .map(|(slot, action)| {
                (
                    action,
                    self.reply_action_rect(message, card, slot),
                    action == ReplyAction::Link && message.blocks.is_empty(),
                )
            })
            .collect();
        let Some((_, first, _)) = actions.first() else {
            return;
        };
        let last = actions.last().unwrap().1;
        let focused = message
            .id
            .is_some_and(|id| self.work_focus() == Some(work_view::WorkHit::Header(id)));
        let _row = koru_draw::inspect::scope(
            "Reply actions",
            "row",
            root,
            scene::rect(first.x, first.y, last.x + last.w - first.x, first.h),
            |record| {
                record.property("gap", "2", "Space::S0_5");
            },
        );
        for (action, rect, disabled) in actions {
            let _button = koru_draw::inspect::scope(
                "Reply action",
                "button",
                root,
                scene::rect(rect.x, rect.y, rect.w, rect.h),
                |record| {
                    record.property("accessible name", t(self.lang, action.words()), "koru_i18n");
                },
            );
            IconButton::new(action.glyph(), rect)
                .with_state(IconButtonState {
                    hovered: rect.contains(self.pointer.0, self.pointer.1),
                    active: false,
                    disabled,
                })
                .draw(canvas, root, &mut self.engine);
            if focused && action == ReplyAction::Details {
                scene::stroke(
                    canvas,
                    root,
                    theme::p().focus_ring,
                    theme::BORDER_W,
                    &scene::rrect(
                        rect.x,
                        rect.y,
                        rect.w,
                        rect.h,
                        theme::tokens::Radius::M.px(Profile::Desktop, rect.w, rect.h),
                    ),
                );
            }
        }
    }

    fn reply_action_hit(&self, x: f32, y: f32) -> Option<(usize, ReplyAction, Rect)> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        for (index, message) in self.message_layouts.iter().enumerate() {
            if !self.reply_actions_visible(index, message) {
                continue;
            }
            let card = message_card(
                area,
                area.y - self.conversation_scroll + message.top,
                self.shown_message_height(message),
                message,
            );
            for (slot, action) in self.reply_actions(message).enumerate() {
                let rect = self.reply_action_rect(message, card, slot);
                if rect.contains(x, y) {
                    return Some((index, action, rect));
                }
            }
        }
        None
    }

    pub(crate) fn response_action_at(&self, x: f32, y: f32) -> Option<ResponseAction> {
        let (index, action, _) = self.reply_action_hit(x, y)?;
        let message = &self.message_layouts[index];
        let id = message.id?;
        Some(match action {
            ReplyAction::Copy => ResponseAction::Copy(message.markdown.clone()),
            ReplyAction::Branch => ResponseAction::Branch(id),
            ReplyAction::Store => ResponseAction::Store(id),
            ReplyAction::Link => ResponseAction::Link(message.blocks.first()?.id),
            ReplyAction::Retry => ResponseAction::Retry(id),
            ReplyAction::Details => ResponseAction::Details(id),
        })
    }

    pub(crate) fn response_action_hover_at(&self, x: f32, y: f32) -> Option<(usize, ReplyAction)> {
        self.reply_action_hit(x, y)
            .map(|(index, action, _)| (index, action))
    }

    pub(crate) fn reply_tooltip_at(&self, x: f32, y: f32) -> Option<TooltipContent> {
        use std::hash::{Hash, Hasher};
        let (index, action, rect) = self.reply_action_hit(x, y)?;
        let mut key = std::collections::hash_map::DefaultHasher::new();
        self.message_layouts[index].id.hash(&mut key);
        action.hash(&mut key);
        Some(TooltipContent::new(
            key.finish(),
            rect,
            t(self.lang, action.words()),
        ))
    }
}

#[cfg(test)]
mod tests;
