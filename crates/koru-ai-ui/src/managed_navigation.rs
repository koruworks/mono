// SPDX-License-Identifier: MPL-2.0
//! Engine navigation composed from the shared element action card. Data is
//! fetched on the reader; this module performs no I/O and starts no timer.
use super::*;
use koru_ai_wire::bot_threads as wire;
use koru_i18n::{Key as Words, t};
use questions::input::Actions;
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct Page {
    pub after: Option<Uuid>,
    pub items: Vec<wire::Card>,
    pub next: Option<Uuid>,
}
#[derive(Default)]
pub(crate) struct State {
    items: Vec<wire::Card>,
    scan: Vec<wire::Card>,
    pub next: Option<Uuid>,
    rows: Vec<Row>,
    cursor: usize,
    pub open: Option<(koru_ai_core::bots::BotId, String, ChatId)>,
    pub pending: Option<wire::Command>,
    pub retry: Option<Instant>,
}
#[derive(Clone)]
enum Action {
    Thread(koru_ai_core::bots::BotId, String, ChatId),
    Document(DocumentId),
    Join,
    Stop(wire::Turn),
}
struct Row {
    id: Uuid,
    heading: String,
    body: String,
    label: String,
    action: Option<Action>,
    before: Option<MessageId>,
    top: f32,
    height: f32,
}
impl Row {
    fn labels(&self) -> Vec<(String, bool)> {
        self.action
            .as_ref()
            .map(|_| vec![(self.label.clone(), false)])
            .unwrap_or_default()
    }
}
impl State {
    pub fn new(page: Page) -> Self {
        let mut s = Self::default();
        s.ingest(page);
        s
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
            return false;
        }
        let changed = self.items != self.scan;
        if changed {
            self.items = std::mem::take(&mut self.scan);
        }
        changed
    }
}
impl ChatSurface {
    pub(crate) fn reset_managed_navigation_positions(&mut self) {
        if let Some(state) = &mut self.managed {
            for row in &mut state.navigation.rows {
                row.top = f32::NAN;
            }
        }
    }
    /// Host opens the selected engine conversation using its normal Bot tabs.
    pub fn take_open_bot_conversation(
        &mut self,
    ) -> Option<(koru_ai_core::bots::BotId, String, ChatId)> {
        self.managed.as_mut()?.navigation.open.take()
    }
    pub fn is_engine_owned(&self) -> bool {
        self.managed.is_some()
    }
    pub(crate) fn rebuild_managed_navigation(&mut self, width: f32) {
        let Some(state) = &mut self.managed else {
            return;
        };
        let lang = self.lang;
        let focused = (self.focus == Focus::ManagedNavigation)
            .then(|| {
                state
                    .navigation
                    .rows
                    .get(state.navigation.cursor)
                    .map(|r| r.id)
            })
            .flatten();
        let mut rows = vec![];
        let first = self.message_layouts.first().and_then(|m| m.id);
        let mut push = |id, heading, body, label, action, before| {
            rows.push(Row {
                id,
                heading,
                body,
                label,
                action,
                before,
                top: f32::NAN,
                height: 0.0,
            })
        };
        if !state.snapshot.joined {
            push(
                state.thread().as_uuid(),
                t(lang, Words::BotsEngineReadAlong).into(),
                String::new(),
                t(lang, Words::BotsEngineJoin).into(),
                Some(Action::Join),
                first,
            );
        }
        if let Some(origin) = state.snapshot.origin.as_ref() {
            let title = if origin.title.is_empty() {
                t(
                    lang,
                    if origin.main {
                        Words::BotsMainConversation
                    } else {
                        Words::BotsOtherConversation
                    },
                )
            } else {
                &origin.title
            };
            let heading = if state.snapshot.conversation.chat.branched_from.is_some() {
                t(lang, Words::BotsEngineOrigin).into()
            } else {
                koru_i18n::fill(
                    t(lang, Words::BotsChildAskedBy),
                    &[&state.snapshot.conversation.label.bot_name],
                )
            };
            push(
                origin.thread.as_uuid(),
                heading,
                format!("{}: {}", t(lang, Words::BotsEngineOrigin), title),
                t(lang, Words::BotsChildOpenConversation).into(),
                Some(Action::Thread(
                    koru_ai_core::bots::BotId::from_uuid(origin.bot),
                    origin.name.clone(),
                    origin.thread,
                )),
                first,
            );
        }
        // Names are display data, never provider identities or copied prose.
        for message in state.messages.values().filter(|m| m.kind == "words") {
            if let Some(bot) = state
                .snapshot
                .participants
                .iter()
                .find(|p| p.bot.to_string() == message.sender)
            {
                push(
                    message.id,
                    bot.name.clone(),
                    String::new(),
                    String::new(),
                    None,
                    Some(MessageId::from_uuid(message.id)),
                );
            }
        }
        for turn in &state.snapshot.turns {
            if let (Some(worker), Some(session)) = (turn.worker, turn.session) {
                if !matches!(turn.state.as_str(), "running" | "needs_you" | "interrupted") {
                    continue;
                }
                let name = state
                    .snapshot
                    .participants
                    .iter()
                    .find(|p| p.bot == turn.bot)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| state.snapshot.conversation.label.bot_name.clone());
                push(
                    turn.id,
                    name,
                    String::new(),
                    t(lang, Words::AiComposerStop).into(),
                    Some(Action::Stop(wire::Turn {
                        id: turn.id,
                        generation: turn.generation,
                        worker,
                        session,
                    })),
                    turn.reply_message.map(MessageId::from_uuid),
                );
            }
        }
        for card in &state.navigation.items {
            let before = if card.sequence == 0 {
                None
            } else {
                state
                    .messages
                    .values()
                    .find(|m| m.sequence > card.sequence && m.kind == "words")
                    .map(|m| MessageId::from_uuid(m.id))
            };
            if let (Some(bot), Some(thread)) = (card.bot, card.thread) {
                let title = if card.title.is_empty() {
                    t(lang, Words::BotsOtherConversation).into()
                } else {
                    card.title.clone()
                };
                let body = koru_i18n::fill(
                    t(lang, Words::BotsEngineThreadSummary),
                    &[&card.messages.to_string(), &card.outputs.to_string()],
                );
                push(
                    card.id,
                    title,
                    body,
                    t(lang, Words::BotsChildOpenConversation).into(),
                    Some(Action::Thread(bot, card.bot_name.clone(), thread)),
                    before,
                );
            } else if let Some(document) = card.document {
                let title = self
                    .document_titles
                    .get(&document)
                    .cloned()
                    .filter(|s| !s.is_empty())
                    .or_else(|| (!card.title.is_empty()).then(|| card.title.clone()))
                    .unwrap_or_else(|| t(lang, Words::AiDocumentLinkUntitled).into());
                push(
                    card.id,
                    t(lang, Words::ArtifactsTitle).into(),
                    title,
                    t(lang, Words::AiOpenStoredDocument).into(),
                    Some(Action::Document(document)),
                    before,
                );
            }
        }
        let positions: HashMap<_, _> = self
            .message_layouts
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.id.map(|id| (id, i)))
            .collect();
        let sequences: HashMap<_, _> = state
            .navigation
            .items
            .iter()
            .map(|c| (c.id, c.sequence))
            .collect();
        rows.sort_by_key(|row| {
            (
                row.before
                    .and_then(|id| positions.get(&id).copied())
                    .unwrap_or(usize::MAX),
                match row.action {
                    Some(Action::Join) => 0,
                    Some(Action::Thread(..) | Action::Document(..)) => 1,
                    _ => 2,
                },
                sequences.get(&row.id).copied().unwrap_or(0),
            )
        });
        for row in &mut rows {
            row.height = Actions {
                id: row.id,
                heading: &row.heading,
                body: &row.body,
                labels: &row.labels(),
                focused: None,
                disabled: false,
            }
            .height(&mut self.engine, width);
        }
        if let Some(id) = focused {
            if let Some(index) = rows.iter().position(|r| r.id == id && r.action.is_some()) {
                state.navigation.cursor = index;
            } else {
                self.focus = Focus::Composer;
                self.composer.set_focused(true);
            }
        }
        state.navigation.rows = rows;
        state.navigation.cursor = state
            .navigation
            .cursor
            .min(state.navigation.rows.len().saturating_sub(1));
    }
    pub(crate) fn place_managed_navigation(
        &mut self,
        before: Option<MessageId>,
        mut top: f32,
    ) -> f32 {
        if let Some(state) = &mut self.managed {
            for row in &mut state.navigation.rows {
                if row.top.is_nan() && (before.is_none() || row.before == before) {
                    row.top = top;
                    top += row.height + CARD_GAP;
                }
            }
        }
        top
    }
    pub(crate) fn draw_managed_navigation(&mut self, canvas: &mut Scene, root: Affine) {
        let area = self.conversation_rect();
        let Some(state) = &self.managed else {
            return;
        };
        for (i, row) in state.navigation.rows.iter().enumerate() {
            let rect = Rect::new(
                area.x,
                area.y - self.conversation_scroll + row.top,
                area.w,
                row.height,
            );
            if rect.y + rect.h < area.y || rect.y > area.y + area.h {
                continue;
            }
            Actions {
                id: row.id,
                heading: &row.heading,
                body: &row.body,
                labels: &row.labels(),
                focused: (self.focus == Focus::ManagedNavigation && state.navigation.cursor == i)
                    .then_some(0),
                disabled: state.navigation.pending.is_some(),
            }
            .draw(canvas, root, &mut self.engine, rect, self.pointer);
        }
    }
    pub(crate) fn managed_navigation_press(&mut self, x: f32, y: f32) -> Option<bool> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        let state = self.managed.as_ref()?;
        for (i, row) in state.navigation.rows.iter().enumerate() {
            let rect = Rect::new(
                area.x,
                area.y - self.conversation_scroll + row.top,
                area.w,
                row.height,
            );
            if !rect.contains(x, y) {
                continue;
            }
            let hit = Actions {
                id: row.id,
                heading: &row.heading,
                body: &row.body,
                labels: &row.labels(),
                focused: None,
                disabled: state.navigation.pending.is_some(),
            }
            .hit(&mut self.engine, rect, x, y);
            return Some(hit.is_some() && self.activate_managed_navigation(i));
        }
        None
    }
    pub(crate) fn focus_managed_navigation(&mut self, last: bool) -> bool {
        let Some(state) = &mut self.managed else {
            return false;
        };
        let positions = state
            .navigation
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.action.is_some())
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        let Some(index) = (if last {
            positions.last()
        } else {
            positions.first()
        }) else {
            return false;
        };
        state.navigation.cursor = *index;
        self.focus = Focus::ManagedNavigation;
        self.composer.set_focused(false);
        self.conversation_scroll = state.navigation.rows[*index].top.max(0.0);
        self.follow_tail = false;
        true
    }
    pub(crate) fn managed_navigation_key(
        &mut self,
        key: &Key,
        mods: ModifiersState,
    ) -> Option<bool> {
        let state = self.managed.as_mut()?;
        if matches!(key, Key::Named(NamedKey::Tab)) && !koru_keys::is_chord(mods) {
            let next = if mods.shift_key() {
                (0..state.navigation.cursor)
                    .rev()
                    .find(|i| state.navigation.rows[*i].action.is_some())
            } else {
                (state.navigation.cursor + 1..state.navigation.rows.len())
                    .find(|i| state.navigation.rows[*i].action.is_some())
            };
            if let Some(index) = next {
                state.navigation.cursor = index;
                self.conversation_scroll = state.navigation.rows[index].top.max(0.0);
            } else if mods.shift_key()
                && (self.focus_managed_attention(true) || self.step_managed_question(None, false))
            {
            } else {
                self.focus = Focus::Composer;
                self.composer.set_focused(true);
            }
            return Some(true);
        }
        if matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space)) && mods.is_empty() {
            let i = state.navigation.cursor;
            return Some(self.activate_managed_navigation(i));
        }
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.focus = Focus::Composer;
            self.composer.set_focused(true);
            return Some(true);
        }
        None
    }
    fn activate_managed_navigation(&mut self, index: usize) -> bool {
        let Some(state) = &mut self.managed else {
            return false;
        };
        if state.navigation.pending.is_some() {
            return false;
        }
        let Some(action) = state
            .navigation
            .rows
            .get(index)
            .and_then(|r| r.action.clone())
        else {
            return false;
        };
        let thread = state.thread();
        let command = match action {
            Action::Thread(bot, name, chat) => {
                state.navigation.open = Some((bot, name, chat));
                return true;
            }
            Action::Document(document) => {
                self.request_document_open(koru_link::KoruLink::Document {
                    document: document.as_uuid(),
                    node: None,
                });
                return true;
            }
            Action::Join => wire::Command::Join {
                id: Uuid::new_v4(),
                thread,
            },
            Action::Stop(turn) => wire::Command::Stop {
                id: Uuid::new_v4(),
                turn,
            },
        };
        if self.queue_command(Command::Engine {
            thread,
            command: command.clone(),
        }) {
            self.managed.as_mut().unwrap().navigation.pending = Some(command);
            self.messages_dirty = true;
        }
        true
    }
}

#[cfg(test)]
mod tests;
