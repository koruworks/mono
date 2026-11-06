// SPDX-License-Identifier: MPL-2.0

//! **The page's heading, as elements**: one row holding the list's colour
//! square (on a list page), the title, the date (on Today) and the open
//! count.
//!
//! ```text
//! Tasks heading
//!   List swatch          a list page only
//!   Heading text
//!     Heading title      18/26, `roles::SUBTITLE`
//!     Heading date       Today only, the title's size in the secondary ink
//!     Heading count      once the store has answered, `roles::META`
//! ```
//!
//! It was one title layout and a second layout of `· Nov 5   5 open` placed
//! after it by hand, so the inspector showed two text lines and no row. The
//! title says *which queue*; the date and the count are notes on it, quieter,
//! and set on the title's **baseline**: each is pushed down by the title's
//! baseline less its own, measured from the shaped lines ([`baseline`]).
//!
//! The heading presses nothing, so it carries no keys and no targets; its
//! geometry is read back only by the suite.

use koru_draw::el::{Align, El, ElState, FrameCx, Leaf, Length, TextCache, leaf, row, text};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{Ink, Space, TextRole, roles};
use vello::Scene;
use vello::kurbo::Affine;

use super::view::{HEADER_SWATCH, TasksView};

pub(super) const HEADING: &str = "Tasks heading";
pub(super) const SWATCH: &str = "List swatch";
pub(super) const HEADING_TEXT: &str = "Heading text";
pub(super) const TITLE: &str = "Heading title";
pub(super) const DATE: &str = "Heading date";
pub(super) const COUNT: &str = "Heading count";

/// The swatch's gap to the title: `SWATCH_GAP`, as a step.
pub(super) const SWATCH_STEP: Space = Space::S2_5;
/// Between the title, the date and the count.
const NOTE_GAP: Space = Space::S2;
/// What leads the date: the separator the heading has always drawn.
pub(super) const DATE_DOT: &str = "\u{00B7} ";

/// A list's colour square, in the rail's recipe (`draw::draw_swatch`).
struct SwatchLeaf {
    token: Option<String>,
}

impl Leaf for SwatchLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (HEADER_SWATCH, HEADER_SWATCH)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        _engine: &mut TextEngine,
        at: Rect,
        _cx: &FrameCx,
        _state: ElState,
    ) {
        super::draw::draw_swatch(scene, root, at, self.token.as_deref());
    }
}

/// What the heading says, read off the page before the tree is built — so
/// building it can borrow the engine the page owns.
pub(super) struct Heading {
    pub swatch: Option<Option<String>>,
    pub title: String,
    pub date: Option<String>,
    pub count: Option<String>,
}

/// Where `content`'s baseline sits below the top of its line box in `role`.
///
/// **Measured from the shaped line, not from the role's numbers.** A line's
/// baseline is half its leading plus its ascent, so two sizes in two line
/// heights do not share a baseline when their boxes share a bottom: the 12 px
/// count sat about 3 px below the 18 px title's baseline while it was pushed
/// down by the difference of the line heights. The shaped metrics also count
/// a fallback face (an emoji in a list's name), which a role cannot know.
pub(super) fn baseline(engine: &mut TextEngine, content: &str, role: TextRole) -> f32 {
    engine
        .simple_layout(content, role.style())
        .lines()
        .next()
        .map_or(0.0, |line| line.metrics().baseline)
}

/// The heading as elements, with each note pushed down until its baseline is
/// the title's.
pub(super) fn tree(heading: &Heading, engine: &mut TextEngine) -> El {
    let title_baseline = baseline(engine, &heading.title, roles::SUBTITLE);
    let mut note = |content: &str, name: &'static str| {
        let drop = (title_baseline - baseline(engine, content, roles::META)).max(0.0);
        text(content.to_owned())
            .named(name)
            .role(roles::META)
            .ink(Ink::Secondary)
            .pad_top(Space::Px(drop))
    };
    // **The date is the title's size, quieter** — `· Nov 5` beside *Today*
    // in the secondary ink, so the two read as one heading and the date as
    // the part that changes. The same role puts it on the title's baseline
    // with no push.
    let date = heading.date.as_deref().map(|date| {
        text(format!("{DATE_DOT}{date}"))
            .named(DATE)
            .role(roles::SUBTITLE)
            .ink(Ink::Secondary)
    });
    let count = heading.count.as_deref().map(|count| note(count, COUNT));
    let swatch = heading.swatch.clone().map(|token| {
        leaf(SWATCH, SwatchLeaf { token })
            .decorative()
            .w(Length::Px(HEADER_SWATCH))
            .h(Length::Px(HEADER_SWATCH))
            .align_self(Align::Centre)
    });
    row(HEADING).gap(SWATCH_STEP).children(swatch).child(
        row(HEADING_TEXT)
            .gap(NOTE_GAP)
            .items_start()
            .child(
                text(heading.title.clone())
                    .named(TITLE)
                    .role(roles::SUBTITLE)
                    .ink(Ink::Primary),
            )
            .children(date)
            .children(count),
    )
}

impl TasksView {
    /// What the heading says on this page.
    pub(super) fn heading(&self) -> Heading {
        Heading {
            swatch: self.title_swatch(),
            title: self.title(),
            date: self.title_date(),
            count: self.count_label(),
        }
    }

    /// Where the heading is laid out: its line, from the page margin to
    /// the header's own controls.
    pub(super) fn heading_area(&self) -> Rect {
        Rect::new(
            super::view::PAD,
            self.title_top(),
            (self.header_dots().x - super::view::PAD).max(0.0),
            roles::SUBTITLE.style().metrics.line_height,
        )
    }

    /// The heading laid out in [`heading_area`](Self::heading_area), for the
    /// suite to read back.
    #[cfg(test)]
    pub(super) fn heading_frame(&self, engine: &mut TextEngine) -> (El, koru_draw::el::Frame) {
        let tree = tree(&self.heading(), engine);
        let frame = tree.layout(
            engine,
            &mut TextCache::default(),
            self.heading_area(),
            FrameCx::root(),
        );
        (tree, frame)
    }

    pub(super) fn paint_heading(&mut self, scene: &mut Scene, root: Affine) {
        let heading = self.heading();
        let area = self.heading_area();
        let tree = tree(&heading, &mut self.engine);
        let frame = tree.layout(
            &mut self.engine,
            &mut TextCache::default(),
            area,
            FrameCx::root(),
        );
        frame.paint(&tree, scene, root, &mut self.engine);
    }
}
