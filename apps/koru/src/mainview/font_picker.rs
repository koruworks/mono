// SPDX-License-Identifier: MPL-2.0

//! The **installed font** picker: choose any font installed on this machine,
//! for one agent's terminals or for one of the app's font roles.
//!
//! A terminal draws a fixed grid, so for a terminal — and for the *Code* role,
//! which sets code blocks — the list opens on the monospaced families until
//! the person asks for every font (*Tab*, or the footer toggle). The other
//! roles open on every family. The first row puts the target back on its
//! default. Typing filters by name; *Enter* or a click chooses.
//!
//! For a font role, the lit row previews on the whole app through the shared
//! preview guard, as Settings' own lists do; a terminal's choice is per agent
//! and is not previewed. The list of installed families is read without
//! waiting: until the scan has finished the picker says it is looking, and
//! fills in on its own when the list is ready.
//!
//! The surface holds no preferences: it answers with an [`Action`] and the
//! container files the choice (`terminal_prefs`, or the font role's setting).

use koru_draw::geometry::Rect;
use koru_draw::installed_fonts::InstalledFont;
use koru_draw::text::TextEngine;
use koru_draw::theme::preview::{Preview, PreviewSlot};
use koru_draw::{icons, scene, theme};
use koru_i18n::{Key as I18n, Lang, t};
use koru_ui::selectable_list::SelectableList;
use koru_ui::text_input::{InputStyle, TextInput};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key, ModifiersState, NamedKey};

pub const SURFACE_RADIUS: f32 = 8.0;

const HEADER_H: f32 = 48.0;
const FOOTER_H: f32 = 34.0;
const ROW_H: f32 = 30.0;
const PAD: f32 = 14.0;

pub enum Action {
    Keep,
    Cancel,
    /// Back to the target's default.
    UseDefault,
    /// This installed family.
    Choose(String),
    /// The lit row moved: a family, the default row, or nothing at all.
    Lit(Lit),
}

/// What the lit row stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lit {
    Default,
    Family(String),
    Nothing,
}

/// One row: the default, or an installed family.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    /// `None` is the default.
    family: Option<String>,
    /// The target draws with it now.
    current: bool,
}

pub struct FontPickerSurface {
    /// `None` until the scan has finished.
    fonts: Option<&'static [InstalledFont]>,
    /// The installed family in force, when one is.
    current: Option<String>,
    show_all: bool,
    /// Whether moving the lit row answers [`Action::Lit`].
    previews: bool,
    /// The first row's words: *Default (Settings)*, *Default (Inter)*.
    default_label: String,
    lang: Lang,
    query: TextInput,
    list: SelectableList,
    viewport: (f32, f32),
}

impl FontPickerSurface {
    /// The terminal's picker: monospaced first, the agent back on Settings'
    /// font as its first row, nothing previewed.
    pub fn new(
        engine: &mut TextEngine,
        fonts: &'static [InstalledFont],
        current: Option<String>,
    ) -> Self {
        let lang = crate::format::lang();
        let default_label =
            t(lang, I18n::FontPickerDefaultFace).replace("{}", t(lang, I18n::SettingsTab));
        Self::with(
            engine,
            Some(fonts),
            current,
            false,
            false,
            default_label,
            lang,
        )
    }

    /// A font role's picker. `fonts` is `None` while the scan runs.
    pub fn for_role(
        engine: &mut TextEngine,
        fonts: Option<&'static [InstalledFont]>,
        current: Option<String>,
        monospaced_first: bool,
        default_face: &str,
    ) -> Self {
        let lang = crate::format::lang();
        let default_label = t(lang, I18n::FontPickerDefaultFace).replace("{}", default_face);
        Self::with(
            engine,
            fonts,
            current,
            !monospaced_first,
            true,
            default_label,
            lang,
        )
    }

    fn with(
        engine: &mut TextEngine,
        fonts: Option<&'static [InstalledFont]>,
        current: Option<String>,
        show_all: bool,
        previews: bool,
        default_label: String,
        lang: Lang,
    ) -> Self {
        let mut surface = Self {
            fonts,
            current,
            show_all,
            previews,
            default_label,
            lang,
            query: TextInput::new(engine, theme::role::row()),
            list: SelectableList::new(ROW_H),
            viewport: (1.0, 1.0),
        };
        surface.light_current();
        surface
    }

    /// Open on the family in force, so the list does not start at the top
    /// of a few hundred.
    fn light_current(&mut self) {
        if let Some(at) = self.rows().iter().position(|row| row.current) {
            self.list.selected = at;
        }
    }

    /// The search field, for a dictation into it.
    pub(super) fn dictation_field(&mut self) -> &mut TextInput {
        &mut self.query
    }

    /// Whether the list of installed families has arrived.
    pub fn has_fonts(&self) -> bool {
        self.fonts.is_some()
    }

    /// The scan finished: show its families, keeping what was typed, the
    /// filter, and the lit family if it is still listed.
    pub fn set_fonts(&mut self, fonts: &'static [InstalledFont]) {
        let lit = self.lit();
        self.fonts = Some(fonts);
        let rows = self.rows();
        let keep = match &lit {
            Lit::Family(family) => rows
                .iter()
                .position(|row| row.family.as_deref() == Some(family.as_str())),
            Lit::Default | Lit::Nothing => None,
        };
        match keep {
            Some(at) => self.list.selected = at,
            None if lit == Lit::Nothing || lit == Lit::Default => self.light_current(),
            None => {}
        }
        let len = rows.len();
        self.list.ensure_visible(len, self.list_height());
    }

    pub fn set_viewport(&mut self, w: f32, h: f32) {
        self.viewport = (w.max(1.0), h.max(1.0));
        let len = self.rows().len();
        self.list.ensure_visible(len, self.list_height());
    }

    fn list_height(&self) -> f32 {
        (self.viewport.1 - HEADER_H - FOOTER_H).max(0.0)
    }

    fn query_rect(&self) -> Rect {
        Rect::new(PAD, 10.0, (self.viewport.0 - PAD * 2.0).max(1.0), 28.0)
    }

    fn toggle_rect(&self) -> Rect {
        Rect::new(
            PAD,
            self.viewport.1 - FOOTER_H,
            (self.viewport.0 * 0.5).max(1.0),
            FOOTER_H,
        )
    }

    /// The rows now: the default first (while nothing is typed), then every
    /// family that passes the filter and the monospace rule.
    fn rows(&self) -> Vec<Row> {
        let needle = self.query.raw_text().trim().to_lowercase();
        let mut rows = Vec::new();
        if needle.is_empty() {
            rows.push(Row {
                family: None,
                current: self.current.is_none(),
            });
        }
        rows.extend(
            self.fonts
                .unwrap_or_default()
                .iter()
                .filter(|font| self.show_all || font.monospace)
                .filter(|font| needle.is_empty() || font.family.to_lowercase().contains(&needle))
                .map(|font| Row {
                    family: Some(font.family.clone()),
                    current: self.current.as_deref() == Some(font.family.as_str()),
                }),
        );
        rows
    }

    /// What the lit row stands for.
    pub fn lit(&self) -> Lit {
        match self.rows().get(self.list.selected) {
            Some(Row {
                family: Some(family),
                ..
            }) => Lit::Family(family.clone()),
            Some(Row { family: None, .. }) => Lit::Default,
            None => Lit::Nothing,
        }
    }

    fn choose(&self, row: Option<&Row>) -> Action {
        match row {
            Some(Row {
                family: Some(family),
                ..
            }) => Action::Choose(family.clone()),
            Some(Row { family: None, .. }) => Action::UseDefault,
            None => Action::Keep,
        }
    }

    fn refilter(&mut self) {
        self.list.selected = 0;
        self.list.scroll = 0.0;
    }

    /// After a step that may have moved the light: [`Action::Lit`] when this
    /// picker previews and the light moved, otherwise [`Action::Keep`].
    fn moved(&self, before: Lit) -> Action {
        let now = self.lit();
        if self.previews && now != before {
            Action::Lit(now)
        } else {
            Action::Keep
        }
    }

    pub fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Action {
        let list_h = self.list_height();
        let before = self.lit();
        match key {
            Key::Named(NamedKey::Escape) => return Action::Cancel,
            Key::Named(NamedKey::ArrowDown) => {
                let len = self.rows().len();
                self.list.step(1, len, list_h);
                return self.moved(before);
            }
            Key::Named(NamedKey::ArrowUp) => {
                let len = self.rows().len();
                self.list.step(-1, len, list_h);
                return self.moved(before);
            }
            Key::Named(NamedKey::PageDown) => {
                let len = self.rows().len();
                self.list.step(10, len, list_h);
                return self.moved(before);
            }
            Key::Named(NamedKey::PageUp) => {
                let len = self.rows().len();
                self.list.step(-10, len, list_h);
                return self.moved(before);
            }
            Key::Named(NamedKey::Enter) => {
                let rows = self.rows();
                return self.choose(rows.get(self.list.selected));
            }
            Key::Named(NamedKey::Tab) => {
                self.show_all = !self.show_all;
                self.refilter();
                return self.moved(before);
            }
            _ => {}
        }
        let typed = self.query.raw_text().to_owned();
        self.query.key(engine, key, text, mods);
        if self.query.raw_text() != typed {
            self.refilter();
        }
        self.moved(before)
    }

    pub fn mouse_down(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> Action {
        let query = self.query_rect();
        if self.query.click(engine, query, x, y) {
            return Action::Keep;
        }
        if self.toggle_rect().contains(x, y) {
            let before = self.lit();
            self.show_all = !self.show_all;
            self.refilter();
            return self.moved(before);
        }
        let rows = self.rows();
        if y < HEADER_H || y > HEADER_H + self.list_height() {
            return Action::Keep;
        }
        match self.list.row_at(HEADER_H, y, rows.len()) {
            Some(index) => {
                self.list.selected = index;
                self.choose(rows.get(index))
            }
            None => Action::Keep,
        }
    }

    pub fn scroll_by(&mut self, delta: f32) {
        let len = self.rows().len();
        self.list.scroll_by(delta, len, self.list_height());
    }

    /// The rows a client can see, in surface coordinates: each row's label,
    /// whether it is in force, whether it is lit, and where it is drawn.
    fn visible_rows(&self) -> Vec<(usize, String, bool, bool, Rect)> {
        let rows = self.rows();
        let list_h = self.list_height();
        let first = (self.list.scroll / ROW_H).floor().max(0.0) as usize;
        let last = (first + (list_h / ROW_H).ceil() as usize + 2).min(rows.len());
        (first..last)
            .filter_map(|index| {
                let y = self.list.row_y(HEADER_H, index);
                if y + ROW_H < HEADER_H || y > HEADER_H + list_h {
                    return None;
                }
                let row = &rows[index];
                let label = row
                    .family
                    .clone()
                    .unwrap_or_else(|| self.default_label.clone());
                Some((
                    index,
                    label,
                    row.current,
                    index == self.list.selected,
                    Rect::new(PAD * 0.5, y, (self.viewport.0 - PAD).max(1.0), ROW_H),
                ))
            })
            .collect()
    }

    pub fn draw(&mut self, scene: &mut Scene, engine: &mut TextEngine, root: Affine) {
        let p = theme::p();
        let (w, h) = self.viewport;
        let lang = self.lang;

        self.query.draw(
            scene,
            root,
            engine,
            self.query_rect(),
            InputStyle::search(),
            Some(t(lang, I18n::FontPickerSearch)),
            true,
        );

        for (index, name, current, lit, rect) in self.visible_rows() {
            let y = rect.y;
            if lit {
                scene::fill(
                    scene,
                    root,
                    p.row_selected,
                    &scene::rrect(rect.x, y, rect.w, ROW_H, 4.0),
                );
            }
            let is_default = self
                .rows()
                .get(index)
                .is_some_and(|row| row.family.is_none());
            let label = if is_default {
                engine.simple_layout(&name, theme::role::row())
            } else {
                // **Each family set in its own face**, so the list shows what
                // choosing it looks like. The sans tail behind it draws what the
                // family lacks; nothing is interned for a row merely shown.
                engine.simple_layout_in(
                    &name,
                    theme::role::row(),
                    &format!("{name}, system-ui, sans-serif"),
                )
            };
            let colour = if is_default { p.text_dim } else { p.text };
            scene::draw_layout_first_line(scene, root, &label, PAD + 4.0, y + 6.0, Some(colour));
            if current {
                let tick = engine.icon_layout(icons::CHECK, 14.0);
                scene::draw_layout(
                    scene,
                    root,
                    &tick,
                    w - PAD - tick.width() - 2.0,
                    y + (ROW_H - tick.height()) * 0.5,
                    Some(p.text_dim),
                );
            }
        }
        let status = if self.fonts.is_none() {
            Some(I18n::FontPickerLooking)
        } else if self.rows().is_empty() {
            Some(I18n::FontPickerNoMatch)
        } else {
            None
        };
        if let Some(status) = status {
            let y = HEADER_H + 8.0 + if self.rows().is_empty() { 0.0 } else { ROW_H };
            let none = engine.simple_layout(t(lang, status), theme::role::row());
            scene::draw_layout_first_line(scene, root, &none, PAD + 4.0, y, Some(p.text_dim));
        }

        // Footer: the toggle on the left, the keys on the right.
        let y = h - FOOTER_H + 7.0;
        let glyph = engine.icon_layout(
            if self.show_all {
                icons::CHECK_SQUARE
            } else {
                icons::SQUARE
            },
            14.0,
        );
        scene::draw_layout_first_line(scene, root, &glyph, PAD, y + 1.0, Some(p.text_dim));
        let toggle = engine.simple_layout(
            &t(lang, I18n::FontPickerShowAll).replace("{}", "Tab"),
            theme::role::hint(),
        );
        scene::draw_layout_first_line(
            scene,
            root,
            &toggle,
            PAD + glyph.width() + 6.0,
            y,
            Some(p.text_dim),
        );
        let hint = engine.simple_layout(
            &t(lang, I18n::FontPickerEnterToChoose).replace("{}", "Enter"),
            theme::role::hint(),
        );
        scene::draw_layout_first_line(
            scene,
            root,
            &hint,
            (w - PAD - hint.width()).max(PAD),
            y,
            Some(p.text_dim),
        );
    }
}

const MODAL_W: f32 = 460.0;
const MODAL_H: f32 = 440.0;
const MODAL_MARGIN: f32 = 16.0;

/// Whose font the picker is choosing.
pub(super) enum Target {
    /// One agent's terminals.
    Terminal(koru_session_protocol::SessionProfileKey),
    /// One of the app's font roles, from Settings' *More fonts…*.
    Role(koru_settings_ui::FontRole),
}

/// The picker, whose font it is choosing, and what it is previewing.
pub(super) struct Open {
    pub(super) surface: FontPickerSurface,
    target: Target,
    /// The lit family on the whole app, for a role. Dropped with the picker,
    /// which is how every way of closing it puts the chosen font back.
    preview: Option<Preview>,
}

impl super::MainView {
    pub(super) fn font_picker_bounds(&self) -> Rect {
        let w = MODAL_W.min((self.viewport.0 - MODAL_MARGIN * 2.0).max(1.0));
        let h = MODAL_H.min((self.viewport.1 - MODAL_MARGIN * 2.0).max(1.0));
        Rect::new(
            (self.viewport.0 - w) * 0.5,
            (self.viewport.1 - h) * 0.5,
            w,
            h,
        )
    }

    /// *More fonts...* on a terminal tab's font flyout.
    pub(super) fn open_font_picker(&mut self, tab: usize) -> bool {
        if self.font_picker.is_some() {
            return true;
        }
        let Some(profile) = self.session_profile_of_tab(tab) else {
            return false;
        };
        let current = crate::terminal_prefs::agent_look(&profile)
            .1
            .and_then(|token| crate::terminal_prefs::family_of_token(&token).map(str::to_owned));
        koru_draw::installed_fonts::warm();
        let mut surface = match koru_draw::installed_fonts::ready() {
            Some(fonts) => FontPickerSurface::new(&mut self.engine, fonts, current),
            None => {
                let mut surface = FontPickerSurface::new(&mut self.engine, &[], current);
                surface.fonts = None;
                surface
            }
        };
        let bounds = self.font_picker_bounds();
        surface.set_viewport(bounds.w, bounds.h);
        self.font_picker = Some(Open {
            surface,
            target: Target::Terminal(profile),
            preview: None,
        });
        self.hover = None;
        true
    }

    /// *More fonts…* at the end of a Settings font list.
    pub(super) fn open_role_font_picker(&mut self, role: koru_settings_ui::FontRole) -> bool {
        if self.font_picker.is_some() {
            return true;
        }
        let slot = role.slot();
        let current = theme::font_setting(slot)
            .installed()
            .map(|name| name.as_str().to_owned());
        let default_face = theme::resolve_font(slot, &koru_settings::FontSetting::FollowDefault);
        koru_draw::installed_fonts::warm();
        let mut surface = FontPickerSurface::for_role(
            &mut self.engine,
            koru_draw::installed_fonts::ready(),
            current,
            role == koru_settings_ui::FontRole::Code,
            default_face.label(),
        );
        let bounds = self.font_picker_bounds();
        surface.set_viewport(bounds.w, bounds.h);
        self.font_picker = Some(Open {
            surface,
            target: Target::Role(role),
            preview: None,
        });
        self.hover = None;
        true
    }

    /// While the picker waits for the scan: hand it the list once it is
    /// ready. Answers whether it did, so the frame repaints.
    pub(super) fn font_picker_fill(&mut self) -> bool {
        let Some(open) = self.font_picker.as_mut() else {
            return false;
        };
        if open.surface.has_fonts() {
            return false;
        }
        let Some(fonts) = koru_draw::installed_fonts::ready() else {
            return false;
        };
        open.surface.set_fonts(fonts);
        true
    }

    /// Whether the picker is waiting for the scan, so maintenance comes back.
    pub(super) fn font_picker_waiting(&self) -> bool {
        self.font_picker
            .as_ref()
            .is_some_and(|open| !open.surface.has_fonts())
    }

    fn apply_font_picker_action(&mut self, action: Action) -> bool {
        match action {
            Action::Keep => true,
            Action::Cancel => {
                self.font_picker = None;
                true
            }
            Action::Lit(lit) => {
                if let Some(open) = self.font_picker.as_mut()
                    && let Target::Role(role) = open.target
                {
                    preview_lit(open, role, &lit);
                }
                true
            }
            Action::UseDefault | Action::Choose(_) => {
                let Some(open) = self.font_picker.take() else {
                    return false;
                };
                match open.target {
                    Target::Terminal(profile) => {
                        let token = match &action {
                            Action::Choose(family) => {
                                Some(crate::terminal_prefs::family_token(family))
                            }
                            _ => None,
                        };
                        crate::terminal_prefs::set_agent_look(
                            &profile,
                            None,
                            Some(token.as_deref()),
                        );
                    }
                    Target::Role(role) => {
                        let setting = match &action {
                            Action::Choose(family) => koru_settings::FamilyName::new(family)
                                .map_or(
                                    koru_settings::FontSetting::FollowDefault,
                                    koru_settings::FontSetting::Installed,
                                ),
                            _ => koru_settings::FontSetting::FollowDefault,
                        };
                        // The choice before the preview ends, so the app goes
                        // straight from the preview to the choice.
                        theme::set_font_setting(role.slot(), setting);
                        drop(open);
                        // The lists tick the new choice even when nothing drawn
                        // moved (the preview already showed it).
                        <Self as koru_workspace_host::View>::invalidate(self);
                    }
                }
                true
            }
        }
    }

    pub(super) fn font_picker_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        let Some(open) = self.font_picker.as_mut() else {
            return false;
        };
        let action = open.surface.key(&mut self.engine, key, text, mods);
        self.apply_font_picker_action(action)
    }

    pub(super) fn font_picker_mouse_down(&mut self, x: f32, y: f32) -> bool {
        let bounds = self.font_picker_bounds();
        if !bounds.contains(x, y) {
            self.font_picker = None;
            return true;
        }
        let Some(open) = self.font_picker.as_mut() else {
            return false;
        };
        let action = open
            .surface
            .mouse_down(&mut self.engine, x - bounds.x, y - bounds.y);
        self.apply_font_picker_action(action)
    }

    pub(super) fn font_picker_scroll(&mut self, delta: f32) {
        if let Some(open) = self.font_picker.as_mut() {
            open.surface.scroll_by(delta);
        }
    }

    pub(super) fn draw_font_picker(&mut self, scene: &mut Scene, root: Affine) {
        let bounds = self.font_picker_bounds();
        let Some(open) = self.font_picker.as_mut() else {
            return;
        };
        scene::fill(
            scene,
            root,
            theme::p().scrim,
            &scene::rect(0.0, 0.0, self.viewport.0, self.viewport.1),
        );
        scene::floating_panel(
            scene,
            root,
            bounds.x,
            bounds.y,
            bounds.w,
            bounds.h,
            &scene::Panel::popover(SURFACE_RADIUS),
        );
        let modal_root = root * Affine::translate((f64::from(bounds.x), f64::from(bounds.y)));
        scene.push_clip_layer(
            vello::peniko::Fill::NonZero,
            modal_root,
            &scene::rrect(0.0, 0.0, bounds.w, bounds.h, SURFACE_RADIUS),
        );
        open.surface.draw(scene, &mut self.engine, modal_root);
        scene.pop_layer();
        scene::stroke(
            scene,
            root,
            theme::p().panel_border,
            theme::BORDER_W,
            &scene::rrect(bounds.x, bounds.y, bounds.w, bounds.h, SURFACE_RADIUS),
        );
    }

    /// The picker as a client hears it: a dialog named for what it does, its
    /// search field, the families on screen with the one in force marked and
    /// the lit one focused, and the show-all toggle. Bounds are window
    /// coordinates, from the same layout the picker draws and hit-tests with.
    pub(super) fn font_picker_semantics(
        &self,
        cx: &super::accessibility::Shell,
    ) -> Option<koru_workspace_host::accessibility::SemanticNode> {
        use accesskit::Role;
        use koru_workspace_host::accessibility::{ControlPart, SemanticNode};
        let open = self.font_picker.as_ref()?;
        let bounds = self.font_picker_bounds();
        let at = |r: Rect| Rect::new(bounds.x + r.x, bounds.y + r.y, r.w, r.h);
        let lang = open.surface.lang;
        let rows = open.surface.visible_rows();
        let total = open.surface.rows().len();
        let mut list: Vec<SemanticNode> = rows
            .into_iter()
            .map(|(index, label, current, lit, rect)| {
                SemanticNode::new(
                    cx.instance(
                        super::accessibility::FONT_PICKER,
                        index as u64,
                        ControlPart::new("font"),
                    ),
                    Role::ListBoxOption,
                )
                .with_label(label)
                .with_bounds(at(rect))
                .with_selected(current || lit)
                .with_position(index)
                .with_size_of_set(total)
            })
            .collect();
        if total == 0 {
            let status = if open.surface.has_fonts() {
                I18n::FontPickerNoMatch
            } else {
                I18n::FontPickerLooking
            };
            list.push(
                SemanticNode::new(
                    cx.surface(
                        super::accessibility::FONT_PICKER,
                        ControlPart::new("font-status"),
                    ),
                    Role::Status,
                )
                .with_label(t(lang, status)),
            );
        }
        let search = SemanticNode::new(
            cx.surface(
                super::accessibility::FONT_PICKER,
                ControlPart::new("font-search"),
            ),
            Role::SearchInput,
        )
        .with_label(t(lang, I18n::FontPickerSearch))
        .with_value(open.surface.query.raw_text().to_owned())
        .with_bounds(at(open.surface.query_rect()));
        let toggle = SemanticNode::new(
            cx.surface(
                super::accessibility::FONT_PICKER,
                ControlPart::new("font-all"),
            ),
            Role::CheckBox,
        )
        .with_label(t(lang, I18n::FontPickerShowAll).replace("{}", "Tab"))
        .with_toggled(open.surface.show_all)
        .with_bounds(at(open.surface.toggle_rect()));
        let listbox = SemanticNode::new(
            cx.surface(
                super::accessibility::FONT_PICKER,
                ControlPart::new("font-list"),
            ),
            Role::ListBox,
        )
        .with_label(t(lang, I18n::FontPickerTitle))
        .with_children(list);
        Some(
            SemanticNode::new(
                cx.surface(
                    super::accessibility::FONT_PICKER,
                    ControlPart::new("font-picker"),
                ),
                Role::Dialog,
            )
            .with_label(t(lang, I18n::FontPickerTitle))
            .with_bounds(bounds)
            .modal()
            .with_children(vec![search, listbox, toggle]),
        )
    }

    /// Where the keyboard is while the picker is up: the lit family, or the
    /// search field when nothing is lit.
    pub(super) fn font_picker_focus(
        &self,
        cx: &super::accessibility::Shell,
    ) -> Option<koru_workspace_host::accessibility::Occurrence> {
        use koru_workspace_host::accessibility::ControlPart;
        let open = self.font_picker.as_ref()?;
        Some(if open.surface.lit() == Lit::Nothing {
            cx.surface(
                super::accessibility::FONT_PICKER,
                ControlPart::new("font-search"),
            )
        } else {
            cx.instance(
                super::accessibility::FONT_PICKER,
                open.surface.list.selected as u64,
                ControlPart::new("font"),
            )
        })
    }
}

/// Show the lit row on the whole app: an installed family installed here, or
/// the role's default; nothing lit leaves the last preview as it was.
fn preview_lit(open: &mut Open, role: koru_settings_ui::FontRole, lit: &Lit) {
    let slot = role.slot();
    let face = match lit {
        Lit::Family(family) => match koru_draw::installed_fonts::resolve(family) {
            Some(id) => theme::FontChoice::Installed(id),
            None => return,
        },
        Lit::Default => theme::resolve_font(slot, &koru_settings::FontSetting::FollowDefault),
        Lit::Nothing => return,
    };
    let value = theme::font_preview_value(face);
    match open.preview.as_mut() {
        Some(preview) => preview.set(value),
        None => open.preview = Some(Preview::new(PreviewSlot::Font(slot), value)),
    }
}

#[cfg(test)]
mod tests;
