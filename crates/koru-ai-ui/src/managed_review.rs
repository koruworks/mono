// SPDX-License-Identifier: MPL-2.0
//! Main-composer Routine review, using the existing reader and shared action card.
use super::*;
use koru_ai_core::Timestamp;
use koru_ai_core::bots::{BotChatPurpose, BotId};
use koru_ai_wire::bot_routines as wire;
use koru_draw::theme::tokens::{Profile, Space};
use koru_i18n::{Key as Words, t};
use questions::input::Actions;
use uuid::Uuid;
mod controls;

#[cfg(test)]
mod tests;

/// A composer attachment keeps its identity while a send is awaiting a receipt.
#[derive(Clone)]
pub(crate) struct Attachment {
    pub detail: wire::ReviewDetail,
    token: Uuid,
}

#[derive(Default)]
pub(crate) struct State {
    pub selected: Option<wire::ReviewDetail>,
    token: Uuid,
    items: Vec<wire::ReviewItem>,
    count: u64,
    next: Option<wire::Cursor>,
    stamp: Option<Timestamp>,
    pending: Option<(Uuid, wire::Command)>,
    requested_token: Uuid,
    failed: bool,
    open: bool,
    cursor: usize,
    scroll: f32,
    scroll_max: f32,
    body: Option<Rect>,
    footer: Option<Rect>,
    advance: bool,
}

impl ChatSurface {
    pub(crate) fn attached_review(&self) -> Option<Attachment> {
        if !self.review.open {
            return None;
        }
        Some(Attachment {
            detail: self.review.selected.clone()?,
            token: self.review.token,
        })
    }
    pub(crate) fn take_review_context(&mut self) -> Option<Attachment> {
        let attachment = self.attached_review();
        self.review.open = false;
        self.review.selected = None;
        attachment
    }
    pub(crate) fn restore_review_context(&mut self, attachment: Option<Attachment>) {
        self.review.selected = attachment
            .filter(|a| {
                self.review_subject()
                    .is_some_and(|s| s.0.as_uuid() == a.detail.bot)
            })
            .map(|a| a.detail);
        self.review.open = self.review.selected.is_some();
        self.review.token = Uuid::new_v4();
        self.review.scroll = 0.0;
    }
    pub(crate) fn review_send_ready(&self) -> bool {
        !self.review.open || self.review.selected.is_some()
    }
    pub(crate) fn review_context_changed(&self, old: Option<&Attachment>) -> bool {
        self.review.open && old.is_none_or(|a| a.token != self.review.token)
    }
    pub(crate) fn accepted_review_context(&mut self, attachment: &Attachment) {
        if self.review.token == attachment.token {
            self.take_review_context();
        }
        self.review.stamp = None;
        self.refresh_reviews();
    }
    pub(crate) fn suspend_reviews(&mut self) {
        self.review.stamp = None;
        self.review.failed = self.review.pending.is_some();
    }
    pub(crate) fn resume_reviews(&mut self) {
        if self.review.failed
            && let Some((_, command)) = self.review.pending.take()
        {
            self.request_review(command);
        } else {
            self.refresh_reviews();
        }
    }
    fn review_subject(&self) -> Option<(BotId, ChatId, Timestamp)> {
        let s = self.managed.as_ref()?;
        (s.snapshot.conversation.label.purpose == BotChatPurpose::Main).then_some((
            s.snapshot.conversation.label.bot_id,
            s.thread(),
            s.snapshot.conversation.chat.updated_at,
        ))
    }
    pub(crate) fn refresh_reviews(&mut self) {
        let Some((bot, _, stamp)) = self.review_subject() else {
            return;
        };
        if self.review.pending.is_none() && !self.review.failed && self.review.stamp != Some(stamp)
        {
            self.review.stamp = Some(stamp);
            self.request_review(wire::Command::Reviews { bot, before: None });
        }
    }
    fn request_review(&mut self, command: wire::Command) -> bool {
        let Some((_, thread, _)) = self.review_subject() else {
            return false;
        };
        if self.review.pending.is_some() && !self.review.failed {
            return false;
        }
        let request = Uuid::new_v4();
        self.review.requested_token = self.review.token;
        self.review.failed = false;
        self.review.pending = Some((request, command.clone()));
        if !self.send_command(Command::RoutineReview {
            thread,
            request,
            command,
        }) {
            self.review.failed = true;
        }
        true
    }
    pub fn open_routine_result(&mut self, bot: BotId, occurrence: Uuid) -> bool {
        if self.review_subject().is_none_or(|s| s.0 != bot) {
            return false;
        }
        if self.review.pending.is_some() {
            return false;
        }
        self.review.open = true;
        self.review.token = Uuid::new_v4();
        self.review.scroll = 0.0;
        self.review.selected = None;
        self.request_review(wire::Command::Review {
            bot,
            occurrence,
            offset: 0,
        })
    }
    pub(crate) fn review_response(
        &mut self,
        thread: ChatId,
        request: Uuid,
        result: Result<wire::Response, koru_ai_wire::Failure>,
    ) -> bool {
        let Some((bot, current, _)) = self.review_subject() else {
            return false;
        };
        if thread != current || self.review.pending.as_ref().is_none_or(|p| p.0 != request) {
            return false;
        }
        let response = match result {
            Ok(value) => value,
            Err(failure) => {
                self.review.failed = true;
                self.status = Some(self.failure_words(&failure));
                return true;
            }
        };
        let (_, command) = self.review.pending.take().unwrap();
        match (command, response) {
            (wire::Command::Reviews { before, .. }, wire::Response::Reviews { page })
                if page.bot == bot.as_uuid() =>
            {
                let previous_len = self.review.items.len();
                self.review.count = page.count;
                if before.is_none() {
                    self.review.items.clear();
                }
                for item in page.items {
                    if !self
                        .review
                        .items
                        .iter()
                        .any(|old| old.occurrence == item.occurrence)
                    {
                        self.review.items.push(item);
                    }
                }
                self.review.next = page.next;
                if self.review.open && (self.review.selected.is_none() || self.review.advance) {
                    let index = if self.review.advance { previous_len } else { 0 };
                    self.review.advance = false;
                    if let Some(item) = self.review.items.get(index) {
                        self.open_routine_result(bot, item.occurrence);
                    }
                }
            }
            (
                wire::Command::Review {
                    occurrence, offset, ..
                },
                wire::Response::Review { detail },
            ) if detail.bot == bot.as_uuid()
                && detail.item.occurrence == occurrence
                && detail.offset == offset
                && self.review.requested_token == self.review.token =>
            {
                self.review.selected = Some(detail);
                self.review.scroll = 0.0;
            }
            (
                wire::Command::SkipReview { id, occurrence, .. },
                wire::Response::SkippedReview { request, item, .. },
            ) if id == request && item.occurrence == occurrence => {
                if self.review.requested_token == self.review.token {
                    self.take_review_context();
                }
                self.review.stamp = None;
                self.refresh_reviews();
                self.note_activity();
            }
            _ => {
                self.review.stamp = None;
            }
        }
        self.messages_dirty = true;
        true
    }
    pub(crate) fn review_label(&self) -> Option<String> {
        self.review_subject()?;
        if self.review.failed {
            return Some(t(self.lang, Words::BotsRetry).into());
        }
        if self.review.count == 0 && !self.review.open {
            return None;
        }
        Some(
            koru_i18n::time::LocaleFormat::for_system(Some(self.lang.tag()), None)
                .whole_number_input(self.review.count.min(u32::MAX as u64) as u32),
        )
    }
    fn review_labels(&self) -> Vec<(&'static str, String, bool)> {
        let mut labels = vec![
            (
                "review-previous",
                t(self.lang, Words::DelegateModelPrevious).into(),
                false,
            ),
            (
                "review-next",
                t(self.lang, Words::DelegateModelNext).into(),
                false,
            ),
            (
                "review-close",
                t(self.lang, Words::ViewerClose).into(),
                false,
            ),
        ];
        if self
            .review
            .selected
            .as_ref()
            .is_some_and(|d| d.item.review.as_deref() == Some("pending"))
        {
            labels.push((
                "review-skip",
                t(self.lang, Words::AiQuestionsSkip).into(),
                false,
            ));
        }
        if self
            .review
            .selected
            .as_ref()
            .is_some_and(|d| d.next.is_some())
        {
            labels.push((
                "review-more",
                t(self.lang, Words::RunActivityMore).into(),
                false,
            ));
        }
        if self.review.failed {
            labels.push(("review-retry", t(self.lang, Words::BotsRetry).into(), true));
        }
        labels
    }
    fn review_action(&mut self, index: usize) -> bool {
        let Some((bot, _, _)) = self.review_subject() else {
            return false;
        };
        if index == 2 {
            self.review.open = false;
            self.review.selected = None;
            self.focus = Focus::Composer;
            return true;
        }
        if self.review.failed && index + 1 == self.review_labels().len() {
            if let Some((_, command)) = self.review.pending.take() {
                return self.request_review(command);
            }
        }
        if self.review.pending.is_some() {
            return false;
        }
        let selected = self.review.selected.as_ref();
        if index < 2 {
            let current = selected.and_then(|s| {
                self.review
                    .items
                    .iter()
                    .position(|i| i.occurrence == s.item.occurrence)
            });
            let next = match (index, current) {
                (0, Some(i)) => i.saturating_sub(1),
                (1, Some(i)) => i + 1,
                _ => 0,
            };
            if let Some(item) = self.review.items.get(next) {
                return self.open_routine_result(bot, item.occurrence);
            }
            if let Some(before) = self.review.next.clone() {
                self.review.advance = true;
                return self.request_review(wire::Command::Reviews {
                    bot,
                    before: Some(before),
                });
            }
        } else if let Some(detail) = selected {
            let pending = detail.item.review.as_deref() == Some("pending");
            if index == 3 && pending {
                return self.request_review(wire::Command::SkipReview {
                    id: Uuid::new_v4(),
                    bot,
                    occurrence: detail.item.occurrence,
                });
            }
            if let Some(offset) = detail.next {
                return self.request_review(wire::Command::Review {
                    bot,
                    occurrence: detail.item.occurrence,
                    offset,
                });
            }
        }
        false
    }
    pub(crate) fn draw_review_control(&mut self, canvas: &mut Scene, root: Affine, rect: Rect) {
        let Some(label) = self.review_label() else {
            return;
        };
        ActionButton::new(&label, ActionStyle::Primary, Some(icons::CARET_DOWN)).draw_focused(
            canvas,
            root,
            &mut self.engine,
            rect,
            rect.contains(self.pointer.0, self.pointer.1),
            self.focus == Focus::RoutineReview,
        );
    }
    pub(crate) fn draw_review(&mut self, canvas: &mut Scene, root: Affine) {
        self.review.body = None;
        self.review.footer = None;
        if !self.review.open {
            return;
        }
        let Some((_, thread, _)) = self.review_subject() else {
            return;
        };
        let labels = self.review_labels();
        let width = self.composer_hit_rect.w;
        let footer = controls::Controls::new(
            &mut self.engine,
            thread.as_uuid(),
            &labels,
            (self.focus == Focus::RoutineReview).then_some(self.review.cursor),
            self.review.pending.is_some() && !self.review.failed,
            width,
        );
        let footer_h = footer.height(width);
        let gap = Space::S2.px(Profile::Desktop);
        // Reserve viewport space; long results scroll inside this bounded surface.
        let available =
            (self.composer_hit_rect.y - self.viewport.y - Space::S6.px(Profile::Desktop))
                .clamp(0.0, 360.0);
        let panel = Rect::new(
            self.composer_hit_rect.x,
            self.composer_hit_rect.y - available - gap,
            width,
            available,
        );
        let bottom = Rect::new(
            panel.x,
            panel.y + (panel.h - footer_h).max(0.0),
            width,
            footer_h,
        );
        let body_rect = Rect::new(panel.x, panel.y, width, (panel.h - footer_h).max(0.0));
        let detail = self.review.selected.as_ref();
        let body = Actions {
            id: thread.as_uuid(),
            heading: detail.map_or(t(self.lang, Words::ChatsStatusNeedsYou), |d| {
                d.item.name.as_str()
            }),
            body: detail.map_or(t(self.lang, Words::SupportLoading), |d| d.text.as_str()),
            labels: &[],
            focused: None,
            disabled: false,
        };
        let height = body.height(&mut self.engine, width);
        self.review.scroll_max = (height - body_rect.h).max(0.0);
        self.review.scroll = self.review.scroll.min(self.review.scroll_max);
        let _panel = scene::floating_panel_scope(
            canvas,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(Space::S3.px(Profile::Desktop)),
        );
        let clip = vello::kurbo::Rect::new(
            body_rect.x as f64,
            body_rect.y as f64,
            (body_rect.x + body_rect.w) as f64,
            (body_rect.y + body_rect.h) as f64,
        );
        canvas.push_clip_layer(vello::peniko::Fill::NonZero, root, &clip);
        body.draw(
            canvas,
            root,
            &mut self.engine,
            Rect::new(body_rect.x, body_rect.y - self.review.scroll, width, height),
            self.pointer,
        );
        canvas.pop_layer();
        footer.draw(canvas, root, &mut self.engine, bottom, self.pointer);
        self.review.body = Some(body_rect);
        self.review.footer = Some(bottom);
    }
    pub(crate) fn review_press(&mut self, x: f32, y: f32) -> Option<bool> {
        let (_, thread, _) = self.review_subject()?;
        if self
            .composer_shell
            .as_ref()
            .and_then(|s| s.review())
            .is_some_and(|r| r.contains(x, y))
        {
            self.focus = Focus::RoutineReview;
            if self.review.failed {
                if let Some((_, command)) = self.review.pending.take() {
                    return Some(self.request_review(command));
                }
            }
            self.review.open = !self.review.open;
            if self.review.open && self.review.selected.is_none() {
                self.review_action(1);
            }
            if !self.review.open {
                self.review.selected = None;
            }
            return Some(true);
        }
        if self.review.open && self.review.footer.is_some_and(|r| r.contains(x, y)) {
            let labels = self.review_labels();
            let rect = self.review.footer.unwrap();
            let hit = controls::Controls::new(
                &mut self.engine,
                thread.as_uuid(),
                &labels,
                None,
                self.review.pending.is_some() && !self.review.failed,
                rect.w,
            )
            .hit(rect, x, y);
            self.focus = Focus::RoutineReview;
            if let Some(i) = hit {
                self.review.cursor = i;
                self.review_action(i);
            }
            return Some(true);
        }
        if self.review.open && self.review.body.is_some_and(|r| r.contains(x, y)) {
            return Some(true);
        }
        None
    }
    pub(crate) fn review_key(&mut self, key: &Key, mods: ModifiersState) -> Option<bool> {
        if self.focus == Focus::Composer
            && mods.shift_key()
            && !koru_keys::is_chord(mods)
            && matches!(key, Key::Named(NamedKey::Tab))
            && self.review_label().is_some()
        {
            self.focus = Focus::RoutineReview;
            self.review.cursor = 0;
            return Some(true);
        }
        if self.focus != Focus::RoutineReview {
            return None;
        }
        match key {
            Key::Named(NamedKey::PageDown | NamedKey::PageUp) if self.review.open => {
                let page = self.review.body.map_or(0.0, |r| r.h);
                let direction = if matches!(key, Key::Named(NamedKey::PageDown)) {
                    1.0
                } else {
                    -1.0
                };
                self.review.scroll =
                    (self.review.scroll + direction * page).clamp(0.0, self.review.scroll_max);
                Some(true)
            }
            Key::Named(NamedKey::Escape) => Some(self.review_action(2)),
            Key::Named(NamedKey::Tab) => {
                if self.review.open
                    && !mods.shift_key()
                    && self.review.cursor + 1 < self.review_labels().len()
                {
                    self.review.cursor += 1;
                } else if self.review.open && mods.shift_key() && self.review.cursor > 0 {
                    self.review.cursor -= 1;
                } else if mods.shift_key()
                    && (self.focus_managed_navigation(true)
                        || self.focus_managed_attention(true)
                        || self.step_managed_question(None, false)
                        || self.focus_last_work())
                {
                } else {
                    self.focus = Focus::Composer;
                }
                Some(true)
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) if mods.is_empty() => {
                if self.review.open {
                    Some(self.review_action(self.review.cursor))
                } else {
                    self.review.open = true;
                    Some(self.review_action(1))
                }
            }
            _ => {
                self.focus = Focus::Composer;
                None
            }
        }
    }
    pub(crate) fn review_scroll(&mut self, delta: MouseScrollDelta) -> Option<bool> {
        if !self.review.open
            || !self
                .review
                .body
                .is_some_and(|r| r.contains(self.pointer.0, self.pointer.1))
        {
            return None;
        }
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) => y * 24.0,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        self.review.scroll = (self.review.scroll - dy).clamp(0.0, self.review.scroll_max);
        Some(true)
    }
}
