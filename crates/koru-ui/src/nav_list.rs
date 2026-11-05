// SPDX-License-Identifier: MPL-2.0

//! Lists of destinations — a settings page's sections, one of them current.
//!
//! Two shapes of the same choice, for two widths:
//!
//! - [`NavList`] — a vertical column of names, each on a 2 px rule whose
//!   current one is the accent (Settings v6, 2026-10-01). Settings and Manage
//!   both draw it; the *filled* look Manage once had went with its grey side
//!   panel (2026-10-07).
//! - [`ChipNav`] — the same names as pill chips that wrap, for a column too
//!   narrow to give the list a column of its own. The current chip is filled.
//!
//! **Navigation, not a choice.** The current item is the page being shown, so
//! it is marked by where it sits (the lit rule, the filled chip) rather than a check: a person reads *where am I*, not *what did I
//! pick*. Geometry is one pure function of the area and the index, used by
//! drawing and by the caller's hit test alike.
//!
//! An item may carry more than its name (maintainers' decision 14, 2026-10-05): a
//! leading [`Tile`] or the dashed *add* square, a trailing word, a status dot
//! and a warning mark. Each is optional, and an item without them draws
//! exactly as a bare name always did. The tile is small enough
//! ([`NavList::TILE`]) that a row with one keeps the bare row's 30 px.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::tile::Tile;
use koru_draw::{icons, inspect, scene, theme};
use vello::Scene;
use vello::kurbo::{Affine, Stroke};
use vello::peniko::Color;

/// What sits before an item's name.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum NavLeading<'a> {
    /// A Profile's or a Workspace's tile — its emoji or initial on its color.
    Tile(Tile<'a>),
    /// The dashed square with a plus that a *New…* row wears. Its name is
    /// drawn muted, because the row is an action rather than a place.
    Add,
    /// An icon glyph in the tile's slot — a computer, a cloud — for a row
    /// that is a place but has no tile of its own (the review's note).
    Glyph(char),
}

/// One destination.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct NavItem<'a> {
    pub label: &'a str,
    pub leading: Option<NavLeading<'a>>,
    /// A short word at the trailing edge: a count, *On*, *Connect*.
    pub trailing: Option<&'a str>,
    /// A small dot before the trailing word — *this one is current*, *this is
    /// on*. The caller chooses the color, from the theme.
    pub dot: Option<Color>,
    /// A warning mark: this destination needs the person's attention.
    pub attention: bool,
}

impl<'a> NavItem<'a> {
    /// A bare name, which is what every item was before the extras.
    pub const fn new(label: &'a str) -> Self {
        Self {
            label,
            leading: None,
            trailing: None,
            dot: None,
            attention: false,
        }
    }

    pub const fn with_tile(mut self, tile: Tile<'a>) -> Self {
        self.leading = Some(NavLeading::Tile(tile));
        self
    }

    pub const fn with_add(mut self) -> Self {
        self.leading = Some(NavLeading::Add);
        self
    }

    pub const fn with_glyph(mut self, glyph: char) -> Self {
        self.leading = Some(NavLeading::Glyph(glyph));
        self
    }

    pub const fn with_trailing(mut self, trailing: &'a str) -> Self {
        self.trailing = Some(trailing);
        self
    }

    pub const fn with_dot(mut self, color: Color) -> Self {
        self.dot = Some(color);
        self
    }

    pub const fn with_attention(mut self, attention: bool) -> Self {
        self.attention = attention;
        self
    }
}

/// The vertical list.
pub struct NavList;

impl NavList {
    pub const ROW_H: f32 = 30.0;
    pub const ROW_GAP: f32 = 1.0;
    /// The leading tile's side — small enough to leave the row its air.
    pub const TILE: f32 = 18.0;
    /// The rule down the row's left edge, and the label's distance from it.
    const RULE_W: f32 = 2.0;
    /// Air between the rule and the focus ring, so the two do not touch
    /// (the review's note).
    const RULE_GAP: f32 = 4.0;
    const LABEL_X: f32 = 12.0;
    /// The trailing marks' distance from the row's right end, and the focus
    /// ring's corner radius.
    const TRAIL_PAD: f32 = 10.0;
    const RADIUS: f32 = 6.0;
    /// Between the leading tile and the name, and between trailing marks.
    const GAP: f32 = 8.0;
    const DOT: f32 = 6.0;
    const MARK: f32 = 14.0;

    /// Row `index`, for a list whose top-left is `area`'s.
    pub fn row_rect(area: Rect, index: usize) -> Rect {
        Rect::new(
            area.x,
            area.y + index as f32 * (Self::ROW_H + Self::ROW_GAP),
            area.w,
            Self::ROW_H,
        )
    }

    /// The height `count` rows take.
    pub fn height(count: usize) -> f32 {
        if count == 0 {
            0.0
        } else {
            count as f32 * Self::ROW_H + (count - 1) as f32 * Self::ROW_GAP
        }
    }

    /// The row under (`x`, `y`) for a list of `count` rows.
    pub fn row_at(area: Rect, count: usize, x: f32, y: f32) -> Option<usize> {
        (0..count).find(|&i| Self::row_rect(area, i).contains(x, y))
    }

    /// Where the leading tile of `row` goes: where a bare name starts.
    pub fn tile_rect(row: Rect) -> Rect {
        Rect::new(
            row.x + Self::LABEL_X,
            row.y + (row.h - Self::TILE) * 0.5,
            Self::TILE,
            Self::TILE,
        )
    }

    /// The list, `current` on the accent rule.
    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    pub fn draw(
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        items: &[NavItem<'_>],
        current: Option<usize>,
        hovered: Option<usize>,
        focused: Option<usize>,
    ) {
        let height = Self::height(items.len());
        let _component = inspect::scope(
            "KoruNavList",
            "component",
            root,
            scene::rect(area.x, area.y, area.w, height),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::nav_list::NavList",
                    "shared implementation",
                );
                r.property("rows", items.len(), "caller items");
                r.property(
                    "current",
                    current.map_or("none".to_owned(), |i| i.to_string()),
                    "instance state",
                );
                r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
            },
        );
        for (index, item) in items.iter().enumerate() {
            let row = Self::row_rect(area, index);
            let state = RowState {
                here: current == Some(index),
                hovered: hovered == Some(index),
                focused: focused == Some(index),
            };
            Self::draw_row(canvas, root, engine, row, item, state);
        }
    }

    #[track_caller]
    fn draw_row(
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        row: Rect,
        item: &NavItem<'_>,
        state: RowState,
    ) {
        let p = theme::p();
        let _row = inspect::scope(
            "Nav row",
            "part",
            root,
            scene::rect(row.x, row.y, row.w, row.h),
            |r| {
                r.property("label", item.label, "caller item");
                r.property("current", state.here, "instance state");
                if let Some(trailing) = item.trailing {
                    r.property("trailing", trailing, "caller item");
                }
                if item.attention {
                    r.property("attention", true, "caller item");
                }
            },
        );
        scene::fill(
            canvas,
            root,
            if state.here { p.accent } else { p.panel_border },
            &scene::rect(row.x, row.y, Self::RULE_W, row.h),
        );
        if state.focused {
            let x = row.x + Self::RULE_W + Self::RULE_GAP;
            let w = (row.w - Self::RULE_W - Self::RULE_GAP).max(0.0);
            scene::stroke(
                canvas,
                root,
                p.focus_ring,
                theme::BORDER_W * 2.0,
                &scene::rrect(x, row.y, w, row.h, Self::RADIUS),
            );
        }
        // The leading tile, and so where the name starts.
        let mut label_x = row.x + Self::LABEL_X;
        if let Some(leading) = item.leading {
            let tile = Self::tile_rect(row);
            match leading {
                NavLeading::Tile(face) => {
                    let _tile = inspect::scope(
                        "Nav tile",
                        "part",
                        root,
                        scene::rect(tile.x, tile.y, tile.w, tile.h),
                        |r| r.property("size", Self::TILE, "NavList::TILE"),
                    );
                    face.draw(canvas, root, engine, tile);
                }
                NavLeading::Add => Self::draw_add(canvas, root, engine, tile),
                NavLeading::Glyph(glyph) => {
                    let p = theme::p();
                    let icon = engine.icon_layout(glyph, tile.w * 0.9);
                    let (x, y) = scene::centre_in(&icon, tile);
                    let color = if state.here || state.hovered {
                        p.text
                    } else {
                        p.text_dim
                    };
                    scene::draw_layout(canvas, root, &icon, x, y, Some(color));
                }
            }
            label_x = tile.x + tile.w + Self::GAP;
        }
        // The trailing marks, right to left: the word, the dot, the warning.
        let mut right = row.x + row.w - Self::TRAIL_PAD;
        if let Some(trailing) = item.trailing {
            let role = theme::role::meta();
            let layout = engine.simple_layout(trailing, role);
            right -= layout.width();
            crate::typography::line(
                "Nav trailing",
                canvas,
                root,
                &layout,
                right,
                row.y + (row.h - scene::first_line_height(&layout)) * 0.5,
                role,
                "theme::role::meta()",
                p.text_dim,
            );
            right -= Self::GAP;
        }
        if let Some(color) = item.dot {
            let cx = right - Self::DOT * 0.5;
            let cy = row.y + row.h * 0.5;
            let _dot = inspect::scope(
                "Nav dot",
                "part",
                root,
                scene::rect(
                    cx - Self::DOT * 0.5,
                    cy - Self::DOT * 0.5,
                    Self::DOT,
                    Self::DOT,
                ),
                |_| {},
            );
            scene::fill(canvas, root, color, &scene::circle(cx, cy, Self::DOT * 0.5));
            right -= Self::DOT + Self::GAP;
        }
        if item.attention {
            let at = Rect::new(
                right - Self::MARK,
                row.y + (row.h - Self::MARK) * 0.5,
                Self::MARK,
                Self::MARK,
            );
            let _mark = inspect::scope(
                "Nav attention",
                "part",
                root,
                scene::rect(at.x, at.y, at.w, at.h),
                |r| r.property("icon", "WARNING", "koru_draw::icons"),
            );
            let mark = engine.icon_layout(icons::WARNING, Self::MARK);
            let (x, y) = scene::centre_in(&mark, at);
            scene::draw_layout(canvas, root, &mark, x, y, Some(p.notice_fg));
            right -= Self::MARK + Self::GAP;
        }
        let role = if state.here {
            theme::TextStyle::new(theme::role::row().metrics, theme::FontWeight::SemiBold)
        } else {
            theme::role::row()
        };
        // Elided rather than clipped: a long Profile name must not run under
        // the marks that say something about it.
        let text = engine.elided(item.label, role, (right - label_x).max(0.0));
        let label = engine.simple_layout(&text, role);
        let line_h = scene::first_line_height(&label);
        let color = match item.leading {
            Some(NavLeading::Add) if !state.hovered => p.text_dim,
            _ if state.here || state.hovered => p.text,
            _ => p.text_dim,
        };
        crate::typography::line(
            "Nav label",
            canvas,
            root,
            &label,
            label_x,
            row.y + (row.h - line_h) * 0.5,
            role,
            "theme::role::row()",
            color,
        );
    }

    /// The *add* square: a dashed outline with a plus in it.
    fn draw_add(canvas: &mut Scene, root: Affine, engine: &mut TextEngine, at: Rect) {
        let p = theme::p();
        let _add = inspect::scope(
            "Nav add",
            "part",
            root,
            scene::rect(at.x, at.y, at.w, at.h),
            |r| r.property("icon", "PLUS", "koru_draw::icons"),
        );
        let inset = theme::BORDER_W as f32 * 0.5;
        canvas.stroke(
            &Stroke::new(theme::BORDER_W).with_dashes(0.0, [3.0, 2.0]),
            root,
            p.control_border,
            None,
            &scene::rrect(
                at.x + inset,
                at.y + inset,
                at.w - inset * 2.0,
                at.h - inset * 2.0,
                at.w * 0.22,
            ),
        );
        let plus = engine.icon_layout(icons::PLUS, at.w * 0.55);
        let (x, y) = scene::centre_in(&plus, at);
        scene::draw_layout(canvas, root, &plus, x, y, Some(p.text_dim));
    }
}

/// What one row is right now.
#[derive(Copy, Clone)]
struct RowState {
    here: bool,
    hovered: bool,
    focused: bool,
}

/// The same destinations as pill chips that wrap — for a column too narrow
/// to give [`NavList`] a column of its own.
pub struct ChipNav;

impl ChipNav {
    pub const H: f32 = 30.0;
    const PAD_X: f32 = 13.0;
    const GAP: f32 = 6.0;

    /// The chips' rects, left to right, wrapping inside `area.w`.
    pub fn layout(engine: &mut TextEngine, area: Rect, items: &[NavItem<'_>]) -> Vec<Rect> {
        let mut x = area.x;
        let mut y = area.y;
        items
            .iter()
            .map(|item| {
                let w = engine.simple_layout(item.label, theme::role::row()).width()
                    + Self::PAD_X * 2.0;
                if x > area.x && x + w > area.x + area.w {
                    x = area.x;
                    y += Self::H + Self::GAP;
                }
                let rect = Rect::new(x, y, w, Self::H);
                x += w + Self::GAP;
                rect
            })
            .collect()
    }

    /// The height `chips` take, from the top of the first to the bottom of
    /// the last.
    pub fn height(chips: &[Rect]) -> f32 {
        match (chips.first(), chips.last()) {
            (Some(first), Some(last)) => last.y + last.h - first.y,
            _ => 0.0,
        }
    }

    /// The chip under (`x`, `y`).
    pub fn chip_at(chips: &[Rect], x: f32, y: f32) -> Option<usize> {
        chips.iter().position(|chip| chip.contains(x, y))
    }

    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    pub fn draw(
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        chips: &[Rect],
        items: &[NavItem<'_>],
        current: Option<usize>,
        hovered: Option<usize>,
        focused: Option<usize>,
    ) {
        let bounds = match (chips.first(), chips.last()) {
            (Some(first), Some(_)) => {
                let right = chips.iter().map(|c| c.x + c.w).fold(first.x, f32::max);
                Rect::new(first.x, first.y, right - first.x, Self::height(chips))
            }
            _ => Rect::new(0.0, 0.0, 0.0, 0.0),
        };
        let _component = inspect::scope(
            "KoruChipNav",
            "component",
            root,
            scene::rect(bounds.x, bounds.y, bounds.w, bounds.h),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::nav_list::ChipNav",
                    "shared implementation",
                );
                r.property("chips", items.len(), "caller items");
                r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
            },
        );
        let p = theme::p();
        for (index, (chip, item)) in chips.iter().zip(items).enumerate() {
            let here = current == Some(index);
            let _chip = inspect::scope(
                "Nav chip",
                "part",
                root,
                scene::rect(chip.x, chip.y, chip.w, chip.h),
                |r| {
                    r.property("label", item.label, "caller item");
                    r.property("current", here, "instance state");
                },
            );
            let shape = scene::rrect(chip.x, chip.y, chip.w, chip.h, chip.h * 0.5);
            if here {
                scene::fill(canvas, root, p.text, &shape);
            } else {
                scene::fill(canvas, root, p.bg, &shape);
                scene::stroke(
                    canvas,
                    root,
                    if hovered == Some(index) {
                        p.text_dim
                    } else {
                        p.control_border
                    },
                    theme::CONTROL_BORDER_W,
                    &shape,
                );
            }
            if focused == Some(index) {
                scene::stroke(
                    canvas,
                    root,
                    p.focus_ring,
                    theme::BORDER_W * 2.0,
                    &scene::rrect(
                        chip.x - 2.0,
                        chip.y - 2.0,
                        chip.w + 4.0,
                        chip.h + 4.0,
                        chip.h * 0.5 + 2.0,
                    ),
                );
            }
            let style = theme::role::row();
            let label = engine.simple_layout(item.label, style);
            crate::typography::line(
                "Nav chip label",
                canvas,
                root,
                &label,
                chip.x + (chip.w - label.width()) * 0.5,
                chip.y + (chip.h - scene::first_line_height(&label)) * 0.5,
                style,
                "theme::role::row()",
                if here { p.bg } else { p.text },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_stack_with_a_gap_and_are_found_by_the_same_arithmetic() {
        let area = Rect::new(10.0, 100.0, 200.0, 400.0);
        let second = NavList::row_rect(area, 1);
        assert_eq!(second.y, 100.0 + NavList::ROW_H + NavList::ROW_GAP);
        let (x, y) = second.centre();
        assert_eq!(NavList::row_at(area, 5, x, y), Some(1));
        assert_eq!(NavList::row_at(area, 1, x, y), None, "past the last row");
        assert_eq!(
            NavList::height(3),
            NavList::ROW_H * 3.0 + NavList::ROW_GAP * 2.0
        );
    }

    fn parts(items: &[NavItem<'_>]) -> Vec<&'static str> {
        let mut engine = TextEngine::new();
        let area = Rect::new(0.0, 0.0, 216.0, 400.0);
        let frame = inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, 300.0, 400.0));
        NavList::draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut engine,
            area,
            items,
            Some(0),
            None,
            None,
        );
        frame
            .finish()
            .records
            .into_iter()
            .map(|record| record.name)
            .collect()
    }

    /// A bare name draws what it always drew — the row and its label, and
    /// nothing the extras add.
    #[test]
    fn a_bare_item_draws_only_its_row_and_label() {
        let names = parts(&[NavItem::new("Editor")]);
        for extra in [
            "Nav tile",
            "Nav add",
            "Nav trailing",
            "Nav dot",
            "Nav attention",
        ] {
            assert!(!names.contains(&extra), "{extra} in {names:?}");
        }
        assert!(names.contains(&"Nav row") && names.contains(&"Nav label"));
    }

    /// Each extra is its own inspectable part.
    #[test]
    fn the_extras_are_parts_of_their_row() {
        let tile = Tile {
            icon: None,
            color: Some("teal"),
            name: "Personal",
        };
        let items = [
            NavItem::new("Personal")
                .with_tile(tile)
                .with_dot(theme::p().accent)
                .with_attention(true),
            NavItem::new("New profile").with_add(),
            NavItem::new("Linked devices").with_trailing("3"),
        ];
        let names = parts(&items);
        for part in [
            "Nav tile",
            "Nav add",
            "Nav trailing",
            "Nav dot",
            "Nav attention",
        ] {
            assert_eq!(
                names.iter().filter(|name| **name == part).count(),
                1,
                "{part}"
            );
        }
    }

    #[test]
    fn the_tile_sits_centred_in_its_row_where_a_bare_name_starts() {
        let row = NavList::row_rect(Rect::new(8.0, 40.0, 216.0, 0.0), 2);
        let tile = NavList::tile_rect(row);
        assert_eq!((tile.w, tile.h), (NavList::TILE, NavList::TILE));
        assert_eq!(tile.y - row.y, row.y + row.h - (tile.y + tile.h));
        assert!(tile.x > row.x && tile.x + tile.w < row.x + row.w);
        assert!(
            NavList::TILE + 8.0 <= NavList::ROW_H,
            "a tiled row keeps the bare row's height and air"
        );
    }

    #[test]
    fn chips_wrap_inside_their_width_and_are_found_where_they_are() {
        let mut engine = TextEngine::new();
        let items = [
            NavItem::new("Appearance"),
            NavItem::new("Editor"),
            NavItem::new("Reminders"),
            NavItem::new("Terminal"),
            NavItem::new("System"),
        ];
        let area = Rect::new(20.0, 50.0, 260.0, 0.0);
        let chips = ChipNav::layout(&mut engine, area, &items);
        assert_eq!(chips.len(), 5);
        assert!(
            chips
                .iter()
                .all(|c| c.x >= area.x && c.x + c.w <= area.x + area.w + 0.5)
        );
        assert!(
            chips.last().unwrap().y > area.y,
            "five chips wrap in 260 px"
        );
        assert!(ChipNav::height(&chips) > ChipNav::H);
        let (x, y) = chips[3].centre();
        assert_eq!(ChipNav::chip_at(&chips, x, y), Some(3));
    }
}
