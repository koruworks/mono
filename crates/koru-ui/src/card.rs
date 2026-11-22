// SPDX-License-Identifier: MPL-2.0

//! A card of setting rows: a quiet title above a bordered group, each row a
//! name (and optionally a sentence) on the left and its one control on the
//! right.
//!
//! **Grouping instead of headings everywhere.** A long page of labels stacked
//! over controls makes every row look equally important and every gap look
//! like a section break. A card says *these belong together* once, and a row
//! says *this is one setting* once — which is what lets a page drop most of
//! its headings.
//!
//! Two pieces, because the row is used without its neighbours as often as with
//! them: [`Card`] owns the title, the box and the dividers; [`CardRow`] owns a
//! row's text and where its control goes. Both measure before they draw and
//! draw from the same numbers, and both return their inspection scope so what
//! the caller paints next — the rows, the control — is recorded *inside* them.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{inspect, scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

use crate::text_paragraph::KoruTextParagraph;

/// The group.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct Card<'a> {
    /// The quiet line above the box. `None` for a card that stands alone.
    pub title: Option<&'a str>,
    /// A second quiet line at the title's right end — a fact about the whole
    /// group, such as *Transcribed on this device*.
    pub aside: Option<&'a str>,
}

impl<'a> Card<'a> {
    pub const RADIUS: f32 = 10.0;
    /// The title line and the air under it.
    pub const TITLE_H: f32 = 16.0;
    pub const TITLE_GAP: f32 = 8.0;
    /// Between one card and the next on a page.
    pub const GAP: f32 = 24.0;

    pub const fn new(title: Option<&'a str>) -> Self {
        Self { title, aside: None }
    }

    pub const fn aside(mut self, aside: &'a str) -> Self {
        self.aside = Some(aside);
        self
    }

    /// How far the box starts below the card's top.
    pub fn header_h(self) -> f32 {
        if self.title.is_some() || self.aside.is_some() {
            Self::TITLE_H + Self::TITLE_GAP
        } else {
            0.0
        }
    }

    /// The box, for a card whose whole rect (title included) is `rect`.
    pub fn body(self, rect: Rect) -> Rect {
        let top = self.header_h();
        Rect::new(rect.x, rect.y + top, rect.w, (rect.h - top).max(0.0))
    }

    /// Paint the title and the box, and a hairline at each of `dividers` (a
    /// row's top edge, in the same coordinates). Hold the returned scope while
    /// the rows are drawn so they nest under the card.
    #[track_caller]
    #[must_use = "rows nest under the card only while the scope is held"]
    pub fn draw(
        self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        dividers: &[f32],
        alpha: f32,
    ) -> inspect::Scope {
        self.draw_highlighted(canvas, root, engine, rect, dividers, alpha, "")
    }

    /// Draw the card with search matches highlighted in its title and aside.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_highlighted(
        self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        rect: Rect,
        dividers: &[f32],
        alpha: f32,
        query: &str,
    ) -> inspect::Scope {
        let scope = inspect::scope(
            "KoruCard",
            "component",
            root,
            scene::rect(rect.x, rect.y, rect.w, rect.h),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::card::Card",
                    "shared implementation",
                );
                r.property("title", self.title.unwrap_or("none"), "caller title");
                r.property("rows", dividers.len() + 1, "caller rows");
                r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
            },
        );
        let p = theme::p();
        let style = theme::role::meta();
        if let Some(title) = self.title {
            let layout = engine.simple_layout(title, style);
            crate::text_paragraph::highlight_layout(
                canvas, root, &layout, title, query, rect.x, rect.y,
            );
            crate::typography::line(
                "Card title",
                canvas,
                root,
                &layout,
                rect.x,
                rect.y,
                style,
                "theme::role::meta()",
                p.text_dim.multiply_alpha(alpha),
            );
        }
        if let Some(aside) = self.aside {
            let layout = engine.simple_layout(aside, style);
            crate::text_paragraph::highlight_layout(
                canvas,
                root,
                &layout,
                aside,
                query,
                rect.x + rect.w - layout.width(),
                rect.y,
            );
            crate::typography::line(
                "Card aside",
                canvas,
                root,
                &layout,
                rect.x + rect.w - layout.width(),
                rect.y,
                style,
                "theme::role::meta()",
                p.placeholder.multiply_alpha(alpha),
            );
        }
        let body = self.body(rect);
        let shape = scene::rrect(body.x, body.y, body.w, body.h, Self::RADIUS);
        scene::fill(canvas, root, p.panel_bg.multiply_alpha(alpha), &shape);
        scene::stroke(
            canvas,
            root,
            p.panel_border.multiply_alpha(alpha),
            theme::BORDER_W,
            &shape,
        );
        for &y in dividers {
            scene::fill(
                canvas,
                root,
                p.panel_border.multiply_alpha(alpha * 0.6),
                &scene::rect(
                    body.x + CardRow::PAD_X,
                    y,
                    body.w - CardRow::PAD_X * 2.0,
                    1.0,
                ),
            );
        }
        scope
    }
}

/// One setting in a card.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CardRow<'a> {
    pub label: &'a str,
    /// The sentence under the name. Wraps inside the text column.
    pub hint: Option<&'a str>,
    /// A link that ends the hint — *apply to all* — flowing after its last
    /// word, or onto the next line when it does not fit. The caller presses
    /// it through [`link_rect`](Self::link_rect).
    pub link: Option<&'a str>,
    /// A [`KeyChip`](crate::KeyChip) beside the name: the chord the row is
    /// about.
    pub chip: Option<&'a str>,
}

impl<'a> CardRow<'a> {
    pub const PAD_X: f32 = 16.0;
    pub const PAD_Y: f32 = 12.0;
    /// Between the text column and the control.
    pub const GAP: f32 = 20.0;
    const HINT_GAP: f32 = 2.0;
    /// The shortest a row is, so a row whose control is taller than its one
    /// line of text still has even air.
    pub const MIN_H: f32 = 48.0;

    /// Between the name and its chip, and the space before a hint's link.
    const CHIP_GAP: f32 = 10.0;
    const LINK_SPACE: f32 = 4.0;

    pub const fn new(label: &'a str) -> Self {
        Self {
            label,
            hint: None,
            link: None,
            chip: None,
        }
    }

    pub const fn hint(mut self, hint: &'a str) -> Self {
        self.hint = Some(hint);
        self
    }

    pub const fn link(mut self, link: &'a str) -> Self {
        self.link = Some(link);
        self
    }

    pub const fn chip(mut self, chip: &'a str) -> Self {
        self.chip = Some(chip);
        self
    }

    /// The name's height: its own lines, or the chip beside it when taller.
    fn label_h(self, engine: &mut TextEngine, text_w: f32) -> f32 {
        let label_w = self.label_w(engine, text_w);
        let h = KoruTextParagraph::new(self.label, Self::label_style(), "", theme::p().text)
            .measure(engine, label_w);
        if self.chip.is_some() {
            h.max(crate::KeyChip::H)
        } else {
            h
        }
    }

    /// The width the name wraps in: the text column, less its chip.
    fn label_w(self, engine: &mut TextEngine, text_w: f32) -> f32 {
        match self.chip {
            Some(chip) => (text_w - crate::KeyChip::width(engine, chip) - Self::CHIP_GAP).max(40.0),
            None => text_w,
        }
    }

    /// The hint's height, its link included, and where the link sits
    /// relative to the hint's top-left.
    fn hint_parts(self, engine: &mut TextEngine, text_w: f32) -> (f32, Option<Rect>) {
        let style = Self::hint_style();
        let line = style.metrics.line_height;
        let (mut h, last_w, last_y) = match self.hint {
            Some(hint) => {
                let laid = engine.simple_layout_wrapped(hint, style, text_w);
                let last = laid.lines().last().map(|l| {
                    let m = l.metrics();
                    (m.advance, m.block_min_coord)
                });
                let (w, y) = last.unwrap_or((0.0, 0.0));
                (laid.height(), Some(w), y)
            }
            None => (0.0, None, 0.0),
        };
        let link = self.link.map(|link| {
            let w = engine.simple_layout(link, style).width();
            match last_w {
                Some(last) if last + Self::LINK_SPACE + w <= text_w => {
                    Rect::new(last + Self::LINK_SPACE, last_y, w, line)
                }
                Some(_) => {
                    let rect = Rect::new(0.0, h, w, line);
                    h += line;
                    rect
                }
                None => {
                    h = line;
                    Rect::new(0.0, 0.0, w, line)
                }
            }
        });
        (h, link)
    }

    fn label_style() -> theme::TextStyle {
        theme::role::ui()
    }

    fn hint_style() -> theme::TextStyle {
        theme::role::meta()
    }

    /// The width left for the text once a control `control_w` wide has its
    /// place, in a row `row_w` wide.
    pub fn text_w(row_w: f32, control_w: f32) -> f32 {
        let control = if control_w > 0.0 {
            control_w + Self::GAP
        } else {
            0.0
        };
        (row_w - Self::PAD_X * 2.0 - control).max(40.0)
    }

    /// The row's height with a control `control_h` tall.
    pub fn height(self, engine: &mut TextEngine, text_w: f32, control_h: f32) -> f32 {
        let label = self.label_h(engine, text_w);
        let hint = self.hint_block_h(engine, text_w);
        (label + hint).max(control_h) + Self::PAD_Y * 2.0
    }

    /// The hint (and its link) with the gap above it; nothing without either.
    fn hint_block_h(self, engine: &mut TextEngine, text_w: f32) -> f32 {
        if self.hint.is_none() && self.link.is_none() {
            return 0.0;
        }
        Self::HINT_GAP + self.hint_parts(engine, text_w).0
    }

    /// Where the text column starts, for a row `row` of this height.
    fn text_top(self, engine: &mut TextEngine, row: Rect, text_w: f32) -> f32 {
        let label = self.label_h(engine, text_w);
        let hint = self.hint_block_h(engine, text_w);
        row.y + (row.h - label - hint) * 0.5
    }

    /// The link's rect in `row`, for the caller's hit test.
    pub fn link_rect(self, engine: &mut TextEngine, row: Rect, text_w: f32) -> Option<Rect> {
        let (_, link) = self.hint_parts(engine, text_w);
        let link = link?;
        let top =
            self.text_top(engine, row, text_w) + self.label_h(engine, text_w) + Self::HINT_GAP;
        Some(Rect::new(
            row.x + Self::PAD_X + link.x,
            top + link.y,
            link.w,
            link.h,
        ))
    }

    /// Where a control `w` by `h` sits: against the row's right edge,
    /// centred on its height.
    pub fn control_slot(row: Rect, w: f32, h: f32) -> Rect {
        Rect::new(
            row.x + row.w - Self::PAD_X - w,
            row.y + (row.h - h) * 0.5,
            w,
            h,
        )
    }

    /// Paint the text. Hold the returned scope while the control is drawn so
    /// it nests under its row.
    #[track_caller]
    #[must_use = "the control nests under its row only while the scope is held"]
    pub fn draw(
        self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        row: Rect,
        text_w: f32,
        alpha: f32,
    ) -> inspect::Scope {
        self.draw_highlighted(canvas, root, engine, row, text_w, alpha, "")
    }

    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    pub fn draw_highlighted(
        self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        row: Rect,
        text_w: f32,
        alpha: f32,
        query: &str,
    ) -> inspect::Scope {
        let scope = inspect::scope(
            "KoruCardRow",
            "component",
            root,
            scene::rect(row.x, row.y, row.w, row.h),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::card::CardRow",
                    "shared implementation",
                );
                r.property("label", self.label, "caller label");
                r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
            },
        );
        let p = theme::p();
        let label_h = self.label_h(engine, text_w);
        let label_w = self.label_w(engine, text_w);
        let x = row.x + Self::PAD_X;
        let y = if self.hint.is_none() && self.link.is_none() {
            let label = engine.simple_layout_wrapped(self.label, Self::label_style(), label_w);
            scene::centre_y_in(&label, row)
        } else {
            self.text_top(engine, row, text_w)
        };
        let drawn_h = KoruTextParagraph::new(
            self.label,
            Self::label_style(),
            "theme::role::ui()",
            p.text.multiply_alpha(alpha),
        )
        .highlight(query)
        .draw(canvas, root, engine, Rect::new(x, y, label_w, 0.0));
        if let Some(chip) = self.chip {
            let first_line = Self::label_style().metrics.line_height.min(drawn_h);
            let w = crate::KeyChip::width(engine, chip);
            let name_w = engine
                .simple_layout(self.label, Self::label_style())
                .width()
                .min(label_w);
            crate::KeyChip::draw(
                canvas,
                root,
                engine,
                Rect::new(
                    x + name_w + Self::CHIP_GAP,
                    y + (first_line - crate::KeyChip::H) * 0.5,
                    w,
                    crate::KeyChip::H,
                ),
                chip,
            );
        }
        if let Some(link) = self.link {
            let (_, at) = self.hint_parts(engine, text_w);
            if let Some(at) = at {
                let style = Self::hint_style();
                crate::text_paragraph::draw_line(
                    canvas,
                    root,
                    engine,
                    link,
                    x + at.x,
                    y + label_h + Self::HINT_GAP + at.y,
                    style,
                    "theme::role::meta() — a link",
                    p.accent.multiply_alpha(alpha),
                );
            }
        }
        if let Some(hint) = self.hint {
            KoruTextParagraph::new(
                hint,
                Self::hint_style(),
                "theme::role::meta()",
                p.text_dim.multiply_alpha(alpha),
            )
            .highlight(query)
            .draw(
                canvas,
                root,
                engine,
                Rect::new(x, y + label_h + Self::HINT_GAP, text_w, 0.0),
            );
        }
        scope
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_titled_card_starts_its_box_below_the_title() {
        let rect = Rect::new(0.0, 100.0, 400.0, 200.0);
        let titled = Card::new(Some("Interface"));
        assert_eq!(titled.body(rect).y, 100.0 + Card::TITLE_H + Card::TITLE_GAP);
        assert_eq!(Card::new(None).body(rect), rect);
        assert_eq!(Card::default().aside("fact").header_h(), titled.header_h());
    }

    #[test]
    fn a_hint_makes_a_row_taller_and_a_tall_control_sets_the_floor() {
        let mut engine = TextEngine::new();
        let w = CardRow::text_w(480.0, 150.0);
        let plain = CardRow::new("Density").height(&mut engine, w, 30.0);
        let hinted = CardRow::new("Density")
            .hint("Spacing of text and controls.")
            .height(&mut engine, w, 30.0);
        assert!(hinted > plain);
        let tall = CardRow::new("Density").height(&mut engine, w, 70.0);
        assert_eq!(tall, 70.0 + CardRow::PAD_Y * 2.0);
    }

    #[test]
    fn the_control_sits_against_the_right_edge_and_the_text_never_reaches_it() {
        let row = Rect::new(10.0, 0.0, 480.0, 48.0);
        let slot = CardRow::control_slot(row, 150.0, 30.0);
        assert_eq!(slot.x + slot.w, row.x + row.w - CardRow::PAD_X);
        assert_eq!(slot.y, 9.0);
        let text_right = row.x + CardRow::PAD_X + CardRow::text_w(row.w, 150.0);
        assert!(text_right + CardRow::GAP <= slot.x + 0.01);
    }
}
