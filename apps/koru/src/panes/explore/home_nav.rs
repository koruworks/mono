// SPDX-License-Identifier: MPL-2.0

//! **Home's nav** (workspace foundation, frame 1): Inbox and Upcoming, dimmed
//! until they exist (QUESTIONS.md 2; rung 7); Today, which is the Home tab;
//! and RECENT, the documents last opened, three at most.
//!
//! The Tasks rail is the model: rows the column draws below its heading, on
//! the column's own row geometry, reporting what the column cannot do itself
//! as an [`Outcome`]. There is no tree here and never will be, so the rows
//! are an enum rather than a `Vec<Row>` — a fixed shape, like the rail's
//! queues.

use koru_core::DocumentId;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
use koru_i18n::{Key, t};
use koru_shell_chrome::tabs::draw_icon;
use koru_shell_chrome::title_icon::PAGE_ICON_SLOT;
use koru_workspace_host::accessibility::{ControlPart, Occurrence, SemanticNode};
use vello::Scene;
use vello::kurbo::Affine;

use super::accessibility::Context;
use super::{draw_row_card, icon_lane, leading_x, row_chip};
use crate::panes::Outcome;

/// How many recents the nav lists. Frame 1 draws three; the page's Continue
/// section draws the same three as cards.
pub const RECENT_MAX: usize = 3;

const SECTION_GAP: f32 = 10.0;
const HEADING_H: f32 = 20.0;
const SURFACE: &str = "home-nav";

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum HomeRow {
    /// Dimmed: the cross-app Inbox is rung 7.
    Inbox,
    /// The Home tab. Always the card: Home has one page today.
    Today,
    /// Dimmed, for Inbox's reason.
    Upcoming,
    RecentHeading,
    Recent(usize),
}

impl HomeRow {
    /// Whether a press does anything. The dimmed rows explain themselves by
    /// their colour (DESIGN.md: a disabled control is never hidden).
    pub fn pressable(self) -> bool {
        matches!(self, Self::Today | Self::Recent(_))
    }

    fn token(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Today => "today",
            Self::Upcoming => "upcoming",
            Self::RecentHeading => "recent",
            Self::Recent(_) => "recent-document",
        }
    }
}

#[derive(Default)]
pub struct HomeNav {
    recents: Vec<(String, DocumentId)>,
    hover: Option<HomeRow>,
}

impl HomeNav {
    /// The documents last opened, most recent first; the container keeps
    /// them current. Returns whether the list moved.
    pub fn set_recents(&mut self, recents: Vec<(String, DocumentId)>) -> bool {
        let recents: Vec<_> = recents.into_iter().take(RECENT_MAX).collect();
        if self.recents == recents {
            return false;
        }
        self.recents = recents;
        if matches!(self.hover, Some(HomeRow::Recent(i)) if i >= self.recents.len()) {
            self.hover = None;
        }
        true
    }

    #[cfg(test)]
    pub fn recents(&self) -> &[(String, DocumentId)] {
        &self.recents
    }

    /// The rows and where they sit, from `top` down: the three fixed rows,
    /// a gap, RECENT and its documents. The nav never scrolls — it is six
    /// rows at most.
    pub fn rows(&self, area: Rect, top: f32) -> Vec<(HomeRow, Rect)> {
        // The library's row band — the pane's width, the chip inset by
        // `row_chip` — so Home's glyphs sit on the library's icon lane and
        // under the heading's title (the CTO's note, E76).
        let x = area.x;
        let w = area.w.max(0.0);
        let mut out = Vec::with_capacity(5 + self.recents.len());
        let mut y = top;
        for row in [HomeRow::Inbox, HomeRow::Today, HomeRow::Upcoming] {
            out.push((row, Rect::new(x, y, w, theme::ROW_H)));
            y += theme::ROW_H;
        }
        if !self.recents.is_empty() {
            y += SECTION_GAP;
            out.push((HomeRow::RecentHeading, Rect::new(x, y, w, HEADING_H)));
            y += HEADING_H;
            for i in 0..self.recents.len() {
                out.push((HomeRow::Recent(i), Rect::new(x, y, w, theme::ROW_H)));
                y += theme::ROW_H;
            }
        }
        out
    }

    pub fn at(&self, area: Rect, top: f32, x: f32, y: f32) -> Option<HomeRow> {
        self.rows(area, top)
            .into_iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(row, _)| row)
    }

    /// A press: Today opens the Home tab, a recent opens that document, and
    /// everything else is the column's own background.
    pub fn mouse_down(&mut self, area: Rect, top: f32, x: f32, y: f32) -> Outcome {
        match self.at(area, top, x, y) {
            Some(HomeRow::Today) => Outcome::OpenHome,
            Some(HomeRow::Recent(i)) => match self.recents.get(i) {
                Some((_, id)) => Outcome::OpenRecentDocument(*id),
                None => Outcome::Consumed,
            },
            _ => Outcome::Consumed,
        }
    }

    pub fn mouse_move(&mut self, area: Rect, top: f32, x: f32, y: f32) -> bool {
        let next = self.at(area, top, x, y).filter(|row| row.pressable());
        let changed = next != self.hover;
        self.hover = next;
        changed
    }

    pub fn clear_hover(&mut self) -> bool {
        self.hover.take().is_some()
    }

    /// The row's word, in `lang`.
    pub fn label(&self, row: HomeRow, lang: koru_i18n::Lang) -> String {
        match row {
            HomeRow::Inbox => t(lang, Key::HomeInbox).to_owned(),
            HomeRow::Today => t(lang, Key::A11yTaskBucketToday).to_owned(),
            HomeRow::Upcoming => t(lang, Key::A11yTaskBucketUpcoming).to_owned(),
            HomeRow::RecentHeading => t(lang, Key::HomeRecent).to_uppercase(),
            HomeRow::Recent(i) => self
                .recents
                .get(i)
                .map(|(title, _)| title.clone())
                .unwrap_or_default(),
        }
    }

    pub fn draw(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        top: f32,
    ) {
        let p = theme::p();
        let lang = crate::format::lang();
        for (row, r) in self.rows(area, top) {
            let _scope = koru_draw::inspect::scope(
                "ExploreRow",
                "container",
                root,
                scene::rect(r.x, r.y, r.w, r.h),
                |record| record.property("row", format!("{row:?}"), "HomeRow"),
            );
            if row == HomeRow::RecentHeading {
                let heading = engine.simple_layout(&self.label(row, lang), theme::role::hint());
                scene::draw_layout_first_line(
                    scene,
                    root,
                    &heading,
                    leading_x(r.x),
                    r.y + (r.h - scene::first_line_height(&heading)) * 0.5,
                    Some(p.row_hint),
                );
                continue;
            }
            let active = row == HomeRow::Today;
            if active {
                draw_row_card(scene, root, r, p);
            } else if self.hover == Some(row) {
                scene::fill(scene, root, p.hover_tint, &row_chip(r));
            }
            let dimmed = !row.pressable();
            let (glyph, glyph_colour) = match row {
                HomeRow::Inbox => (icons::TRAY, p.row_hint),
                HomeRow::Today => (icons::SUN, p.text_dim),
                HomeRow::Upcoming => (icons::CLOCK, p.row_hint),
                HomeRow::Recent(_) => (icons::DOCUMENT, p.text_dim),
                HomeRow::RecentHeading => unreachable!("drawn above"),
            };
            let icon_x = icon_lane(r.x, 0, false);
            draw_icon(
                scene,
                root,
                engine,
                glyph,
                Rect::new(icon_x, r.y, PAGE_ICON_SLOT, r.h),
                glyph_colour,
            );
            // The library's ink rule: full strength only on the card, dim
            // otherwise — and the hint tone for a row that cannot be pressed.
            let colour = if dimmed {
                p.row_hint
            } else if active {
                p.text
            } else {
                p.text_dim
            };
            let text_x = icon_x + PAGE_ICON_SLOT + 4.0;
            let right = r.x + r.w - theme::NAV_PAD * 2.0;
            let mut label_right = right;
            if dimmed {
                // Why it is dimmed, at the row's end: the rung it waits for.
                let why = engine.simple_layout(t(lang, Key::HomeLater), theme::role::hint());
                let why_x = right - why.width();
                scene::draw_layout_first_line(
                    scene,
                    root,
                    &why,
                    why_x,
                    r.y + (r.h - scene::first_line_height(&why)) * 0.5,
                    Some(p.row_hint),
                );
                label_right = why_x - 8.0;
            }
            let label = self.label(row, lang);
            let label = engine.elided(&label, theme::role::row(), (label_right - text_x).max(1.0));
            let label = engine.simple_layout(&label, theme::role::row());
            scene::draw_layout_first_line(
                scene,
                root,
                &label,
                text_x,
                r.y + (r.h - scene::first_line_height(&label)) * 0.5,
                Some(colour),
            );
        }
    }

    /// A row's identity for a screen reader: Today is the surface's own
    /// control, a recent document *is* that document (the popover's rule).
    fn occurrence(&self, cx: &Context, row: HomeRow) -> Option<Occurrence> {
        match row {
            HomeRow::Today => Some(Occurrence::surface(
                cx.window,
                cx.view,
                SURFACE,
                ControlPart::new(row.token()),
            )),
            HomeRow::Recent(i) => self.recents.get(i).map(|(_, id)| {
                Occurrence::model(cx.window, cx.view, id.as_uuid(), ControlPart::ROOT)
            }),
            _ => None,
        }
    }

    /// The nav as a list: one button per row a press does something on,
    /// Today selected; the dimmed rows are furniture a screen reader skips.
    pub(crate) fn semantics(&self, cx: &Context, top: f32) -> SemanticNode {
        use accesskit::{Action, Role};
        let rows = self.rows(cx.area, top);
        let children: Vec<SemanticNode> = rows
            .iter()
            .filter_map(|(row, bounds)| {
                let occurrence = self.occurrence(cx, *row)?;
                Some(
                    SemanticNode::new(occurrence, Role::Button)
                        .with_label(self.label(*row, cx.lang))
                        .with_bounds(*bounds)
                        .with_selected(*row == HomeRow::Today)
                        .with_actions([Action::Focus, Action::Click]),
                )
            })
            .collect();
        let bottom = rows.last().map_or(top, |(_, r)| r.y + r.h);
        SemanticNode::new(
            Occurrence::surface(cx.window, cx.view, SURFACE, ControlPart::ROOT),
            Role::List,
        )
        .with_label(t(cx.lang, Key::HomeTitle))
        .with_bounds(Rect::new(
            cx.area.x,
            top,
            cx.area.w,
            (bottom - top).max(0.0),
        ))
        .with_size_of_set(children.len())
        .with_children(children)
    }

    /// A press from the accessibility tree: Today, or the recent document
    /// the occurrence *is*. Answers what the pointer's press would.
    pub(crate) fn accessibility_press(&self, target: Occurrence) -> Option<Outcome> {
        use koru_workspace_host::accessibility::ObjectKey;
        match target.object {
            ObjectKey::Surface(name) if name == SURFACE && target.part.name() == "today" => {
                Some(Outcome::OpenHome)
            }
            ObjectKey::Model(id) => self
                .recents
                .iter()
                .find(|(_, doc)| doc.as_uuid() == id)
                .map(|(_, doc)| Outcome::OpenRecentDocument(*doc)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0.0, 0.0, 256.0, 600.0)
    }

    /// Three fixed rows, then RECENT with at most three documents; the
    /// dimmed rows take no press, Today opens the tab, a recent opens it.
    #[test]
    fn the_nav_lists_its_rows_and_answers_presses() {
        let mut nav = HomeNav::default();
        let rows: Vec<HomeRow> = nav
            .rows(area(), 100.0)
            .into_iter()
            .map(|(r, _)| r)
            .collect();
        assert_eq!(rows, [HomeRow::Inbox, HomeRow::Today, HomeRow::Upcoming]);

        let ids: Vec<DocumentId> = (0..5).map(|_| DocumentId::new()).collect();
        assert!(
            nav.set_recents(
                ids.iter()
                    .enumerate()
                    .map(|(i, id)| (format!("Doc {i}"), *id))
                    .collect()
            )
        );
        assert_eq!(nav.recents().len(), RECENT_MAX, "three at most");
        let rows = nav.rows(area(), 100.0);
        assert_eq!(rows.len(), 3 + 1 + RECENT_MAX);
        assert_eq!(rows[3].0, HomeRow::RecentHeading);

        let (x, y) = rows[0].1.centre();
        assert_eq!(
            nav.mouse_down(area(), 100.0, x, y),
            Outcome::Consumed,
            "Inbox is dimmed"
        );
        let (x, y) = rows[1].1.centre();
        assert_eq!(nav.mouse_down(area(), 100.0, x, y), Outcome::OpenHome);
        let (x, y) = rows[5].1.centre();
        assert_eq!(
            nav.mouse_down(area(), 100.0, x, y),
            Outcome::OpenRecentDocument(ids[1])
        );
        // Hover lands only on rows a press does something on.
        let (x, y) = rows[2].1.centre();
        assert!(!nav.mouse_move(area(), 100.0, x, y));
        let (x, y) = rows[1].1.centre();
        assert!(nav.mouse_move(area(), 100.0, x, y));
        assert!(
            !nav.set_recents(nav.recents().to_vec()),
            "unchanged: no redraw"
        );
    }
}
