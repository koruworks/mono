// SPDX-License-Identifier: MPL-2.0
//! Main-composer Routine review, using the existing reader and shared action card.
use super::*;
use koru_ai_core::Timestamp;
use koru_ai_core::bots::{BotChatPurpose, BotId};
use koru_ai_wire::bot_routines as wire;
use koru_draw::theme::tokens::{Profile, Space};
use koru_i18n::{Key as Words, t};
use uuid::Uuid;
mod card;
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
    cursor: Part,
    scroll: f32,
    scroll_max: f32,
    header: Option<Rect>,
    body: Option<Rect>,
    footer: Option<Rect>,
    advance: bool,
    /// Where a pending Skip's result sat; the card resumes there once the
    /// refreshed queue arrives.
    skipped_at: Option<usize>,
    resume_at: Option<usize>,
    /// What the card last drew, kept while the next result loads.
    shown: Option<wire::ReviewDetail>,
    /// Each control where it was last drawn: hover, tooltips, presses.
    targets: Vec<(Part, Rect, bool)>,
    /// A whole result being read part by part to be copied.
    copying: Option<(Uuid, String)>,
    /// A save of the result as a document, kept after an uncertain answer.
    saving: Option<wire::Command>,
    save_waiting: bool,
    words: Option<card::WordsLayout>,
    /// Where the words were last drawn, scroll included.
    words_at: Option<Rect>,
    /// The words' selection, anchor then focus, in bytes.
    selection: Option<(usize, usize)>,
    selecting: bool,
}

/// The tallest the card grows before its result scrolls inside it.
const MAX_PANEL_H: f32 = 360.0;

/// One control of the review card, named rather than numbered: the header's
/// arrows and close, and the footer's actions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Part {
    Previous,
    Next,
    #[default]
    Close,
    More,
    Retry,
    Skip,
    Copy,
    Save,
}

impl Part {
    fn name(self) -> &'static str {
        match self {
            Self::Previous => "review-previous",
            Self::Next => "review-next",
            Self::Close => "review-close",
            Self::More => "review-more",
            Self::Retry => "review-retry",
            Self::Skip => "review-skip",
            Self::Copy => "review-copy",
            Self::Save => "review-save",
        }
    }
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
        self.review.shown = self.review.selected.clone();
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
                if self.review.count != page.count {
                    self.note_activity();
                }
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
                    let resume = self.review.resume_at.take();
                    let index = if self.review.advance {
                        previous_len
                    } else {
                        resume.map_or(0, |at| at.min(self.review.items.len().saturating_sub(1)))
                    };
                    self.review.advance = false;
                    if let Some(item) = self.review.items.get(index) {
                        self.open_routine_result(bot, item.occurrence);
                    } else if self.review.selected.is_none() {
                        // Nothing is waiting any more: the card has
                        // nothing to show.
                        self.review.open = false;
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
                && self
                    .review
                    .copying
                    .as_ref()
                    .is_some_and(|(o, words)| *o == occurrence && words.len() == offset) =>
            {
                self.copied_part(bot, occurrence, detail);
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
                self.review.shown = Some(detail.clone());
                self.review.selected = Some(detail);
                self.review.selection = None;
                self.review.scroll = 0.0;
            }
            (
                wire::Command::SaveReview { id, .. },
                wire::Response::SavedReview {
                    request,
                    document,
                    title,
                    ..
                },
            ) if id == request => {
                self.review.saving = None;
                self.apply_update(Update::Exported(
                    title,
                    Some(koru_ai_core::DocumentId::from_uuid(document)),
                ));
            }
            (
                wire::Command::SkipReview { id, occurrence, .. },
                wire::Response::SkippedReview { request, item, .. },
            ) if id == request && item.occurrence == occurrence => {
                // A card closed while the skip was in flight stays closed.
                let resume = self.review.skipped_at.take().filter(|_| self.review.open);
                if self.review.requested_token == self.review.token {
                    self.take_review_context();
                    // Move on to whatever now waits in the skipped
                    // result's place; the refresh below opens it.
                    if let Some(at) = resume {
                        self.review.open = true;
                        self.review.token = Uuid::new_v4();
                        self.review.resume_at = Some(at);
                    }
                }
                self.review.stamp = None;
                self.refresh_reviews();
                self.note_activity();
            }
            _ => {
                self.review.stamp = None;
            }
        }
        if self.review.save_waiting
            && self.review.pending.is_none()
            && let Some(command) = self.review.saving.clone()
        {
            self.review.save_waiting = false;
            self.request_review(command);
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
    /// Where the selected result sits among the loaded waiting results.
    fn review_position(&self) -> Option<usize> {
        let selected = self.review_displayed()?;
        self.review
            .items
            .iter()
            .position(|item| item.occurrence == selected.item.occurrence)
    }
    fn review_selected_pending(&self) -> bool {
        self.review_displayed()
            .is_some_and(|d| d.item.review.as_deref() == Some("pending"))
    }
    fn review_can_step(&self, forward: bool) -> bool {
        match (forward, self.review_position()) {
            (false, Some(i)) => i > 0,
            (false, None) => false,
            (true, Some(i)) => i + 1 < self.review.items.len() || self.review.next.is_some(),
            // A result reopened from a reply is not in the queue: Next
            // starts the queue.
            (true, None) => !self.review.items.is_empty(),
        }
    }
    /// The footer's labelled actions: *Show more* reads on, *Retry* repeats
    /// a failed request, *Skip* dismisses a waiting result.
    fn review_actions(&self) -> Vec<(Part, String, bool)> {
        let mut actions = vec![];
        if self.review_displayed().is_some_and(|d| d.next.is_some()) {
            actions.push((
                Part::More,
                t(self.lang, Words::RunActivityMore).into(),
                false,
            ));
        }
        if self.review.failed {
            actions.push((Part::Retry, t(self.lang, Words::BotsRetry).into(), true));
        }
        if self.review_selected_pending() {
            actions.push((
                Part::Skip,
                t(self.lang, Words::AiQuestionsSkip).into(),
                false,
            ));
        }
        actions
    }
    /// The parts the keyboard steps through, in order: the header's
    /// arrows that act now and Close, then the footer.
    fn review_parts(&self) -> Vec<Part> {
        let mut parts = vec![];
        if self.review_can_step(false) {
            parts.push(Part::Previous);
        }
        if self.review_can_step(true) {
            parts.push(Part::Next);
        }
        parts.push(Part::Close);
        parts.extend(self.review_actions().into_iter().map(|(part, _, _)| part));
        parts
    }
    fn review_chrome(&mut self, id: Uuid, width: f32) -> controls::Chrome {
        let pending = self.review_selected_pending();
        let heading = if self.review_displayed().is_none() || pending {
            t(self.lang, Words::DelegateRunNeedsYou)
        } else {
            t(self.lang, Words::BotsReviewResult)
        };
        let position = self.review_position_text();
        let actions = self.review_actions();
        let shown = self.review_displayed().is_some();
        let tools = [
            controls::Arrow {
                part: Part::Copy,
                enabled: shown,
                name: t(self.lang, Words::AiReplyCopy),
            },
            controls::Arrow {
                part: Part::Save,
                enabled: shown,
                name: t(self.lang, Words::AiReplySave),
            },
        ];
        let spec = controls::Spec {
            id,
            heading,
            position: position.as_deref(),
            arrows: [
                controls::Arrow {
                    part: Part::Previous,
                    enabled: self.review_can_step(false),
                    name: t(self.lang, Words::BotsReviewPrevious),
                },
                controls::Arrow {
                    part: Part::Next,
                    enabled: self.review_can_step(true),
                    name: t(self.lang, Words::BotsReviewNext),
                },
                controls::Arrow {
                    part: Part::Close,
                    enabled: true,
                    name: t(self.lang, Words::ViewerClose),
                },
            ],
            tools: if shown { &tools } else { &[] },
            actions: &actions,
            focused: (self.focus == Focus::RoutineReview).then_some(self.review.cursor),
            busy: self.review.pending.is_some() && !self.review.failed,
        };
        controls::Chrome::new(&mut self.engine, &spec, width)
    }
    /// "2 of 8" for a waiting result in the loaded queue; nothing for one
    /// reopened from a reply, which is not in the queue.
    fn review_position_text(&self) -> Option<String> {
        let index = self
            .review_position()
            .filter(|_| self.review_selected_pending())?;
        let format = koru_i18n::time::LocaleFormat::for_system(Some(self.lang.tag()), None);
        let number = |n: u64| format.whole_number_input(n.min(u32::MAX as u64) as u32);
        Some(koru_i18n::fill(
            t(self.lang, Words::BotsReviewPosition),
            &[
                &number(index as u64 + 1),
                &number(self.review.count.max(index as u64 + 1)),
            ],
        ))
    }
    /// The line above the result and the result's words.
    fn review_words(&self) -> (String, String) {
        let Some(detail) = self.review_displayed() else {
            return (String::new(), t(self.lang, Words::SupportLoading).into());
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64);
        let when = koru_i18n::time::relative_long(self.lang, detail.item.published_at_ms, now);
        (
            koru_i18n::fill(
                t(self.lang, Words::BotsReviewFrom),
                &[&detail.item.name, &when],
            ),
            detail.text.clone(),
        )
    }
    /// The composer's hint while a result card is attached.
    pub(crate) fn review_placeholder(&self) -> Option<String> {
        if !self.review.open {
            return None;
        }
        let state = self.managed.as_ref()?;
        self.review_subject()?;
        let ask = koru_i18n::fill(
            t(self.lang, Words::BotsReviewComposerPlaceholder),
            &[&state.snapshot.conversation.label.bot_name],
        );
        // The card's keys, from the registry, where hints are shown.
        let hint = |id| koru_keys::show_hint(id, koru_keys::Section::Guide);
        let keys = match (
            hint(koru_keys::ShortcutId::ReviewPrevious),
            hint(koru_keys::ShortcutId::ReviewNext),
            hint(koru_keys::ShortcutId::ReviewSkip),
        ) {
            (Some(previous), Some(next), Some(skip)) => koru_i18n::fill(
                t(self.lang, Words::BotsReviewComposerKeys),
                &[&previous, &next, &skip],
            ),
            _ => return Some(ask),
        };
        Some(format!("{ask} · {keys}"))
    }
    pub(crate) fn review_action(&mut self, part: Part) -> bool {
        let Some((bot, _, _)) = self.review_subject() else {
            return false;
        };
        if part == Part::Close {
            self.review.open = false;
            self.review.selected = None;
            self.review.resume_at = None;
            self.focus = Focus::Composer;
            return true;
        }
        if part == Part::Copy {
            return self.copy_review(bot);
        }
        if part == Part::Save {
            return self.ask_review_save_name();
        }
        if part == Part::Retry {
            if self.review.failed
                && let Some((_, command)) = self.review.pending.take()
            {
                return self.request_review(command);
            }
            return false;
        }
        if self.review.pending.is_some() {
            return false;
        }
        match part {
            Part::Previous | Part::Next => {
                if !self.review_can_step(part == Part::Next) {
                    return false;
                }
                let next = match (part, self.review_position()) {
                    (Part::Previous, Some(i)) => i - 1,
                    (_, Some(i)) => i + 1,
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
                false
            }
            Part::Skip => {
                let Some(detail) = self.review.selected.as_ref() else {
                    return false;
                };
                if detail.item.review.as_deref() != Some("pending") {
                    return false;
                }
                let occurrence = detail.item.occurrence;
                // After the skip lands, the card moves on to the result
                // that took this one's place.
                self.review.skipped_at = Some(self.review_position().unwrap_or(0));
                self.request_review(wire::Command::SkipReview {
                    id: Uuid::new_v4(),
                    bot,
                    occurrence,
                })
            }
            Part::More => {
                let Some((occurrence, offset)) = self
                    .review
                    .selected
                    .as_ref()
                    .and_then(|d| Some((d.item.occurrence, d.next?)))
                else {
                    return false;
                };
                self.request_review(wire::Command::Review {
                    bot,
                    occurrence,
                    offset,
                })
            }
            Part::Close | Part::Retry | Part::Copy | Part::Save => false,
        }
    }
    pub(crate) fn draw_review(&mut self, canvas: &mut Scene, root: Affine) {
        self.review.header = None;
        self.review.body = None;
        self.review.footer = None;
        self.review.targets.clear();
        self.review.words_at = None;
        if !self.review.open {
            // Closed: nothing is kept to show next time.
            self.review.shown = None;
            self.review.selection = None;
            self.review.selecting = false;
            return;
        }
        let Some((_, thread, _)) = self.review_subject() else {
            return;
        };
        let width = self.composer_hit_rect.w;
        let chrome = self.review_chrome(thread.as_uuid(), width);
        let header_h = chrome.header_height(width);
        let footer_h = chrome.footer_height(width);
        let (meta, words) = self.review_words();
        let words = self.review_words_layout(
            &words,
            (width - 2.0 * Space::S3.px(Profile::Desktop)).max(1.0),
        );
        let body = controls::body(&meta, words, self.review_selection_range());
        let content_h = controls::measure(&body, controls::BODY, width);
        let gap = Space::S2.px(Profile::Desktop);
        // The card fits its result, up to the room above the composer;
        // a long result scrolls inside it.
        let available =
            (self.composer_hit_rect.y - self.viewport.y - Space::S6.px(Profile::Desktop))
                .clamp(0.0, MAX_PANEL_H);
        let panel_h = (header_h + content_h + footer_h).min(available);
        let panel = Rect::new(
            self.composer_hit_rect.x,
            self.composer_hit_rect.y - panel_h - gap,
            width,
            panel_h,
        );
        let header = Rect::new(panel.x, panel.y, width, header_h.min(panel_h));
        let body_rect = Rect::new(
            panel.x,
            panel.y + header.h,
            width,
            (panel_h - header_h - footer_h).max(0.0),
        );
        let footer = (footer_h > 0.0)
            .then(|| Rect::new(panel.x, panel.y + panel_h - footer_h, width, footer_h));
        self.review.scroll_max = (content_h - body_rect.h).max(0.0);
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
        chrome.draw(canvas, root, &mut self.engine, header, footer, self.pointer);
        let clip = vello::kurbo::Rect::new(
            body_rect.x as f64,
            body_rect.y as f64,
            (body_rect.x + body_rect.w) as f64,
            (body_rect.y + body_rect.h) as f64,
        );
        canvas.push_clip_layer(vello::peniko::Fill::NonZero, root, &clip);
        let area = Rect::new(
            body_rect.x,
            body_rect.y - self.review.scroll,
            width,
            content_h,
        );
        let frame = questions::elements::layout(&body, area, koru_draw::el::FrameCx::root());
        frame.paint(&body, canvas, root, &mut self.engine);
        self.review.words_at = frame.rect_of(controls::WORDS);
        canvas.pop_layer();
        self.review.targets = chrome.targets(header, footer);
        self.review.header = Some(header);
        self.review.body = Some(body_rect);
        self.review.footer = footer;
    }
    pub(crate) fn review_press(&mut self, x: f32, y: f32) -> Option<bool> {
        let (_, thread, _) = self.review_subject()?;
        // A press in the result's words starts a selection; any other
        // press lets the old one go, as in a chat reply.
        if self.review.open && self.review_text_press(x, y) {
            return Some(true);
        }
        self.clear_review_selection();
        if self
            .composer_shell
            .as_ref()
            .and_then(|s| s.review())
            .is_some_and(|r| r.contains(x, y))
        {
            self.focus = Focus::RoutineReview;
            if self.review.failed
                && let Some((_, command)) = self.review.pending.take()
            {
                return Some(self.request_review(command));
            }
            self.review.open = !self.review.open;
            if self.review.open && self.review.selected.is_none() {
                self.review_action(Part::Next);
            }
            if !self.review.open {
                self.review.selected = None;
                self.review.resume_at = None;
            }
            return Some(true);
        }
        if !self.review.open {
            return None;
        }
        let header = self.review.header?;
        let footer = self.review.footer;
        if header.contains(x, y) || footer.is_some_and(|r| r.contains(x, y)) {
            let chrome = self.review_chrome(thread.as_uuid(), header.w);
            self.focus = Focus::RoutineReview;
            if let Some(part) = chrome.hit(header, footer, x, y) {
                self.review.cursor = part;
                self.review_action(part);
            }
            return Some(true);
        }
        if self.review.body.is_some_and(|r| r.contains(x, y)) {
            return Some(true);
        }
        None
    }
    pub(crate) fn review_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        if self.review.open && self.review_subject().is_some() {
            let chord = |id| koru_keys::registry::get(id).chord.matches(key, text, mods);
            // Alt+S skips the result on the card wherever the keyboard is.
            if chord(koru_keys::ShortcutId::ReviewSkip) {
                return Some(self.review_action(Part::Skip));
            }
            // Escape lets go of a selection in the result first, then
            // closes the card, wherever the keyboard is.
            // The composer's own popup is closed by its Escape first.
            if matches!(key, Key::Named(NamedKey::Escape))
                && mods.is_empty()
                && !self.composer.picker_open()
            {
                if self.clear_review_selection() {
                    return Some(true);
                }
                return Some(self.review_action(Part::Close));
            }
            // ← and → step through the results while the card holds the
            // keyboard, or the composer is empty: a caret keeps its arrows.
            let arrows_free = self.focus == Focus::RoutineReview
                || (self.focus == Focus::Composer && self.composer.text().is_empty());
            if arrows_free && chord(koru_keys::ShortcutId::ReviewPrevious) {
                self.review_action(Part::Previous);
                return Some(true);
            }
            if arrows_free && chord(koru_keys::ShortcutId::ReviewNext) {
                self.review_action(Part::Next);
                return Some(true);
            }
        }
        if self.focus == Focus::Composer
            && mods.shift_key()
            && !koru_keys::is_chord(mods)
            && matches!(key, Key::Named(NamedKey::Tab))
            && self.review_label().is_some()
        {
            self.focus = Focus::RoutineReview;
            self.review.cursor = self.review_parts()[0];
            return Some(true);
        }
        if self.focus != Focus::RoutineReview {
            return None;
        }
        let parts = self.review_parts();
        if !parts.contains(&self.review.cursor) {
            self.review.cursor = parts[0];
        }
        let at = parts
            .iter()
            .position(|part| *part == self.review.cursor)
            .unwrap_or(0);
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
            Key::Named(NamedKey::Escape) => Some(self.review_action(Part::Close)),
            Key::Named(NamedKey::Tab) => {
                if self.review.open && !mods.shift_key() && at + 1 < parts.len() {
                    self.review.cursor = parts[at + 1];
                } else if self.review.open && mods.shift_key() && at > 0 {
                    self.review.cursor = parts[at - 1];
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
                    Some(self.review_action(Part::Next))
                }
            }
            _ => {
                self.focus = Focus::Composer;
                None
            }
        }
    }
    /// The pill's width: the compact action's content and air, and the
    /// extra its round ends need.
    pub(crate) fn review_control_width(&mut self, label: &str) -> f32 {
        ActionButton::new(label, ActionStyle::Primary, Some(icons::CARET_DOWN))
            .width(&mut self.engine)
            + 2.0 * Space::S1.px(Profile::Desktop)
    }
    /// **The review pill**: fully round, in the accent, its count and a
    /// chevron that turns up while the card is open and back as it closes.
    pub(crate) fn draw_review_control(&mut self, canvas: &mut Scene, root: Affine, rect: Rect) {
        let Some(label) = self.review_label() else {
            return;
        };
        self.review_chevron
            .set(self.review.open, std::time::Instant::now());
        let p = theme::p();
        let radius = rect.h * 0.5;
        let shape = scene::rrect(rect.x, rect.y, rect.w, rect.h, radius);
        let _scope = koru_draw::inspect::scope(
            "Routine review pill",
            "button",
            root,
            scene::rect(rect.x, rect.y, rect.w, rect.h),
            |record| record.property("radius", "h / 2", "fully round"),
        );
        scene::fill(canvas, root, p.accent, &shape);
        if rect.contains(self.pointer.0, self.pointer.1) {
            scene::fill(canvas, root, p.hover_tint, &shape);
        }
        if self.focus == Focus::RoutineReview {
            scene::stroke(
                canvas,
                root,
                p.accent,
                theme::BORDER_W,
                &scene::rrect(
                    rect.x - 2.0,
                    rect.y - 2.0,
                    rect.w + 4.0,
                    rect.h + 4.0,
                    radius + 2.0,
                ),
            );
        }
        let text = self.engine.simple_layout(&label, role::row());
        let caret = self
            .engine
            .icon_layout(icons::CARET_DOWN, ActionButton::ICON_SIZE)
            .width();
        let content = text.width() + ActionButton::GAP + caret;
        let x = rect.x + (rect.w - content) * 0.5;
        scene::draw_layout(
            canvas,
            root,
            &text,
            x,
            koru_ui::text_field::baseline_centred_y(&text, rect.y, rect.h),
            Some(p.on_accent),
        );
        paint_turning_caret(
            canvas,
            root,
            &mut self.engine,
            Rect::new(x + text.width() + ActionButton::GAP, rect.y, caret, rect.h),
            ActionButton::ICON_SIZE,
            self.review_chevron.progress(),
            p.on_accent,
        );
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
