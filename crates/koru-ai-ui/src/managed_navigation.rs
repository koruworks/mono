// SPDX-License-Identifier: MPL-2.0
//! Engine navigation composed from the shared element action card. Data is
//! fetched on the reader; this module performs no I/O and starts no timer.
use super::*;
use koru_ai_wire::bot_threads as wire;
use koru_i18n::{Key as Words, t};
use questions::input::Actions;
use uuid::Uuid;
mod chip;
mod exchange;
mod reply;
mod reply_context;
mod routine;
mod turn;

static THREAD_STATES: koru_draw::el::StateRecipe = koru_draw::el::StateRecipe {
    name: "Bot thread link",
    base: koru_draw::el::Look::NONE,
    selected: koru_draw::el::Look::NONE,
    hover: koru_draw::el::Look::NONE,
    pressed: koru_draw::el::Look::NONE,
    selected_hover: koru_draw::el::Look::NONE,
    selected_pressed: koru_draw::el::Look::NONE,
    disabled: koru_draw::el::Look::NONE,
    focus: Some(koru_draw::el::Ring::Line(koru_draw::theme::Line::Focus)),
};

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
    target_cursor: usize,
    pub open: Option<(koru_ai_core::bots::BotId, String, ChatId)>,
    pub pending: Option<wire::Command>,
    pub retry: Option<Instant>,
}
#[derive(Clone)]
enum Action {
    Thread(koru_ai_core::bots::BotId, String, ChatId),
    Document(DocumentId),
    Review(koru_ai_core::bots::BotId, Uuid),
    Join,
}
struct Row {
    id: Uuid,
    heading: String,
    body: String,
    label: String,
    action: Option<Action>,
    before: Option<MessageId>,
    after: Option<MessageId>,
    top: f32,
    height: f32,
    targets: Vec<exchange::Target>,
    replies: Vec<reply::Reply>,
    routine: bool,
}
impl Row {
    fn rect(&self, area: Rect, scroll: f32) -> Rect {
        Rect::new(area.x, area.y - scroll + self.top, area.w, self.height)
    }

    fn compact_height(
        &self,
        engine: &mut TextEngine,
        faces: &HashMap<Uuid, u8>,
        width: f32,
    ) -> f32 {
        // Measure the actual row inside a column. The question-card helper
        // only recognizes question-card names and returned zero for these rows.
        let tree =
            koru_draw::el::col("Bot thread slot").child(self.compact_tree(engine, faces, width));
        questions::elements::layout(
            &tree,
            Rect::new(0.0, 0.0, width, f32::from(u16::MAX)),
            koru_draw::el::FrameCx::root(),
        )
        .rect_of("Bot message thread")
        .expect("thread row exists")
        .h
    }

    fn compact(&self) -> bool {
        matches!(
            self.action,
            Some(Action::Thread(..) | Action::Review(..)) | None
        )
    }
    fn compact_tree(
        &self,
        engine: &mut TextEngine,
        faces: &HashMap<Uuid, u8>,
        width: f32,
    ) -> koru_draw::el::El {
        if matches!(self.action, Some(Action::Review(..))) {
            return reply_context::tree(self, engine, width);
        }
        if self.routine {
            return routine::tree(self, engine, faces, width);
        }
        if !self.targets.is_empty() {
            return exchange::tree(self, engine, faces, width);
        }
        use koru_draw::el::{Key, Role, row, text};
        use koru_draw::theme::{Ink, Space, roles};
        let words = if self.body.is_empty() {
            self.heading.clone()
        } else {
            format!("{} · {}", self.heading, self.body)
        };
        let mut tree = row("Bot message thread")
            .id(Key::model(self.id.as_u128(), "bot-thread"))
            .pad_x(Space::S2)
            .pad_y(Space::S1)
            .gap(Space::S2)
            .radius(koru_draw::theme::Radius::M)
            .items_center()
            .states(&THREAD_STATES);
        if let Some(Action::Thread(bot, name, _)) = &self.action {
            tree = tree
                .child(
                    koru_draw::el::leaf(
                        "Bot identity",
                        chip::BotChip::new(
                            engine,
                            faces.get(&bot.as_uuid()).copied().unwrap_or(0),
                            name,
                        )
                        .with_hover_parent(Key::model(self.id.as_u128(), "bot-thread")),
                    )
                    .decorative(),
                )
                .child(
                    text(&self.body)
                        .role(roles::META)
                        .ink(Ink::Secondary)
                        .fill()
                        .wrap(),
                );
        } else {
            tree = tree.child(
                text(words.clone())
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .wrap(),
            );
        }
        if self.action.is_some() {
            tree.hit()
                .focusable()
                .action(koru_draw::el::Action::Click)
                .semantics(Role::Button)
                .name_text(words)
        } else {
            tree
        }
    }
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
    pub(crate) fn observer_key(&mut self, key: &Key, mods: ModifiersState) -> bool {
        if self.focus == Focus::ManagedNavigation {
            return self.managed_navigation_key(key, mods).unwrap_or(false);
        }
        match key {
            Key::Named(NamedKey::Tab) => {
                self.focus_managed_navigation(mods.shift_key()) || self.focus_last_work()
            }
            Key::Named(NamedKey::PageDown | NamedKey::PageUp) => {
                let delta = if matches!(key, Key::Named(NamedKey::PageDown)) {
                    -1.0
                } else {
                    1.0
                };
                self.scroll_by(MouseScrollDelta::PixelDelta(
                    winit::dpi::PhysicalPosition::new(
                        0.0,
                        (delta * self.conversation_hit_rect.h) as f64,
                    ),
                ))
            }
            _ => false,
        }
    }

    pub(crate) fn observer_mouse_down(&mut self, x: f32, y: f32) -> bool {
        if let Some(handled) = self.link_card_mouse_down(x, y) {
            return handled;
        }
        if self
            .jump_button()
            .is_some_and(|button| button.contains(x, y))
        {
            return self.jump_to_latest();
        }
        if let Some(scrollbar) = self
            .conversation_scrollbar()
            .filter(|bar| bar.in_gutter(x, y))
        {
            self.follow_tail = false;
            if scrollbar.on_thumb(x, y) {
                self.conversation_scroll_drag = Some(y - scrollbar.thumb_top);
            } else {
                let delta = if y < scrollbar.thumb_top {
                    -self.conversation_hit_rect.h
                } else {
                    self.conversation_hit_rect.h
                };
                self.conversation_scroll =
                    (self.conversation_scroll + delta).clamp(0.0, scrollbar.max_scroll);
            }
            return true;
        }
        if let Some(handled) = self.managed_navigation_press(x, y) {
            return handled;
        }
        if let Some(hit) = self.work_hit_at(x, y) {
            self.focus_work(hit);
            return self.toggle_work(hit);
        }
        if let Some(url) = self.user_link_at(x, y) {
            return self.follow_link(&url);
        }
        if let Some(url) = self.source_link_at(x, y) {
            open_web_link(&url);
            return true;
        }
        if let Some(hit) = self.response_text_hit(x, y) {
            self.focus = Focus::None;
            self.response_selection = Some(ResponseSelection {
                anchor: hit.position,
                focus: hit.position,
                dragging: true,
                pressed_link: hit.link,
            });
            return true;
        }
        self.response_selection = None;
        false
    }

    fn navigation(&self) -> &State {
        self.managed
            .as_ref()
            .map_or(&self.host_navigation, |state| &state.navigation)
    }
    fn navigation_mut(&mut self) -> &mut State {
        self.managed
            .as_mut()
            .map_or(&mut self.host_navigation, |state| &mut state.navigation)
    }
    pub fn observing_bot_thread(&self) -> bool {
        self.managed.as_ref().is_some_and(|state| {
            state.snapshot.origin.is_some()
                && state.snapshot.conversation.chat.branched_from.is_none()
        })
    }
    pub(crate) fn reset_managed_navigation_positions(&mut self) {
        {
            for row in &mut self.navigation_mut().rows {
                row.top = f32::NAN;
            }
        }
    }
    /// Host opens the selected engine conversation using its normal Bot tabs.
    pub fn take_open_bot_conversation(
        &mut self,
    ) -> Option<(koru_ai_core::bots::BotId, String, ChatId)> {
        self.navigation_mut().open.take()
    }
    pub(crate) fn rebuild_managed_navigation(&mut self, width: f32) {
        let observer = self.observing_bot_thread();
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
                after: None,
                top: f32::NAN,
                height: 0.0,
                targets: Vec::new(),
                replies: Vec::new(),
                routine: false,
            })
        };
        if !state.snapshot.joined && !observer {
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
        let single_main = state.snapshot.conversation.label.purpose
            == koru_ai_core::bots::BotChatPurpose::Main
            && state.snapshot.participants.len() == 1
            && state.snapshot.participants[0].bot
                == state.snapshot.conversation.label.bot_id.as_uuid();
        for message in state
            .messages
            .values()
            .filter(|m| m.kind == "words" && !single_main)
        {
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
        for card in &state.navigation.items {
            if !card.dispatches.is_empty() || card.routine.is_some() {
                continue;
            }
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
                let body = koru_i18n::fill(
                    t(lang, Words::BotsEngineThreadSummary),
                    &[&card.messages.to_string(), &card.outputs.to_string()],
                );
                push(
                    card.id,
                    card.bot_name.clone(),
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
        drop(push);
        rows.extend(reply_context::rows(state, lang));
        for card in state.navigation.items.iter().filter(|card| {
            card.routine.is_some()
                && state.snapshot.conversation.label.purpose
                    != koru_ai_core::bots::BotChatPurpose::Main
        }) {
            let before = state
                .messages
                .values()
                .find(|message| message.sequence > card.sequence && message.kind == "words")
                .map(|message| MessageId::from_uuid(message.id));
            if let Some(row) = routine::row(card, before, lang) {
                rows.push(row);
            }
        }
        // An observed exchange identifies the author of each actual message.
        // These are labels, not controls that send or join the conversation.
        for row in rows.iter_mut().filter(|row| row.action.is_none()) {
            if let Some(message) = state.messages.values().find(|m| m.id == row.id)
                && let Some(speaker) = state
                    .snapshot
                    .participants
                    .iter()
                    .find(|p| p.bot.to_string() == message.sender)
            {
                let bot = koru_ai_core::bots::BotId::from_uuid(speaker.bot);
                row.targets.push(exchange::Target {
                    bot,
                    name: speaker.name.clone(),
                    owner: bot,
                    owner_name: speaker.name.clone(),
                    chat: state.thread(),
                });
            }
        }
        rows.extend(exchange::rows(
            &state.navigation.items,
            &self.message_layouts,
            lang,
        ));
        let positions: HashMap<_, _> = self
            .message_layouts
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.id.map(|id| (id, i)))
            .collect();
        let mut sequences: HashMap<_, _> = state
            .navigation
            .items
            .iter()
            .map(|c| (c.id, c.sequence))
            .collect();
        for card in &state.navigation.items {
            for dispatch in &card.dispatches {
                sequences
                    .entry(dispatch.group)
                    .and_modify(|sequence| *sequence = (*sequence).min(card.sequence))
                    .or_insert(card.sequence);
            }
        }
        rows.sort_by_key(|row| {
            (
                row.before
                    .and_then(|id| positions.get(&id).copied())
                    .unwrap_or(usize::MAX),
                match row.action {
                    Some(Action::Join) => 0,
                    Some(Action::Thread(..) | Action::Document(..) | Action::Review(..)) => 1,
                    _ => 2,
                },
                sequences.get(&row.id).copied().unwrap_or(0),
            )
        });
        for row in &mut rows {
            reply::prepare(row, &mut self.engine, width);
            row.height = if row.compact() {
                row.compact_height(&mut self.engine, &self.bot_faces, width)
            } else {
                Actions {
                    id: row.id,
                    heading: &row.heading,
                    body: &row.body,
                    labels: &row.labels(),
                    focused: None,
                    disabled: false,
                }
                .height(&mut self.engine, width)
            };
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
        {
            for row in &mut self.navigation_mut().rows {
                if row.top.is_nan()
                    && row.after.is_none()
                    && (before.is_none() || row.before == before)
                {
                    row.top = top;
                    top += row.height + CARD_GAP;
                }
            }
        }
        top
    }
    pub(crate) fn place_managed_navigation_after(
        &mut self,
        message: MessageId,
        mut top: f32,
    ) -> f32 {
        for row in &mut self.navigation_mut().rows {
            if row.top.is_nan() && row.after == Some(message) {
                row.top = top;
                top += row.height + CARD_GAP;
            }
        }
        top
    }
    pub(crate) fn navigation_turn_bounds(
        &self,
        message: MessageId,
    ) -> impl Iterator<Item = (f32, f32)> + '_ {
        self.navigation()
            .rows
            .iter()
            .filter(move |row| row.after.or(row.before) == Some(message))
            .map(|row| (row.top, row.top + row.height))
    }

    pub(crate) fn draw_managed_navigation_for(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        message: Option<MessageId>,
    ) {
        let area = self.conversation_rect();
        let nav = self
            .managed
            .as_ref()
            .map_or(&self.host_navigation, |state| &state.navigation);
        for (i, row) in nav.rows.iter().enumerate() {
            let belongs = match message {
                Some(id) => row.after.or(row.before) == Some(id),
                None => !row.after.or(row.before).is_some_and(|id| {
                    self.message_layouts
                        .iter()
                        .any(|message| message.id == Some(id))
                }),
            };
            if !belongs {
                continue;
            }
            let rect = row.rect(area, self.conversation_scroll);
            if rect.y + rect.h < area.y || rect.y > area.y + area.h {
                continue;
            }
            if row.compact() {
                let tree = row.compact_tree(&mut self.engine, &self.bot_faces, rect.w);
                let bounds =
                    questions::elements::layout(&tree, rect, koru_draw::el::FrameCx::root());
                let key = row
                    .targets
                    .get(nav.target_cursor)
                    .map(exchange::target_key)
                    .unwrap_or_else(|| koru_draw::el::Key::model(row.id.as_u128(), "bot-thread"));
                let frame = questions::elements::layout(
                    &tree,
                    rect,
                    koru_draw::el::FrameCx::root().with_interaction(koru_draw::el::Interaction {
                        hot: bounds.hit_at(self.pointer.0, self.pointer.1),
                        pressed: None,
                        focused: (self.focus == Focus::ManagedNavigation && nav.cursor == i)
                            .then_some(key),
                    }),
                );
                frame.paint(&tree, canvas, root, &mut self.engine);
                continue;
            }
            Actions {
                id: row.id,
                heading: &row.heading,
                body: &row.body,
                labels: &row.labels(),
                focused: (self.focus == Focus::ManagedNavigation && nav.cursor == i).then_some(0),
                disabled: nav.pending.is_some(),
            }
            .draw(canvas, root, &mut self.engine, rect, self.pointer);
        }
    }
    pub(crate) fn managed_navigation_extent(&self) -> f32 {
        self.navigation()
            .rows
            .iter()
            .filter(|row| row.top.is_finite())
            .map(|row| row.top + row.height + CARD_GAP)
            .fold(0.0, f32::max)
    }
    pub(crate) fn managed_navigation_press(&mut self, x: f32, y: f32) -> Option<bool> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        let nav = self
            .managed
            .as_ref()
            .map_or(&self.host_navigation, |state| &state.navigation);
        for (i, row) in nav.rows.iter().enumerate() {
            let rect = row.rect(area, self.conversation_scroll);
            if !rect.contains(x, y) {
                continue;
            }
            if row.compact() {
                if !row.targets.is_empty() || row.routine {
                    let tree = row.compact_tree(&mut self.engine, &self.bot_faces, rect.w);
                    let frame =
                        questions::elements::layout(&tree, rect, koru_draw::el::FrameCx::root());
                    if let Some(url) = reply::link_at(row, &frame, x, y) {
                        return Some(self.follow_link(&url));
                    }
                    if let Some(target) = reply::target_at(row, &frame, x, y).cloned() {
                        self.navigation_mut().open =
                            Some((target.owner, target.owner_name, target.chat));
                        return Some(true);
                    }
                    if row.routine
                        && frame.hit_at(x, y)
                            == Some(koru_draw::el::Key::model(row.id.as_u128(), "bot-thread"))
                    {
                        return Some(self.activate_managed_navigation(i));
                    }
                    if let Some(target) = row
                        .targets
                        .iter()
                        .find(|target| frame.hit_at(x, y) == Some(exchange::target_key(target)))
                    {
                        let target = target.clone();
                        self.navigation_mut().open =
                            Some((target.owner, target.owner_name, target.chat));
                        return Some(true);
                    }
                    return Some(false);
                }
                return Some(self.activate_managed_navigation(i));
            }
            let hit = Actions {
                id: row.id,
                heading: &row.heading,
                body: &row.body,
                labels: &row.labels(),
                focused: None,
                disabled: nav.pending.is_some(),
            }
            .hit(&mut self.engine, rect, x, y);
            return Some(hit.is_some() && self.activate_managed_navigation(i));
        }
        None
    }
    pub(crate) fn focus_managed_navigation(&mut self, last: bool) -> bool {
        let nav = self
            .managed
            .as_mut()
            .map_or(&mut self.host_navigation, |state| &mut state.navigation);
        let positions = nav
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
        nav.cursor = *index;
        nav.target_cursor = 0;
        self.focus = Focus::ManagedNavigation;
        self.composer.set_focused(false);
        self.conversation_scroll = nav.rows[*index].top.max(0.0);
        self.follow_tail = false;
        true
    }
    pub(crate) fn managed_navigation_key(
        &mut self,
        key: &Key,
        mods: ModifiersState,
    ) -> Option<bool> {
        let nav = self
            .managed
            .as_mut()
            .map_or(&mut self.host_navigation, |state| &mut state.navigation);
        if matches!(key, Key::Named(NamedKey::Tab)) && !koru_keys::is_chord(mods) {
            let targets = nav.rows.get(nav.cursor).map_or(0, |row| row.targets.len());
            if !mods.shift_key() && nav.target_cursor + 1 < targets {
                nav.target_cursor += 1;
                return Some(true);
            }
            if mods.shift_key() && nav.target_cursor > 0 {
                nav.target_cursor -= 1;
                return Some(true);
            }
            let next = if mods.shift_key() {
                (0..nav.cursor)
                    .rev()
                    .find(|i| nav.rows[*i].action.is_some())
            } else {
                (nav.cursor + 1..nav.rows.len()).find(|i| nav.rows[*i].action.is_some())
            };
            if let Some(index) = next {
                nav.cursor = index;
                nav.target_cursor = if mods.shift_key() {
                    nav.rows[index].targets.len().saturating_sub(1)
                } else {
                    0
                };
                self.conversation_scroll = nav.rows[index].top.max(0.0);
            } else if mods.shift_key()
                && (self.focus_managed_attention(true)
                    || self.step_managed_question(None, false)
                    || self.focus_last_work())
            {
            } else {
                self.focus = Focus::Composer;
                self.composer.set_focused(true);
            }
            return Some(true);
        }
        if matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space)) && mods.is_empty() {
            let i = nav.cursor;
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
        if let Some(target) = self
            .navigation()
            .rows
            .get(index)
            .and_then(|row| row.targets.get(self.navigation().target_cursor))
            .cloned()
        {
            self.navigation_mut().open = Some((target.owner, target.owner_name, target.chat));
            return true;
        }
        if let Some(Action::Thread(bot, name, chat)) = self
            .navigation()
            .rows
            .get(index)
            .and_then(|row| row.action.clone())
        {
            self.navigation_mut().open = Some((bot, name, chat));
            return true;
        }
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
            Action::Review(bot, occurrence) => return self.open_routine_result(bot, occurrence),
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
