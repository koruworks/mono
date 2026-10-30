// SPDX-License-Identifier: MPL-2.0

//! **Elements**: a surface written as a short tree of named boxes, text,
//! icons, scroll viewports and leaves, laid out by one bounded stack solver
//! into one frame that paint, pointer targets, accessibility and the
//! inspector all read.
//!
//! The contract is `docs/contributing/refactor-ui/ELEMENTS.final.md`: §3 for
//! the authoring API and its defaults, §5 for what the solver promises. The
//! authoring guide is `docs/features/koruui/ELEMENTS-GUIDE.md`.
//!
//! ```text
//! row("Workspace row")            a neutral layout box
//!     .min_h(metrics::ROW_MIN)    a tagged minimum, never a bare 26
//!     .pad_x(Space::S1)           a spacing step, never a bare 4
//!     .child(text(&name).fill().truncate())
//! ```
//!
//! Every size is a typed [`Length`] and every colour a role, so the frame can
//! say where a value came from; a raw number is either runtime geometry,
//! named for its source, or an exception (`Space::Px`, `Length::Px`) the
//! source scanner holds to an allowance.
//!
//! **Identity is the host's.** A [`Key`] names a model object or a fixed
//! surface plus a control part; it is what a pointer target, a pressed or
//! focused state and an accessibility node are about, and it survives
//! reordering, renaming and translation. A list position never is one.

use std::panic::Location;

use vello::Scene;
use vello::kurbo::Affine;

use crate::geometry::Rect;
use crate::text::TextEngine;
use crate::theme::themes::{self, Contrast, Theme};
use crate::theme::{
    FontWeight, IconSize, Ink, Line, Metric, Palette, Profile, Radius, Space, Surface, Tagged,
    TextRole, roles,
};

mod frame;
mod record;
mod solve;

pub use frame::{Diagnostic, Frame, HitRegion, Node, SemNode, TextCache};

/// What an element stands for: a model object or a fixed surface.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Object {
    /// A part of a surface no model object stands behind, by code keyword.
    Surface(&'static str),
    /// A model object, by the id it already has.
    Model(u128),
}

/// A stable identity within the host's occurrence: an object and one of its
/// control parts. Never a label, never a list position.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub object: Object,
    pub part: &'static str,
}

impl Key {
    pub const fn surface(name: &'static str, part: &'static str) -> Self {
        Self {
            object: Object::Surface(name),
            part,
        }
    }

    pub const fn model(id: u128, part: &'static str) -> Self {
        Self {
            object: Object::Model(id),
            part,
        }
    }
}

/// A node's native role. The host maps it to its platform's (accesskit's).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Role {
    Menu,
    MenuItem,
    Heading,
    Button,
    Group,
    List,
    ListItem,
    Image,
}

/// A request a node advertises and the host implements for its key.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Click,
    Focus,
}

/// What a node is called: application chrome by translation key, or model
/// content by its own text. A literal is not a key, so chrome cannot slip
/// through as content by type.
#[derive(Clone, Debug, PartialEq)]
pub enum Name {
    Key(koru_i18n::Key),
    Text(String),
}

/// The host's pointer and keyboard state this frame, by key.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Interaction {
    /// Under the pointer, or lit by the keyboard where the two share one.
    pub hot: Option<Key>,
    pub pressed: Option<Key>,
    pub focused: Option<Key>,
}

/// One element's resolved state, from its persistent declarations and the
/// host's interaction state.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ElState {
    pub hovered: bool,
    pub pressed: bool,
    pub focused: bool,
    pub selected: bool,
    pub disabled: bool,
}

/// What a state rule paints: a fill and an ink, either left as they were.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Look {
    pub bg: Option<Surface>,
    pub ink: Option<Ink>,
}

impl Look {
    pub const NONE: Self = Self {
        bg: None,
        ink: None,
    };

    pub const fn bg(surface: Surface) -> Self {
        Self {
            bg: Some(surface),
            ink: None,
        }
    }
}

/// A control's state table (§3.7). Base, then selected, hover and pressed
/// over it in that order, pressed winning; the explicit selected+hover and
/// selected+pressed rules over those; disabled alone suppresses hover and
/// press. Focus is decoration and changes nothing here.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct StateRecipe {
    pub name: &'static str,
    pub base: Look,
    pub selected: Look,
    pub hover: Look,
    pub pressed: Look,
    pub selected_hover: Look,
    pub selected_pressed: Look,
    pub disabled: Look,
}

impl StateRecipe {
    /// The look `state` resolves to, and the rule that won.
    pub fn resolve(&self, state: ElState) -> (Look, &'static str) {
        fn over(look: &mut Look, top: Look) -> bool {
            let mut changed = false;
            if top.bg.is_some() {
                look.bg = top.bg;
                changed = true;
            }
            if top.ink.is_some() {
                look.ink = top.ink;
                changed = true;
            }
            changed
        }
        let mut look = self.base;
        let mut rule = "base";
        if state.disabled {
            if over(&mut look, self.disabled) {
                rule = "disabled";
            }
            return (look, rule);
        }
        if state.selected && over(&mut look, self.selected) {
            rule = "selected";
        }
        if state.hovered && over(&mut look, self.hover) {
            rule = "hover";
        }
        if state.pressed && over(&mut look, self.pressed) {
            rule = "pressed";
        }
        if state.selected && state.hovered && over(&mut look, self.selected_hover) {
            rule = "selected+hover";
        }
        if state.selected && state.pressed && over(&mut look, self.selected_pressed) {
            rule = "selected+pressed";
        }
        (look, rule)
    }
}

/// What a frame is resolved against: the theme and contrast its colours come
/// from, the profile its dimensions come from, and the host's interaction
/// state. A subtree can carry another theme (D3: scope context, not a
/// global).
#[derive(Copy, Clone, Debug)]
pub struct FrameCx {
    pub theme: &'static Theme,
    pub contrast: Contrast,
    pub profile: Profile,
    pub interaction: Interaction,
}

impl FrameCx {
    pub fn new(theme: &'static Theme, contrast: Contrast) -> Self {
        Self {
            theme,
            contrast,
            profile: Profile::Desktop,
            interaction: Interaction::default(),
        }
    }

    /// The host's root theme and contrast.
    pub fn root() -> Self {
        Self::new(themes::active(), themes::contrast())
    }

    pub fn with_interaction(mut self, interaction: Interaction) -> Self {
        self.interaction = interaction;
        self
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

    /// Where the length came from, for the inspector.
    pub fn provenance(self) -> String {
        match self {
            Self::Space(space) => format!("Space::{}", space.name()),
            Self::Metric(metric) => format!("metrics::{}", metric.name),
            Self::Runtime(_, source) => format!("runtime: {source}"),
            Self::Px(v) => format!("Px({v}) exception"),
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

/// A spacing step as it was authored: the step, and the alias that named it
/// when a component's use of a step has a name of its own (`NAV_PAD` is
/// `S2`). The inspector shows both.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Spacing {
    pub space: Space,
    pub alias: Option<&'static str>,
}

impl Spacing {
    pub const NONE: Self = Self {
        space: Space::S0,
        alias: None,
    };

    pub fn px(self, profile: Profile) -> f32 {
        self.space.px(profile)
    }
}

impl From<Space> for Spacing {
    fn from(space: Space) -> Self {
        Self { space, alias: None }
    }
}

impl From<Tagged<Space>> for Spacing {
    fn from(alias: Tagged<Space>) -> Self {
        Self {
            space: alias.token,
            alias: Some(alias.name),
        }
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
/// height it needs at an assigned width otherwise. Paint gets the allocation,
/// the frame context its colours come from, and the element's resolved
/// state, so a paint-only reveal (the gear on a lit row) needs no second
/// element.
pub trait Leaf {
    fn measure(&self, width: Option<f32>) -> (f32, f32);

    /// The pointer region inside the allocation `at`; the whole allocation
    /// unless the adapter says otherwise.
    fn hit_region(&self, at: Rect) -> Rect {
        at
    }

    fn paint(
        &self,
        _scene: &mut Scene,
        _root: Affine,
        _engine: &mut TextEngine,
        _at: Rect,
        _cx: &FrameCx,
        _state: ElState,
    ) {
    }
}

pub(crate) struct TextEl {
    pub content: String,
    pub role: TextRole,
    pub ink: Ink,
    pub weight: Option<FontWeight>,
    pub mode: TextMode,
    /// A single line centred on its glyphs' ink rather than its line box.
    pub ink_centred: bool,
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
    /// Where it was written: the inspector's source line (§3.9).
    pub(crate) site: &'static Location<'static>,
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
    pub(crate) pad: [Spacing; 4],
    pub(crate) gap: Spacing,
    pub(crate) align: Align,
    pub(crate) align_self: Option<Align>,
    pub(crate) bg: Option<Surface>,
    pub(crate) radius: Radius,
    pub(crate) states: Option<&'static StateRecipe>,
    pub(crate) theme: Option<&'static Theme>,
    pub(crate) key: Option<Key>,
    /// Keys whose interaction drives this element's state besides its own:
    /// a neutral row lit by its primary or its gear.
    pub(crate) activity: Vec<Key>,
    pub(crate) hit: bool,
    /// This box's allocation presses another element's target: a row whose
    /// margins press its primary, the existing hit policy kept explicit.
    pub(crate) hit_as: Option<Key>,
    pub(crate) focusable: bool,
    pub(crate) actions: Vec<Action>,
    pub(crate) role: Option<Role>,
    pub(crate) label: Option<Name>,
    pub(crate) selected: Option<bool>,
    pub(crate) disabled: bool,
    /// Declared decorative: laid out and painted, never announced, never a
    /// target.
    pub(crate) decorative: bool,
    pub(crate) children: Vec<El>,
}

#[track_caller]
fn element(name: &'static str, kind: Kind) -> El {
    El {
        name,
        site: Location::caller(),
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
        pad: [Spacing::NONE; 4],
        gap: Spacing::NONE,
        align: Align::Stretch,
        align_self: None,
        bg: None,
        radius: Radius::None,
        states: None,
        theme: None,
        key: None,
        activity: Vec::new(),
        hit: false,
        hit_as: None,
        focusable: false,
        actions: Vec::new(),
        role: None,
        label: None,
        selected: None,
        disabled: false,
        decorative: false,
        children: Vec::new(),
    }
}

/// Children side by side.
#[track_caller]
pub fn row(name: &'static str) -> El {
    element(name, Kind::Stack(Axis::Horizontal))
}

/// Children one under another.
#[track_caller]
pub fn col(name: &'static str) -> El {
    element(name, Kind::Stack(Axis::Vertical))
}

/// One child in a padded, decorated box.
#[track_caller]
pub fn boxed(name: &'static str) -> El {
    element(name, Kind::Stack(Axis::Vertical))
}

/// A bounded viewport over a vertical column of children, scrolled by the
/// host's offset.
#[track_caller]
pub fn scroll_y(name: &'static str) -> El {
    element(name, Kind::Scroll { offset: 0.0 })
}

/// Text in a role, Primary ink, single-line unless asked. Its words are
/// paint; the control that holds it owns the accessible name.
#[track_caller]
pub fn text(content: impl Into<String>) -> El {
    element(
        "Text",
        Kind::Text(TextEl {
            content: content.into(),
            role: roles::UI,
            ink: Ink::Primary,
            weight: None,
            mode: TextMode::Single,
            ink_centred: false,
        }),
    )
}

/// A glyph in a named icon size. **Not an element until it is classified**:
/// [`Unclassified::decorative`] or [`Unclassified::alt`].
#[track_caller]
pub fn icon(glyph: char, size: IconSize) -> Unclassified {
    Unclassified(element(
        "Icon",
        Kind::Icon(IconEl {
            glyph,
            size,
            ink: Ink::Secondary,
        }),
    ))
}

/// An existing drawable, measured and placed like any element. **Not an
/// element until it is classified**: decorative, or a simple semantic node.
#[track_caller]
pub fn leaf(name: &'static str, adapter: impl Leaf + 'static) -> Unclassified {
    Unclassified(element(name, Kind::Leaf(Box::new(adapter))))
}

/// An icon or leaf that has not said whether it means anything (§4, the
/// typed insertion gate). It has no `child`-able form: only classifying it
/// produces an [`El`].
#[must_use = "an icon or leaf is inserted only once it is classified"]
pub struct Unclassified(El);

impl Unclassified {
    /// Laid out and painted, never announced, never a target. Its control
    /// owns the name and the action.
    pub fn decorative(mut self) -> El {
        self.0.decorative = true;
        self.0
    }

    /// A simple meaningful image, named by a translation key.
    pub fn alt(mut self, name: koru_i18n::Key) -> El {
        self.0.role = Some(Role::Image);
        self.0.label = Some(Name::Key(name));
        self.0
    }

    /// A control of its own: it declares its role, name and actions next.
    pub fn semantic(self) -> El {
        self.0
    }
}

/// Space that takes what its stack leaves over.
#[track_caller]
pub fn spacer() -> El {
    let mut el = element("Spacer", Kind::Stack(Axis::Horizontal));
    el.fill = Some(1.0);
    el
}

/// A hairline across its stack, one border width thick along the stack's
/// axis.
#[track_caller]
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

    /// A step or a tagged alias (`alias::NAV_PAD`); the alias keeps its name.
    pub fn pad(mut self, space: impl Into<Spacing>) -> Self {
        self.pad = [space.into(); 4];
        self
    }

    /// Logical start and end: the same on both sides in either direction.
    pub fn pad_x(mut self, space: impl Into<Spacing>) -> Self {
        let space = space.into();
        self.pad[0] = space;
        self.pad[2] = space;
        self
    }

    pub fn pad_y(mut self, space: impl Into<Spacing>) -> Self {
        let space = space.into();
        self.pad[1] = space;
        self.pad[3] = space;
        self
    }

    pub fn pad_start(mut self, space: impl Into<Spacing>) -> Self {
        self.pad[0] = space.into();
        self
    }

    pub fn pad_end(mut self, space: impl Into<Spacing>) -> Self {
        self.pad[2] = space.into();
        self
    }

    pub fn pad_top(mut self, space: impl Into<Spacing>) -> Self {
        self.pad[1] = space.into();
        self
    }

    pub fn pad_bottom(mut self, space: impl Into<Spacing>) -> Self {
        self.pad[3] = space.into();
        self
    }

    /// The space a stack keeps between its children.
    pub fn gap(mut self, space: impl Into<Spacing>) -> Self {
        self.gap = space.into();
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

    /// The control's state table: its fill and ink by state.
    pub fn states(mut self, recipe: &'static StateRecipe) -> Self {
        self.states = Some(recipe);
        self
    }

    /// Lay out and paint this subtree under `theme` (D3: a subtree may
    /// override the frame's).
    pub fn theme(mut self, theme: &'static Theme) -> Self {
        self.theme = Some(theme);
        self
    }

    /// A stable local key. It does not make a target, a tab stop or a node.
    pub fn id(mut self, key: Key) -> Self {
        self.key = Some(key);
        self
    }

    /// Other keys whose hover, press or focus light this element.
    pub fn activity(mut self, keys: impl IntoIterator<Item = Key>) -> Self {
        self.activity.extend(keys);
        self
    }

    /// A pointer target for this element's key.
    pub fn hit(mut self) -> Self {
        self.hit = true;
        self
    }

    /// Make this box's whole allocation a pointer region of `target`, the
    /// key of an element inside it (§3.6: an existing hit policy stays
    /// explicit). The box itself stays neutral: no node, no action. A
    /// target painted later inside it, a row's gear, is still the one
    /// pressed there.
    pub fn hit_as(mut self, target: Key) -> Self {
        self.hit_as = Some(target);
        self
    }

    /// Eligible for keyboard focus within the host's policy.
    pub fn focusable(mut self) -> Self {
        self.focusable = true;
        self
    }

    /// Advertise a request the host implements for this key.
    pub fn action(mut self, action: Action) -> Self {
        if !self.actions.contains(&action) {
            self.actions.push(action);
        }
        self
    }

    /// This element's native role: it becomes an accessibility node.
    pub fn semantics(mut self, role: Role) -> Self {
        self.role = Some(role);
        self
    }

    /// Its application-owned name, by translation key.
    pub fn name(mut self, key: koru_i18n::Key) -> Self {
        self.label = Some(Name::Key(key));
        self
    }

    /// Its name from model content: a workspace's title.
    pub fn name_text(mut self, content: impl Into<String>) -> Self {
        self.label = Some(Name::Text(content.into()));
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = Some(selected);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
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

    /// Centre a single line on its ink, not its line box: the drawn glyphs
    /// sit in the middle of the height, whatever the font's ascent and
    /// descent say. How a row's name has always been set in the switcher.
    pub fn ink_centred(mut self) -> Self {
        if let Kind::Text(t) = &mut self.kind {
            t.ink_centred = true;
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
    /// measurement needs, then validates the composed tree.
    pub fn layout(
        &self,
        engine: &mut TextEngine,
        cache: &mut TextCache,
        area: Rect,
        cx: FrameCx,
    ) -> Frame {
        solve::layout(self, engine, cache, area, cx)
    }
}

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod tests;
