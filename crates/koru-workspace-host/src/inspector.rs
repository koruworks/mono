// SPDX-License-Identifier: MPL-2.0

//! Window-local, read-only presentation of draw evidence. The host owns input,
//! clipboard and accessibility; the draw crate owns the measurements.

use accesskit::{Action, Role};
use koru_draw::geometry::Rect;
use koru_draw::{
    inspect::{self, Snapshot},
    scene,
    text::TextEngine,
    theme,
};
use koru_i18n::{Key as CopyKey, t};
use koru_keys::{Section, ShortcutId, show_hint};
use std::collections::BTreeSet;
use vello::{Scene, kurbo::Affine};
use winit::{
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::{Key, NamedKey},
};

use crate::accessibility::{ControlPart, Occurrence, SemanticNode, WindowSlot};

mod detached;
mod details;
mod hierarchy;
mod history;
mod placement;
mod scrolling;
mod spacing;
pub(crate) use placement::{Geometry, Placement};

const PAD: f32 = 12.0;
const ROW: f32 = 22.0;
const HEADER: f32 = 34.0;
const BODY_TOP: f32 = HEADER + 36.0;
const TREE_TOP: f32 = BODY_TOP + 30.0;
const DETAILS_TOP: f32 = BODY_TOP + 114.0;
const PICK: usize = 0;
const COPY: usize = 1;
const REFRESH: usize = 2;
const CLOSE: usize = 3;

/// Focus transfer replays held keys with `is_synthetic`, even when `repeat`
/// is false. Opening or closing the native inspector transfers focus, so only
/// a deliberate initial press may toggle it in either window.
pub(crate) fn shortcut_pressed(state: ElementState, repeat: bool, is_synthetic: bool) -> bool {
    state.is_pressed() && !repeat && !is_synthetic
}

const PIN: usize = 9;
const RAYS: usize = 10;
const PARTS: [&str; 11] = [
    "inspect.pick",
    "inspect.copy",
    "inspect.refresh",
    "inspect.close",
    "inspect.tree",
    "inspect.details",
    "inspect.move",
    "inspect.tab.details",
    "inspect.tab.hierarchy",
    "inspect.pin",
    "inspect.spacing-rays",
];

pub(crate) struct Inspector {
    pub snapshot: Snapshot,
    pub picking: bool,
    refresh_paused: bool,
    pub selected: Option<usize>,
    pub stale: bool,
    pub focus: bool,
    pub dirty_semantics: bool,
    pub frame: u64,
    bounds: Rect,
    pointer: (f32, f32),
    drag: Option<(f32, f32)>,
    scrollbar_drag: Option<f32>,
    captured_press: bool,
    focus_index: usize,
    scroll: usize,
    details_cache: scrolling::DetailsCache,
    hierarchy_scroll: usize,
    collapsed_hierarchy: BTreeSet<usize>,
    show_hierarchy: bool,
    hierarchy_click: Option<(usize, u64, std::time::Instant, (f32, f32))>,
    history: history::History,
    /// Details, grouped, as laid out; `lines` is its plain text.
    details_lines: Vec<details::Line>,
    /// Whether Details shows the capture's notes at its foot.
    notes_open: bool,
    /// The box's labels and what each says on hover.
    box_tips: Vec<(Rect, String)>,
    engine: Option<TextEngine>,
    lines: Vec<String>,
    status: Option<CopyKey>,
    pub semantic_details: Vec<String>,
    pub capture_micros: u128,
    semantic_candidates: Vec<(Rect, String)>,
    view_revision: Option<u64>,
    viewport: (f32, f32),
    detached: bool,
    source_pointer: Option<(f32, f32)>,
    pub pinned: bool,
    pub spacing_rays: bool,
}

impl Inspector {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            snapshot: Snapshot::default(),
            picking: true,
            refresh_paused: false,
            selected: None,
            stale: false,
            focus: true,
            dirty_semantics: true,
            frame: 0,
            bounds: Rect::new(
                (width - 490.0).max(0.0),
                48.0,
                480.0_f32.min(width),
                650.0_f32.min(height),
            ),
            pointer: (0.0, 0.0),
            drag: None,
            scrollbar_drag: None,
            captured_press: false,
            focus_index: 0,
            scroll: 0,
            details_cache: scrolling::DetailsCache::default(),
            hierarchy_scroll: 0,
            collapsed_hierarchy: BTreeSet::new(),
            // Opens on the tree: a picked element is shown where it sits, and
            // its details are one double-click or the Details tab away.
            show_hierarchy: true,
            hierarchy_click: None,
            history: history::History::default(),
            details_lines: Vec::new(),
            notes_open: false,
            box_tips: Vec::new(),
            engine: None,
            lines: Vec::new(),
            status: None,
            semantic_details: Vec::new(),
            capture_micros: 0,
            semantic_candidates: Vec::new(),
            view_revision: None,
            viewport: (width, height),
            detached: false,
            source_pointer: None,
            pinned: false,
            spacing_rays: false,
        }
    }

    pub fn constrain(&mut self, width: f32, height: f32) {
        self.viewport = (width, height);
        if self.detached {
            return;
        }
        self.bounds.w = 480.0_f32.min(width);
        self.bounds.h = 650.0_f32.min(height);
        self.bounds.x = self.bounds.x.clamp(0.0, (width - self.bounds.w).max(0.0));
        self.bounds.y = self.bounds.y.clamp(0.0, (height - self.bounds.h).max(0.0));
    }

    pub fn invalidate_selection(&mut self) {
        if !self.picking && self.selected.is_some() {
            self.stale = true;
            if !self.refresh_paused {
                self.selected = None;
            }
            self.dirty_semantics = true;
        }
    }

    pub fn observe_view_revision(&mut self, revision: u64) {
        if self
            .view_revision
            .is_some_and(|previous| previous != revision)
        {
            self.invalidate_selection();
            if !self.refresh_paused {
                self.semantic_candidates.clear();
                self.semantic_details.clear();
            }
        }
        self.view_revision = Some(revision);
    }

    pub fn host_changed(&mut self) {
        self.invalidate_selection();
        if self.refresh_paused {
            return;
        }
        self.semantic_candidates.clear();
        self.semantic_details.clear();
        self.dirty_semantics = true;
    }

    pub fn accept(&mut self, snapshot: Snapshot) {
        // Operating the panel must not remap the saved index onto a different
        // frame. Resume waits for a pointer over the app to choose a new target.
        if self.refresh_paused || (self.picking && !self.pointer_over_source()) {
            return;
        }
        if snapshot.truncated {
            self.invalidate_selection();
        }
        // A frame index is not a stable domain identity. Preserve a pinned visual
        // only while its complete ancestry/source/geometry still agrees. Host
        // input or semantic revision changes explicitly retire it first.
        if !self.picking
            && let Some(index) = self.selected
        {
            let same = self
                .snapshot
                .records
                .get(index)
                .zip(snapshot.records.get(index))
                .is_some_and(|_| same_ancestry(&self.snapshot, &snapshot, index));
            if !same {
                self.invalidate_selection();
            }
        }
        if self.snapshot.revision != snapshot.revision {
            // Record indices belong to one captured frame. A collapsed index
            // must never be carried onto a different frame's unrelated node.
            self.collapsed_hierarchy.clear();
        }
        self.snapshot = snapshot;
        let selection_changed = if self.picking && self.pointer_over_source() {
            let pointer = self.source_pointer.unwrap_or(self.pointer);
            self.select(self.snapshot.pick(pointer.0 as f64, pointer.1 as f64))
        } else {
            false
        };
        // A frame can change the properties at the same selected index too.
        if !selection_changed {
            self.refresh_semantic_details();
        }
    }

    fn select(&mut self, selected: Option<usize>) -> bool {
        let changed = self.selected != selected;
        if changed {
            self.selected = selected;
            self.scroll = 0;
            self.semantic_details.clear();
            self.status = None;
            self.dirty_semantics = true;
        }
        if !self.refresh_paused {
            self.stale = false;
        }
        if changed {
            self.reveal_selection();
            self.refresh_semantic_details();
        }
        changed
    }

    pub fn event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        zoom: f32,
        mods: winit::keyboard::ModifiersState,
    ) -> EventResult {
        let mut result = EventResult::default();
        let previous_tab = self.show_hierarchy;
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = (
                    (position.x / scale) as f32 / zoom,
                    (position.y / scale) as f32 / zoom,
                );
                if let Some(grab) = self.scrollbar_drag {
                    self.scroll_to_pointer(grab);
                } else if let Some((dx, dy)) = self.drag {
                    self.bounds.x = self.pointer.0 - dx;
                    self.bounds.y = self.pointer.1 - dy;
                    self.dirty_semantics = true;
                } else if !self.detached
                    && self.picking
                    && !self.bounds.contains(self.pointer.0, self.pointer.1)
                {
                    self.select(
                        self.snapshot
                            .pick(self.pointer.0 as f64, self.pointer.1 as f64),
                    );
                }
                result.consumed = self.drag.is_some()
                    || self.scrollbar_drag.is_some()
                    || self.picking
                    || self.bounds.contains(self.pointer.0, self.pointer.1);
                result.redraw = result.consumed;
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let inside = self.bounds.contains(self.pointer.0, self.pointer.1);
                result.consumed =
                    inside || self.picking || self.drag.is_some() || self.captured_press;
                if *state == ElementState::Released {
                    self.scrollbar_drag = None;
                    result.placement_changed = self.drag.take().is_some();
                    self.constrain(self.viewport.0, self.viewport.1);
                    self.captured_press = false;
                    return result;
                }
                self.captured_press = result.consumed;
                // The mouse's thumb buttons walk the panel's history, as
                // they walk a browser's.
                if inside && matches!(button, MouseButton::Back | MouseButton::Forward) {
                    self.hierarchy_click = None;
                    result.redraw = self.go(*button == MouseButton::Back);
                    result.structure_changed = previous_tab != self.show_hierarchy;
                    return result;
                }
                if !self.show_hierarchy
                    || !self
                        .control_rect(4)
                        .contains(self.pointer.0, self.pointer.1)
                {
                    self.hierarchy_click = None;
                }
                if *button == MouseButton::Left && inside {
                    self.focus = true;
                    let y = self.pointer.1 - self.bounds.y;
                    if let Some(bar) = self
                        .scrollbar()
                        .filter(|bar| bar.in_gutter(self.pointer.0, self.pointer.1))
                    {
                        self.hierarchy_click = None;
                        self.pause_refresh();
                        let grab = if bar.on_thumb(self.pointer.0, self.pointer.1) {
                            self.pointer.1 - bar.thumb_top
                        } else {
                            bar.thumb_h / 2.0
                        };
                        self.scrollbar_drag = Some(grab);
                        self.focus_index = if self.show_hierarchy { 4 } else { 5 };
                        self.scroll_to_pointer(grab);
                    } else if self
                        .control_rect(CLOSE)
                        .contains(self.pointer.0, self.pointer.1)
                    {
                        self.focus_index = CLOSE;
                        result.close = true;
                    } else if self
                        .control_rect(PIN)
                        .contains(self.pointer.0, self.pointer.1)
                    {
                        self.focus_index = PIN;
                        self.activate(PIN);
                    } else if let Some(index) = [PICK, COPY, RAYS].into_iter().find(|i| {
                        self.control_rect(*i)
                            .contains(self.pointer.0, self.pointer.1)
                    }) {
                        self.focus_index = index;
                        result.close = self.activate(index);
                    } else if y < HEADER {
                        self.drag = Some((self.pointer.0 - self.bounds.x, y));
                        self.focus_index = 6;
                    } else if self.show_hierarchy
                        && self
                            .control_rect(REFRESH)
                            .contains(self.pointer.0, self.pointer.1)
                    {
                        self.focus_index = REFRESH;
                        self.activate(REFRESH);
                    } else if self
                        .control_rect(7)
                        .contains(self.pointer.0, self.pointer.1)
                    {
                        self.activate(7);
                    } else if self
                        .control_rect(8)
                        .contains(self.pointer.0, self.pointer.1)
                    {
                        self.activate(8);
                    } else if self.show_hierarchy
                        && self
                            .control_rect(4)
                            .contains(self.pointer.0, self.pointer.1)
                    {
                        let row = ((y - TREE_TOP) / ROW) as usize + self.hierarchy_scroll;
                        if let Some(tree_row) = self.hierarchy_rows().get(row).copied() {
                            if tree_row.has_children
                                && self
                                    .hierarchy_disclosure_contains(tree_row.depth, self.pointer.0)
                            {
                                self.pause_refresh();
                                self.select(Some(tree_row.index));
                                self.toggle_hierarchy_row(tree_row.index);
                                self.hierarchy_click = None;
                                self.focus_index = 4;
                            } else {
                                result.reveal_source = self
                                    .click_hierarchy_row(tree_row.index, std::time::Instant::now());
                            }
                        }
                    } else if !self.show_hierarchy && y >= DETAILS_TOP {
                        self.focus_index = 5;
                        match self.details_hit() {
                            Some(details::Hit::Crumb(index)) => self.go_to(index),
                            Some(details::Hit::Fold) => {
                                self.notes_open = !self.notes_open;
                                self.rebuild_lines();
                            }
                            None => {}
                        }
                    }
                    result.redraw = true;
                    self.dirty_semantics = true;
                } else if self.picking {
                    self.select(
                        self.snapshot
                            .pick(self.pointer.0 as f64, self.pointer.1 as f64),
                    );
                    self.pause_refresh();
                    self.focus = true;
                    self.focus_index = if self.show_hierarchy { 4 } else { 5 };
                    result.redraw = true;
                    self.dirty_semantics = true;
                } else {
                    self.focus = false;
                    self.invalidate_selection();
                }
            }
            WindowEvent::MouseWheel { delta, .. }
                if self.bounds.contains(self.pointer.0, self.pointer.1) =>
            {
                self.hierarchy_click = None;
                let y = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                if self.show_hierarchy {
                    self.hierarchy_scroll = if y > 0.0 {
                        self.hierarchy_scroll.saturating_sub(3)
                    } else {
                        (self.hierarchy_scroll + 3)
                            .min(self.hierarchy_rows().len().saturating_sub(1))
                    };
                } else {
                    self.scroll = if y > 0.0 {
                        self.scroll.saturating_sub(ROW as usize * 3)
                    } else {
                        self.scroll.saturating_add(ROW as usize * 3)
                    };
                }
                result.consumed = true;
                result.redraw = true;
            }
            WindowEvent::KeyboardInput { event, .. } if self.focus || self.picking => {
                result.consumed = true;
                if !event.state.is_pressed() {
                    return result;
                }
                result.redraw = true;
                self.dirty_semantics = true;
                match &event.logical_key {
                    Key::Named(NamedKey::Escape) => {
                        if self.picking {
                            self.pause_refresh();
                            self.focus = true;
                        } else {
                            result.close = true;
                        }
                    }
                    Key::Named(NamedKey::Tab) => {
                        let order = self.focus_order();
                        let current = order
                            .iter()
                            .position(|i| *i == self.focus_index)
                            .unwrap_or(0);
                        self.focus_index = order[(current
                            + if mods.shift_key() { order.len() - 1 } else { 1 })
                            % order.len()];
                    }
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        result.close = self.activate(self.focus_index);
                    }
                    Key::Named(key @ (NamedKey::ArrowLeft | NamedKey::ArrowRight))
                        if mods.alt_key() =>
                    {
                        self.go(*key == NamedKey::ArrowLeft);
                    }
                    Key::Named(
                        key @ (NamedKey::ArrowUp
                        | NamedKey::ArrowDown
                        | NamedKey::ArrowLeft
                        | NamedKey::ArrowRight),
                    ) => {
                        let forward = matches!(key, NamedKey::ArrowDown | NamedKey::ArrowRight);
                        if matches!(self.focus_index, 7 | 8) {
                            self.activate(if forward { 7 } else { 8 });
                        } else if self.focus_index == 6 {
                            result.placement_changed = true;
                            let delta = if forward { 12.0 } else { -12.0 };
                            if matches!(key, NamedKey::ArrowLeft | NamedKey::ArrowRight) {
                                self.bounds.x += delta;
                            } else {
                                self.bounds.y += delta;
                            }
                        } else if self.focus_index == 5 {
                            self.scroll = if forward {
                                self.scroll.saturating_add(ROW as usize)
                            } else {
                                self.scroll.saturating_sub(ROW as usize)
                            };
                        } else {
                            self.navigate(*key);
                            self.focus_index = 4;
                        }
                    }
                    Key::Named(NamedKey::PageDown) if self.show_hierarchy => {
                        self.hierarchy_scroll = (self.hierarchy_scroll + 8)
                            .min(self.hierarchy_rows().len().saturating_sub(1));
                    }
                    Key::Named(NamedKey::PageUp) if self.show_hierarchy => {
                        self.hierarchy_scroll = self.hierarchy_scroll.saturating_sub(8)
                    }
                    Key::Named(NamedKey::PageDown) => {
                        self.scroll = self.scroll.saturating_add(self.control_rect(5).h as usize)
                    }
                    Key::Named(NamedKey::PageUp) => {
                        self.scroll = self.scroll.saturating_sub(self.control_rect(5).h as usize)
                    }
                    _ => {}
                }
            }
            WindowEvent::Ime(_) if self.focus || self.picking => {
                // An IME commit is text input too, even when it has no key event.
                result.consumed = true;
            }
            WindowEvent::Focused(false) => {
                self.hierarchy_click = None;
                self.scrollbar_drag = None;
                result.placement_changed = self.drag.take().is_some();
                self.captured_press = false;
            }
            WindowEvent::MouseWheel { .. }
            | WindowEvent::KeyboardInput { .. }
            | WindowEvent::Resized(_)
            | WindowEvent::ScaleFactorChanged { .. } => self.invalidate_selection(),
            _ => {}
        }
        if result.placement_changed {
            self.constrain(self.viewport.0, self.viewport.1);
        }
        self.clamp_scroll();
        result.structure_changed = previous_tab != self.show_hierarchy;
        result
    }

    fn navigate(&mut self, key: NamedKey) {
        self.pause_refresh();
        self.show_tab(true);
        let rows = self.hierarchy_rows();
        if rows.is_empty() {
            return;
        }
        let position = self
            .selected
            .and_then(|selected| rows.iter().position(|row| row.index == selected))
            .unwrap_or(0);
        let current = rows[position];
        let next = match key {
            NamedKey::ArrowLeft if current.has_children && !current.collapsed => {
                self.collapsed_hierarchy.insert(current.index);
                current.index
            }
            NamedKey::ArrowLeft => self.snapshot.records[current.index]
                .parent
                .unwrap_or(current.index),
            NamedKey::ArrowRight if current.has_children && current.collapsed => {
                self.collapsed_hierarchy.remove(&current.index);
                current.index
            }
            NamedKey::ArrowRight => rows
                .get(position + 1)
                .filter(|row| row.depth == current.depth + 1)
                .map_or(current.index, |row| row.index),
            NamedKey::ArrowUp => rows[position.saturating_sub(1)].index,
            _ => rows[(position + 1).min(rows.len() - 1)].index,
        };
        self.select(Some(next));
        self.picking = false;
    }

    fn activate(&mut self, index: usize) -> bool {
        match index {
            PICK => {
                if self.picking {
                    self.pause_refresh();
                } else {
                    self.resume_refresh();
                }
                self.focus = true;
            }
            COPY => {
                let report = serde_json::to_string_pretty(&serde_json::json!({
                    "schema": 1,
                    "host": "koru-workspace-host",
                    "platform": std::env::consts::OS,
                    "host_version": env!("CARGO_PKG_VERSION"),
                    "appearance": if theme::is_dark() { "dark" } else { "light" },
                    "viewport_logical": self.viewport,
                    "view_revision": self.view_revision,
                    "selected": self.selected,
                    "stale": self.stale,
                    "refresh_paused": self.refresh_paused,
                    "spacing_rays": self.spacing_rays,
                    "snapshot": self.snapshot,
                    "accessibility_candidates": self.semantic_details,
                    "accessibility_coverage": "last prepared AT publication; geometric candidates only",
                }));
                self.status = Some(
                    if report
                        .ok()
                        .is_some_and(|text| koru_clipboard::system().set_text(&text).is_ok())
                    {
                        CopyKey::InspectorCopied
                    } else {
                        CopyKey::InspectorCopyFailed
                    },
                );
            }
            REFRESH => {
                if self.refresh_paused {
                    self.resume_refresh();
                } else {
                    self.pause_refresh();
                }
            }
            CLOSE => return true,
            PIN => self.pinned = !self.pinned,
            RAYS => self.spacing_rays = !self.spacing_rays,
            4 if self.show_hierarchy && self.selected.is_some() => {
                self.pause_refresh();
                return self.activate(7);
            }
            7 | 8 => {
                let was_hierarchy = self.show_hierarchy;
                self.show_tab(index == 8);
                self.tab_shown(was_hierarchy);
            }
            _ => {}
        }
        self.dirty_semantics = true;
        false
    }

    /// What follows a tab change, whether clicked or walked to through the
    /// history.
    fn tab_shown(&mut self, was_hierarchy: bool) {
        self.hierarchy_click = None;
        if self.show_hierarchy {
            self.pause_refresh();
            if !was_hierarchy && let Some(selected) = self.selected {
                let visible = (self.control_rect(4).h / ROW).floor().max(1.0) as usize;
                self.hierarchy_scroll = selected.saturating_sub(visible / 2);
                self.clamp_scroll();
            }
        }
        self.focus_index = if self.show_hierarchy { 8 } else { 7 };
        self.focus = true;
    }

    fn pause_refresh(&mut self) {
        self.picking = false;
        self.refresh_paused = true;
    }

    /// F12 resumes a paused inspection; only a live inspection asks to close.
    /// Both native windows use this transition after filtering key replays.
    pub fn shortcut_requests_close(&mut self) -> bool {
        if self.refresh_paused {
            self.resume_refresh();
            self.dirty_semantics = true;
            false
        } else {
            true
        }
    }

    /// Some means the inspector owns F11; its bool says a repaint is needed.
    pub fn spacing_shortcut(
        &mut self,
        event: &WindowEvent,
        mods: winit::keyboard::ModifiersState,
    ) -> Option<bool> {
        let WindowEvent::KeyboardInput {
            event,
            is_synthetic,
            ..
        } = event
        else {
            return None;
        };
        if !koru_keys::registry::get(ShortcutId::ToggleInspectorSpacingRays)
            .chord
            .matches(&event.logical_key, event.text.as_deref(), mods)
        {
            return None;
        }
        let changed = shortcut_pressed(event.state, event.repeat, *is_synthetic);
        if changed {
            self.activate(RAYS);
        }
        Some(changed)
    }

    fn resume_refresh(&mut self) {
        self.refresh_paused = false;
        self.picking = true;
    }

    pub fn wants_arrow_cursor(&self) -> bool {
        if self.detached {
            return self.picking;
        }
        self.picking || self.bounds.contains(self.pointer.0, self.pointer.1) || self.drag.is_some()
    }

    pub fn capture_live(&self) -> bool {
        !self.refresh_paused
    }

    pub fn rebuild_lines(&mut self) {
        self.lines.clear();
        let lang = crate::accessibility::ui_lang();
        let coverage = if self.snapshot.truncated {
            CopyKey::InspectorLimit
        } else if self.selected.and_then(|i| self.snapshot.owner(i)).is_some() {
            CopyKey::InspectorRegistered
        } else if self
            .selected
            .is_some_and(|i| is_unregistered(&self.snapshot, i))
        {
            CopyKey::InspectorUnregistered
        } else if self.selected.is_some() {
            CopyKey::InspectorUnknown
        } else {
            CopyKey::InspectorEmpty
        };
        self.details_lines.clear();
        let Some(index) = self.selected else {
            self.lines.push(t(lang, coverage).into());
            self.details_lines
                .push(details::Line::Prose(t(lang, coverage).into()));
            return;
        };
        if index >= self.snapshot.records.len() {
            return;
        }
        let lines = self.details(index, coverage);
        self.lines = lines.iter().map(details::Line::text).collect();
        self.details_lines = lines;
    }

    /// Pausing freezes the visual selection as well as its measurements. Stale
    /// describes the age of that evidence; it must not erase its highlight.
    fn highlighted_record(&self) -> Option<(usize, &inspect::Record)> {
        let index = self.selected?;
        self.snapshot
            .records
            .get(index)
            .map(|record| (index, record))
    }

    pub fn draw_overlay(&mut self, canvas: &mut Scene, root: Affine) {
        if self.spacing_rays {
            self.draw_spacing_rays(canvas, root);
        }
        let p = theme::p();
        if let Some((index, record)) = self.highlighted_record() {
            let r = record.visible();
            scene::fill(canvas, root, p.accent.multiply_alpha(0.12), &r);
            scene::stroke(canvas, root, p.accent, 1.5, &r);
            if let Some(owner) = self.snapshot.owner(index) {
                scene::stroke(
                    canvas,
                    root,
                    p.text_dim,
                    1.0,
                    &self.snapshot.records[owner].visible(),
                );
            }
            let font = font_hint(record);
            let label = format!(
                "{} · {:7.1} × {:7.1}",
                selection_name(&self.snapshot, index),
                r.width(),
                r.height()
            );
            let width = self.viewport.0.min(440.0);
            let height = ROW * if font.is_some() { 2.0 } else { 1.0 } + 4.0;
            if let Some(hint) = hint_bounds(r, self.viewport, width, height) {
                let (x, y) = (hint.x, hint.y);
                scene::floating_panel(
                    canvas,
                    root,
                    x,
                    y,
                    width,
                    height,
                    &scene::Panel::tooltip(theme::PAGE_RADIUS),
                );
                // Tooltip surfaces stay dark in both themes. Pair their background
                // with bubble_fg, rather than the normal panel text foreground.
                self.diagnostic_text(
                    canvas,
                    root,
                    &label,
                    x + 6.0,
                    y + 2.0,
                    width - 12.0,
                    p.bubble_fg,
                );
                if let Some(font) = font {
                    self.diagnostic_text(
                        canvas,
                        root,
                        &font,
                        x + 6.0,
                        y + ROW + 2.0,
                        width - 12.0,
                        p.bubble_fg,
                    );
                }
            }
        }
    }

    pub fn draw(&mut self, canvas: &mut Scene, root: Affine) {
        let lang = crate::accessibility::ui_lang();
        let p = theme::p();
        let b = self.bounds;
        scene::floating_panel(
            canvas,
            root,
            b.x,
            b.y,
            b.w,
            b.h,
            &scene::Panel::popover(theme::PAGE_RADIUS),
        );
        let clip = scene::rect(b.x, b.y, b.w, b.h);
        canvas.push_clip_layer(vello::peniko::Fill::NonZero, root, &clip);
        let hint = show_hint(ShortcutId::ToggleUiInspector, Section::Tooltip).unwrap_or_default();
        let hovered = [PICK, COPY, RAYS, PIN].into_iter().find(|i| {
            self.control_rect(*i)
                .contains(self.pointer.0, self.pointer.1)
        });
        let title = if let Some(status) = self.status {
            t(lang, status).to_string()
        } else if hovered == Some(RAYS) {
            format!(
                "{} · {}",
                t(lang, CopyKey::InspectorSpacingRays),
                show_hint(ShortcutId::ToggleInspectorSpacingRays, Section::Tooltip)
                    .unwrap_or_default()
            )
        } else if let Some(index) = hovered {
            t(
                lang,
                match index {
                    PICK if self.picking => CopyKey::InspectorPause,
                    PICK => CopyKey::InspectorPick,
                    COPY => CopyKey::InspectorCopy,
                    _ => CopyKey::TerminalWindowAlwaysOnTop,
                },
            )
            .to_string()
        } else {
            format!("{} · {hint}", t(lang, CopyKey::InspectorTitle))
        };
        self.text(
            canvas,
            root,
            &title,
            b.x + PAD,
            b.y + 6.0,
            b.w - 2.0 * PAD - HEADER * 5.0,
        );
        for (index, glyph) in [
            (
                PICK,
                if self.picking {
                    koru_draw::icons::PAUSE
                } else {
                    koru_draw::icons::CURSOR
                },
            ),
            (COPY, koru_draw::icons::COPY),
            (RAYS, koru_draw::icons::RULER),
        ] {
            let rect = self.control_rect(index);
            koru_ui::icon_button::IconButton::new(glyph, rect)
                .with_state(koru_ui::icon_button::State {
                    hovered: rect.contains(self.pointer.0, self.pointer.1),
                    active: index == RAYS && self.spacing_rays,
                    ..Default::default()
                })
                .draw(
                    canvas,
                    root,
                    self.engine.get_or_insert_with(TextEngine::new),
                );
        }
        let pin = self.control_rect(PIN);
        koru_ui::icon_button::IconButton::new(koru_draw::icons::PUSH_PIN, pin)
            .with_state(koru_ui::icon_button::State {
                hovered: pin.contains(self.pointer.0, self.pointer.1),
                active: self.pinned,
                ..Default::default()
            })
            .draw(
                canvas,
                root,
                self.engine.get_or_insert_with(TextEngine::new),
            );
        let close = self.control_rect(CLOSE);
        koru_ui::icon_button::IconButton::new(koru_draw::icons::WINDOW_CLOSE, close)
            .with_state(koru_ui::icon_button::State {
                hovered: close.contains(self.pointer.0, self.pointer.1),
                ..Default::default()
            })
            .draw(
                canvas,
                root,
                self.engine.get_or_insert_with(TextEngine::new),
            );
        for (index, key) in [
            (8, CopyKey::InspectorHierarchy),
            (7, CopyKey::InspectorDetails),
        ] {
            let selected = self.show_hierarchy == (index == 8);
            let rect = self.control_rect(index);
            koru_ui::button::ActionButton::new(
                t(lang, key),
                if selected {
                    koru_ui::button::ActionStyle::Secondary
                } else {
                    koru_ui::button::ActionStyle::Ghost
                },
                None,
            )
            .draw_focused(
                canvas,
                root,
                self.engine.get_or_insert_with(TextEngine::new),
                rect,
                false,
                self.focus && self.focus_index == index,
            );
        }
        if self.show_hierarchy {
            self.draw_hierarchy(canvas, root);
        } else {
            self.draw_box(canvas, root);
            self.draw_details(canvas, root);
        }
        self.draw_scrollbar(canvas, root);
        self.draw_box_tip(canvas, root);
        if self.focus {
            let focus = self.control_rect(self.focus_index);
            scene::stroke(
                canvas,
                root,
                p.accent,
                2.0,
                &scene::rect(focus.x, focus.y, focus.w, focus.h),
            );
        }
        canvas.pop_layer();
    }

    fn text(&mut self, canvas: &mut Scene, root: Affine, text: &str, x: f32, y: f32, width: f32) {
        let engine = self.engine.get_or_insert_with(TextEngine::new);
        let label = engine.elided(text, theme::role::meta(), width.max(0.0));
        let layout = engine.simple_layout(&label, theme::role::meta());
        scene::draw_layout_first_line(canvas, root, &layout, x, y, Some(theme::p().text));
    }

    fn diagnostic_text(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        text: &str,
        x: f32,
        y: f32,
        width: f32,
        color: vello::peniko::Color,
    ) {
        let engine = self.engine.get_or_insert_with(TextEngine::new);
        let label = engine.elided_in(text, diagnostic_style(), diagnostic_stack(), width.max(0.0));
        let layout = engine.simple_layout_in(&label, diagnostic_style(), diagnostic_stack());
        scene::draw_layout_first_line(canvas, root, &layout, x, y, Some(color));
    }

    fn measurement_text(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        text: &str,
        x: f32,
        y: f32,
        width: f32,
    ) {
        self.diagnostic_text(canvas, root, text, x, y, width, theme::p().text);
    }

    /// The selected element inside its parent: each edge names the
    /// padding, gap and siblings that make it, or says it was measured; the
    /// numbers are on hover.
    fn draw_box(&mut self, canvas: &mut Scene, root: Affine) {
        self.box_tips.clear();
        let Some(index) = self.selected else {
            return;
        };
        let Some(record) = self.snapshot.records.get(index) else {
            return;
        };
        let selected = record.rect();
        let rule = details::size_rule(record);
        let parent_name = record
            .parent
            .and_then(|i| self.snapshot.records.get(i))
            .map(|r| display_name(r).to_string());
        let edges = details::edges(&self.snapshot, index);
        let p = theme::p();
        let b = self.bounds;
        const SIDE: f32 = 112.0;
        let outer = scene::rect(b.x + PAD, b.y + BODY_TOP, b.w - PAD * 2.0, 102.0);
        let inner = Rect::new(
            b.x + PAD + SIDE,
            b.y + BODY_TOP + 32.0,
            (b.w - 2.0 * (PAD + SIDE)).max(1.0),
            38.0,
        );
        let inner_shape = scene::rect(inner.x, inner.y, inner.w, inner.h);
        scene::fill(canvas, root, p.hover_tint, &outer);
        scene::stroke(canvas, root, p.text_dim, 1.0, &outer);
        scene::fill(canvas, root, p.accent.multiply_alpha(0.14), &inner_shape);
        scene::stroke(canvas, root, p.accent, 1.0, &inner_shape);
        let size = format!("{:.1} × {:.1} px", selected.width(), selected.height());
        let (label, tip) = match rule {
            Some(rule) => (rule, size),
            None => (size.clone(), format!("{size}, measured")),
        };
        self.measurement_text(
            canvas,
            root,
            &label,
            inner.x + 8.0,
            inner.y + 8.0,
            inner.w - 16.0,
        );
        self.box_tips.push((inner, tip));
        let (Some(parent_name), Some([left, top, right, bottom])) = (parent_name, edges) else {
            self.text(
                canvas,
                root,
                "Parent: unavailable",
                inner.x,
                b.y + BODY_TOP + 4.0,
                inner.w,
            );
            return;
        };
        let ink = |edge: &details::Edge| if edge.token { p.accent } else { p.text_dim };
        let line = Rect::new(inner.x, b.y + BODY_TOP + 2.0, inner.w, ROW);
        self.diagnostic_text(
            canvas,
            root,
            &format!("{parent_name} · top {}", top.short),
            line.x,
            line.y + 2.0,
            line.w,
            ink(&top),
        );
        self.box_tips.push((line, format!("top\n{}", top.full)));
        let line = Rect::new(inner.x, b.y + BODY_TOP + 74.0, inner.w, ROW);
        self.diagnostic_text(
            canvas,
            root,
            &format!("bottom {}", bottom.short),
            line.x,
            line.y + 2.0,
            line.w,
            ink(&bottom),
        );
        self.box_tips
            .push((line, format!("bottom\n{}", bottom.full)));
        for (edge, x, name) in [
            (&left, b.x + PAD, "left"),
            (&right, b.x + b.w - PAD - SIDE, "right"),
        ] {
            let parts: Vec<&str> = edge.short.split(" + ").collect();
            for (n, part) in parts.iter().take(3).enumerate() {
                let text = match n {
                    0 => (*part).to_string(),
                    2 if parts.len() > 3 => "+ …".to_string(),
                    _ => format!("+ {part}"),
                };
                self.diagnostic_text(
                    canvas,
                    root,
                    &text,
                    x + 6.0,
                    b.y + BODY_TOP + 28.0 + n as f32 * 16.0,
                    SIDE - 12.0,
                    ink(edge),
                );
            }
            self.box_tips.push((
                Rect::new(x, b.y + BODY_TOP + 26.0, SIDE, 50.0),
                format!("{name}\n{}", edge.full),
            ));
        }
    }

    /// The hovered box label's numbers.
    fn draw_box_tip(&mut self, canvas: &mut Scene, root: Affine) {
        if self.show_hierarchy {
            return;
        }
        let Some(text) = self
            .box_tips
            .iter()
            .find(|(r, _)| r.contains(self.pointer.0, self.pointer.1))
            .map(|(_, text)| text.clone())
        else {
            return;
        };
        let lines: Vec<&str> = text.lines().collect();
        let b = self.bounds;
        let width = (b.w - 2.0 * PAD).min(380.0);
        let height = ROW * lines.len() as f32 + 6.0;
        let x = (self.pointer.0 + 12.0)
            .min(b.x + b.w - width - 4.0)
            .max(b.x + 4.0);
        let y = (self.pointer.1 + 18.0).min(b.y + b.h - height - 4.0);
        scene::floating_panel(
            canvas,
            root,
            x,
            y,
            width,
            height,
            &scene::Panel::tooltip(theme::PAGE_RADIUS),
        );
        for (n, line) in lines.iter().enumerate() {
            self.diagnostic_text(
                canvas,
                root,
                line,
                x + 6.0,
                y + 3.0 + ROW * n as f32,
                width - 12.0,
                theme::p().bubble_fg,
            );
        }
    }

    fn control_rect(&self, index: usize) -> Rect {
        let b = self.bounds;
        match index {
            PICK => Rect::new(b.x + b.w - HEADER * 5.0, b.y + 3.0, 28.0, 28.0),
            COPY => Rect::new(b.x + b.w - HEADER * 4.0, b.y + 3.0, 28.0, 28.0),
            RAYS => Rect::new(b.x + b.w - HEADER * 3.0, b.y + 3.0, 28.0, 28.0),
            REFRESH => Rect::new(b.x + PAD, b.y + BODY_TOP, b.w - 2.0 * PAD, 28.0),
            CLOSE => Rect::new(b.x + b.w - HEADER, b.y + 3.0, 28.0, 28.0),
            PIN => Rect::new(b.x + b.w - HEADER * 2.0, b.y + 3.0, 28.0, 28.0),
            4 => Rect::new(
                b.x + PAD,
                b.y + TREE_TOP,
                b.w - PAD * 2.0,
                (b.h - TREE_TOP - PAD).max(0.0),
            ),
            5 => Rect::new(
                b.x + PAD,
                b.y + DETAILS_TOP,
                b.w - PAD * 2.0,
                (b.h - DETAILS_TOP - PAD).max(0.0),
            ),
            7 | 8 => Rect::new(
                b.x + PAD + (8 - index) as f32 * (b.w - 2.0 * PAD) / 2.0,
                b.y + HEADER,
                (b.w - 2.0 * PAD) / 2.0,
                28.0,
            ),
            _ => Rect::new(b.x, b.y, (b.w - HEADER * 5.0).max(0.0), HEADER),
        }
    }

    pub fn semantics(&self, slot: WindowSlot, zoom: f32) -> Vec<SemanticNode> {
        let lang = crate::accessibility::ui_lang();
        let labels = [
            t(
                lang,
                if self.picking {
                    CopyKey::InspectorPause
                } else {
                    CopyKey::InspectorPick
                },
            )
            .to_string(),
            t(lang, CopyKey::InspectorCopy).into(),
            t(
                lang,
                if self.refresh_paused {
                    CopyKey::InspectorResumeRefresh
                } else {
                    CopyKey::InspectorPauseRefresh
                },
            )
            .into(),
            t(lang, CopyKey::InspectorClose).into(),
            format!(
                "{}: {}",
                t(lang, CopyKey::InspectorHierarchy),
                self.hierarchy_description()
            ),
            self.lines.join("\n"),
            t(lang, CopyKey::InspectorTitle).into(),
            t(lang, CopyKey::InspectorDetails).into(),
            t(lang, CopyKey::InspectorHierarchy).into(),
            t(lang, CopyKey::TerminalWindowAlwaysOnTop).into(),
            t(lang, CopyKey::InspectorSpacingRays).into(),
        ];
        let order = self.focus_order();
        let mut nodes: Vec<_> = labels
            .into_iter()
            .enumerate()
            .filter(|(index, _)| order.contains(index))
            .map(|(index, label)| {
                let r = self.control_rect(index);
                let node = SemanticNode::new(
                    Occurrence::window_command(slot, ControlPart::new(PARTS[index])),
                    if matches!(index, 7 | 8) {
                        Role::Tab
                    } else if index < 4 || index == PIN || index == RAYS {
                        Role::Button
                    } else {
                        Role::Label
                    },
                )
                .with_label(label)
                .with_bounds(Rect::new(r.x * zoom, r.y * zoom, r.w * zoom, r.h * zoom))
                .with_action(Action::Focus);
                if index == RAYS {
                    node.with_action(Action::Click)
                        .with_toggled(self.spacing_rays)
                } else if index == PIN {
                    node.with_action(Action::Click).with_toggled(self.pinned)
                } else if matches!(index, 7 | 8) {
                    node.with_action(Action::Click)
                        .with_selected(self.show_hierarchy == (index == 8))
                } else if index < 4 {
                    node.with_action(Action::Click)
                } else {
                    node
                }
            })
            .collect();
        nodes.sort_by_key(|node| {
            order
                .iter()
                .position(|index| node.occurrence.part == ControlPart::new(PARTS[*index]))
        });
        nodes
    }

    /// Read the existing publication, never ask the document to build another
    /// semantic tree for pointer motion. Retain no labels or text values.
    pub fn observe_semantics(&mut self, nodes: &[SemanticNode]) {
        if self.refresh_paused {
            return;
        }
        self.semantic_candidates.clear();
        fn collect(nodes: &[SemanticNode], result: &mut Vec<(Rect, String)>, budget: &mut usize) {
            for node in nodes {
                if *budget == 0 {
                    return;
                }
                *budget -= 1;
                if PARTS
                    .iter()
                    .any(|p| node.occurrence.part == ControlPart::new(p))
                {
                    continue;
                }
                if result.len() >= 512 {
                    return;
                }
                if let Some(bounds) = node.bounds {
                    result.push((bounds, format!("role={:?}; name source={}; actions={:?}; disabled={}; selected={:?}; semantic bounds={bounds:?}; occurrence={}",
                        node.role, if node.label.is_some() { "host label (value omitted)" } else { "no host label" }, node.actions, node.disabled, node.selected, node.occurrence.locator())));
                }
                collect(&node.children, result, budget);
            }
        }
        collect(nodes, &mut self.semantic_candidates, &mut 512);
        self.refresh_semantic_details();
    }

    pub fn refresh_semantic_details(&mut self) {
        self.semantic_details.clear();
        if let Some(r) = self.selected.and_then(|i| self.snapshot.records.get(i)) {
            let p = r.rect().center();
            for (bounds, detail) in &self.semantic_candidates {
                if bounds.contains(p.x as f32, p.y as f32) {
                    self.semantic_details.push(format!("Geometric candidate from last AT publication (not an identity binding): {detail}"));
                    if self.semantic_details.len() >= 4 {
                        break;
                    }
                }
            }
        }
        self.rebuild_lines();
    }

    pub fn focused(&self, slot: WindowSlot) -> Option<Occurrence> {
        self.focus
            .then(|| Occurrence::window_command(slot, ControlPart::new(PARTS[self.focus_index])))
    }

    pub fn accessibility_action(&mut self, part: ControlPart, action: Action) -> Option<bool> {
        let index = PARTS.iter().position(|p| ControlPart::new(p) == part)?;
        self.focus = true;
        self.focus_index = index;
        self.dirty_semantics = true;
        Some(action == Action::Click && self.activate(index))
    }
}

fn same_ancestry(previous: &Snapshot, next: &Snapshot, mut index: usize) -> bool {
    loop {
        let Some((a, b)) = previous.records.get(index).zip(next.records.get(index)) else {
            return false;
        };
        if a.source != b.source
            || a.parent != b.parent
            || a.bounds != b.bounds
            || a.clip != b.clip
            || a.name != b.name
            || a.kind != b.kind
        {
            return false;
        }
        match a.parent {
            Some(parent) => index = parent,
            None => return true,
        }
    }
}

fn is_unregistered(snapshot: &Snapshot, mut index: usize) -> bool {
    loop {
        let Some(record) = snapshot.records.get(index) else {
            return false;
        };
        if record.kind == "unregistered" {
            return true;
        }
        if record.kind == "component" {
            return false;
        }
        match record.parent {
            Some(parent) => index = parent,
            None => return false,
        }
    }
}

#[derive(Default)]
pub(crate) struct EventResult {
    /// Explicitly opening a hierarchy node also reveals the inspected window.
    pub reveal_source: bool,
    pub drag_window: bool,
    pub consumed: bool,
    pub redraw: bool,
    pub close: bool,
    pub placement_changed: bool,
    pub structure_changed: bool,
}

fn selection_name(snapshot: &Snapshot, index: usize) -> String {
    let record = &snapshot.records[index];
    if let Some(name) = icon_name(snapshot, index) {
        return match snapshot.owner(index) {
            Some(owner) => format!("{} / {name}", snapshot.records[owner].name),
            None => format!(
                "{} / {name}",
                icon_property(snapshot, index, "icon pack").unwrap_or("Icon")
            ),
        };
    }
    match snapshot.owner(index) {
        Some(owner) if snapshot.records[owner].name != record.name => {
            format!(
                "{} / {}",
                display_name(&snapshot.records[owner]),
                display_name(record)
            )
        }
        _ => display_name(record).to_string(),
    }
}

fn display_name(record: &inspect::Record) -> &str {
    match record.name {
        "icon" => "Icon",
        "text run"
            if property(record, "rendered family")
                .is_some_and(|family| family.starts_with("Phosphor")) =>
        {
            "Phosphor icon"
        }
        "text run" => "Text",
        "text line" | "label line" => "Text line",
        name => name,
    }
}

/// Use recorded icon identity, never a guess from its visual shape. A nested
/// component/local boundary must not borrow an unrelated outer icon's name.
fn icon_name(snapshot: &Snapshot, index: usize) -> Option<&str> {
    icon_property(snapshot, index, "icon name")
}

fn icon_property<'a>(snapshot: &'a Snapshot, mut index: usize, key: &str) -> Option<&'a str> {
    loop {
        let record = snapshot.records.get(index)?;
        if let Some(name) = property(record, key) {
            return Some(name);
        }
        if matches!(record.kind, "component" | "unregistered") {
            return None;
        }
        index = record.parent?;
    }
}

fn diagnostic_stack() -> &'static str {
    theme::default_code_font().stack()
}
fn diagnostic_style() -> theme::TextStyle {
    theme::role::meta().with_weight(theme::FontWeight::Regular)
}

fn property<'a>(record: &'a inspect::Record, name: &str) -> Option<&'a str> {
    record
        .properties
        .iter()
        .find(|p| p.name == name)
        .map(|p| p.value.as_str())
}

fn font_hint(record: &inspect::Record) -> Option<String> {
    let size = property(record, "font size")?;
    Some(format!(
        "{} · {} · {} · {} px",
        property(record, "rendered family").unwrap_or("Family unavailable"),
        weight_label(property(record, "font weight").unwrap_or("unavailable")),
        property(record, "font style").unwrap_or("Style unavailable"),
        size.parse::<f32>()
            .map_or_else(|_| size.to_string(), |size| format!("{size:.1}")),
    ))
}

fn weight_label(value: &str) -> String {
    let name = match value {
        "100" => "Thin",
        "200" => "Extra Light",
        "300" => "Light",
        "400" => "Regular",
        "500" => "Medium",
        "600" => "Semi Bold",
        "700" => "Bold",
        "800" => "Extra Bold",
        "900" => "Black",
        _ => return value.to_string(),
    };
    format!("{value} {name}")
}

fn typography_details(snapshot: &Snapshot, index: usize) -> Vec<String> {
    let record = &snapshot.records[index];
    let Some(size) = property(record, "font size") else {
        return Vec::new();
    };
    let mut lines = vec![
        format!(
            "Font family: {}",
            property(record, "rendered family").unwrap_or("unavailable")
        ),
        format!(
            "Font weight: {}",
            weight_label(property(record, "font weight").unwrap_or("unavailable"))
        ),
        format!(
            "Font style: {}",
            property(record, "font style").unwrap_or("unavailable")
        ),
        format!("Font size: {size} px (logical)"),
    ];
    let mut parent = record.parent;
    let mut role = None;
    while let Some(index) = parent {
        let ancestor = &snapshot.records[index];
        if let Some(leading) = property(ancestor, "line height") {
            lines.push(format!("Line height: {leading} px (logical)"));
        }
        if let Some(p) = ancestor
            .properties
            .iter()
            .find(|p| p.name == "font size / line height")
        {
            role = Some(p.origin);
        }
        parent = ancestor.parent;
    }
    lines.push(format!("Text role: {}", role.unwrap_or("not exposed")));
    if let Some(skew) = property(record, "synthetic italic") {
        lines.push(format!("Synthetic italic: {skew}"));
    }
    lines.push("Rendering unit: text run (one font/style); not a named Koru text role.".into());
    lines
}

fn rectangle_numbers(r: [f64; 4]) -> String {
    format!("[{:.1}, {:.1}, {:.1}, {:.1}]", r[0], r[1], r[2], r[3])
}

/// Prefer above, then below, then either side. Never obscure the inspected bounds.
fn hint_bounds(
    target: vello::kurbo::Rect,
    viewport: (f32, f32),
    width: f32,
    height: f32,
) -> Option<Rect> {
    let gap = 6.0;
    if width > viewport.0 || height > viewport.1 {
        return None;
    }
    let x = (target.x0 as f32).clamp(0.0, viewport.0 - width);
    let y = (target.y0 as f32).clamp(0.0, viewport.1 - height);
    [
        Rect::new(x, target.y0 as f32 - height - gap, width, height),
        Rect::new(x, target.y1 as f32 + gap, width, height),
        Rect::new(target.x0 as f32 - width - gap, y, width, height),
        Rect::new(target.x1 as f32 + gap, y, width, height),
    ]
    .into_iter()
    .find(|r| r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= viewport.0 && r.y + r.h <= viewport.1)
}

#[cfg(test)]
mod tests;
