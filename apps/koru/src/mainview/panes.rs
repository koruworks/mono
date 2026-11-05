// SPDX-License-Identifier: MPL-2.0

//! Per-pane tab groups and split-view support.
//!
//! A `PaneTree` is a binary tree of `PaneNode`s: each leaf is a `TabGroup`,
//! and each internal node splits its space either horizontally or vertically.
//! The focused path names which leaf owns the keyboard.

// The split-intent helpers and some group operations are not wired yet; they
// are kept for the cross-pane drag/split step that follows.
#![allow(dead_code)]

use std::time::Instant;

use koru_app_shell::tabstrip::TabStrip;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
use koru_shell_chrome::tabs;
use vello::Scene;
use vello::kurbo::Affine;

use super::tab::{Tab, TabView};
use super::tab_rules::KoruTabs;
use koru_app_shell::tabslide::TabSlides;

pub const TEAR_OFF_PX: f32 = 20.0;

/// The hairline between two split panes. One pixel of `panel_border`, the same
/// weight and colour the sidebar's content edge and every chrome band's closing
/// rule wear (`chrome::band_hairline`). It is *carved* out of the second child
/// rather than drawn over either pane, so nothing in a pane is ever covered by
/// the divider.
pub const DIVIDER_W: f32 = 1.0;

/// The smallest a pane may be dragged to: 400 wide, 200 tall. Below that a tab
/// strip stops being a strip and an editor stops being an editor.
pub const PANE_MIN_W: f32 = 400.0;
pub const PANE_MIN_H: f32 = 200.0;

/// The band a divider can be grabbed by, straddling the hairline — the same
/// reach the sidebar's resize grip gets (`layout::GRIP`).
pub const DIVIDER_GRIP: f32 = 8.0;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum SplitDir {
    Left,
    Right,
    Top,
    Bottom,
}

impl SplitDir {
    fn axis(self) -> SplitAxis {
        match self {
            Self::Left | Self::Right => SplitAxis::Vertical,
            Self::Top | Self::Bottom => SplitAxis::Horizontal,
        }
    }

    fn new_side(self) -> usize {
        match self {
            Self::Left | Self::Top => 0,
            Self::Right | Self::Bottom => 1,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PanePath(Vec<u8>);

impl PanePath {
    pub fn root() -> Self {
        Self(Vec::new())
    }

    pub fn child(&self, index: usize) -> Self {
        let mut path = self.0.clone();
        path.push(index as u8);
        Self(path)
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

/// A tab the pointer has hold of inside its own group strip.
#[derive(Clone, Debug)]
pub struct TabDrag {
    pub index: usize,
    pub grab_dx: f32,
    pub x: f32,
    pub origin_y: f32,
    pub y: f32,
}

/// A tab being dragged between groups or out of the window.
#[derive(Clone, Debug)]
pub struct PaneDrag {
    pub source: PanePath,
    pub tab_id: u64,
    pub grab_dx: f32,
    pub origin_y: f32,
    pub x: f32,
    pub y: f32,
}

/// A divider being dragged. `grab` is the pointer's offset from the divider's
/// near edge, so the hairline stays under the pixel it was grabbed at rather
/// than jumping its grip to centre. `moved` is what makes a press-and-release
/// on the hairline cost no reflow.
#[derive(Clone, Debug)]
pub struct DividerDrag {
    pub path: PanePath,
    pub axis: SplitAxis,
    pub grab: f32,
    pub moved: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum HitKind {
    Tab(usize),
    TabClose(usize),
    NewTab,
    TabScroll,
    Strip,
}

pub struct TabGroup {
    tabs: TabStrip<Tab>,
    pub(crate) scroll: f32,
    pub(crate) widths: Vec<f32>,
    pub(crate) widths_stale: bool,
    pub(crate) slides: TabSlides,
    pub(crate) drag: Option<TabDrag>,
    pub(crate) scroll_drag: Option<f32>,
    pub(crate) last_header: Option<Rect>,
}

impl TabGroup {
    pub fn new(first: Tab) -> Self {
        Self {
            tabs: TabStrip::new(first, Tab::welcome),
            scroll: 0.0,
            widths: Vec::new(),
            widths_stale: true,
            slides: TabSlides::new(),
            drag: None,
            scroll_drag: None,
            last_header: None,
        }
    }

    pub fn tabs(&self) -> &TabStrip<Tab> {
        &self.tabs
    }

    pub fn tabs_mut(&mut self) -> &mut TabStrip<Tab> {
        &mut self.tabs
    }

    pub fn active(&self) -> usize {
        self.tabs.active()
    }

    pub fn active_tab(&self) -> &Tab {
        self.tabs.active_tab()
    }

    pub fn active_mut(&mut self) -> &mut Tab {
        self.tabs.active_mut()
    }

    pub fn active_view(&mut self) -> &mut TabView {
        self.tabs.active_view()
    }

    pub fn select(&mut self, i: usize) -> bool {
        self.tabs.select(i)
    }

    pub fn ensure_widths(&mut self, engine: &mut TextEngine) {
        let len = self.tabs.len();
        if !self.widths_stale && self.widths.len() == len {
            return;
        }
        let pin_count = self.tabs.pinned_count();
        self.widths = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let has_icon = t.view.icon().is_some() || t.icon.is_some() || t.is_pinned();
                let label = if t.is_pinned() && !t.show_title {
                    ""
                } else {
                    &t.label
                };
                let w = tabs::preferred_width(engine, label, has_icon, !t.is_pinned());
                if pin_count > 0 && i + 1 == pin_count {
                    w + theme::TAB_PIN_GAP
                } else {
                    w
                }
            })
            .collect();
        self.widths_stale = false;
    }

    pub fn mark_widths_stale(&mut self) {
        self.widths_stale = true;
    }

    /// One strip entry, in `koru_shell_chrome::tabs`'s vocabulary.
    ///
    /// The icon chain answers the same question the window-level strip always
    /// did: a Tasks tab over a list wears that list's colour (the swatch is
    /// precomputed by `draw_strip` because the token is borrowed from it), a
    /// session tab shows its profile's brand mark, then the view glyph, the
    /// bin glyph for a root that has been trashed, the note file glyph, the
    /// tab's own emoji, and the pinned-document fallback.
    fn tab_item<'a>(
        t: &'a Tab,
        carried: Option<(usize, f32)>,
        i: usize,
        pin_count: usize,
        swatch: Option<&'a Option<String>>,
        notes: &'a std::collections::HashSet<koru_core::DocumentId>,
        ws: &'a koru_core::schema::WorkspaceDoc,
    ) -> tabs::Tab<'a> {
        let dragging = carried.is_some_and(|(j, _)| j == i);
        let slide = match carried {
            Some((j, dx)) if j == i => dx,
            _ => 0.0,
        };
        let icon = swatch
            .map(|token| tabs::TabIcon::Swatch(token.as_deref()))
            // A session tab's vector product mark takes the slot next; the
            // glyph chain below is unchanged and catches everything else
            // (including the same session when its profile has no brand mark).
            .or_else(|| t.view.brand().map(tabs::TabIcon::Brand))
            .or_else(|| {
                t.view
                    .icon()
                    .or_else(|| {
                        t.view
                            .root()
                            .filter(|r| ws.in_trash(*r))
                            .map(|_| koru_draw::icons::TRASH)
                    })
                    .map(tabs::TabIcon::Glyph)
            })
            .or_else(|| {
                // A note tab wears the file glyph: its label already names the
                // file, and the two together say what the tab is a note *of*.
                t.document
                    .filter(|d| notes.contains(d))
                    .map(|_| tabs::TabIcon::Glyph(koru_draw::icons::FILE))
            })
            .or_else(|| t.icon.as_deref().map(tabs::TabIcon::Emoji))
            .or_else(|| {
                t.is_pinned()
                    .then_some(tabs::TabIcon::Glyph(koru_draw::icons::DOCUMENT))
            });
        let label = if t.is_pinned() && !t.show_title {
            ""
        } else {
            &t.label
        };
        // The pin group's gap rides in the last pin's width (`ensure_widths`);
        // the chip stops short of it so the pin is its neighbours' size and
        // its icon sits centred (the review's note).
        let gap_after = if pin_count > 0 && i + 1 == pin_count {
            theme::TAB_PIN_GAP
        } else {
            0.0
        };
        tabs::Tab {
            label,
            icon,
            closable: !t.is_pinned(),
            slide,
            dragging,
            gap_after,
            status: t.view.status_mark(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_strip(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        header: Rect,
        hovered_tab: Option<usize>,
        hovered_close: Option<usize>,
        hovered_new: bool,
        show_scrollbar: bool,
        scrollbar_active: bool,
        notes: &std::collections::HashSet<koru_core::DocumentId>,
        ws: &koru_core::schema::WorkspaceDoc,
    ) {
        self.last_header = Some(header);
        self.ensure_widths(engine);
        let carried = self.carry_offset();
        // **Held out here because the strip borrows it.** A Tasks tab showing a
        // list wears that list's colour, the same square the rail and the
        // page's own heading draw — a third reading of one fact that has to
        // look like the other two. The view answers with an owned token and
        // `TabIcon` carries a reference, so the answers live for as long as
        // `swatches` does. The outer `Option` is *is this a list at all*; the
        // inner one is a list whose colour the table has forgotten, which
        // draws the neutral square rather than nothing.
        let swatches: Vec<Option<Option<String>>> = self
            .tabs
            .iter()
            .map(|t| match &t.view {
                TabView::Tasks(tasks) => tasks.title_swatch(),
                _ => None,
            })
            .collect();
        let pin_count = self.tabs.pinned_count();
        let items: Vec<tabs::Tab<'_>> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| Self::tab_item(t, carried, i, pin_count, swatches[i].as_ref(), notes, ws))
            .collect();
        tabs::draw(
            scene,
            root,
            engine,
            header,
            &items,
            &self.widths,
            self.scroll,
            self.tabs.active(),
            hovered_tab,
            hovered_close,
            hovered_new,
            show_scrollbar,
            scrollbar_active,
        );
    }

    pub fn hit(&self, header: Rect, pointer: (f32, f32)) -> Option<HitKind> {
        let (px, py) = pointer;
        if !header.contains(px, py) {
            return None;
        }
        let plus = tabs::new_tab(header, &self.widths, self.scroll);
        if plus.contains(px, py) {
            return Some(HitKind::NewTab);
        }
        if let Some(track) = tabs::scroll_track(header, &self.widths) {
            if track.contains(px, py) {
                return Some(HitKind::TabScroll);
            }
        }
        let lane = tabs::lane(header);
        let view_right = lane.x + lane.w;
        if px >= view_right {
            return Some(HitKind::Strip);
        }
        for t in tabs::layout(header, &self.widths, self.scroll) {
            if t.body.x + t.body.w <= header.x || t.body.x >= view_right {
                continue;
            }
            if t.close.contains(px, py) {
                return Some(HitKind::TabClose(t.index));
            }
            if t.body.contains(px, py) {
                return Some(HitKind::Tab(t.index));
            }
        }
        Some(HitKind::Strip)
    }

    pub fn start_drag(&mut self, index: usize, grab_dx: f32, x: f32, y: f32) {
        self.drag = Some(TabDrag {
            index,
            grab_dx,
            x,
            origin_y: y,
            y,
        });
    }

    pub fn update_drag(&mut self, x: f32, y: f32) {
        if let Some(d) = self.drag.as_mut() {
            d.x = x;
            d.y = y;
        }
    }

    pub fn clear_drag(&mut self) {
        self.drag = None;
    }

    pub fn drag_index(&self) -> Option<usize> {
        self.drag.as_ref().map(|d| d.index)
    }

    pub fn carry_offset(&self) -> Option<(usize, f32)> {
        let header = self.last_header?;
        let d = self.drag.as_ref()?;
        let rect = tabs::layout(header, &self.widths, self.scroll)
            .get(d.index)
            .copied()?;
        let w = rect.body.w;
        let x = tabs::carry_x(header, w, d.x - d.grab_dx);
        let slot = tabs::layout(header, &self.widths, self.scroll)
            .get(d.index)?
            .body
            .x;
        Some((d.index, x - slot))
    }

    pub fn is_tearing(&self) -> bool {
        self.drag
            .as_ref()
            .is_some_and(|d| (d.y - d.origin_y).abs() >= TEAR_OFF_PX)
    }

    pub fn move_tab(&mut self, from: usize, to: usize) -> bool {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return false;
        }
        let header = self
            .last_header
            .unwrap_or_else(|| Rect::new(0.0, 0.0, 10000.0, theme::CAPTION_H));
        let old_xs = tabs::slot_xs(header, &self.widths);
        if from < self.widths.len() && to < self.widths.len() {
            let w = self.widths.remove(from);
            self.widths.insert(to, w);
        }
        if !self.tabs.move_tab(from, to) {
            if from < self.widths.len() && to < self.widths.len() {
                let w = self.widths.remove(to);
                self.widths.insert(from, w);
            }
            return false;
        }
        let new_xs = tabs::slot_xs(header, &self.widths);
        self.slides
            .moved(&old_xs, &new_xs, from, to, Instant::now());
        if let Some(d) = self.drag.as_mut() {
            d.index = to;
        }
        true
    }

    pub fn insert_tab_after_active(&mut self, tab: Tab) -> usize {
        let i = self.tabs.insert_inactive(tab);
        self.widths_stale = true;
        i
    }

    /// Insert at the flat index the drop marker showed. A drop never lands
    /// among the pins — they hold the left of the strip, and nothing dragged
    /// is pinned.
    pub fn insert_tab_at(&mut self, tab: Tab, at: usize) -> usize {
        let i = self.tabs.insert(at.max(self.tabs.pinned_count()), tab);
        self.widths_stale = true;
        i
    }

    /// Where a tab dropped at window-space `x` would land in this strip: the
    /// flat insertion index and the x the marker draws at. The slot answers
    /// the question VS Code's `|` answers — *here*, between these two.
    ///
    /// `None` until the strip has been drawn once: the widths live behind
    /// `ensure_widths`, and guessing at slots with stale widths would put the
    /// marker a tab away from where the drop lands.
    pub fn drop_slot(&self, header: Rect, x: f32) -> Option<(usize, f32)> {
        if self.widths.is_empty() {
            return None;
        }
        let pinned = self.tabs.pinned_count();
        let rects = tabs::layout(header, &self.widths, self.scroll);
        for t in rects.iter().skip(pinned) {
            if x < t.body.x + t.body.w * 0.5 {
                return Some((t.index, t.body.x - theme::TAB_GAP * 0.5));
            }
        }
        // Past the last main tab — or, with none, right of the pins.
        let last = rects.iter().skip(pinned).last().or(rects.last())?;
        Some((
            (last.index + 1).max(pinned),
            last.body.x + last.body.w + theme::TAB_GAP * 0.5,
        ))
    }

    /// Where the slot marker draws for a pending drop at flat `index`, from
    /// the strip's *current* widths — the index is the state, the x is always
    /// re-derived, so a width that settled between the move and the frame
    /// cannot leave the marker a tab away from the landing slot.
    pub fn slot_marker_x(&self, header: Rect, index: usize) -> Option<f32> {
        if self.widths.is_empty() {
            return None;
        }
        let rects = tabs::layout(header, &self.widths, self.scroll);
        if let Some(t) = rects.iter().find(|t| t.index == index) {
            return Some(t.body.x - theme::TAB_GAP * 0.5);
        }
        // At or past the end.
        let last = rects.last()?;
        Some(last.body.x + last.body.w + theme::TAB_GAP * 0.5)
    }

    pub fn take_tab(&mut self, index: usize) -> Option<Tab> {
        let tab = self.tabs.take_no_floor(index)?;
        self.widths_stale = true;
        Some(tab)
    }

    pub fn close_tab(&mut self, index: usize) -> bool {
        let changed = self.tabs.close(index);
        if changed {
            self.widths_stale = true;
        }
        changed
    }

    pub fn ensure_tab_visible(&mut self, i: usize) {
        if let Some(header) = self.last_header {
            self.scroll = tabs::ensure_visible(header, &self.widths, self.scroll, i);
        }
    }

    pub fn tab_scrollbar_visible(&self) -> bool {
        self.last_header
            .is_some_and(|h| tabs::max_scroll(h, &self.widths) > 0.0)
    }

    pub fn tab_scrollbar_active_at(&self, pointer: (f32, f32)) -> bool {
        let Some(header) = self.last_header else {
            return false;
        };
        tabs::scroll_track(header, &self.widths).is_some_and(|t| t.contains(pointer.0, pointer.1))
    }

    pub fn iter_tabs(&self) -> impl Iterator<Item = &Tab> {
        self.tabs.iter()
    }

    pub fn iter_tabs_mut(&mut self) -> impl Iterator<Item = &mut Tab> {
        self.tabs.iter_mut()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.len() == 0
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }
}

pub enum PaneNode {
    Group(TabGroup),
    Split {
        axis: SplitAxis,
        ratio: f32,
        children: [Box<PaneNode>; 2],
    },
}

pub struct PaneTree {
    root: PaneNode,
    focused: PanePath,
}

impl PaneTree {
    pub fn new(first: Tab) -> Self {
        Self::from_root(PaneNode::Group(TabGroup::new(first)))
    }

    pub fn from_root(root: PaneNode) -> Self {
        Self {
            root,
            focused: PanePath::root(),
        }
    }

    pub fn focused(&self) -> &TabGroup {
        self.group(&self.focused)
            .expect("focused path points to a group")
    }

    pub fn focused_mut(&mut self) -> &mut TabGroup {
        let path = self.focused.clone();
        self.group_mut(&path)
            .expect("focused path points to a group")
    }

    pub fn focused_path(&self) -> &PanePath {
        &self.focused
    }

    pub fn group(&self, path: &PanePath) -> Option<&TabGroup> {
        let mut node = &self.root;
        for &i in &path.0 {
            match node {
                PaneNode::Group(_) => return None,
                PaneNode::Split { children, .. } => node = children.get(i as usize)?,
            }
        }
        match node {
            PaneNode::Group(g) => Some(g),
            PaneNode::Split { .. } => None,
        }
    }

    pub fn group_mut(&mut self, path: &PanePath) -> Option<&mut TabGroup> {
        let mut node = &mut self.root;
        for &i in &path.0 {
            match node {
                PaneNode::Group(_) => return None,
                PaneNode::Split { children, .. } => {
                    node = children.get_mut(i as usize)?;
                }
            }
        }
        match node {
            PaneNode::Group(g) => Some(g),
            PaneNode::Split { .. } => None,
        }
    }

    pub fn set_focus(&mut self, path: PanePath) {
        self.focused = path;
    }

    pub fn leaf_rects(&self, content: Rect) -> Vec<(PanePath, Rect)> {
        let mut out = Vec::new();
        Self::collect_leaf_rects(&self.root, content, PanePath::root(), &mut out);
        out
    }

    /// The hairline of every split in the tree, in window space. A single-group
    /// window has none — a divider only exists where two panes actually meet.
    pub fn divider_rects(&self, content: Rect) -> Vec<Rect> {
        self.dividers(content)
            .into_iter()
            .map(|(_, _, rect)| rect)
            .collect()
    }

    /// Every divider with the path of the split that owns it — the hit-testing
    /// and dragging view of the same geometry [`divider_rects`] draws.
    ///
    /// [`divider_rects`]: Self::divider_rects
    pub fn dividers(&self, content: Rect) -> Vec<(PanePath, SplitAxis, Rect)> {
        let mut out = Vec::new();
        Self::collect_dividers(&self.root, content, PanePath::root(), &mut out);
        out
    }

    fn collect_dividers(
        node: &PaneNode,
        rect: Rect,
        path: PanePath,
        out: &mut Vec<(PanePath, SplitAxis, Rect)>,
    ) {
        if let PaneNode::Split {
            axis,
            ratio,
            children,
        } = node
        {
            let (r0, divider, r1) = split_with_divider(rect, *axis, *ratio);
            out.push((path.clone(), *axis, divider));
            Self::collect_dividers(&children[0], r0, path.child(0), out);
            Self::collect_dividers(&children[1], r1, path.child(1), out);
        }
    }

    /// Move the divider at `path` so it sits at `pos` (window space, along the
    /// split's axis), clamped so no pane in either subtree goes under its
    /// minimum. Returns whether the ratio actually changed.
    ///
    /// A window smaller than the sum of the minimums leaves the divider where
    /// it is — the clamp inverts there, and honouring it would teleport the
    /// split to a nonsense ratio.
    pub fn drag_divider(&mut self, content: Rect, path: &PanePath, pos: f32) -> bool {
        let mut node = &self.root;
        let mut rect = content;
        for &i in &path.0 {
            match node {
                PaneNode::Group(_) => return false,
                PaneNode::Split {
                    axis,
                    ratio,
                    children,
                } => {
                    let (r0, _, r1) = split_with_divider(rect, *axis, *ratio);
                    rect = if i == 0 { r0 } else { r1 };
                    node = &children[i as usize];
                }
            }
        }
        let PaneNode::Split { axis, children, .. } = node else {
            return false;
        };
        let (origin, size) = match axis {
            SplitAxis::Vertical => (rect.x, rect.w),
            SplitAxis::Horizontal => (rect.y, rect.h),
        };
        let lo = min_extent(&children[0], *axis);
        let hi = size - DIVIDER_W - min_extent(&children[1], *axis);
        if lo > hi {
            return false;
        }
        let ratio = (pos - origin).clamp(lo, hi) / size;
        self.set_split_ratio(path, ratio)
    }

    /// The two sides of the split at `path`, in window space — what the size
    /// labels of a divider drag sit over. `None` if the path is not a split.
    pub fn split_sides(&self, content: Rect, path: &PanePath) -> Option<(Rect, Rect)> {
        let mut node = &self.root;
        let mut rect = content;
        for &i in &path.0 {
            match node {
                PaneNode::Group(_) => return None,
                PaneNode::Split {
                    axis,
                    ratio,
                    children,
                } => {
                    let (r0, _, r1) = split_with_divider(rect, *axis, *ratio);
                    rect = if i == 0 { r0 } else { r1 };
                    node = &children[i as usize];
                }
            }
        }
        match node {
            PaneNode::Split { axis, ratio, .. } => Some(split_rect(rect, *axis, *ratio)),
            PaneNode::Group(_) => None,
        }
    }

    /// Set a split's ratio. Returns whether it changed.
    pub fn set_split_ratio(&mut self, path: &PanePath, ratio: f32) -> bool {
        let mut node = &mut self.root;
        for &i in &path.0 {
            match node {
                PaneNode::Group(_) => return false,
                PaneNode::Split { children, .. } => node = &mut children[i as usize],
            }
        }
        match node {
            PaneNode::Split { ratio: r, .. } => {
                let ratio = ratio.clamp(0.0, 1.0);
                if (*r - ratio).abs() < f32::EPSILON {
                    false
                } else {
                    *r = ratio;
                    true
                }
            }
            PaneNode::Group(_) => false,
        }
    }

    fn collect_leaf_rects(
        node: &PaneNode,
        rect: Rect,
        path: PanePath,
        out: &mut Vec<(PanePath, Rect)>,
    ) {
        match node {
            PaneNode::Group(_) => out.push((path, rect)),
            PaneNode::Split {
                axis,
                ratio,
                children,
            } => {
                let (r0, r1) = split_rect(rect, *axis, *ratio);
                Self::collect_leaf_rects(&children[0], r0, path.child(0), out);
                Self::collect_leaf_rects(&children[1], r1, path.child(1), out);
            }
        }
    }

    pub fn iter_groups(&self) -> GroupIter<'_> {
        GroupIter {
            stack: vec![(&self.root, PanePath::root())],
        }
    }

    pub fn iter_groups_mut(&mut self) -> GroupIterMut<'_> {
        GroupIterMut {
            stack: vec![(&mut self.root, PanePath::root())],
        }
    }

    /// A pane's tab lane. A pane touching the top of the window **shares the
    /// title bar**: its lane is the caption band above its content, so the
    /// window has one top row rather than a caption row plus a strip row. Any
    /// other pane's lane is the top of its own rect.
    ///
    /// The window chrome the lane intersects (sidebar toggle, window buttons)
    /// is subtracted by the caller — the tree knows geometry, not furniture.
    pub fn lane_rect(content: Rect, leaf: Rect) -> Rect {
        if leaf.y == content.y {
            Rect::new(
                leaf.x,
                content.y - theme::CAPTION_H,
                leaf.w,
                theme::CAPTION_H,
            )
        } else {
            Rect::new(leaf.x, leaf.y, leaf.w, theme::CAPTION_H)
        }
    }

    /// The pane's view: everything below its lane — and for a top-row pane the
    /// lane has moved up into the caption band, so the view is the whole leaf.
    pub fn view_rect_in(content: Rect, leaf: Rect) -> Rect {
        if leaf.y == content.y {
            leaf
        } else {
            Self::view_rect(leaf)
        }
    }

    pub fn view_rect(group_full_rect: Rect) -> Rect {
        let h = (group_full_rect.h - theme::CAPTION_H).max(0.0);
        Rect::new(
            group_full_rect.x,
            group_full_rect.y + theme::CAPTION_H,
            group_full_rect.w,
            h,
        )
    }

    pub fn split(&mut self, target: &PanePath, dir: SplitDir, new_tab: Tab) {
        let new_side = dir.new_side();
        if let Some(old_group) = Self::take_group_at(&mut self.root, target) {
            let new_group = TabGroup::new(new_tab);
            let (first, second) = if new_side == 0 {
                (new_group, old_group)
            } else {
                (old_group, new_group)
            };
            let split = PaneNode::Split {
                axis: dir.axis(),
                ratio: 0.5,
                children: [
                    Box::new(PaneNode::Group(first)),
                    Box::new(PaneNode::Group(second)),
                ],
            };
            Self::place_node(&mut self.root, target, split);
            let mut new_path = target.clone();
            new_path.0.push(new_side as u8);
            self.focused = new_path;
        }
    }

    fn take_group_at(node: &mut PaneNode, path: &PanePath) -> Option<TabGroup> {
        if path.0.is_empty() {
            let mut tmp = PaneNode::Group(TabGroup::new(Tab::welcome()));
            std::mem::swap(node, &mut tmp);
            match tmp {
                PaneNode::Group(g) => Some(g),
                other => {
                    *node = other;
                    None
                }
            }
        } else {
            match node {
                PaneNode::Group(_) => None,
                PaneNode::Split { children, .. } => {
                    let i = path.0[0] as usize;
                    let rest = PanePath(path.0[1..].to_vec());
                    children
                        .get_mut(i)
                        .and_then(|c| Self::take_group_at(c, &rest))
                }
            }
        }
    }

    fn place_node(node: &mut PaneNode, path: &PanePath, new_node: PaneNode) {
        if path.0.is_empty() {
            *node = new_node;
        } else {
            match node {
                PaneNode::Group(_) => {}
                PaneNode::Split { children, .. } => {
                    let i = path.0[0] as usize;
                    let rest = PanePath(path.0[1..].to_vec());
                    if let Some(child) = children.get_mut(i) {
                        Self::place_node(child, &rest, new_node);
                    }
                }
            }
        }
    }

    pub fn collapse_empty_groups(&mut self) {
        while Self::collapse_once(&mut self.root) {}
        if self.group(&self.focused).is_none() {
            // `PanePath::root()` is only a group when the window holds a single
            // pane — under a surviving split it names the split, and the next
            // `focused_mut` panics on it. Focus the first leaf instead.
            self.focused = self.first_group_path();
        }
    }

    /// The first pane in tree order — where focus lands when its pane is gone.
    pub fn first_group_path(&self) -> PanePath {
        let mut node = &self.root;
        let mut path = PanePath::root();
        while let PaneNode::Split { children, .. } = node {
            path.0.push(0);
            node = &children[0];
        }
        path
    }

    fn collapse_once(node: &mut PaneNode) -> bool {
        match node {
            PaneNode::Group(_) => false,
            PaneNode::Split { children, .. } => {
                for child in children.iter_mut() {
                    if Self::collapse_once(child) {
                        return true;
                    }
                }
                let empty = |c: &PaneNode| matches!(c, PaneNode::Group(g) if g.is_empty());
                if empty(&children[0]) {
                    let mut survivor = Box::new(PaneNode::Group(TabGroup::new(Tab::welcome())));
                    std::mem::swap(&mut children[1], &mut survivor);
                    *node = *survivor;
                    return true;
                }
                if empty(&children[1]) {
                    let mut survivor = Box::new(PaneNode::Group(TabGroup::new(Tab::welcome())));
                    std::mem::swap(&mut children[0], &mut survivor);
                    *node = *survivor;
                    return true;
                }
                false
            }
        }
    }

    /// Drop into a group: the tab lands after the active one and takes over —
    /// a drop is the user saying "this tab lives here now", and landing on the
    /// tab you just moved is how every editor answers it.
    pub fn insert_into_group(&mut self, target: &PanePath, tab: Tab) {
        if let Some(g) = self.group_mut(target) {
            let i = g.insert_tab_after_active(tab);
            g.select(i);
        }
        self.focused = target.clone();
    }

    /// Drop into the slot the marker showed: the tab lands at `index`, takes
    /// over, and the group takes the focus.
    pub fn insert_tab_at(&mut self, target: &PanePath, tab: Tab, index: usize) {
        if let Some(g) = self.group_mut(target) {
            let i = g.insert_tab_at(tab, index);
            g.select(i);
        }
        self.focused = target.clone();
    }

    pub fn view_rect_for(&self, content: Rect, path: &PanePath) -> Option<Rect> {
        self.leaf_rects(content)
            .into_iter()
            .find(|(p, _)| p == path)
            .map(|(_, r)| PaneTree::view_rect_in(content, r))
    }

    pub fn take_tab_by_id(&mut self, path: &PanePath, tab_id: u64) -> Option<Tab> {
        let index = self.group(path)?.iter_tabs().position(|t| t.id == tab_id)?;
        self.group_mut(path)?.take_tab(index)
    }

    pub fn contains_group(&self, path: &PanePath) -> bool {
        self.group(path).is_some()
    }

    pub fn root(&self) -> &PaneNode {
        &self.root
    }
}

pub struct GroupIter<'a> {
    stack: Vec<(&'a PaneNode, PanePath)>,
}

impl<'a> Iterator for GroupIter<'a> {
    type Item = (PanePath, &'a TabGroup);

    fn next(&mut self) -> Option<Self::Item> {
        let (node, path) = self.stack.pop()?;
        match node {
            PaneNode::Group(g) => Some((path, g)),
            PaneNode::Split { children, .. } => {
                self.stack.push((&children[1], path.child(1)));
                self.stack.push((&children[0], path.child(0)));
                self.next()
            }
        }
    }
}

pub struct GroupIterMut<'a> {
    stack: Vec<(&'a mut PaneNode, PanePath)>,
}

impl<'a> Iterator for GroupIterMut<'a> {
    type Item = (PanePath, &'a mut TabGroup);

    fn next(&mut self) -> Option<Self::Item> {
        let (node, path) = self.stack.pop()?;
        match node {
            PaneNode::Group(g) => Some((path, g)),
            PaneNode::Split { children, .. } => {
                let [left, right] = children;
                self.stack.push((right, path.child(1)));
                self.stack.push((left, path.child(0)));
                self.next()
            }
        }
    }
}

fn split_rect(rect: Rect, axis: SplitAxis, ratio: f32) -> (Rect, Rect) {
    let (a, _, b) = split_with_divider(rect, axis, ratio);
    (a, b)
}

/// The one cut a split makes: the two children and the [`DIVIDER_W`] hairline
/// between them. The divider is taken out of the **second** child, so the first
/// child's geometry is exactly `ratio` of the parent — a ratio survives a
/// resize without the pixel migrating between the two sides.
///
/// Everything that lays out or draws the split comes here, so the rect a
/// divider is painted in is definitionally the pixel neither child got.
fn split_with_divider(rect: Rect, axis: SplitAxis, ratio: f32) -> (Rect, Rect, Rect) {
    let ratio = ratio.clamp(0.0, 1.0);
    match axis {
        SplitAxis::Vertical => {
            let w0 = rect.w * ratio;
            let w1 = (rect.w - w0 - DIVIDER_W).max(0.0);
            (
                Rect::new(rect.x, rect.y, w0, rect.h),
                Rect::new(rect.x + w0, rect.y, DIVIDER_W, rect.h),
                Rect::new(rect.x + w0 + DIVIDER_W, rect.y, w1, rect.h),
            )
        }
        SplitAxis::Horizontal => {
            let h0 = rect.h * ratio;
            let h1 = (rect.h - h0 - DIVIDER_W).max(0.0);
            (
                Rect::new(rect.x, rect.y, rect.w, h0),
                Rect::new(rect.x, rect.y + h0, rect.w, DIVIDER_W),
                Rect::new(rect.x, rect.y + h0 + DIVIDER_W, rect.w, h1),
            )
        }
    }
}

/// The grab band a divider answers to: the hairline plus half of
/// [`DIVIDER_GRIP`] either side, so a 1px line is grabbable without pixel
/// hunting — the same rule the sidebar's edge grip follows.
pub fn divider_grip(rect: Rect, axis: SplitAxis) -> Rect {
    let over = (DIVIDER_GRIP - DIVIDER_W) * 0.5;
    match axis {
        SplitAxis::Vertical => Rect::new(rect.x - over, rect.y, DIVIDER_GRIP, rect.h),
        SplitAxis::Horizontal => Rect::new(rect.x, rect.y - over, rect.w, DIVIDER_GRIP),
    }
}

/// The divider as drawn. A divider between two top-row panes extends through
/// the title bar, so their lanes read as two; every other divider is
/// content-space. Vertical only: a horizontal split's junction never touches
/// the top band, and the lane above a top-row pane belongs to the pane, not
/// to the divider under it.
pub fn divider_draw_rect(content: Rect, divider: Rect) -> Rect {
    if divider.y == content.y && divider.w <= DIVIDER_W {
        Rect::new(
            divider.x,
            content.y - theme::CAPTION_H,
            divider.w,
            divider.h + theme::CAPTION_H,
        )
    } else {
        divider
    }
}

/// The smallest the subtree may measure along `axis` while every pane in it
/// keeps its minimum: a side-by-side split *adds* its children's floors (plus
/// its divider), a stacked split shares the axis so it needs only the larger.
fn min_extent(node: &PaneNode, axis: SplitAxis) -> f32 {
    match node {
        PaneNode::Group(_) => match axis {
            SplitAxis::Vertical => PANE_MIN_W,
            SplitAxis::Horizontal => PANE_MIN_H,
        },
        PaneNode::Split {
            axis: own,
            children,
            ..
        } => {
            let a = min_extent(&children[0], axis);
            let b = min_extent(&children[1], axis);
            if *own == axis {
                a + DIVIDER_W + b
            } else {
                a.max(b)
            }
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct SplitIntent {
    pub target: PanePath,
    pub dir: SplitDir,
    pub since: Instant,
}

/// The placement a document dragged out of the explore column asked for,
/// parked while the open goes through the store: the row named a manifest
/// entry the workspace does not have loaded, so there is no tab to move yet
/// and the pane, the lane slot, or the split the drop pointed at has to wait
/// for the landing. Consumed by the arrival of `document` — never by a
/// stranger's.
#[derive(Clone, PartialEq, Debug)]
pub struct PendingPaneDrop {
    pub document: koru_core::DocumentId,
    pub target: PanePath,
    pub slot: Option<usize>,
    pub split: Option<SplitDir>,
}

pub fn resolve_split_intent(
    view_rect: Rect,
    pointer: (f32, f32),
    target: &PanePath,
    current: Option<&SplitIntent>,
) -> Option<SplitIntent> {
    let (px, py) = pointer;
    if !view_rect.contains(px, py) {
        return None;
    }
    let nx = (px - view_rect.x) / view_rect.w;
    let ny = (py - view_rect.y) / view_rect.h;
    let d_left = nx;
    let d_right = 1.0 - nx;
    let d_top = ny;
    let d_bottom = 1.0 - ny;

    let candidates = [
        (SplitDir::Left, d_left),
        (SplitDir::Right, d_right),
        (SplitDir::Top, d_top),
        (SplitDir::Bottom, d_bottom),
    ];
    let (nearest_dir, nearest_dist) = candidates
        .iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .copied()?;

    if let Some(cur) = current {
        if cur.target != *target {
            // Pointer moved to a different pane: recompute from scratch.
            if nearest_dist <= 0.25 {
                return Some(SplitIntent {
                    target: target.clone(),
                    dir: nearest_dir,
                    since: Instant::now(),
                });
            }
            return None;
        }
        let cur_dist = match cur.dir {
            SplitDir::Left => d_left,
            SplitDir::Right => d_right,
            SplitDir::Top => d_top,
            SplitDir::Bottom => d_bottom,
        };
        if cur_dist > 0.30 {
            return None;
        }
        if nearest_dir == cur.dir {
            return Some(SplitIntent {
                target: target.clone(),
                dir: cur.dir,
                since: cur.since,
            });
        }
        if nearest_dist <= cur_dist - 0.03 {
            return Some(SplitIntent {
                target: target.clone(),
                dir: nearest_dir,
                since: Instant::now(),
            });
        }
        Some(SplitIntent {
            target: target.clone(),
            dir: cur.dir,
            since: cur.since,
        })
    } else if nearest_dist <= 0.25 {
        Some(SplitIntent {
            target: target.clone(),
            dir: nearest_dir,
            since: Instant::now(),
        })
    } else {
        None
    }
}

pub fn preview_rect(view_rect: Rect, dir: SplitDir) -> Rect {
    match dir {
        SplitDir::Left => Rect::new(view_rect.x, view_rect.y, view_rect.w * 0.5, view_rect.h),
        SplitDir::Right => Rect::new(
            view_rect.x + view_rect.w * 0.5,
            view_rect.y,
            view_rect.w * 0.5,
            view_rect.h,
        ),
        SplitDir::Top => Rect::new(view_rect.x, view_rect.y, view_rect.w, view_rect.h * 0.5),
        SplitDir::Bottom => Rect::new(
            view_rect.x,
            view_rect.y + view_rect.h * 0.5,
            view_rect.w,
            view_rect.h * 0.5,
        ),
    }
}

/// The strength of the accent wash a split preview settles at — and the tint
/// the tab-drop slot marker wears, so "where the tab lands" and "where the
/// pane lands" answer in the same colour.
pub const PREVIEW_TINT_ALPHA: f32 = 0.25;

pub fn draw_split_preview(
    scene: &mut Scene,
    engine: &mut TextEngine,
    root: Affine,
    rect: Rect,
    dir: SplitDir,
    alpha: f32,
) {
    let color = theme::p().accent.with_alpha(alpha.clamp(0.0, 1.0));
    scene::fill(
        scene,
        root,
        color,
        &scene::rect(rect.x, rect.y, rect.w, rect.h),
    );
    let glyph = match dir {
        SplitDir::Left => icons::CARET_LEFT,
        SplitDir::Right => icons::CARET_RIGHT,
        SplitDir::Top => icons::CARET_UP,
        SplitDir::Bottom => icons::CARET_DOWN,
    };
    tabs::draw_icon(scene, root, engine, glyph, rect, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn welcome_tab(label: &str) -> Tab {
        let mut t = Tab::welcome();
        t.label = label.to_string();
        t
    }

    #[test]
    fn split_intent_dead_centre_is_none() {
        let rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert!(resolve_split_intent(rect, (50.0, 50.0), &PanePath::root(), None).is_none());
    }

    #[test]
    fn split_intent_each_edge() {
        let rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert_eq!(
            resolve_split_intent(rect, (5.0, 50.0), &PanePath::root(), None).map(|i| i.dir),
            Some(SplitDir::Left)
        );
        assert_eq!(
            resolve_split_intent(rect, (95.0, 50.0), &PanePath::root(), None).map(|i| i.dir),
            Some(SplitDir::Right)
        );
        assert_eq!(
            resolve_split_intent(rect, (50.0, 5.0), &PanePath::root(), None).map(|i| i.dir),
            Some(SplitDir::Top)
        );
        assert_eq!(
            resolve_split_intent(rect, (50.0, 95.0), &PanePath::root(), None).map(|i| i.dir),
            Some(SplitDir::Bottom)
        );
    }

    #[test]
    fn split_intent_corner_picks_closer_edge() {
        let rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        let intent = resolve_split_intent(rect, (5.0, 5.0), &PanePath::root(), None).unwrap();
        assert!(matches!(intent.dir, SplitDir::Left | SplitDir::Top));
    }

    #[test]
    fn split_intent_hysteresis() {
        let rect = Rect::new(0.0, 0.0, 100.0, 100.0);
        let current = SplitIntent {
            target: PanePath::root(),
            dir: SplitDir::Left,
            since: Instant::now(),
        };
        assert!(
            resolve_split_intent(rect, (24.0, 50.0), &PanePath::root(), Some(&current)).is_some()
        );
        assert!(
            resolve_split_intent(rect, (26.0, 50.0), &PanePath::root(), Some(&current)).is_some()
        );
        assert!(
            resolve_split_intent(rect, (28.0, 50.0), &PanePath::root(), Some(&current)).is_some()
        );
        assert!(
            resolve_split_intent(rect, (31.0, 50.0), &PanePath::root(), Some(&current)).is_none()
        );
    }

    #[test]
    fn drag_divider_clamps_to_the_pane_minimums() {
        let mut tree = PaneTree::new(welcome_tab("A"));
        tree.split(&PanePath::root(), SplitDir::Right, welcome_tab("B"));
        let content = Rect::new(0.0, 0.0, 1000.0, 600.0);

        // Far past the left floor: the left pane stops at its minimum and the
        // right keeps the rest.
        assert!(tree.drag_divider(content, &PanePath::root(), 50.0));
        let leaves = tree.leaf_rects(content);
        assert_eq!(leaves[0].1.w, PANE_MIN_W);
        assert_eq!(leaves[1].1.w, 1000.0 - PANE_MIN_W - DIVIDER_W);

        // Far past the right floor: the right pane stops at its minimum.
        assert!(tree.drag_divider(content, &PanePath::root(), 950.0));
        let leaves = tree.leaf_rects(content);
        assert_eq!(leaves[1].1.w, PANE_MIN_W);

        // Inside the band the divider lands exactly where it is put…
        assert!(tree.drag_divider(content, &PanePath::root(), 450.0));
        assert_eq!(tree.leaf_rects(content)[0].1.w, 450.0);
        // …and a move that changes nothing reports nothing.
        assert!(!tree.drag_divider(content, &PanePath::root(), 450.0));
    }

    #[test]
    fn drag_divider_uses_the_height_floor_on_a_stacked_split() {
        let mut tree = PaneTree::new(welcome_tab("A"));
        tree.split(&PanePath::root(), SplitDir::Bottom, welcome_tab("B"));
        let content = Rect::new(0.0, 0.0, 800.0, 600.0);

        assert!(tree.drag_divider(content, &PanePath::root(), 50.0));
        let leaves = tree.leaf_rects(content);
        assert_eq!(leaves[0].1.h, PANE_MIN_H);
        assert_eq!(leaves[1].1.h, 600.0 - PANE_MIN_H - DIVIDER_W);
    }

    #[test]
    fn drag_divider_counts_nested_panes_in_the_minimum() {
        // The left subtree is itself split left|right, so it needs
        // 400 + 1 + 400 of width before the outer divider may pass it.
        let mut tree = PaneTree::new(welcome_tab("A"));
        tree.split(&PanePath::root(), SplitDir::Right, welcome_tab("B"));
        tree.split(
            &PanePath::root().child(0),
            SplitDir::Right,
            welcome_tab("C"),
        );
        let content = Rect::new(0.0, 0.0, 1400.0, 600.0);

        assert!(tree.drag_divider(content, &PanePath::root(), 500.0));
        let leaves = tree.leaf_rects(content);
        let left_subtree_w = leaves[0].1.w + DIVIDER_W + leaves[1].1.w;
        let want = PANE_MIN_W + DIVIDER_W + PANE_MIN_W;
        assert!((left_subtree_w - want).abs() < 0.001, "{left_subtree_w}");
    }

    #[test]
    fn drag_divider_holds_still_when_the_window_cannot_honour_the_minimums() {
        let mut tree = PaneTree::new(welcome_tab("A"));
        tree.split(&PanePath::root(), SplitDir::Right, welcome_tab("B"));
        // 400 + 1 + 400 does not fit in 700: the clamp inverts, and honouring
        // it would teleport the split, so the drag is a no-op.
        let content = Rect::new(0.0, 0.0, 700.0, 600.0);
        assert!(!tree.drag_divider(content, &PanePath::root(), 350.0));
        assert_eq!(tree.leaf_rects(content)[0].1.w, 350.0);
    }

    #[test]
    fn divider_grip_straddles_the_hairline() {
        let rect = Rect::new(100.0, 20.0, DIVIDER_W, 300.0);
        let grip = divider_grip(rect, SplitAxis::Vertical);
        assert_eq!(grip.w, DIVIDER_GRIP);
        // The hairline sits in the middle of the band, reachable from both
        // sides, and the band spans the divider's full length.
        assert!(grip.contains(rect.x - 1.0, 50.0));
        assert!(grip.contains(rect.x + DIVIDER_W, 50.0));
        assert_eq!(grip.h, rect.h);
    }

    #[test]
    fn leaf_rects_single_group() {
        let tree = PaneTree::new(welcome_tab("A"));
        let content = Rect::new(0.0, 0.0, 200.0, 100.0);
        let leaves = tree.leaf_rects(content);
        assert_eq!(leaves.len(), 1);
        assert_eq!(leaves[0].0, PanePath::root());
        assert_eq!(leaves[0].1, content);
    }

    #[test]
    fn leaf_rects_vertical_split() {
        let mut tree = PaneTree::new(welcome_tab("A"));
        tree.split(&PanePath::root(), SplitDir::Right, welcome_tab("B"));
        let leaves = tree.leaf_rects(Rect::new(0.0, 0.0, 200.0, 100.0));
        assert_eq!(leaves.len(), 2);
        // The left child is exactly `ratio` of the parent; the right one gives
        // up the divider's pixel.
        assert_eq!(leaves[0].1.w, 100.0);
        assert_eq!(leaves[1].1.w, 99.0);
        assert_eq!(leaves[0].1.h, 100.0);
        assert_eq!(leaves[1].1.h, 100.0);
    }

    #[test]
    fn leaf_rects_nested_split_sums_to_content() {
        let mut tree = PaneTree::new(welcome_tab("A"));
        tree.split(&PanePath::root(), SplitDir::Right, welcome_tab("B"));
        tree.split(
            &PanePath::root().child(0),
            SplitDir::Bottom,
            welcome_tab("C"),
        );
        let content = Rect::new(0.0, 0.0, 200.0, 100.0);
        let leaves = tree.leaf_rects(content);
        assert_eq!(leaves.len(), 3);
        // Panes plus the dividers between them tile the content exactly.
        let leaf_area: f32 = leaves.iter().map(|(_, r)| r.w * r.h).sum();
        let divider_area: f32 = tree.divider_rects(content).iter().map(|r| r.w * r.h).sum();
        assert!((leaf_area + divider_area - content.w * content.h).abs() < 0.001);
    }

    #[test]
    fn divider_rects_fill_the_gap_between_siblings() {
        let mut tree = PaneTree::new(welcome_tab("A"));
        tree.split(&PanePath::root(), SplitDir::Right, welcome_tab("B"));
        let content = Rect::new(10.0, 20.0, 200.0, 100.0);
        // A single group has no divider: a hairline exists only where two
        // panes actually meet.
        assert!(
            PaneTree::new(welcome_tab("A"))
                .divider_rects(content)
                .is_empty()
        );

        let leaves = tree.leaf_rects(content);
        let dividers = tree.divider_rects(content);
        assert_eq!(dividers.len(), 1);
        let d = dividers[0];
        // Full height, one pixel wide, flush against both siblings.
        assert_eq!((d.x, d.y, d.w, d.h), (10.0 + 100.0, 20.0, DIVIDER_W, 100.0));
        assert_eq!(leaves[0].1.x + leaves[0].1.w, d.x);
        assert_eq!(leaves[1].1.x, d.x + d.w);
    }

    #[test]
    fn split_then_collapse_restores_single_group() {
        let mut tree = PaneTree::new(welcome_tab("A"));
        tree.split(&PanePath::root(), SplitDir::Right, welcome_tab("B"));
        let right_path = PanePath::root().child(1);
        {
            let right = tree.group_mut(&right_path).unwrap();
            right.take_tab(0);
            assert!(right.is_empty());
        }
        tree.collapse_empty_groups();
        assert!(matches!(tree.root, PaneNode::Group(_)));
        assert_eq!(tree.focused, PanePath::root());
    }

    /// A top-row pane's lane is its share of the title bar; every other pane's
    /// lane is the top of its own rect. The view takes back the row the lane
    /// vacated.
    #[test]
    fn top_row_lanes_share_the_title_bar() {
        let content = Rect::new(40.0, theme::CAPTION_H, 800.0, 600.0);
        let mut tree = PaneTree::new(welcome_tab("A"));
        tree.split(&PanePath::root(), SplitDir::Bottom, welcome_tab("B"));
        let leaves = tree.leaf_rects(content);
        let (top, bottom) = (&leaves[0].1, &leaves[1].1);

        let lane = PaneTree::lane_rect(content, *top);
        assert_eq!(lane.y, 0.0, "the top pane's lane is the caption band");
        assert_eq!(lane.h, theme::CAPTION_H);
        assert_eq!(lane.x, top.x);
        assert_eq!(lane.w, top.w);
        assert_eq!(
            PaneTree::view_rect_in(content, *top),
            *top,
            "the top pane's view takes back the row the lane vacated"
        );

        let lane = PaneTree::lane_rect(content, *bottom);
        assert_eq!(lane.y, bottom.y, "the bottom pane's lane is its own row");
        assert_eq!(lane.h, theme::CAPTION_H);
        assert_eq!(
            PaneTree::view_rect_in(content, *bottom),
            PaneTree::view_rect(*bottom),
            "the bottom pane's view is below its own lane"
        );
    }

    /// A vertical split's divider reaches the top of the content: drawn, it
    /// extends through the title bar so the two lanes read as two. A
    /// horizontal split's divider and any lower divider stay in the content.
    #[test]
    fn a_top_row_divider_extends_through_the_title_bar() {
        let content = Rect::new(0.0, theme::CAPTION_H, 800.0, 600.0);
        let mut tree = PaneTree::new(welcome_tab("A"));
        tree.split(&PanePath::root(), SplitDir::Right, welcome_tab("B"));
        tree.split(
            &PanePath::root().child(0),
            SplitDir::Bottom,
            welcome_tab("C"),
        );
        let dividers = tree.divider_rects(content);
        assert_eq!(dividers.len(), 2);

        let vertical = dividers.iter().find(|d| d.w <= DIVIDER_W).unwrap();
        let drawn = divider_draw_rect(content, *vertical);
        assert_eq!(drawn.y, 0.0, "through the title bar");
        assert_eq!(drawn.h, vertical.h + theme::CAPTION_H);

        let horizontal = dividers.iter().find(|d| d.h <= DIVIDER_W).unwrap();
        assert_eq!(
            divider_draw_rect(content, *horizontal),
            *horizontal,
            "a lower junction does not reach into the title bar"
        );
    }

    /// A collapse that leaves a split at the root must not reset focus to the
    /// root — the root names the split, not a pane, and `focused_mut` panics
    /// on it. Focus falls to the first surviving pane instead.
    #[test]
    fn collapse_under_a_surviving_split_focuses_a_leaf_not_the_root() {
        let mut tree = PaneTree::new(welcome_tab("W"));
        tree.split(&PanePath::root(), SplitDir::Right, welcome_tab("S"));
        // Empty the right group without collapsing — the mid-drag state.
        let s = tree
            .group_mut(&PanePath::root().child(1))
            .unwrap()
            .take_tab(0)
            .unwrap();
        // Split the left group with it, as the drop does.
        tree.split(&PanePath::root().child(0), SplitDir::Bottom, s);
        assert_eq!(tree.focused, PanePath::root().child(0).child(1));
        tree.collapse_empty_groups();
        assert!(
            tree.group(tree.focused_path()).is_some(),
            "focused={:?}",
            tree.focused_path()
        );
    }
}
