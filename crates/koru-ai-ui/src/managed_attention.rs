// SPDX-License-Identifier: MPL-2.0
//! The person's uncertain-write decision, using the engine's exact operation set.
use super::*;
use koru_ai_wire::bot_threads as wire;
use koru_i18n::{Key as Words, t};
use questions::input::Actions;

#[derive(Default)]
pub(crate) struct Page {
    pub after: Option<wire::OperationRef>,
    pub items: Vec<wire::OperationAttention>,
    pub next: Option<wire::OperationRef>,
}

#[derive(Default)]
pub(crate) struct State {
    items: Vec<wire::OperationAttention>,
    scan: Vec<wire::OperationAttention>,
    pub next: Option<wire::OperationRef>,
    complete: bool,
    acknowledged: Vec<wire::OperationRef>,
    pub pending: Option<wire::Command>,
    pub retry: Option<Instant>,
    cursor: usize,
    pub top: f32,
    pub height: f32,
}

impl State {
    pub fn new(page: Page) -> Self {
        let mut state = Self::default();
        state.ingest(page);
        state
    }
    pub fn ingest(&mut self, page: Page) -> bool {
        if page.after != self.next {
            return false;
        }
        if page.after.is_none() {
            self.scan.clear();
        }
        self.scan.extend(page.items);
        self.next = page.next;
        if self.next.is_some() {
            // Keep the last complete view during the next bounded scan. The
            // engine rechecks its entire current set atomically on Continue.
            return false;
        }
        self.acknowledged
            .retain(|key| self.scan.iter().any(|item| &item.key == key));
        self.scan
            .retain(|item| !self.acknowledged.contains(&item.key));
        let changed = !self.complete || self.items != self.scan;
        self.complete = true;
        self.items = std::mem::take(&mut self.scan);
        if changed {
            // A disappearing page must not move keyboard focus onto Continue.
            self.cursor = 0;
        }
        changed
    }
    pub fn documents(&self) -> Vec<DocumentId> {
        let mut docs = Vec::new();
        for item in &self.items {
            if !docs.contains(&item.document) {
                docs.push(item.document);
            }
        }
        docs
    }
    fn ready(&self) -> bool {
        self.complete
            && !self.items.is_empty()
            && self.pending.is_none()
            && self
                .items
                .iter()
                .all(|item| item.unresolved && item.drained)
    }
    fn visible(&self, turns: &[wire::ActiveTurn]) -> bool {
        self.items.iter().any(|item| {
            item.unresolved
                || !turns
                    .iter()
                    .any(|turn| turn.bot == item.key.bot && turn.state == "running")
        })
    }
    fn labels(
        &self,
        lang: koru_i18n::Lang,
        titles: &HashMap<DocumentId, String>,
    ) -> Vec<(String, bool)> {
        let mut labels = self
            .documents()
            .into_iter()
            .enumerate()
            .map(|(i, doc)| {
                (
                    titles.get(&doc).cloned().unwrap_or_else(|| {
                        koru_i18n::fill(
                            t(lang, Words::BotsEnginePageNumber),
                            &[&(i + 1).to_string()],
                        )
                    }),
                    false,
                )
            })
            .collect::<Vec<_>>();
        if self.ready() {
            labels.push((t(lang, Words::Continue).into(), true));
        }
        labels
    }
    fn body(&self, lang: koru_i18n::Lang) -> &'static str {
        t(
            lang,
            if self.pending.is_some() {
                Words::BotsEngineContinuePending
            } else if self.ready() {
                Words::BotsEnginePageUncertainDetail
            } else {
                Words::BotsEnginePageChecking
            },
        )
    }
}

impl ChatSurface {
    pub(crate) fn rebuild_managed_attention(&mut self, width: f32) {
        let Some(state) = &mut self.managed else {
            return;
        };
        let attention = &mut state.attention;
        attention.height = if attention.visible(&state.snapshot.turns) {
            Actions {
                id: state.snapshot.conversation.chat.id.as_uuid(),
                heading: t(self.lang, Words::BotsEnginePageUncertain),
                body: attention.body(self.lang),
                labels: &attention.labels(self.lang, &self.document_titles),
                focused: None,
                disabled: false,
            }
            .height(&mut self.engine, width)
        } else {
            0.0
        };
        if attention.height == 0.0 && self.focus == Focus::ManagedAttention {
            self.focus = Focus::Composer;
            self.composer.set_focused(true);
        }
    }
    pub(crate) fn place_managed_attention(&mut self, top: f32) -> f32 {
        let Some(state) = &mut self.managed else {
            return top;
        };
        state.attention.top = top;
        top + if state.attention.height > 0.0 {
            state.attention.height + CARD_GAP
        } else {
            0.0
        }
    }
    pub(crate) fn draw_managed_attention(&mut self, canvas: &mut Scene, root: Affine) {
        let area = self.conversation_rect();
        let Some(state) = &self.managed else {
            return;
        };
        let attention = &state.attention;
        if attention.height == 0.0 {
            return;
        }
        let row = Rect::new(
            area.x,
            area.y - self.conversation_scroll + attention.top,
            area.w,
            attention.height,
        );
        if row.y + row.h < area.y || row.y > area.y + area.h {
            return;
        }
        Actions {
            id: state.thread().as_uuid(),
            heading: t(self.lang, Words::BotsEnginePageUncertain),
            body: attention.body(self.lang),
            labels: &attention.labels(self.lang, &self.document_titles),
            focused: (self.focus == Focus::ManagedAttention).then_some(attention.cursor),
            disabled: false,
        }
        .draw(canvas, root, &mut self.engine, row, self.pointer);
    }
    pub(crate) fn managed_attention_press(&mut self, x: f32, y: f32) -> Option<bool> {
        let state = self.managed.as_ref()?;
        let attention = &state.attention;
        let area = self.conversation_hit_rect;
        let row = Rect::new(
            area.x,
            area.y - self.conversation_scroll + attention.top,
            area.w,
            attention.height,
        );
        if attention.height == 0.0 || !area.contains(x, y) || !row.contains(x, y) {
            return None;
        }
        let hit = Actions {
            id: state.thread().as_uuid(),
            heading: t(self.lang, Words::BotsEnginePageUncertain),
            body: attention.body(self.lang),
            labels: &attention.labels(self.lang, &self.document_titles),
            focused: None,
            disabled: false,
        }
        .hit(&mut self.engine, row, x, y);
        self.focus_managed_attention(false);
        Some(hit.is_none_or(|index| self.activate_managed_attention(index)))
    }
    pub(crate) fn focus_managed_attention(&mut self, last: bool) -> bool {
        let Some(state) = &mut self.managed else {
            return false;
        };
        let attention = &mut state.attention;
        if attention.height == 0.0 {
            return false;
        }
        let count = attention.labels(self.lang, &self.document_titles).len();
        if count == 0 {
            return false;
        }
        attention.cursor = if last { count - 1 } else { 0 };
        self.focus = Focus::ManagedAttention;
        self.composer.set_focused(false);
        self.conversation_scroll = attention.top.max(0.0);
        self.follow_tail = false;
        true
    }
    pub(crate) fn managed_attention_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        let state = self.managed.as_mut()?;
        let attention = &mut state.attention;
        let count = attention.labels(self.lang, &self.document_titles).len();
        match key {
            Key::Named(NamedKey::Tab) if !koru_keys::is_chord(mods) => {
                if mods.shift_key() && attention.cursor > 0 {
                    attention.cursor -= 1;
                } else if !mods.shift_key() && attention.cursor + 1 < count {
                    attention.cursor += 1;
                } else if mods.shift_key() && self.step_managed_question(None, false) {
                } else if !mods.shift_key() && self.focus_managed_navigation(false) {
                } else {
                    self.focus = Focus::Composer;
                    self.composer.set_focused(true);
                }
                Some(true)
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) if mods.is_empty() => {
                let index = attention.cursor;
                Some(self.activate_managed_attention(index))
            }
            Key::Named(NamedKey::Escape) => {
                self.focus = Focus::Composer;
                self.composer.set_focused(true);
                Some(true)
            }
            _ if text.is_some_and(|text| !text.trim().is_empty()) && !koru_keys::is_chord(mods) => {
                self.focus = Focus::Composer;
                self.composer.set_focused(true);
                None
            }
            _ => Some(false),
        }
    }
    fn activate_managed_attention(&mut self, index: usize) -> bool {
        let Some(state) = &self.managed else {
            return false;
        };
        let documents = state.attention.documents();
        if let Some(document) = documents.get(index) {
            self.request_document_open(koru_link::KoruLink::Document {
                document: document.as_uuid(),
                node: None,
            });
            return true;
        }
        if index != documents.len() || !state.attention.ready() {
            return false;
        }
        let thread = state.thread();
        let command = wire::Command::Continue {
            id: uuid::Uuid::new_v4(),
            thread,
            operations: state
                .attention
                .items
                .iter()
                .map(|item| item.key.clone())
                .collect(),
        };
        if self.queue_command(Command::Engine {
            thread,
            command: command.clone(),
        }) {
            let attention = &mut self
                .managed
                .as_mut()
                .expect("retained by enqueue")
                .attention;
            attention.pending = Some(command);
            attention.retry = None;
            self.focus = Focus::Composer;
            self.composer.set_focused(true);
            self.messages_dirty = true;
        }
        true
    }
    pub(crate) fn settle_managed_attention(&mut self, thread: ChatId, reply: &wire::Response) {
        let Some(state) = &mut self.managed else {
            return;
        };
        if state.thread() != thread {
            return;
        }
        let wire::Response::Continued { request, .. } = reply else {
            return;
        };
        let attention = &mut state.attention;
        if let Some(wire::Command::Continue { id, operations, .. }) = &attention.pending
            && id == request
        {
            attention.acknowledged.extend(operations.iter().cloned());
            attention
                .items
                .retain(|item| !operations.contains(&item.key));
            attention.pending = None;
            attention.retry = None;
        }
    }
    pub(crate) fn failed_managed_attention(
        &mut self,
        thread: ChatId,
        command: &wire::Command,
        failure: &koru_ai_wire::Failure,
    ) -> bool {
        let Some(state) = &mut self.managed else {
            return false;
        };
        if state.thread() != thread {
            return false;
        }
        let wire::Command::Continue { id, .. } = command else {
            return false;
        };
        let attention = &mut state.attention;
        if !matches!(&attention.pending, Some(wire::Command::Continue { id: pending, .. }) if id == pending)
        {
            return false;
        }
        if failure.retryable && failure.code != "recovery_pending" {
            attention.retry = Some(Instant::now() + Duration::from_secs(2));
        } else {
            attention.pending = None;
            attention.retry = None;
            attention.complete = false;
            attention.next = None;
            attention.scan.clear();
        }
        state.poll_at = Instant::now();
        self.waiting = false;
        self.status = Some(self.failure_words(failure));
        self.messages_dirty = true;
        true
    }
    pub(crate) fn maintain_managed_attention(&mut self, now: Instant) {
        let (Some(reader), Some(state)) = (&self.reader, &mut self.managed) else {
            return;
        };
        if state.attention.retry.is_some_and(|at| now >= at)
            && let Some(command) = state.attention.pending.clone()
        {
            state.attention.retry = if reader
                .send(Command::Engine {
                    thread: state.thread(),
                    command,
                })
                .is_ok()
            {
                None
            } else {
                Some(now + Duration::from_secs(2))
            };
        }
    }
}

#[cfg(test)]
mod tests;
