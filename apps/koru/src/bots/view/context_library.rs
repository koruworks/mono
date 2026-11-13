// SPDX-License-Identifier: MPL-2.0

//! Canonical Library links. BotView keeps the only reader and draft; this
//! surface owns just layout and transient interaction in window coordinates.
use super::{BotView, controls::ButtonLeaf};
use koru_ai_core::bots::{BotId, BotLifecycle, BotPage};
use koru_draw::el::{
    El, Frame, FrameCx, Interaction, Key, Role, TextCache, col, leaf, row, scroll_y, text,
};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{Ink, Space, roles};
use koru_i18n::{Key as Words, Lang, t};
use koru_ui::button::ActionStyle;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use winit::keyboard::{Key as InputKey, ModifiersState, NamedKey};

pub(crate) const SURFACE: &str = "bot-context-library";
pub(crate) const BODY: Key = Key::surface(SURFACE, "body");
pub(crate) const RELOAD: Key = Key::surface(SURFACE, "reload");
pub(crate) const MANAGE: Key = Key::surface(SURFACE, "manage");
static NEXT_OCCURRENCE: AtomicU64 = AtomicU64::new(1);
pub(super) fn mint_instance() -> u64 {
    NEXT_OCCURRENCE.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Page {
    pub key: Key,
    pub page: BotPage,
    pub label: String,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Data {
    pub tab: u64,
    pub instance: u64,
    pub revision: u64,
    pub bot: BotId,
    pub lang: Lang,
    pub pages: Vec<Page>,
    pub notices: Vec<String>,
    pub loading: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    Open(BotPage),
    Reload,
    Manage,
    Escape,
}

impl BotView {
    pub(crate) fn context_library_data(&self, tab: u64) -> Option<Data> {
        let bot = self.settings.bot()?;
        if self.create.is_some()
            || self.load == super::Load::Denied
            || bot.lifecycle == BotLifecycle::Trashed
            || self.facts.as_ref().is_some_and(|facts| facts.trashed)
        {
            return None;
        }
        let mut pages = Vec::new();
        if let Some(held) = self.library.held() {
            let mut add = |page: &BotPage, part: &'static str, purpose: Words| {
                pages.push(Page {
                    key: Key::model(page.document.as_uuid().as_u128(), part),
                    page: page.clone(),
                    label: format!("{} · {}", t(self.lang, purpose), self.page_title(page)),
                })
            };
            add(&held.role, "context-role", Words::BotsRole);
            for page in &held.knowledge {
                add(page, "context-knowledge", Words::BotsKnowledge);
            }
            if let Some(page) = &held.memory {
                add(page, "context-memory", Words::BotsMemory);
            }
        }
        let mut notices = Vec::new();
        if let Some(words) = self.library.notice() {
            notices.push(self.words(words));
        }
        if self.library.loading() || (self.library.held().is_none() && notices.is_empty()) {
            notices.push(self.words(Words::BotsLoading));
        }
        if self.library.dirty() || self.library.saving() || self.library.retrying() {
            notices.push(self.words(Words::BotsLibraryDraftInSettings));
        }
        Some(Data {
            tab,
            instance: self.context_instance,
            revision: bot.revision.max(self.library.known_revision()),
            bot: bot.id,
            lang: self.lang,
            pages,
            notices,
            loading: self.library.loading(),
        })
    }
    pub(crate) fn show_context_library(&mut self, visible: bool) -> bool {
        if self.context_library_visible == visible {
            return false;
        }
        self.context_library_visible = visible;
        if visible {
            self.library.owe_read();
        }
        true
    }
    pub(crate) fn perform_context_library(&mut self, action: Action) -> bool {
        let Some(data) = self.context_library_data(0) else {
            return false;
        };
        match action {
            Action::Open(page) => {
                if !data.pages.iter().any(|p| p.page == page) {
                    return false;
                }
                self.role_request = Some(page);
                true
            }
            Action::Reload => self.perform_library(super::library::Action::Reload),
            Action::Manage => {
                self.set_settings_open(true);
                self.focus = Some(super::region::OPEN_ROLE);
                self.reveal_focus();
                true
            }
            Action::Escape => self.release_keyboard(),
        }
    }
}

pub(crate) struct Surface {
    pub data: Option<Data>,
    pub area: Rect,
    pub focus: Option<Key>,
    pub hot: Option<Key>,
    pub pressed: Option<Key>,
    pub scroll: f32,
    pub floating: bool,
    pub drag: Option<f32>,
    cache: RefCell<TextCache>,
    occurrences: HashMap<Key, u64>,
}
impl Default for Surface {
    fn default() -> Self {
        Self {
            data: None,
            area: Rect::new(0.0, 0.0, 0.0, 0.0),
            focus: None,
            hot: None,
            pressed: None,
            scroll: 0.0,
            floating: false,
            drag: None,
            cache: RefCell::default(),
            occurrences: HashMap::new(),
        }
    }
}
impl Surface {
    pub(crate) fn update(&mut self, data: Option<Data>, area: Rect, floating: bool) -> bool {
        let switched = self.data.as_ref().map(|d| (d.tab, d.bot, d.instance))
            != data.as_ref().map(|d| (d.tab, d.bot, d.instance));
        let revised = self.data.as_ref().map(|d| d.revision) != data.as_ref().map(|d| d.revision);
        let changed = self.data != data || self.area != area || self.floating != floating;
        if switched {
            self.focus = None;
            self.hot = None;
            self.pressed = None;
            self.drag = None;
            self.scroll = 0.0;
            self.occurrences.clear();
        }
        if revised {
            self.pressed = None;
            self.drag = None;
            self.hot = None;
            self.occurrences.clear();
        }
        self.data = data;
        self.area = area;
        self.floating = floating;
        let mut keys = self.keys();
        if self.data.is_some() && !keys.contains(&RELOAD) {
            keys.push(RELOAD);
        }
        self.occurrences
            .retain(|key, _| *key == BODY || keys.contains(key));
        for key in std::iter::once(BODY).chain(keys.iter().copied()) {
            self.occurrences
                .entry(key)
                .or_insert_with(|| NEXT_OCCURRENCE.fetch_add(1, Ordering::Relaxed));
        }
        if self.focus.is_some_and(|key| !keys.contains(&key)) {
            self.focus = None;
        }
        if self.pressed.is_some_and(|key| !keys.contains(&key)) {
            self.pressed = None;
        }
        changed
    }
    pub(crate) fn occurrence(&self, key: Key) -> Option<u64> {
        self.occurrences.get(&key).copied()
    }
    pub(crate) fn key_for_occurrence(&self, id: u64) -> Option<Key> {
        self.occurrences
            .iter()
            .find_map(|(key, found)| (*found == id).then_some(*key))
    }
    pub(crate) fn keys(&self) -> Vec<Key> {
        let Some(data) = &self.data else {
            return Vec::new();
        };
        data.pages
            .iter()
            .map(|p| p.key)
            .chain((!data.loading).then_some(RELOAD))
            .chain([MANAGE])
            .collect()
    }
    pub(crate) fn action(&self, key: Key) -> Option<Action> {
        let data = self.data.as_ref()?;
        match key {
            RELOAD if !data.loading => Some(Action::Reload),
            MANAGE => Some(Action::Manage),
            _ => data
                .pages
                .iter()
                .find(|p| p.key == key)
                .map(|p| Action::Open(p.page.clone())),
        }
    }
    fn button(&self, engine: &mut TextEngine, key: Key, label: String, disabled: bool) -> El {
        leaf(
            "Context Library action",
            ButtonLeaf::new(
                engine,
                label.clone(),
                ActionStyle::Ghost,
                disabled,
                self.focus == Some(key),
            ),
        )
        .semantic()
        .id(key)
        .hit()
        .focusable()
        .disabled(disabled)
        .semantics(Role::Button)
        .name_text(label)
        .fill()
    }
    pub(crate) fn layout(&self, engine: &mut TextEngine) -> Option<(El, Frame)> {
        let data = self.data.as_ref()?;
        let mut content = col("Context Library content")
            .pad(Space::S3)
            .gap(Space::S2)
            .child(
                text(t(data.lang, Words::BotsLibrary))
                    .named("Library heading")
                    .role(roles::UI),
            );
        for notice in &data.notices {
            content = content.child(
                text(notice.clone())
                    .named("Library notice")
                    .role(roles::META)
                    .ink(Ink::Secondary)
                    .wrap(),
            );
        }
        for page in &data.pages {
            content = content.child(
                row("Context Library page")
                    .gap(Space::S2)
                    .items_center()
                    .child(
                        text(page.label.clone())
                            .named("Library page title")
                            .role(roles::ROW)
                            .fill()
                            .truncate(),
                    )
                    .child(
                        leaf(
                            "Open Library page",
                            super::controls::IconLeaf {
                                glyph: koru_draw::icons::POP_OUT,
                                disabled: false,
                            },
                        )
                        .semantic()
                        .id(page.key)
                        .hit()
                        .focusable()
                        .states(&super::actions::ICON_STATES)
                        .semantics(Role::Button)
                        .name_text(page.label.clone()),
                    ),
            );
        }
        content = content
            .child(row("Context Library refresh").child(self.button(
                engine,
                RELOAD,
                t(data.lang, Words::BotsRefreshLibrary).into(),
                data.loading,
            )))
            .child(row("Context Library management").child(self.button(
                engine,
                MANAGE,
                t(data.lang, Words::BotsManageLibrary).into(),
                false,
            )));
        let tree = scroll_y("Bot context Library")
            .id(BODY)
            .offset(self.scroll)
            .scrollbar()
            .semantics(Role::Group)
            .name(Words::BotsLibrary)
            .child(content);
        let frame = tree.layout(
            engine,
            &mut self.cache.borrow_mut(),
            self.area,
            FrameCx::root().with_interaction(Interaction {
                hot: self.hot,
                pressed: self.pressed,
                focused: self.focus,
            }),
        );
        Some((tree, frame))
    }
    pub(crate) fn with_frame<R>(&self, f: impl FnOnce(&Frame) -> R) -> Option<R> {
        crate::input_engine::with(|engine| self.layout(engine).map(|(_, frame)| f(&frame)))
    }
    pub(crate) fn hit(&self, x: f32, y: f32) -> Option<Key> {
        self.with_frame(|frame| frame.hit_at(x, y)).flatten()
    }
    pub(crate) fn press_scrollbar(&mut self, x: f32, y: f32) -> bool {
        let Some(bar) = self.with_frame(|frame| frame.scrollbar(BODY)).flatten() else {
            return false;
        };
        if !bar.in_gutter(x, y) {
            return false;
        }
        let grab = if bar.on_thumb(x, y) {
            y - bar.thumb_top
        } else {
            bar.thumb_h / 2.0
        };
        self.drag = Some(grab);
        self.scroll = bar.scroll_for_pointer(y, grab);
        true
    }
    pub(crate) fn drag_to(&mut self, y: f32) -> bool {
        let Some(grab) = self.drag else {
            return false;
        };
        if let Some(scroll) = self
            .with_frame(|frame| frame.drag_offset(BODY, y, grab))
            .flatten()
        {
            self.scroll = scroll;
        }
        true
    }
    pub(crate) fn scroll_by(&mut self, dy: f32) {
        let max = self
            .with_frame(|frame| {
                frame
                    .node(BODY)
                    .and_then(|n| n.scroll)
                    .zip(frame.node(BODY).and_then(|n| n.viewport))
                    .map_or(0.0, |((extent, _), viewport)| {
                        (extent - viewport.h).max(0.0)
                    })
            })
            .unwrap_or(0.0);
        self.scroll = (self.scroll + dy).clamp(0.0, max);
    }
    pub(crate) fn focus_first(&mut self) -> bool {
        self.focus = self.keys().first().copied();
        self.reveal();
        self.focus.is_some()
    }
    pub(crate) fn reveal(&mut self) {
        let Some(key) = self.focus else {
            return;
        };
        let delta = self
            .with_frame(|frame| {
                let rect = frame.node(key)?.rect;
                let viewport = frame.node(BODY)?.viewport?;
                Some(if rect.y < viewport.y {
                    rect.y - viewport.y
                } else {
                    (rect.y + rect.h - viewport.y - viewport.h).max(0.0)
                })
            })
            .flatten()
            .unwrap_or(0.0);
        self.scroll_by(delta);
    }
    pub(crate) fn key(&mut self, key: &InputKey, mods: ModifiersState) -> Option<Option<Action>> {
        self.focus?;
        if mods.control_key() || mods.alt_key() || mods.super_key() {
            return None;
        }
        if mods.shift_key() && !matches!(key, InputKey::Named(NamedKey::Tab)) {
            return None;
        }
        match key {
            InputKey::Named(NamedKey::Escape) => Some(Some(Action::Escape)),
            InputKey::Named(NamedKey::Enter | NamedKey::Space) => {
                Some(self.focus.and_then(|k| self.action(k)))
            }
            InputKey::Named(
                NamedKey::Tab
                | NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown,
            ) => {
                let keys = self.keys();
                if keys.is_empty() {
                    return Some(None);
                }
                let back = mods.shift_key()
                    || matches!(
                        key,
                        InputKey::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp)
                    );
                let at = self
                    .focus
                    .and_then(|k| keys.iter().position(|v| *v == k))
                    .unwrap_or(0);
                self.focus = Some(keys[(at + if back { keys.len() - 1 } else { 1 }) % keys.len()]);
                self.reveal();
                Some(None)
            }
            _ => None,
        }
    }
}
