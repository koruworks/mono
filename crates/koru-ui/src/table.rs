// SPDX-License-Identifier: MPL-2.0

//! A table: a column strip, bands of rows, and a lane at the right for
//! per-row controls.
//!
//! **One function owns the row arithmetic** — [`Table::row_rect`] — and drawing,
//! hit-testing, the cull and the scroll clamp all come here for it. That is
//! `DESIGN.md`'s first rule for chrome, and the reason it is a rule is visible
//! here: the workspace manager and the trash screen are the same table with
//! different cells, and left to themselves each would grow its own copy of the
//! same six numbers and then drift.
//!
//! **It knows nothing about a workspace or a node.** It is handed a `&[Band]`
//! saying only which entries are section headings and which are rows, and hands
//! back rectangles. What a row *is* stays with the view.
//!
//! Rows are passed in on every call rather than owned, the way `menu::Menu`
//! takes its rows: the caller's list is a pure function of the caller's data,
//! and a cached copy here would be a second one to keep in step.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Color;

/// A column: where it starts, and what its heading says.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Column {
    pub x: f32,
    pub label: &'static str,
}

impl Column {
    pub const fn new(x: f32, label: &'static str) -> Self {
        Self { x, label }
    }
}

/// Whether an entry is a section heading or a row.
///
/// The caller's own row enum maps onto this and no further — the table needs to
/// know that a heading is shorter and is not selectable, and nothing else.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Band {
    Section,
    Row,
}

/// The shape of a table: how tall each part is, and the two things about a row
/// that are a *look* rather than a size.
///
/// Stated once per table, so a view says how it reads in one place rather than
/// in eight constants scattered through two files.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Metrics {
    /// The inset the table shares with its columns and its footer.
    pub pad: f32,
    /// The block above the column strip — a search row, or a heading and a
    /// subtitle. The table reserves it; what goes in it is the caller's.
    pub header_h: f32,
    pub columns_h: f32,
    /// Air between the column rule and the first row.
    ///
    /// Its own number rather than part of `columns_h`, because the rule is
    /// drawn at the *bottom* of the column strip: folding the gap into that
    /// height would push the rule down with the rows and close the gap again.
    pub column_gap: f32,
    pub row_h: f32,
    pub section_h: f32,
    /// Reserved at the floor. Zero for a table with no footer.
    pub footer_h: f32,
    /// The corner radius of a row's fill. Zero draws the full-bleed band the
    /// workspace manager has always had; a radius makes the fill read as a
    /// *chip under the row* rather than as a stripe across the table, which is
    /// what a list with generous margins wants.
    pub row_radius: f32,
    /// How strongly a hover fills, as a multiplier on `hover_tint`. Below one is
    /// lighter. A table with a lot of air around it wants a quieter hover than
    /// one packed edge to edge, or the highlight shouts.
    pub hover_strength: f32,
    /// How far a row's fill reaches **past the content column**, each side.
    ///
    /// Padding around the text rather than an inset from the row, so the
    /// highlight always contains what it is highlighting. Setting it equal to
    /// [`pad`](Self::pad) gives the full-bleed band, which is what the workspace
    /// manager has always drawn.
    pub row_pad_x: f32,
    /// How far a row's fill is inset from the top and bottom of its band. Zero
    /// fills the row edge to edge vertically.
    pub row_pad_y: f32,
}

impl Metrics {
    /// The workspace manager's numbers, which the trash screen also uses for
    /// everything but its taller heading block.
    pub const fn manager() -> Self {
        Self {
            pad: 16.0,
            header_h: 52.0,
            columns_h: 28.0,
            column_gap: 0.0,
            row_h: 40.0,
            section_h: 26.0,
            footer_h: 30.0,
            // The manager's look, unchanged: a full-bleed band at full strength.
            // `row_pad_x == pad` is what makes it reach both edges.
            row_radius: 0.0,
            hover_strength: 1.0,
            row_pad_x: 16.0,
            row_pad_y: 0.0,
        }
    }

    pub const fn with_row_h(mut self, h: f32) -> Self {
        self.row_h = h;
        self
    }

    pub const fn with_pad(mut self, pad: f32) -> Self {
        self.pad = pad;
        // Full-bleed is *defined* as "the fill reaches the page edge", so a table
        // that has not asked for padded rows keeps that when its margin changes.
        if self.row_radius == 0.0 {
            self.row_pad_x = pad;
        }
        self
    }

    /// Rounded, quieter rows — for a table with room to breathe.
    ///
    /// The fill becomes a chip that hugs the text: eight either side, four above
    /// and below. Against a wide margin a band running to both edges reads as a
    /// rule through the page rather than as one row being pointed at.
    pub const fn roomy(mut self) -> Self {
        self.row_radius = 6.0;
        self.hover_strength = 0.55;
        // The rounded fill starts a hair inside its band, so without this the
        // first row's chip sits almost on the rule and reads as attached to it.
        self.column_gap = 2.0;
        self.row_pad_x = 8.0;
        // Half a pixel each side, so two neighbouring fills are separated by
        // exactly one — enough to read as separate rows, not enough to read as
        // a gap between them.
        self.row_pad_y = 0.5;
        self
    }

    pub const fn with_header(mut self, h: f32) -> Self {
        self.header_h = h;
        self
    }

    pub const fn with_footer(mut self, h: f32) -> Self {
        self.footer_h = h;
        self
    }
}

/// How a row is drawn. Selected outranks hovered; neither is a fill on its own.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum RowState {
    #[default]
    Idle,
    Hovered,
    Selected,
}

/// The table's geometry.
///
/// Built per call from the view's own numbers — it holds no rows, no text and no
/// engine, so every rect in it is testable without a font.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct Table {
    /// What the table fills, in the view's own coordinates. It excludes a
    /// details panel or anything else beside it: the caller subtracts that
    /// before it gets here.
    pub area: Rect,
    pub metrics: Metrics,
    pub scroll: f32,
}

impl Table {
    pub fn new(area: Rect, metrics: Metrics, scroll: f32) -> Self {
        Self {
            area,
            metrics,
            scroll,
        }
    }

    // ------------------------------------------------------------- bands

    /// Where the first band's top edge sits when nothing is scrolled.
    pub fn top(&self) -> f32 {
        self.area.y + self.metrics.header_h + self.metrics.columns_h + self.metrics.column_gap
    }

    /// The strip the column headings sit in.
    pub fn columns_rect(&self) -> Rect {
        Rect::new(
            self.area.x,
            self.area.y + self.metrics.header_h,
            self.area.w,
            self.metrics.columns_h,
        )
    }

    /// The band rows may occupy, between [`top`](Self::top) and the footer.
    pub fn body(&self) -> Rect {
        let top = self.top();
        let bottom = self.area.y + self.area.h - self.metrics.footer_h;
        Rect::new(self.area.x, top, self.area.w, (bottom - top).max(0.0))
    }

    pub fn footer_rect(&self) -> Rect {
        Rect::new(
            self.area.x,
            self.area.y + self.area.h - self.metrics.footer_h,
            self.area.w,
            self.metrics.footer_h,
        )
    }

    pub fn band_h(&self, band: Band) -> f32 {
        match band {
            Band::Section => self.metrics.section_h,
            Band::Row => self.metrics.row_h,
        }
    }

    /// Where band `i` is, scrolled. **The only place that arithmetic exists.**
    ///
    /// It walks rather than multiplying, because a section heading is shorter
    /// than a row and a table with three sections in it would otherwise drift
    /// three headings' worth by the bottom.
    pub fn row_rect(&self, bands: &[Band], i: usize) -> Rect {
        let mut y = self.top() - self.scroll;
        for b in bands.iter().take(i) {
            y += self.band_h(*b);
        }
        let h = bands.get(i).map_or(self.metrics.row_h, |b| self.band_h(*b));
        Rect::new(self.area.x, y, self.area.w, h)
    }

    pub fn total_h(&self, bands: &[Band]) -> f32 {
        bands.iter().map(|b| self.band_h(*b)).sum()
    }

    pub fn max_scroll(&self, bands: &[Band]) -> f32 {
        (self.total_h(bands) - self.body().h).max(0.0)
    }

    /// The bands whose rects intersect the [`body`](Self::body) — the cull and
    /// the hit test's bounds check in one function, so a row can never be
    /// clickable where it is not drawn.
    pub fn visible(&self, bands: &[Band]) -> Vec<usize> {
        let body = self.body();
        let (top, bottom) = (body.y, body.y + body.h);
        (0..bands.len())
            .filter(|&i| {
                let r = self.row_rect(bands, i);
                r.y + r.h > top && r.y < bottom
            })
            .collect()
    }

    /// Which band `(x, y)` is on, or `None` outside the body.
    ///
    /// A `Section` is returned like any other band: whether a heading is
    /// clickable is policy, and policy is the caller's.
    pub fn row_at(&self, bands: &[Band], x: f32, y: f32) -> Option<usize> {
        if !self.body().contains(x, y) {
            return None;
        }
        self.visible(bands).into_iter().find(|&i| {
            let r = self.row_rect(bands, i);
            y >= r.y && y < r.y + r.h
        })
    }

    // ------------------------------------------------------------- cells

    /// Column `c`'s cell inside row `r`: from that column's `x` to the next
    /// column's, or to the trailing lane for the last one.
    ///
    /// Bounded on both sides so an elided cell cannot spill into its neighbour,
    /// which is the failure that makes a table look like a paragraph.
    pub fn cell(&self, cols: &[Column], lane_w: f32, c: usize, r: Rect) -> Rect {
        let x = cols.get(c).map_or(self.metrics.pad, |col| col.x);
        let end = match cols.get(c + 1) {
            Some(next) => next.x,
            None => self.trailing_lane(r, lane_w).x,
        };
        Rect::new(x, r.y, (end - x - CELL_GAP).max(0.0), r.h)
    }

    /// The lane at the right-hand end of **every** row, reserved whether or not
    /// a control is showing.
    ///
    /// A button that took its space only on hover would reflow the name beside
    /// it as the pointer moved down the list, which reads as the list twitching
    /// rather than as a button arriving.
    pub fn trailing_lane(&self, r: Rect, lane_w: f32) -> Rect {
        // Clamped against the row *minus its inset*, not the row: taking the
        // full width and then insetting would put the lane's left edge outside
        // the row it belongs to.
        let w = lane_w.clamp(0.0, (r.w - self.metrics.pad).max(0.0));
        Rect::new(r.x + r.w - self.metrics.pad - w, r.y, w, r.h)
    }

    /// `widths.len()` controls laid out right-to-left inside the lane, returned
    /// in reading order.
    ///
    /// One arithmetic, so a Restore button is pressable exactly where it is
    /// drawn.
    pub fn trailing_rects(&self, r: Rect, lane_w: f32, widths: &[f32], gap: f32) -> Vec<Rect> {
        let lane = self.trailing_lane(r, lane_w);
        let h = self.control_h();
        let mut out = Vec::with_capacity(widths.len());
        let mut x = lane.x + lane.w;
        for w in widths.iter().rev() {
            x -= w;
            out.push(Rect::new(x, lane.y + (lane.h - h) * 0.5, *w, h));
            x -= gap;
        }
        out.reverse();
        out
    }

    /// How tall a control in the trailing lane is.
    ///
    /// **Derived from the row, not fixed.** It used to be 24, which was fine
    /// while a row was 40 and overflowed the moment the rows were tightened to
    /// match the explore column — a button standing proud of the band it is
    /// sitting in. Deriving it means a row height can be changed without
    /// remembering that a second number depends on it.
    pub fn control_h(&self) -> f32 {
        let fill = self.metrics.row_h - self.metrics.row_pad_y * 2.0;
        (fill - CONTROL_INSET * 2.0).max(CONTROL_MIN_H)
    }

    // -------------------------------------------------------------- paint

    pub fn paint_columns(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        cols: &[Column],
    ) {
        let p = theme::p();
        let strip = self.columns_rect();
        // **A rule, not a band.** A filled strip reads as a toolbar and cuts the
        // page in two; the headings are labels for the columns under them, and a
        // hairline is enough to say so. The rule is inset to the table's margin
        // so it lines up with the rows rather than bleeding to the window edge.
        let inset = self.metrics.pad * 0.5;
        scene::fill(
            scene,
            root,
            p.panel_border,
            &scene::rect(
                strip.x + inset,
                strip.y + strip.h - 1.0,
                (strip.w - inset * 2.0).max(0.0),
                1.0,
            ),
        );
        for col in cols {
            let l = engine.simple_layout(col.label, theme::role::hint());
            scene::draw_layout_first_line(
                scene,
                root,
                &l,
                col.x,
                strip.y + (strip.h - scene::first_line_height(&l)) * 0.5,
                Some(p.row_hint),
            );
        }
    }

    pub fn paint_row_fill(&self, scene: &mut Scene, root: Affine, r: Rect, state: RowState) {
        let p = theme::p();
        let fill = match state {
            RowState::Idle => return,
            RowState::Hovered => p.hover_tint.multiply_alpha(self.metrics.hover_strength),
            RowState::Selected => p.row_selected,
        };
        // Padding **around the content column**, not an inset from the row, so
        // the fill always contains the text it is highlighting however wide the
        // page margin gets.
        let m = self.metrics;
        let x = r.x + m.pad - m.row_pad_x;
        let w = r.w - (m.pad - m.row_pad_x) * 2.0;
        scene::fill(
            scene,
            root,
            fill,
            &scene::rrect(
                x,
                r.y + m.row_pad_y,
                w.max(0.0),
                (r.h - m.row_pad_y * 2.0).max(0.0),
                m.row_radius,
            ),
        );
    }

    pub fn paint_section(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        r: Rect,
        label: &str,
    ) {
        let p = theme::p();
        let l = engine.simple_layout(label, theme::role::hint());
        scene::draw_layout_first_line(
            scene,
            root,
            &l,
            r.x + self.metrics.pad,
            r.y + (r.h - scene::first_line_height(&l)) * 0.5,
            Some(p.row_hint),
        );
    }

    /// One cell of text, elided to the cell and vertically centred.
    ///
    /// **Returns the width it drew**, because a caller that puts a badge or a
    /// suffix after the text needs to know where the text ended — and measuring
    /// it a second time at the call site is how the two come to disagree.
    pub fn paint_cell(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        cell: Rect,
        text: &str,
        style: theme::TextStyle,
        colour: Color,
    ) -> f32 {
        if cell.w <= 0.0 || text.is_empty() {
            return 0.0;
        }
        let fitted = engine.elided(text, style, cell.w);
        let l = engine.simple_layout(&fitted, style);
        scene::draw_layout_first_line(
            scene,
            root,
            &l,
            cell.x,
            cell.y + (cell.h - scene::first_line_height(&l)) * 0.5,
            Some(colour),
        );
        l.width()
    }

    /// The Restore shape: an outlined pill, tinted on hover, red when
    /// destructive. The dialog button recipe at row scale, kept out of a
    /// fourteenth hand-rolled copy.
    #[allow(clippy::too_many_arguments)]
    pub fn paint_trailing_button(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        r: Rect,
        label: &str,
        hot: bool,
        destructive: bool,
    ) {
        let p = theme::p();
        let fg = if destructive { p.close_hover } else { p.text };
        let shape = scene::rrect(r.x, r.y, r.w, r.h, 5.0);
        scene::fill(scene, root, p.panel_bg, &shape);
        scene::stroke(scene, root, p.panel_border, theme::BORDER_W, &shape);
        if hot {
            scene::fill(scene, root, p.hover_tint, &shape);
        }
        let l = engine.simple_layout(label, theme::role::hint());
        scene::draw_layout_first_line(
            scene,
            root,
            &l,
            r.x + (r.w - l.width()) * 0.5,
            r.y + (r.h - scene::first_line_height(&l)) * 0.5,
            Some(fg),
        );
    }

    /// The `…` that opens a row's menu.
    /// A row's `…`: **the icon button every other `…` in the app is** — the
    /// caption's, a document title's, Explore's — with `DOTS_THREE`, lit while
    /// the pointer is on it. It was three dots painted by hand, which looked
    /// like the icon only until either changed.
    pub fn paint_trailing_dots(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        r: Rect,
        hot: bool,
    ) {
        crate::icon_button::draw_with(
            scene,
            root,
            engine,
            r,
            koru_draw::icons::DOTS_THREE,
            crate::icon_button::State {
                hovered: hot,
                active: false,
                disabled: false,
            },
            crate::icon_button::Metrics::INLINE,
        );
    }
}

/// Air between one cell's text and the next column's start.
const CELL_GAP: f32 = 12.0;
/// The shortest a trailing control gets, whatever the row does. Below this a
/// button stops looking like one.
const CONTROL_MIN_H: f32 = 16.0;
/// Air above and below a trailing control, inside the row's own fill.
const CONTROL_INSET: f32 = 2.0;

#[cfg(test)]
mod tests;
