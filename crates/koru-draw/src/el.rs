// SPDX-License-Identifier: MPL-2.0

//! **Elements**: a surface written as a short tree of named boxes, text,
//! icons, scroll viewports and leaves, laid out by one bounded stack solver
//! and painted from the one frame that layout produced.
//!
//! The contract is `docs/contributing/refactor-ui/ELEMENTS.final.md`: §3 for
//! the authoring API and its defaults, §5 for what the solver promises. This
//! is rung 1's first increment (ELEMENTS-PLAN P1): the vocabulary and the
//! solver, and only as much of each as the four gate fixtures in `tests.rs`
//! need. Identity, hit targets, semantics and the inspector's full record
//! arrive with the frame contract (P2).
//!
//! ```text
//! row("Workspace row")            a neutral layout box
//!     .min_h(metrics::ROW_MIN)    a tagged minimum, never a bare 26
//!     .pad_x(Space::S1)           a spacing step, never a bare 4
//!     .child(text(&name).fill().truncate())
//! ```
//!
//! Every size is a typed [`Length`] so the frame can say where it came from;
//! a raw number is either runtime geometry, named for its source, or an
//! exception (`Space::Px`, `Length::Px`) the source scanner will hold to an
//! allowance.

use vello::Scene;
use vello::kurbo::Affine;

use crate::text::TextEngine;
use crate::theme::themes::{self, Contrast, Theme};
use crate::theme::{
    FontWeight, IconSize, Ink, Line, Metric, Palette, Profile, Radius, Space, Surface, TextRole,
    roles,
};

mod frame;
mod solve;

pub use frame::{Diagnostic, Frame, Node, TextCache};

/// What a frame is resolved against: the theme and contrast its colours come
/// from, and the profile its dimensions come from. A subtree can be laid out
/// and painted under another (D3: theme is scope context, not a global).
#[derive(Copy, Clone, Debug)]
pub struct FrameCx {
    pub theme: &'static Theme,
    pub contrast: Contrast,
    pub profile: Profile,
}

impl FrameCx {
    pub fn new(theme: &'static Theme, contrast: Contrast) -> Self {
        Self {
            theme,
            contrast,
            profile: Profile::Desktop,
        }
    }

    /// The host's root theme and contrast.
    pub fn root() -> Self {
        Self::new(themes::active(), themes::contrast())
    }

    pub fn palette(&self) -> &'static Palette {
        self.theme.palette(self.contrast)
    }
}

/// A length an element is sized with, and where it came from.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Length {
    /// A spacing step.
    Space(Space),
    /// A tagged metric: a row's minimum, a tile, a lane.
    Metric(Metric),
    /// Geometry the running app supplies — a resized width, a measured
    /// extent — named for its source. Not a design value.
    Runtime(f32, &'static str),
    /// An authored raw length: the escape path, held to an allowance.
    Px(f32),
}

impl Length {
    pub fn px(self, profile: Profile) -> f32 {
        match self {
            Self::Space(space) => space.px(profile),
            Self::Metric(metric) => metric.value,
            Self::Runtime(value, _) | Self::Px(value) => value,
        }
    }
}

impl From<Space> for Length {
    fn from(space: Space) -> Self {
        Self::Space(space)
    }
}

impl From<Metric> for Length {
    fn from(metric: Metric) -> Self {
        Self::Metric(metric)
    }
}

/// Cross-axis placement of a stack's children (§3.3: Stretch by default).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Stretch,
    Start,
    Centre,
    End,
}

/// A stack's main axis.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// How text meets the width it is given. Single-line unless asked.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum TextMode {
    /// One line at its natural width; a narrower box clips it.
    #[default]
    Single,
    /// One line, elided to the box's width.
    Truncate,
    /// As many lines as the width needs; height follows the breaks.
    Wrap,
}

/// An existing drawable placed by the solver: a tile, a lane, a picture.
///
/// Width first (§5.2): the natural size when `width` is `None`, and the
/// height it needs at an assigned width otherwise. Paint gets the allocation
/// and the frame context and reads its colours from the latter.
pub trait Leaf {
    fn measure(&self, width: Option<f32>) -> (f32, f32);

    fn paint(
        &self,
        _scene: &mut Scene,
        _root: Affine,
        _engine: &mut TextEngine,
        _at: crate::geometry::Rect,
        _cx: &FrameCx,
    ) {
    }
}

pub(crate) struct TextEl {
    pub content: String,
    pub role: TextRole,
    pub ink: Ink,
    pub weight: Option<FontWeight>,
    pub mode: TextMode,
}

pub(crate) struct IconEl {
    pub glyph: char,
    pub size: IconSize,
    pub ink: Ink,
}

pub(crate) enum Kind {
    Stack(Axis),
    Text(TextEl),
    Icon(IconEl),
    Scroll { offset: f32 },
    Leaf(Box<dyn Leaf>),
    Divider(Line),
}

/// One element: a box, text, an icon, a scroll viewport or a leaf.
pub struct El {
    pub(crate) name: &'static str,
    pub(crate) kind: Kind,
    /// `Fill(weight)` along the parent's stack axis.
    pub(crate) fill: Option<f32>,
    pub(crate) w: Option<Length>,
    pub(crate) h: Option<Length>,
    pub(crate) hug_w: bool,
    pub(crate) hug_h: bool,
    pub(crate) min_w: Option<Length>,
    pub(crate) max_w: Option<Length>,
    pub(crate) min_h: Option<Length>,
    pub(crate) max_h: Option<Length>,
    /// Logical start, top, end, bottom.
    pub(crate) pad: [Space; 4],
    pub(crate) gap: Space,
    pub(crate) align: Align,
    pub(crate) align_self: Option<Align>,
    pub(crate) bg: Option<Surface>,
    pub(crate) radius: Radius,
    pub(crate) children: Vec<El>,
}

fn element(name: &'static str, kind: Kind) -> El {
    El {
        name,
        kind,
        fill: None,
        w: None,
        h: None,
        hug_w: false,
        hug_h: false,
        min_w: None,
        max_w: None,
        min_h: None,
        max_h: None,
        pad: [Space::S0; 4],
        gap: Space::S0,
        align: Align::Stretch,
        align_self: None,
        bg: None,
        radius: Radius::None,
        children: Vec::new(),
    }
}

/// Children side by side.
pub fn row(name: &'static str) -> El {
    element(name, Kind::Stack(Axis::Horizontal))
}

/// Children one under another.
pub fn col(name: &'static str) -> El {
    element(name, Kind::Stack(Axis::Vertical))
}

/// One child in a padded, decorated box.
pub fn boxed(name: &'static str) -> El {
    element(name, Kind::Stack(Axis::Vertical))
}

/// A bounded viewport over a vertical column of children, scrolled by the
/// host's offset.
pub fn scroll_y(name: &'static str) -> El {
    element(name, Kind::Scroll { offset: 0.0 })
}

/// Text in a role, Primary ink, single-line unless asked.
pub fn text(content: impl Into<String>) -> El {
    element(
        "Text",
        Kind::Text(TextEl {
            content: content.into(),
            role: roles::UI,
            ink: Ink::Primary,
            weight: None,
            mode: TextMode::Single,
        }),
    )
}

/// A glyph in a named icon size, Secondary ink.
pub fn icon(glyph: char, size: IconSize) -> El {
    element(
        "Icon",
        Kind::Icon(IconEl {
            glyph,
            size,
            ink: Ink::Secondary,
        }),
    )
}

/// An existing drawable, measured and placed like any element.
pub fn leaf(name: &'static str, adapter: impl Leaf + 'static) -> El {
    element(name, Kind::Leaf(Box::new(adapter)))
}

/// Space that takes what its stack leaves over.
pub fn spacer() -> El {
    let mut el = element("Spacer", Kind::Stack(Axis::Horizontal));
    el.fill = Some(1.0);
    el
}

/// A hairline across its stack, one border width thick along the stack's
/// axis.
pub fn divider(line: Line) -> El {
    element("Divider", Kind::Divider(line))
}

impl El {
    /// Replace the developer name. Free text; repetition is normal.
    pub fn named(mut self, name: &'static str) -> Self {
        self.name = name;
        self
    }

    /// `Fill(1)` along the parent's stack axis.
    pub fn fill(self) -> Self {
        self.fill_weight(1.0)
    }

    pub fn fill_weight(mut self, weight: f32) -> Self {
        self.fill = Some(weight);
        self
    }

    pub fn w(mut self, w: impl Into<Length>) -> Self {
        self.w = Some(w.into());
        self
    }

    pub fn h(mut self, h: impl Into<Length>) -> Self {
        self.h = Some(h.into());
        self
    }

    /// Natural width even where the parent would stretch it.
    pub fn hug_w(mut self) -> Self {
        self.hug_w = true;
        self
    }

    /// Natural height even where the parent would stretch it.
    pub fn hug_h(mut self) -> Self {
        self.hug_h = true;
        self
    }

    pub fn min_w(mut self, v: impl Into<Length>) -> Self {
        self.min_w = Some(v.into());
        self
    }

    pub fn max_w(mut self, v: impl Into<Length>) -> Self {
        self.max_w = Some(v.into());
        self
    }

    pub fn min_h(mut self, v: impl Into<Length>) -> Self {
        self.min_h = Some(v.into());
        self
    }

    pub fn max_h(mut self, v: impl Into<Length>) -> Self {
        self.max_h = Some(v.into());
        self
    }

    pub fn pad(mut self, space: Space) -> Self {
        self.pad = [space; 4];
        self
    }

    /// Logical start and end: the same on both sides in either direction.
    pub fn pad_x(mut self, space: Space) -> Self {
        self.pad[0] = space;
        self.pad[2] = space;
        self
    }

    pub fn pad_y(mut self, space: Space) -> Self {
        self.pad[1] = space;
        self.pad[3] = space;
        self
    }

    pub fn pad_start(mut self, space: Space) -> Self {
        self.pad[0] = space;
        self
    }

    pub fn pad_end(mut self, space: Space) -> Self {
        self.pad[2] = space;
        self
    }

    pub fn pad_top(mut self, space: Space) -> Self {
        self.pad[1] = space;
        self
    }

    pub fn pad_bottom(mut self, space: Space) -> Self {
        self.pad[3] = space;
        self
    }

    /// The space a stack keeps between its children.
    pub fn gap(mut self, space: Space) -> Self {
        self.gap = space;
        self
    }

    pub fn items_start(mut self) -> Self {
        self.align = Align::Start;
        self
    }

    pub fn items_center(mut self) -> Self {
        self.align = Align::Centre;
        self
    }

    pub fn items_end(mut self) -> Self {
        self.align = Align::End;
        self
    }

    pub fn items_stretch(mut self) -> Self {
        self.align = Align::Stretch;
        self
    }

    /// This child's cross-axis placement, over its parent's.
    pub fn align_self(mut self, align: Align) -> Self {
        self.align_self = Some(align);
        self
    }

    pub fn bg(mut self, surface: Surface) -> Self {
        self.bg = Some(surface);
        self
    }

    pub fn radius(mut self, radius: Radius) -> Self {
        self.radius = radius;
        self
    }

    pub fn child(mut self, child: El) -> Self {
        self.children.push(child);
        self
    }

    pub fn children(mut self, children: impl IntoIterator<Item = El>) -> Self {
        self.children.extend(children);
        self
    }

    /// The host's scroll offset; layout clamps it to the content.
    pub fn offset(mut self, offset: f32) -> Self {
        if let Kind::Scroll { offset: at } = &mut self.kind {
            *at = offset;
        }
        self
    }

    pub fn role(mut self, role: TextRole) -> Self {
        if let Kind::Text(t) = &mut self.kind {
            t.role = role;
        }
        self
    }

    pub fn ink(mut self, ink: Ink) -> Self {
        match &mut self.kind {
            Kind::Text(t) => t.ink = ink,
            Kind::Icon(i) => i.ink = ink,
            _ => {}
        }
        self
    }

    /// The current item's treatment: SemiBold over the role's own weight. A
    /// persistent variant, so it is resolved before measurement.
    pub fn emphasis(mut self, on: bool) -> Self {
        if let Kind::Text(t) = &mut self.kind {
            t.weight = on.then_some(FontWeight::SemiBold);
        }
        self
    }

    pub fn truncate(mut self) -> Self {
        if let Kind::Text(t) = &mut self.kind {
            t.mode = TextMode::Truncate;
        }
        self
    }

    pub fn wrap(mut self) -> Self {
        if let Kind::Text(t) = &mut self.kind {
            t.mode = TextMode::Wrap;
        }
        self
    }

    /// Lay the tree out in `area`, the root's definite border box, under
    /// `cx`. One public operation; the solver walks as often as width-first
    /// measurement needs.
    pub fn layout(
        &self,
        engine: &mut TextEngine,
        cache: &mut TextCache,
        area: crate::geometry::Rect,
        cx: FrameCx,
    ) -> Frame {
        solve::layout(self, engine, cache, area, cx)
    }
}

#[cfg(test)]
mod tests;
