// SPDX-License-Identifier: MPL-2.0

//! The **terminal font** picker: choose any font installed on this machine for
//! one agent's terminals.
//!
//! The terminal draws a fixed grid, so the list shows the monospaced families
//! unless the person asks for every font (*Tab*, or the footer toggle). The
//! first row puts the agent back on the Settings font. Typing filters by name;
//! *Enter* or a click chooses.
//!
//! The surface holds no preferences: it answers with an [`Action`] and the
//! container files the choice (`terminal_prefs`).

use koru_draw::geometry::Rect;
use koru_draw::installed_fonts::InstalledFont;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
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
    /// Back to the Settings page's font.
    UseDefault,
    /// This installed family.
    Choose(String),
}

/// One row: the Settings default, or an installed family.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    /// `None` is the Settings default.
    family: Option<String>,
    /// The agent draws with it now.
    current: bool,
}

pub struct FontPickerSurface {
    fonts: &'static [InstalledFont],
    /// The installed family the agent uses now, when it uses one.
    current: Option<String>,
    show_all: bool,
    query: TextInput,
    list: SelectableList,
    viewport: (f32, f32),
}

impl FontPickerSurface {
    pub fn new(
        engine: &mut TextEngine,
        fonts: &'static [InstalledFont],
        current: Option<String>,
    ) -> Self {
        let mut surface = Self {
            fonts,
            current,
            show_all: false,
            query: TextInput::new(engine, theme::role::row()),
            list: SelectableList::new(ROW_H),
            viewport: (1.0, 1.0),
        };
        // Open on the family in force, so the list does not start at the top
        // of a few hundred.
        let at = surface.rows().iter().position(|row| row.current);
        if let Some(at) = at {
            surface.list.selected = at;
        }
        surface
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

    pub fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Action {
        let list_h = self.list_height();
        match key {
            Key::Named(NamedKey::Escape) => return Action::Cancel,
            Key::Named(NamedKey::ArrowDown) => {
                let len = self.rows().len();
                self.list.step(1, len, list_h);
                return Action::Keep;
            }
            Key::Named(NamedKey::ArrowUp) => {
                let len = self.rows().len();
                self.list.step(-1, len, list_h);
                return Action::Keep;
            }
            Key::Named(NamedKey::PageDown) => {
                let len = self.rows().len();
                self.list.step(10, len, list_h);
                return Action::Keep;
            }
            Key::Named(NamedKey::PageUp) => {
                let len = self.rows().len();
                self.list.step(-10, len, list_h);
                return Action::Keep;
            }
            Key::Named(NamedKey::Enter) => {
                let rows = self.rows();
                return self.choose(rows.get(self.list.selected));
            }
            Key::Named(NamedKey::Tab) => {
                self.show_all = !self.show_all;
                self.refilter();
                return Action::Keep;
            }
            _ => {}
        }
        let before = self.query.raw_text().to_owned();
        self.query.key(engine, key, text, mods);
        if self.query.raw_text() != before {
            self.refilter();
        }
        Action::Keep
    }

    pub fn mouse_down(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> Action {
        let query = self.query_rect();
        if self.query.click(engine, query, x, y) {
            return Action::Keep;
        }
        if self.toggle_rect().contains(x, y) {
            self.show_all = !self.show_all;
            self.refilter();
            return Action::Keep;
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

    pub fn draw(&mut self, scene: &mut Scene, engine: &mut TextEngine, root: Affine) {
        let p = theme::p();
        let (w, h) = self.viewport;

        self.query.draw(
            scene,
            root,
            engine,
            self.query_rect(),
            InputStyle::search(),
            Some("Search fonts\u{2026}"),
            true,
        );

        let rows = self.rows();
        let list_h = self.list_height();
        let first = (self.list.scroll / ROW_H).floor().max(0.0) as usize;
        let last = (first + (list_h / ROW_H).ceil() as usize + 2).min(rows.len());
        for index in first..last {
            let y = self.list.row_y(HEADER_H, index);
            if y + ROW_H < HEADER_H || y > HEADER_H + list_h {
                continue;
            }
            if index == self.list.selected {
                scene::fill(
                    scene,
                    root,
                    p.row_selected,
                    &scene::rrect(PAD * 0.5, y, (w - PAD).max(1.0), ROW_H, 4.0),
                );
            }
            let row = &rows[index];
            let name = row.family.as_deref().unwrap_or("Default (Settings)");
            let colour = if row.family.is_none() {
                p.text_dim
            } else {
                p.text
            };
            let label = engine.simple_layout(name, theme::role::row());
            scene::draw_layout_first_line(scene, root, &label, PAD + 4.0, y + 6.0, Some(colour));
            if row.current {
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
        if rows.is_empty() {
            let none = engine.simple_layout("No font matches", theme::role::row());
            scene::draw_layout_first_line(
                scene,
                root,
                &none,
                PAD + 4.0,
                HEADER_H + 8.0,
                Some(p.text_dim),
            );
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
        let toggle = engine.simple_layout("Show all fonts (Tab)", theme::role::hint());
        scene::draw_layout_first_line(
            scene,
            root,
            &toggle,
            PAD + glyph.width() + 6.0,
            y,
            Some(p.text_dim),
        );
        let hint = engine.simple_layout("Enter to choose", theme::role::hint());
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

/// The picker and whose font it is choosing.
pub(super) struct Open {
    surface: FontPickerSurface,
    profile: koru_session_protocol::SessionProfileKey,
}

impl super::MainView {
    fn font_picker_bounds(&self) -> Rect {
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
        let mut surface = FontPickerSurface::new(
            &mut self.engine,
            koru_draw::installed_fonts::installed_fonts(),
            current,
        );
        let bounds = self.font_picker_bounds();
        surface.set_viewport(bounds.w, bounds.h);
        self.font_picker = Some(Open { surface, profile });
        self.hover = None;
        true
    }

    fn apply_font_picker_action(&mut self, action: Action) -> bool {
        match action {
            Action::Keep => true,
            Action::Cancel => {
                self.font_picker = None;
                true
            }
            Action::UseDefault | Action::Choose(_) => {
                let Some(open) = self.font_picker.take() else {
                    return false;
                };
                let token = match action {
                    Action::Choose(family) => Some(crate::terminal_prefs::family_token(&family)),
                    _ => None,
                };
                crate::terminal_prefs::set_agent_look(&open.profile, None, Some(token.as_deref()));
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
}

#[cfg(test)]
mod tests;
