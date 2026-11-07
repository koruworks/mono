// SPDX-License-Identifier: MPL-2.0

//! [`EmojiColorPanel`]: choosing the emoji and color of a workspace, a
//! Profile, or either one before it exists.
//!
//! One floating panel for every place an appearance is chosen — the main
//! view's workspace and Profile menus and both first-run steps. The modal
//! owns the preview, color swatches and Save/Cancel actions. Its emoji body is the editor's complete `/emoji` picker: one implementation for
//! search, categories, scrolling, recents and long-press skin tones. It lives
//! in a leaf crate so first-run onboarding and the full workspace application
//! use the same surface without either one owning a second picker.

use koru_core::{NodeId, ProfileId, WorkspaceId};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::tile::Tile;
use koru_draw::{inspect, scene, theme};
use koru_editor::components::Outcome as EmojiOutcome;
use koru_editor::components::draw::draw_emoji_picker_surface;
use koru_editor::components::emoji_picker::{self, EmojiPicker};
use koru_editor::trigger::{Anchor, Trigger, TriggerKind};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

const PAD: f32 = 20.0;
const WIDTH: f32 = emoji_picker::PANEL_W + PAD * 2.0;
const RADIUS: f32 = 10.0;
const PREVIEW: f32 = 46.0;
const BTN_H: f32 = 32.0;
const BTN_GAP: f32 = 10.0;
const SECTION_GAP: f32 = 18.0;
const HEADING: theme::TextStyle =
    theme::TextStyle::new(theme::text_metrics::MICRO, theme::FontWeight::Medium);
const BODY: theme::TextStyle =
    theme::TextStyle::new(theme::text_metrics::XS, theme::FontWeight::Medium);
const HEADING_H: f32 = HEADING.metrics.line_height;

pub enum Outcome {
    Keep,
    Cancel,
    Save {
        icon: Option<String>,
        color: Option<String>,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Target {
    Workspace(WorkspaceId),
    Profile(ProfileId),
    /// Appearance for a workspace that does not have a Store identity yet.
    Draft,
    /// Appearance for a Profile that does not have a Store identity yet.
    DraftProfile,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Hit {
    Swatch(usize),
    ClearEmoji,
    Cancel,
    Save,
}

/// Tab stops owned by the modal. The editor emoji picker uses Tab to change
/// category because it has no Save button; this modal must reach Save.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Focus {
    Color,
    Search,
    Emoji,
    Category,
    Clear,
    Cancel,
    Save,
}

const TAB_ORDER: [Focus; 7] = [
    Focus::Color,
    Focus::Search,
    Focus::Emoji,
    Focus::Category,
    Focus::Clear,
    Focus::Cancel,
    Focus::Save,
];

pub struct EmojiColorPanel {
    pub target: Target,
    name: String,
    icon: Option<String>,
    color: Option<String>,
    picker: EmojiPicker,
    hover: Option<Hit>,
    focus: Focus,
    color_index: usize,
    cursor: CursorIcon,
    field_dragging: bool,
    body_scroll: f32,
    max_body_scroll: f32,
    viewport: Option<(f32, f32)>,
}

impl EmojiColorPanel {
    /// The picker's search box, for a dictation the container is routing.
    ///
    /// `None` for the shortcode-completion form of the picker, which leaves
    /// its query in the document and so has no second editable field.
    pub fn dictation_field(&mut self) -> Option<&mut koru_ui::TextField> {
        self.picker.query.as_mut()
    }

    pub fn new(
        engine: &mut TextEngine,
        id: WorkspaceId,
        name: &str,
        icon: Option<String>,
        color: Option<String>,
    ) -> Self {
        Self::build(engine, Target::Workspace(id), name, icon, color)
    }

    pub fn for_profile(
        engine: &mut TextEngine,
        id: ProfileId,
        name: &str,
        icon: Option<String>,
        color: Option<String>,
    ) -> Self {
        Self::build(engine, Target::Profile(id), name, icon, color)
    }

    pub fn for_draft(
        engine: &mut TextEngine,
        name: &str,
        icon: Option<String>,
        color: Option<String>,
    ) -> Self {
        Self::build(engine, Target::Draft, name, icon, color)
    }

    pub fn for_profile_draft(
        engine: &mut TextEngine,
        name: &str,
        icon: Option<String>,
        color: Option<String>,
    ) -> Self {
        Self::build(engine, Target::DraftProfile, name, icon, color)
    }

    fn build(
        engine: &mut TextEngine,
        target: Target,
        name: &str,
        icon: Option<String>,
        color: Option<String>,
    ) -> Self {
        Self {
            target,
            name: name.to_owned(),
            icon,
            color: color.clone(),
            picker: {
                let mut picker = EmojiPicker::standalone(
                    engine,
                    Trigger {
                        kind: TriggerKind::Slash,
                        start: 0,
                        node: NodeId::new(),
                        anchor: Anchor::Insert,
                        stable: None,
                    },
                );
                // The list spans the panel's content column, edge to edge
                // with the COLOR and EMOJI labels above it.
                picker.set_fill_width(WIDTH - PAD * 2.0);
                picker
            },
            hover: None,
            focus: Focus::Color,
            color_index: Self::index_of(&color),
            cursor: CursorIcon::Default,
            field_dragging: false,
            body_scroll: 0.0,
            max_body_scroll: 0.0,
            viewport: None,
        }
    }

    fn height(&self) -> f32 {
        PAD + PREVIEW
            + SECTION_GAP
            + Self::color_height(self.picker.geometry().width)
            + SECTION_GAP
            + HEADING_H
            + self.picker.height()
            + 14.0
            + BTN_H
            + PAD
    }

    pub fn panel(&self, viewport: (f32, f32)) -> Rect {
        let w = WIDTH.min((viewport.0 - 32.0).max(200.0));
        let h = self.height().min((viewport.1 - 32.0).max(160.0));
        Rect::new(
            ((viewport.0 - w) * 0.5).max(8.0),
            ((viewport.1 - h) * 0.5).max(8.0),
            w,
            h,
        )
    }

    fn fit(&mut self, engine: &mut TextEngine, viewport: (f32, f32)) {
        self.viewport = Some(viewport);
        let width = self.panel(viewport).w - PAD * 2.0;
        self.picker.set_fill_width(width);
        let panel = self.panel(viewport);
        self.max_body_scroll = self.max_scroll(engine, panel);
        self.body_scroll = self.body_scroll.clamp(0.0, self.max_body_scroll);
    }

    fn body_rect(&self, engine: &mut TextEngine, panel: Rect) -> Rect {
        let (clear, cancel, save) = self.buttons(engine, panel);
        let bottom = clear.y.min(cancel.y).min(save.y) - 14.0;
        Rect::new(
            panel.x + PAD,
            panel.y + PAD,
            panel.w - PAD * 2.0,
            (bottom - panel.y - PAD).max(0.0),
        )
    }

    fn max_scroll(&self, engine: &mut TextEngine, panel: Rect) -> f32 {
        let body = self.body_rect(engine, panel);
        let picker = self.picker_rect(panel);
        (picker.y + picker.h - body.y - body.h).max(0.0)
    }

    fn reveal_focus(&mut self, engine: &mut TextEngine) {
        let Some(viewport) = self.viewport else {
            return;
        };
        let panel = self.panel(viewport);
        let body = self.body_rect(engine, panel);
        let picker = self.picker_rect(panel);
        let local = match self.focus {
            Focus::Color => self.color_rect(panel),
            Focus::Search | Focus::Emoji | Focus::Category => {
                let geometry = self.picker.geometry();
                let rect = match self.focus {
                    Focus::Search => geometry.search_rect(),
                    Focus::Emoji => self
                        .picker
                        .cell_rect(self.picker.sel)
                        .unwrap_or(geometry.grid_rect()),
                    _ => geometry.category_rect(0).unwrap(),
                };
                Rect::new(picker.x + rect.x, picker.y + rect.y, rect.w, rect.h)
            }
            _ => return,
        };
        if local.y - self.body_scroll < body.y {
            self.body_scroll = local.y - body.y;
        } else if local.y + local.h - self.body_scroll > body.y + body.h {
            self.body_scroll = local.y + local.h - body.y - body.h;
        }
        self.body_scroll = self.body_scroll.clamp(0.0, self.max_scroll(engine, panel));
    }

    /// Every colour on offer, "none" first.
    fn colors() -> Vec<Option<vello::peniko::Color>> {
        std::iter::once(None)
            .chain(
                theme::workspace_color_names()
                    .iter()
                    .map(|name| theme::workspace_color(name)),
            )
            .collect()
    }

    /// The shared colour row, labelled; its selection and focus are added
    /// where it is drawn.
    fn color_row(colors: &[Option<vello::peniko::Color>]) -> koru_ui::ColorPickerRow<'_> {
        koru_ui::ColorPickerRow::new(colors).label("COLOR")
    }

    fn color_columns(width: f32) -> usize {
        ((width + koru_ui::Swatch::GAP) / (koru_ui::Swatch::SIZE + koru_ui::Swatch::GAP))
            .floor()
            .max(1.0) as usize
    }

    fn color_height(width: f32) -> f32 {
        let rows = Self::colors().len().div_ceil(Self::color_columns(width));
        Self::color_row(&Self::colors()).height()
            + (rows - 1) as f32 * (koru_ui::Swatch::SIZE + koru_ui::Swatch::RING_REACH * 2.0)
            + koru_ui::Swatch::RING_REACH
    }

    fn color_rows(&self, panel: Rect) -> Vec<(usize, Rect)> {
        let bounds = self.color_rect(panel);
        let columns = Self::color_columns(bounds.w);
        let first_height = Self::color_row(&Self::colors()).height();
        (0..Self::colors().len())
            .step_by(columns)
            .enumerate()
            .map(|(row, start)| {
                let y = if row == 0 {
                    bounds.y
                } else {
                    bounds.y
                        + first_height
                        + koru_ui::Swatch::RING_REACH * 2.0
                        + (row - 1) as f32
                            * (koru_ui::Swatch::SIZE + koru_ui::Swatch::RING_REACH * 2.0)
                };
                (
                    start,
                    Rect::new(
                        bounds.x,
                        y,
                        bounds.w,
                        if row == 0 {
                            first_height
                        } else {
                            koru_ui::Swatch::SIZE
                        },
                    ),
                )
            })
            .collect()
    }

    fn color_rect(&self, panel: Rect) -> Rect {
        Rect::new(
            panel.x + PAD,
            panel.y + PAD + PREVIEW + SECTION_GAP,
            panel.w - PAD * 2.0,
            Self::color_height(panel.w - PAD * 2.0),
        )
    }

    /// The `n`th swatch, where it is drawn and hit.
    #[cfg(test)]
    fn swatch_rect(&self, panel: Rect, n: usize) -> Option<Rect> {
        let colors = Self::colors();
        let columns = Self::color_columns(panel.w - PAD * 2.0);
        let (start, rect) = self
            .color_rows(panel)
            .into_iter()
            .find(|(start, _)| n >= *start && n < *start + columns)?;
        let row = koru_ui::ColorPickerRow::new(&colors[start..(start + columns).min(colors.len())]);
        let row = if start == 0 { row.label("COLOR") } else { row };
        row.swatches(rect).get(n - start).copied()
    }

    fn picker_rect(&self, panel: Rect) -> Rect {
        let colors = self.color_rect(panel);
        let y = colors.y + colors.h + SECTION_GAP + HEADING_H;
        Rect::new(
            panel.x + PAD,
            y,
            self.picker.geometry().width,
            self.picker.height(),
        )
    }

    fn buttons(&self, engine: &mut TextEngine, panel: Rect) -> (Rect, Rect, Rect) {
        let y = panel.y + panel.h - PAD - BTN_H;
        let save_w = button_width(engine, "Save");
        let cancel_w = button_width(engine, "Cancel");
        let clear_w = button_width(engine, "Clear emoji");
        let save = Rect::new(panel.x + panel.w - PAD - save_w, y, save_w, BTN_H);
        let cancel = Rect::new(save.x - BTN_GAP - cancel_w, y, cancel_w, BTN_H);
        let clear_y = if panel.x + PAD + clear_w + BTN_GAP > cancel.x {
            y - BTN_H - BTN_GAP
        } else {
            y
        };
        let clear = Rect::new(panel.x + PAD, clear_y, clear_w, BTN_H);
        (clear, cancel, save)
    }

    fn hit_at(&self, engine: &mut TextEngine, viewport: (f32, f32), x: f32, y: f32) -> Option<Hit> {
        let panel = self.panel(viewport);
        let (clear, cancel, save) = self.buttons(engine, panel);
        if save.contains(x, y) {
            return Some(Hit::Save);
        }
        if cancel.contains(x, y) {
            return Some(Hit::Cancel);
        }
        if clear.contains(x, y) {
            return Some(Hit::ClearEmoji);
        }
        if !self.body_rect(engine, panel).contains(x, y) {
            return None;
        }
        let colors = Self::colors();
        let columns = Self::color_columns(panel.w - PAD * 2.0);
        self.color_rows(panel)
            .into_iter()
            .find_map(|(start, rect)| {
                let row = koru_ui::ColorPickerRow::new(
                    &colors[start..(start + columns).min(colors.len())],
                );
                let row = if start == 0 { row.label("COLOR") } else { row };
                row.hit(rect, x, y + self.body_scroll)
                    .map(|index| Hit::Swatch(start + index))
            })
    }

    fn color_of(n: usize) -> Option<String> {
        n.checked_sub(1)
            .and_then(|i| theme::workspace_color_names().get(i).map(|s| s.to_string()))
    }

    fn swatch_count() -> usize {
        theme::workspace_color_names().len() + 1
    }

    fn index_of(color: &Option<String>) -> usize {
        match color {
            None => 0,
            Some(name) => theme::workspace_color_names()
                .iter()
                .position(|candidate| candidate == name)
                .map(|index| index + 1)
                .unwrap_or(0),
        }
    }

    fn apply_swatch(&mut self, index: usize) {
        self.color_index = index;
        self.color = Self::color_of(index);
    }

    fn step_focus(&mut self, delta: isize) {
        let current = TAB_ORDER
            .iter()
            .position(|stop| *stop == self.focus)
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(TAB_ORDER.len() as isize) as usize;
        self.focus = TAB_ORDER[next];
    }

    fn take(&self) -> Outcome {
        Outcome::Save {
            icon: self.icon.clone(),
            color: self.color.clone(),
        }
    }

    fn settle_picker(&mut self, outcome: EmojiOutcome) -> Outcome {
        match outcome {
            EmojiOutcome::CommitText(value) => {
                self.icon = Some(value);
                Outcome::Keep
            }
            EmojiOutcome::Close => Outcome::Cancel,
            _ => Outcome::Keep,
        }
    }

    pub fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Outcome {
        let outcome = self.key_inner(engine, key, text, mods);
        self.reveal_focus(engine);
        outcome
    }

    fn key_inner(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Outcome {
        if matches!(key, Key::Named(NamedKey::Escape)) && self.picker.tone_strip.is_none() {
            return Outcome::Cancel;
        }
        if matches!(key, Key::Named(NamedKey::Tab)) {
            self.step_focus(if mods.shift_key() { -1 } else { 1 });
            return Outcome::Keep;
        }
        match self.focus {
            Focus::Color => {
                let count = Self::swatch_count() as isize;
                match key {
                    Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) if mods.is_empty() => {
                        self.apply_swatch(
                            ((self.color_index as isize + 1).rem_euclid(count)) as usize,
                        );
                    }
                    Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) if mods.is_empty() => {
                        self.apply_swatch(
                            ((self.color_index as isize - 1).rem_euclid(count)) as usize,
                        );
                    }
                    Key::Named(NamedKey::Enter | NamedKey::Space) if mods.is_empty() => {
                        self.apply_swatch(self.color_index);
                    }
                    _ => {}
                }
                Outcome::Keep
            }
            Focus::Search => {
                self.picker.type_in_search(engine, key, text, mods);
                Outcome::Keep
            }
            Focus::Emoji => {
                let outcome = self.picker.key(engine, key, text, mods);
                self.settle_picker(outcome)
            }
            Focus::Category => {
                match key {
                    Key::Named(NamedKey::ArrowRight | NamedKey::ArrowDown) if mods.is_empty() => {
                        self.picker.cycle_category(engine, 1);
                    }
                    Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowUp) if mods.is_empty() => {
                        self.picker.cycle_category(engine, -1);
                    }
                    _ => {}
                }
                Outcome::Keep
            }
            Focus::Clear => {
                if matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space)) && mods.is_empty() {
                    self.icon = None;
                }
                Outcome::Keep
            }
            Focus::Cancel => {
                if matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space)) && mods.is_empty() {
                    Outcome::Cancel
                } else {
                    Outcome::Keep
                }
            }
            Focus::Save => {
                if matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space)) && mods.is_empty() {
                    self.take()
                } else {
                    Outcome::Keep
                }
            }
        }
    }

    pub fn mouse_down(
        &mut self,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        x: f32,
        y: f32,
    ) -> Outcome {
        self.fit(engine, viewport);
        let panel = self.panel(viewport);
        match self.hit_at(engine, viewport, x, y) {
            Some(Hit::Save) => {
                self.focus = Focus::Save;
                return self.take();
            }
            Some(Hit::Cancel) => {
                self.focus = Focus::Cancel;
                return Outcome::Cancel;
            }
            Some(Hit::ClearEmoji) => {
                self.focus = Focus::Clear;
                self.icon = None;
                return Outcome::Keep;
            }
            Some(Hit::Swatch(n)) => {
                self.focus = Focus::Color;
                self.apply_swatch(n);
                return Outcome::Keep;
            }
            None => {}
        }

        let picker = self.picker_rect(panel);
        if self.body_rect(engine, panel).contains(x, y) && picker.contains(x, y + self.body_scroll)
        {
            let (lx, ly) = (x - picker.x, y + self.body_scroll - picker.y);
            self.focus = if self
                .picker
                .field_rect()
                .is_some_and(|field| field.contains(lx, ly))
            {
                Focus::Search
            } else if self.picker.geometry().category_at(lx, ly).is_some() {
                Focus::Category
            } else {
                Focus::Emoji
            };
            let outcome = if self
                .picker
                .field_rect()
                .is_some_and(|field| field.contains(lx, ly))
            {
                self.picker.click_field(engine, lx);
                self.field_dragging = true;
                EmojiOutcome::Consumed
            } else {
                self.picker.click(engine, lx, ly)
            };
            return self.settle_picker(outcome);
        }
        if panel.contains(x, y) {
            Outcome::Keep
        } else {
            Outcome::Cancel
        }
    }

    pub fn mouse_move(
        &mut self,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        x: f32,
        y: f32,
    ) -> bool {
        self.fit(engine, viewport);
        let panel = self.panel(viewport);
        let picker = self.picker_rect(panel);
        let in_body = self.body_rect(engine, panel).contains(x, y);
        let (lx, ly) = if in_body || self.field_dragging || self.picker.is_dragging_scrollbar() {
            (x - picker.x, y + self.body_scroll - picker.y)
        } else {
            (-1.0, -1.0)
        };
        if self.picker.is_dragging_scrollbar() {
            self.picker.drag_scrollbar(ly);
            return true;
        }
        if self.field_dragging {
            self.picker.drag_field(engine, lx);
        }
        let mut changed = self.picker.set_tone_hover(lx, ly);
        if let Some(item) = self.picker.item_at(lx, ly)
            && self.picker.sel != item
        {
            self.picker.sel = item;
            changed = true;
        }
        let was = self.hover;
        self.hover = self.hit_at(engine, viewport, x, y);
        self.cursor = if self.hover.is_some() {
            koru_ui::cursor::PRESS
        } else if self
            .picker
            .field_rect()
            .is_some_and(|field| field.contains(lx, ly))
        {
            CursorIcon::Text
        } else if in_body && picker.contains(x, y + self.body_scroll) {
            koru_ui::cursor::PRESS
        } else {
            CursorIcon::Default
        };
        changed || was != self.hover
    }

    pub fn mouse_up(&mut self) -> Outcome {
        self.field_dragging = false;
        self.picker.stop_scrollbar_drag();
        let outcome = self.picker.release();
        self.settle_picker(outcome)
    }

    pub fn scroll_by(&mut self, delta: MouseScrollDelta) {
        let pixels = match delta {
            MouseScrollDelta::LineDelta(_, y) => -y * 32.0,
            MouseScrollDelta::PixelDelta(p) => -p.y as f32,
        };
        let next = (self.body_scroll + pixels).clamp(0.0, self.max_body_scroll);
        if next != self.body_scroll {
            self.body_scroll = next;
            self.hover = None;
            return;
        }
        let y = match delta {
            MouseScrollDelta::LineDelta(_, y) => y,
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        self.picker.scroll_rows(if y < 0.0 { 1 } else { -1 });
    }

    pub fn animating(&self) -> bool {
        self.picker.press_animating()
    }

    pub fn cursor(&self) -> CursorIcon {
        self.cursor
    }

    pub fn draw(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: (f32, f32),
    ) {
        self.fit(engine, viewport);
        let p = theme::p();
        scene::fill(
            scene,
            root,
            p.scrim,
            &scene::rect(0.0, 0.0, viewport.0, viewport.1),
        );
        let panel = self.panel(viewport);
        // One component, one panel, and everything below drawn inside both:
        // the preview, the swatches, the picker and the buttons are the
        // panel's contents, not its siblings.
        let _component = inspect::scope(
            "EmojiColorPanel",
            "component",
            root,
            scene::rect(panel.x, panel.y, panel.w, panel.h),
            |record| {
                record.property("target", format!("{:?}", self.target), "Target");
            },
        );
        let _panel = scene::floating_panel_scope(
            scene,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(RADIUS),
        );

        let body = self.body_rect(engine, panel);
        let reach = koru_ui::Swatch::RING_REACH;
        let clip = scene::rect(body.x - reach, body.y, body.w + reach * 2.0, body.h);
        scene.push_clip_layer(vello::peniko::Fill::NonZero, root, &clip);
        let body_clip = inspect::clip(root, clip);
        let modal_root = root;
        let root = root * Affine::translate((0.0, -f64::from(self.body_scroll)));
        let tile = Rect::new(panel.x + PAD, panel.y + PAD, PREVIEW, PREVIEW);
        Tile {
            icon: self.icon.as_deref(),
            color: self.color.as_deref(),
            name: &self.name,
        }
        .draw(scene, root, engine, tile);
        let tx = tile.x + tile.w + 14.0;
        let title = engine.simple_layout(&self.name, theme::role::section_heading());
        scene::draw_layout_first_line(scene, root, &title, tx, tile.y + 2.0, Some(p.text));
        let explanation = match self.target {
            Target::Workspace(_) => "Icon and color appear in the switcher and the window title.",
            Target::Profile(_) => "Icon and color appear anywhere you choose a profile.",
            Target::Draft => "Choose how this workspace will appear throughout Koru.",
            Target::DraftProfile => "Choose how this Profile will appear throughout Koru.",
        };
        let sub = engine.simple_layout_wrapped(explanation, BODY, panel.x + panel.w - PAD - tx);
        scene::draw_layout(scene, root, &sub, tx, tile.y + 24.0, Some(p.text_dim));

        let colors = Self::colors();
        let hovered = match self.hover {
            Some(Hit::Swatch(n)) => Some(n),
            _ => None,
        };
        let columns = Self::color_columns(panel.w - PAD * 2.0);
        for (start, rect) in self.color_rows(panel) {
            let end = (start + columns).min(colors.len());
            let local = |index: usize| {
                index
                    .checked_sub(start)
                    .filter(|index| *index < end - start)
            };
            let row = koru_ui::ColorPickerRow::new(&colors[start..end]);
            let row = if start == 0 { row.label("COLOR") } else { row };
            row.selected(local(Self::index_of(&self.color)))
                .hovered(hovered.and_then(local))
                .focused(
                    (self.focus == Focus::Color)
                        .then(|| local(self.color_index))
                        .flatten(),
                )
                .draw(scene, root, engine, rect);
        }

        let picker = self.picker_rect(panel);
        heading(
            scene,
            root,
            engine,
            "EMOJI",
            panel.x + PAD,
            picker.y - HEADING_H,
        );
        draw_emoji_picker_surface(
            scene,
            root,
            engine,
            &mut self.picker,
            picker.x,
            picker.y,
            picker.w,
        );

        scene.pop_layer();
        drop(body_clip);
        let root = modal_root;

        let (clear, cancel, save) = self.buttons(engine, panel);
        button(
            scene,
            root,
            engine,
            clear,
            "Clear emoji",
            self.hover == Some(Hit::ClearEmoji),
            self.focus == Focus::Clear,
            false,
        );
        button(
            scene,
            root,
            engine,
            cancel,
            "Cancel",
            self.hover == Some(Hit::Cancel),
            self.focus == Focus::Cancel,
            false,
        );
        button(
            scene,
            root,
            engine,
            save,
            "Save",
            self.hover == Some(Hit::Save),
            self.focus == Focus::Save,
            true,
        );
    }
}

fn heading(scene: &mut Scene, root: Affine, engine: &mut TextEngine, text: &str, x: f32, y: f32) {
    let l = engine.simple_layout(text, HEADING);
    scene::draw_layout_first_line(scene, root, &l, x, y, Some(theme::p().row_hint));
}

fn button_width(engine: &mut TextEngine, label: &str) -> f32 {
    engine.simple_layout(label, BODY).width().max(60.0) + 26.0
}

fn button(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    r: Rect,
    label: &str,
    hot: bool,
    focused: bool,
    primary: bool,
) {
    let p = theme::p();
    let (fill, fg) = if primary {
        (p.accent, p.on_accent)
    } else {
        (p.panel_bg, p.text)
    };
    scene::fill(scene, root, fill, &scene::rrect(r.x, r.y, r.w, r.h, 6.0));
    if !primary {
        scene::stroke(
            scene,
            root,
            p.panel_border,
            theme::BORDER_W,
            &scene::rrect(r.x, r.y, r.w, r.h, 6.0),
        );
    }
    if hot {
        scene::fill(
            scene,
            root,
            p.hover_tint,
            &scene::rrect(r.x, r.y, r.w, r.h, 6.0),
        );
    }
    if focused {
        scene::stroke(
            scene,
            root,
            p.focus_ring,
            2.0,
            &scene::rrect(r.x - 2.0, r.y - 2.0, r.w + 4.0, r.h + 4.0, 8.0),
        );
    }
    let t = engine.simple_layout(label, BODY);
    scene::draw_layout_first_line(
        scene,
        root,
        &t,
        r.x + (r.w - t.width()) * 0.5,
        r.y + (r.h - scene::first_line_height(&t)) * 0.5,
        Some(fg),
    );
}

#[cfg(test)]
mod tests;
