// SPDX-License-Identifier: MPL-2.0

//! A popover menu: labelled rows, an optional leading icon or check, an
//! optional chord hint, dividers, and keyboard stepping.
//!
//! **Subject-free, like everything in this crate.** The host describes each
//! row through [`MenuItem`] and decides what choosing one does; what lives
//! here is the menu's geometry and drawing — where it sits, which row the
//! pointer is over, how it paints. That split keeps the geometry testable
//! without a viewport, and it is what lets the container's context menus and
//! the search window's `…` menu share one implementation rather than two
//! recipes drifting apart.
//!
//! The items are fixed when the menu opens, and the width is measured from
//! them then: a menu whose rows were recomputed each frame could relabel
//! itself between the press and the release, and a panel that resizes under
//! the pointer is worse than one occasionally a few pixels too wide.

use std::borrow::Cow;

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

mod inspection;
pub use inspection::KoruMenuInspection;

/// One row, as the host describes it. Only [`label`](MenuItem::label) is
/// required; every other answer has a default that leaves the row plain.
pub trait MenuItem {
    fn label(&self) -> Cow<'_, str>;
    /// A font stack the label is set in instead of the interface face — a
    /// font picker's rows each show the family they name. Measured in that
    /// face too, so the panel is as wide as the widest *drawn* row.
    fn font_stack(&self) -> Option<&'static str> {
        None
    }
    /// The glyph drawn ahead of the label, if the row carries one.
    fn icon(&self) -> Option<char> {
        None
    }
    /// Whether the glyph draws in the solid weight — "on", not "available".
    fn filled_icon(&self) -> bool {
        false
    }
    /// A colour square in the leading slot instead of a glyph — the accent
    /// token the row's subject wears, or `Some(None)` for a subject that has
    /// none and draws the neutral square. A row with both keeps the glyph.
    fn swatch(&self) -> Option<Option<&str>> {
        None
    }
    /// A product's mark drawn as vector path data — for a row naming a
    /// third-party product whose logo the icon font cannot carry (a launch
    /// profile's row). When set it takes the leading slot and
    /// [`icon`](MenuItem::icon) is not consulted.
    fn brand_icon(&self) -> Option<&'static koru_draw::brand::BrandIcon> {
        None
    }
    /// The chord that does the same thing, drawn right-aligned and dimmed. A
    /// blank column says "no shortcut" more honestly than an invented chord.
    fn shortcut(&self) -> Option<String> {
        None
    }
    /// A `›` where a chord would be: there is more behind this row.
    fn has_submenu(&self) -> bool {
        false
    }
    /// A horizontal rule. An item as far as indices go — so an `enabled`
    /// slice lines up without the host saying which rows are real — but never
    /// hovered, never chosen, and shorter.
    fn divider(&self) -> bool {
        false
    }
    /// A toggle row: `Some(state)` reserves the check column and draws the
    /// check when set, so toggling never shifts the label; `None` is an
    /// ordinary row.
    fn checked(&self) -> Option<bool> {
        None
    }
    /// Chosen-or-not styling: named in red while it can be chosen, dimmed
    /// like everything else when it cannot — a red row you cannot press reads
    /// as an error rather than a warning.
    fn destructive(&self) -> bool {
        false
    }
    /// Why a dimmed row cannot be chosen, drawn as a hint beside it. A dimmed
    /// row that does not explain itself is a dead end.
    fn disabled_reason(&self) -> Option<Cow<'static, str>> {
        None
    }
    fn height(&self) -> f32 {
        if self.divider() { DIVIDER_H } else { ITEM_H }
    }
    /// Whether the pointer and the keyboard may land here.
    fn selectable(&self) -> bool {
        !self.divider()
    }
    /// Small controls at this row's right-hand end, in place of the chord
    /// column — the `−  100%  +  ↺` of a stepper.
    ///
    /// A row that has these is **not chosen as a whole**: it should answer
    /// `false` to [`selectable`](MenuItem::selectable), so the pointer and the
    /// keyboard land on the controls rather than on the row, and pressing one
    /// leaves the menu open. [`Menu::control_at`] is the hit test.
    fn controls(&self) -> Vec<Control> {
        Vec::new()
    }
    /// A release card in place of a label — the help popover's *What's new*:
    /// a badge, a date, a title, a line or two under it and a button at its
    /// foot. The row is still chosen as a whole (the card's `›`), and its
    /// button is the row's one [`Control`](MenuItem::controls), which
    /// [`Menu::control_rect`] places at the card's foot rather than in the
    /// chord column. A card is as tall as its text, measured when the menu
    /// opens; [`Menu::at_width`] gives it [`CARD_FALLBACK_H`].
    fn card(&self) -> Option<MenuCard> {
        None
    }
    /// A glyph where a chord would be — the `↗` of a row that leaves the app
    /// for the browser. Dimmed like a chord, and never beside one.
    fn trailing_icon(&self) -> Option<char> {
        None
    }
    /// A dot at the row's right-hand end: there is news behind this heading.
    fn unread(&self) -> bool {
        false
    }
}

/// What a card row shows. See [`MenuItem::card`].
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MenuCard {
    /// The small chip leading the card's first line — `STABLE · 1.0.1`.
    pub badge: String,
    /// Beside the chip, dimmed. Empty when unknown.
    pub date: String,
    pub title: String,
    /// A sentence or two under the title. Empty for none.
    pub body: String,
    /// The button at the card's foot — its label and whether it can be
    /// pressed — or `None` for no button. Its glyph is the row's first
    /// control's.
    pub button: Option<(String, bool)>,
}

/// One cell at a row's right-hand end.
///
/// **Subject-free, like the rest of this widget**: a button carries a glyph and
/// whether it can be pressed, and what pressing it *means* is the host's — it
/// gets back the cell's index and decides. The readout between two buttons is
/// the value they move, and is never pressable; a stepper whose number was also
/// a button would be a row with three different things to click and no way to
/// tell which is which.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Control {
    /// A pressable glyph. `enabled` is `false` at the end of its range —
    /// drawn dim and refusing the press, rather than vanishing and shifting
    /// the cells beside it under the pointer.
    Button { glyph: char, enabled: bool },
    /// What the buttons move. Fixed width (see [`READOUT_W`]) so `100%`
    /// becoming `90%` does not slide the `+` out from under the pointer.
    Readout(String),
}

impl Control {
    /// How wide this cell is. **Arithmetic, never measured**: the hit test has
    /// no `TextEngine`, and a stepper whose targets moved with the digits in
    /// them would be a stepper you cannot hold down.
    pub const fn width(&self) -> f32 {
        match self {
            Self::Button { .. } => CONTROL_W,
            Self::Readout(_) => READOUT_W,
        }
    }

    const fn pressable(&self) -> bool {
        matches!(self, Self::Button { enabled: true, .. })
    }
}

/// The total width a row's controls occupy.
fn controls_width(controls: &[Control]) -> f32 {
    controls.iter().map(Control::width).sum()
}

/// The narrowest and widest a menu gets. The floor stops a two-letter menu
/// reading as a tooltip; the ceiling stops a long label turning it into a
/// panel.
pub const MIN_W: f32 = 148.0;
pub const MAX_W: f32 = 300.0;
/// Air either side of a row's contents.
pub const ROW_PAD_X: f32 = 10.0;
/// The least space between a label and its chord, so the two never touch.
const SHORTCUT_GAP: f32 = 28.0;
/// The colour square a row may lead with — the rail's, at a row's scale.
const SWATCH: f32 = 12.0;
/// A stepper button's square. Wider than the glyph inside it on purpose: this
/// is a target pressed repeatedly, so it is sized for the finger rather than
/// for the mark.
const CONTROL_W: f32 = 26.0;
/// The readout between two stepper buttons. Fixed, and wide enough for the
/// longest value a stepper here shows (`200%`).
pub const READOUT_W: f32 = 46.0;
const ITEM_H: f32 = theme::ROW_H;
const DIVIDER_H: f32 = 8.0;
/// A card's panel is inset from the row's edges by this, top and bottom too.
const CARD_INSET: f32 = 6.0;
/// Air inside the card.
const CARD_PAD: f32 = 10.0;
/// The badge chip's height; its text is `role::hint`.
const CARD_BADGE_H: f32 = 20.0;
/// Between the card's lines.
const CARD_GAP: f32 = 6.0;
const CARD_BUTTON_H: f32 = 28.0;
const CARD_BUTTON_MIN_W: f32 = 96.0;
const CARD_RADIUS: f32 = 8.0;
/// The least width a menu with a card gets: a title that wraps to two lines
/// rather than four.
pub const CARD_MIN_W: f32 = 272.0;
/// A card row's height when nothing measured it — see [`Menu::at_width`].
pub const CARD_FALLBACK_H: f32 = 132.0;
/// The dot an unread heading wears.
const UNREAD_DOT: f32 = 8.0;
/// The padding band above and below the rows.
pub const PAD: f32 = theme::PANEL_PAD;
const RADIUS: f32 = 8.0;
/// Kept off the window edge so the shadow and border are not flush with it.
pub const EDGE_MARGIN: f32 = 4.0;

/// Which corner of the menu is pinned to the point it was opened at.
///
/// A right-click has no width, so the menu hangs down-and-right from the
/// cursor, the way a native one does. A **button** does have a width, and the
/// `…` sits at the right-hand end of a row — so a menu that hung right from
/// it would immediately hit the window edge and clamp, landing somewhere with
/// no relationship to the thing pressed.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Anchor {
    /// Top-left at the point. For a right-click.
    Cursor,
    /// Top-**right** at the point. For a button at the right of a row.
    ButtonRight,
}

/// What a row leads with, if anything: its own icon, or the check a toggle
/// row reserves the column for whether or not it is set.
fn leading<T: MenuItem>(item: &T) -> Option<char> {
    if let Some(glyph) = item.icon() {
        return Some(glyph);
    }
    if item.checked().is_some() {
        return Some(koru_draw::icons::CHECK);
    }
    None
}

/// How wide the panel has to be to hold `items` comfortably: the widest row
/// wins — left pad, the leading slot when the row has one, the label, a gap
/// and the chord or submenu caret when the row has one, right pad. Dividers
/// are skipped; they carry nothing and would measure as empty.
///
/// Public so a host can ask the same question about its own rows — the menu
/// measures them itself at open time.
pub fn measure_width<T: MenuItem>(engine: &mut TextEngine, items: &[T]) -> f32 {
    let mut widest: f32 = 0.0;
    for item in items.iter().filter(|item| !item.divider()) {
        // A card asks for room, not for its text's width: its text wraps to
        // whatever the panel gives it.
        if item.card().is_some() {
            widest = widest.max(CARD_MIN_W);
            continue;
        }
        let mut w = ROW_PAD_X * 2.0 + label_layout(engine, item).width();
        if item.unread() {
            w += SHORTCUT_GAP + UNREAD_DOT;
        }
        // Outside the `selectable` gate below: a stepper row is deliberately
        // *not* selectable — the controls are what you press — and measuring it
        // as a bare label would leave the panel too narrow for the very cells
        // that make it a stepper.
        let controls = item.controls();
        if !controls.is_empty() {
            w += SHORTCUT_GAP + controls_width(&controls);
        }
        if item.selectable() {
            if item.brand_icon().is_some() {
                // A brand mark fills a CAPTION_ICON square, so its width is
                // its size — no layout to measure.
                w += theme::CAPTION_ICON + 8.0;
            } else if let Some(glyph) = leading(item) {
                w += engine.icon_layout(glyph, theme::CAPTION_ICON).width() + 8.0;
            } else if item.swatch().is_some() {
                w += SWATCH + 8.0;
            }
            w += trailing_width(engine, item);
        }
        widest = widest.max(w);
    }
    widest.clamp(MIN_W, MAX_W)
}

/// What a row draws at its right edge — the submenu's `›`, a chord or a
/// trailing glyph — with the gap before it: the room its label leaves.
fn trailing_width<T: MenuItem>(engine: &mut TextEngine, item: &T) -> f32 {
    if item.has_submenu() {
        SHORTCUT_GAP
            + engine
                .icon_layout(koru_draw::icons::CARET_RIGHT, theme::CAPTION_ICON)
                .width()
    } else if let Some(chord) = item.shortcut() {
        SHORTCUT_GAP + engine.simple_layout(&chord, theme::role::hint()).width()
    } else if let Some(glyph) = item.trailing_icon() {
        SHORTCUT_GAP + engine.icon_layout(glyph, theme::CAPTION_ICON).width()
    } else {
        0.0
    }
}

/// A row's label, in its own face when it names one.
fn label_layout<T: MenuItem>(engine: &mut TextEngine, item: &T) -> parley::Layout<theme::Brush> {
    match item.font_stack() {
        Some(stack) => engine.simple_layout_in(item.label().as_ref(), theme::role::row(), stack),
        None => engine.simple_layout(item.label().as_ref(), theme::role::row()),
    }
}

/// What a row's geometry needs beyond its item: a card's measured height and
/// its button's width. Measured once, when the menu opens, by the constructors
/// that have a `TextEngine`; every other row is `item.height()`, so the
/// arithmetic below stays arithmetic.
#[derive(Clone, Copy, PartialEq, Debug)]
struct RowMetrics {
    height: f32,
    button_w: f32,
}

fn measure_rows<T: MenuItem>(engine: &mut TextEngine, items: &[T], width: f32) -> Vec<RowMetrics> {
    items
        .iter()
        .map(|item| match item.card() {
            Some(card) => {
                let glyph = item.controls().first().and_then(|c| match c {
                    Control::Button { glyph, .. } => Some(*glyph),
                    Control::Readout(_) => None,
                });
                measure_card(engine, &card, glyph, width)
            }
            None => RowMetrics {
                height: item.height(),
                button_w: 0.0,
            },
        })
        .collect()
}

/// The rows' geometry without a font: a card is [`CARD_FALLBACK_H`] tall
/// with a [`CARD_BUTTON_MIN_W`] button.
fn fallback_rows<T: MenuItem>(items: &[T]) -> Vec<RowMetrics> {
    items
        .iter()
        .map(|item| {
            if item.card().is_some() {
                RowMetrics {
                    height: CARD_FALLBACK_H,
                    button_w: CARD_BUTTON_MIN_W,
                }
            } else {
                RowMetrics {
                    height: item.height(),
                    button_w: 0.0,
                }
            }
        })
        .collect()
}

/// How wide a card's text runs at a panel width.
fn card_inner_w(width: f32) -> f32 {
    (width - (CARD_INSET + CARD_PAD) * 2.0).max(1.0)
}

fn measure_card(
    engine: &mut TextEngine,
    card: &MenuCard,
    glyph: Option<char>,
    width: f32,
) -> RowMetrics {
    let inner = card_inner_w(width);
    let mut h = CARD_INSET + CARD_PAD + CARD_BADGE_H;
    if !card.title.is_empty() {
        h += CARD_GAP
            + engine
                .simple_layout_wrapped(&card.title, theme::role::section_heading(), inner)
                .height();
    }
    if !card.body.is_empty() {
        h += CARD_GAP * 0.5
            + engine
                .simple_layout_wrapped(&card.body, theme::role::row(), inner)
                .height();
    }
    let mut button_w = 0.0;
    if let Some((label, _)) = &card.button {
        h += CARD_GAP + CARD_BUTTON_H;
        let text = engine.simple_layout(label, theme::role::row()).width();
        let icon = glyph.map_or(0.0, |g| {
            engine.icon_layout(g, theme::CAPTION_ICON).width() + 6.0
        });
        button_w = (text + icon + 24.0).max(CARD_BUTTON_MIN_W);
    }
    h += CARD_PAD + CARD_INSET;
    RowMetrics {
        height: h,
        button_w,
    }
}

/// A menu over whatever was clicked: the rows, the anchor, and the pointer's
/// row. What a row *does* is the host's business.
#[derive(Clone)]
pub struct Menu<T: MenuItem> {
    /// Where the press landed, in window space. The menu opens there.
    anchor: (f32, f32),
    /// Which of its corners that point is.
    align: Anchor,
    /// Which row the pointer is over, if any. Tracked whether or not the row
    /// is enabled; drawing and acting gate on `enabled` separately.
    pub hover: Option<usize>,
    /// Which control of which row the pointer is over — a stepper row's own
    /// hover, kept apart from [`hover`](Self::hover) because they are different
    /// shapes: a row lights the full width, a control lights its own square.
    pub control_hover: Option<(usize, usize)>,
    /// Measured from the items at open time. See [`measure_width`].
    width: f32,
    /// Fixed when the menu opened — see the module docs.
    items: Vec<T>,
    /// One per item, in step with `items`. See [`RowMetrics`].
    rows: Vec<RowMetrics>,
}

impl<T: MenuItem> Menu<T> {
    /// A menu hanging down-and-right from the point, for a right-click.
    pub fn new(engine: &mut TextEngine, anchor: (f32, f32), items: Vec<T>) -> Self {
        Self::aligned(engine, anchor, items, Anchor::Cursor)
    }

    /// ...with the corner said out loud. What a `…` button uses.
    pub fn aligned(
        engine: &mut TextEngine,
        anchor: (f32, f32),
        items: Vec<T>,
        align: Anchor,
    ) -> Self {
        // Measured now, for the reason the items are fixed now: the panel
        // must not resize under the pointer because something else changed.
        let width = measure_width(engine, &items);
        let rows = measure_rows(engine, &items, width);
        Self {
            anchor,
            align,
            hover: None,
            control_hover: None,
            width,
            items,
            rows,
        }
    }

    /// A flyout beside its parent row: **prefer the right**, fall back left,
    /// with the row tops lined up so the pointer's horizontal handoff never
    /// crosses another parent row.
    pub fn as_submenu(
        engine: &mut TextEngine,
        parent: Rect,
        items: Vec<T>,
        viewport: (f32, f32),
    ) -> Self {
        let width = measure_width(engine, &items);
        let rows = measure_rows(engine, &items, width);
        let right_x = parent.x + parent.w;
        let fits_right = right_x + width <= viewport.0 - EDGE_MARGIN;
        let (anchor, align) = if fits_right {
            ((right_x, parent.y), Anchor::Cursor)
        } else {
            ((parent.x, parent.y), Anchor::ButtonRight)
        };
        Self {
            anchor,
            align,
            hover: None,
            control_hover: None,
            width,
            items,
            rows,
        }
    }

    /// The same menu at a width somebody else measured.
    ///
    /// For tests, and it is what keeps this widget's geometry testable
    /// **without a font**: everything below the width — the clamp, the row
    /// arithmetic, the hit test — is arithmetic, and making all of it wait on
    /// a `TextEngine` to answer one question would be the wrong trade. Public
    /// rather than `cfg(test)` because the hosts' test suites live in their
    /// own crates, where this crate's `cfg(test)` never fires.
    pub fn at_width(anchor: (f32, f32), items: Vec<T>, align: Anchor, width: f32) -> Self {
        let rows = fallback_rows(&items);
        Self {
            anchor,
            align,
            hover: None,
            control_hover: None,
            width,
            items,
            rows,
        }
    }

    /// Where this menu was anchored. For tests — the point of an anchor is
    /// where the panel lands, which `panel` already answers; this exists so a
    /// test can say *which button asked* without also asserting the clamping.
    pub fn anchor_for_test(&self) -> (f32, f32) {
        self.anchor
    }

    /// Move an open menu to a new anchor point.
    ///
    /// **The one thing that legitimately changes a live menu**, and it is
    /// geometry rather than rows: display zoom divides the viewport the anchor
    /// was captured in, so the button a menu hangs from moves while the point
    /// does not. A menu with a zoom stepper in it makes that happen on every
    /// press, which is what this is for. The rows and the measured width are
    /// untouched — text lays out in logical units, which zoom does not move.
    pub fn set_anchor(&mut self, anchor: (f32, f32)) {
        self.anchor = anchor;
    }

    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// The rows, mutable, for the one change an open menu may see: a toggle
    /// row's `checked` answer after it was chosen and the menu stayed open.
    pub fn items_mut(&mut self) -> &mut [T] {
        &mut self.items
    }

    /// The panel's height: a `PAD` above and below the row band. Summed from
    /// the rows rather than multiplied, because they are not all one height.
    pub fn height(&self) -> f32 {
        PAD * 2.0
            + (0..self.items.len())
                .map(|i| self.row_height(i))
                .sum::<f32>()
    }

    /// Row `i`'s height: measured for a card, the item's own otherwise.
    fn row_height(&self, i: usize) -> f32 {
        match self.rows.get(i) {
            Some(m) => m.height,
            None => self.items.get(i).map_or(ITEM_H, MenuItem::height),
        }
    }

    /// The card's panel inside row `i`, inset from the row's edges; `None`
    /// for a row that is not a card.
    pub fn card_rect(&self, viewport: (f32, f32), i: usize) -> Option<Rect> {
        self.items.get(i)?.card()?;
        let r = self.item_rect(viewport, i);
        Some(Rect::new(
            r.x + CARD_INSET,
            r.y + CARD_INSET,
            (r.w - CARD_INSET * 2.0).max(0.0),
            (r.h - CARD_INSET * 2.0).max(0.0),
        ))
    }

    /// The button at card `i`'s foot — its one control.
    fn card_button_rect(&self, viewport: (f32, f32), i: usize) -> Option<Rect> {
        let card = self.items.get(i)?.card()?;
        card.button.as_ref()?;
        let r = self.card_rect(viewport, i)?;
        let w = self.rows.get(i).map_or(CARD_BUTTON_MIN_W, |m| m.button_w);
        Some(Rect::new(
            r.x + CARD_PAD,
            r.y + r.h - CARD_PAD - CARD_BUTTON_H,
            w,
            CARD_BUTTON_H,
        ))
    }

    /// Where the menu sits in window space — anchored at the press, then
    /// shifted back inside the viewport when it would overflow an edge.
    pub fn panel(&self, viewport: (f32, f32)) -> Rect {
        let h = self.height();
        let mut x = match self.align {
            Anchor::Cursor => self.anchor.0,
            // The point is the menu's right edge, so it hangs back under the
            // button rather than out past it.
            Anchor::ButtonRight => self.anchor.0 - self.width,
        };
        let mut y = self.anchor.1;
        if x + self.width > viewport.0 - EDGE_MARGIN {
            x = (viewport.0 - EDGE_MARGIN - self.width).max(EDGE_MARGIN);
        }
        x = x.max(EDGE_MARGIN);
        if y + h > viewport.1 - EDGE_MARGIN {
            y = (viewport.1 - EDGE_MARGIN - h).max(EDGE_MARGIN);
        }
        Rect::new(x, y, self.width, h)
    }

    /// The row rectangle for `item`, in window space.
    ///
    /// Walks the rows, because a divider is shorter than an item. This is the
    /// only place that arithmetic exists — hit-testing and drawing both come
    /// here, so a divider added to one list cannot shift the other's targets.
    pub fn item_rect(&self, viewport: (f32, f32), item: usize) -> Rect {
        let p = self.panel(viewport);
        let mut y = p.y + PAD;
        for earlier in 0..item.min(self.items.len()) {
            y += self.row_height(earlier);
        }
        Rect::new(p.x, y, p.w, self.row_height(item))
    }

    /// Which row sits under `(x, y)`, or `None` if the point is outside the
    /// menu, in its padding band, or on a divider. Whether the row found is
    /// actionable is a separate question (`enabled`).
    pub fn item_at(&self, viewport: (f32, f32), x: f32, y: f32) -> Option<usize> {
        if !self.panel(viewport).contains(x, y) {
            return None;
        }
        (0..self.items.len())
            .find(|&i| self.items[i].selectable() && self.item_rect(viewport, i).contains(x, y))
    }

    /// The rectangle of control `control` on row `item`, in window space.
    ///
    /// The band is laid out **right to left from the row's trailing pad**, the
    /// same edge a chord is aligned to, and each cell takes its own fixed
    /// width. Pure arithmetic, like [`item_rect`](Self::item_rect): the hit
    /// test and the paint both come here, so a target can never be somewhere
    /// the glyph is not.
    pub fn control_rect(&self, viewport: (f32, f32), item: usize, control: usize) -> Option<Rect> {
        let row = self.items.get(item)?;
        let controls = row.controls();
        if control >= controls.len() {
            return None;
        }
        // A card's control is the button at its foot.
        if row.card().is_some() {
            return (control == 0)
                .then(|| self.card_button_rect(viewport, item))
                .flatten();
        }
        let r = self.item_rect(viewport, item);
        // A row that is nothing but its controls — a strip of buttons — spreads
        // them across its width, each in a cell of its own, rather than
        // packing them into the chord column.
        if row.label().is_empty() {
            let inner_w = (r.w - ROW_PAD_X * 2.0).max(0.0);
            let cell = inner_w / controls.len() as f32;
            return Some(Rect::new(
                r.x + ROW_PAD_X + cell * control as f32,
                r.y,
                cell,
                r.h,
            ));
        }
        let mut x = r.x + r.w - ROW_PAD_X - controls_width(&controls);
        for earlier in controls.iter().take(control) {
            x += earlier.width();
        }
        Some(Rect::new(x, r.y, controls[control].width(), r.h))
    }

    /// Which control of which row sits under `(x, y)`.
    ///
    /// **Answers for a dimmed button too.** The caller needs to know the press
    /// landed on the stepper at all — a `−` at 50% must swallow the press and
    /// leave the menu up, not read as a click on the panel and dismiss it.
    /// Whether it *does* anything is [`Control::Button::enabled`], which the
    /// caller already has to consult.
    pub fn control_at(&self, viewport: (f32, f32), x: f32, y: f32) -> Option<(usize, usize)> {
        if !self.panel(viewport).contains(x, y) {
            return None;
        }
        for i in 0..self.items.len() {
            // The rects come from `control_rect` rather than being walked here:
            // it is the only place the band's arithmetic exists, so a target
            // can never end up somewhere the glyph is not.
            let cells = self.items[i].controls().len();
            for c in 0..cells {
                if self
                    .control_rect(viewport, i, c)
                    .is_some_and(|r| r.contains(x, y))
                {
                    return Some((i, c));
                }
            }
        }
        None
    }

    /// Whether pressing the control under `(x, y)` would do something —
    /// `control_at` narrowed to the buttons that are actually live.
    pub fn pressable_control_at(
        &self,
        viewport: (f32, f32),
        x: f32,
        y: f32,
    ) -> Option<(usize, usize)> {
        let (i, c) = self.control_at(viewport, x, y)?;
        self.items
            .get(i)?
            .controls()
            .get(c)
            .is_some_and(Control::pressable)
            .then_some((i, c))
    }

    /// Step the highlight, skipping dividers and disabled rows, wrapping at
    /// either end and starting from whichever end you arrived from.
    pub fn step(&mut self, enabled: &[bool], delta: isize) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let usable = |i: usize| self.items[i].selectable() && *enabled.get(i).unwrap_or(&false);
        let start = match self.hover {
            Some(i) => i as isize,
            None if delta > 0 => -1,
            None => n as isize,
        };
        for step in 1..=n as isize {
            let i = (start + delta * step).rem_euclid(n as isize) as usize;
            if usable(i) {
                self.hover = Some(i);
                return;
            }
        }
    }

    /// A hint beside a dimmed row, saying why it cannot be chosen. Drawn only
    /// for the row under the pointer, and only when there is a reason worth
    /// giving.
    fn draw_disabled_hint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        enabled: &[bool],
        association: &KoruMenuInspection<'_>,
    ) {
        let Some(i) = self.hover.filter(|&i| enabled.get(i) == Some(&false)) else {
            return;
        };
        let Some(why) = self.items.get(i).and_then(MenuItem::disabled_reason) else {
            return;
        };
        let p = theme::p();
        let r = self.item_rect(viewport, i);
        let label = engine.simple_layout(&why, theme::role::hint());
        let pad = 7.0;
        let (w, h) = (label.width() + pad * 2.0, label.height() + pad * 2.0);
        // To the left of the menu, so it does not cover the rows below the one
        // it is about; flipped to the right when there is no room.
        let x = if r.x - 6.0 - w >= 4.0 {
            r.x - 6.0 - w
        } else {
            (r.x + r.w + 6.0).min((viewport.0 - w - 4.0).max(4.0))
        };
        let y = (r.y + (r.h - h) * 0.5).clamp(4.0, (viewport.1 - h - 4.0).max(4.0));
        let _inspection = inspection::disabled_hint(root, Rect::new(x, y, w, h), i, association);
        scene::floating_panel(scene, root, x, y, w, h, &scene::Panel::tooltip(6.0));
        crate::typography::line(
            "Disabled reason",
            scene,
            root,
            &label,
            x + pad,
            y + pad,
            theme::role::hint(),
            "theme::role::hint()",
            p.bubble_fg,
        );
    }

    /// Paint one row's trailing controls: a dim square behind the one under the
    /// pointer, then each glyph centred in its own cell, with the readout
    /// between them in the ordinary text colour.
    ///
    /// A disabled button keeps its cell and draws dim — the range it has run
    /// out of is information, and a control that vanished at 50% would move
    /// the `+` under the pointer mid-press.
    fn draw_controls(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        item: usize,
    ) {
        let pal = theme::p();
        for (c, control) in self.items[item].controls().iter().enumerate() {
            let Some(cell) = self.control_rect(viewport, item, c) else {
                continue;
            };
            let _inspection = inspection::control(
                root,
                cell,
                control,
                c,
                self.control_hover == Some((item, c)),
            );
            match control {
                Control::Button { glyph, enabled } => {
                    if *enabled && self.control_hover == Some((item, c)) {
                        // A spread strip's cell can be wider than a button; the
                        // square stays a button's width, centred on its glyph.
                        let hw = cell.w.min(CONTROL_W);
                        scene::fill(
                            scene,
                            root,
                            pal.hover_tint,
                            &scene::rrect(
                                cell.x + (cell.w - hw) * 0.5,
                                cell.y + 2.0,
                                hw,
                                cell.h - 4.0,
                                5.0,
                            ),
                        );
                    }
                    let icon = engine.icon_layout(*glyph, theme::CAPTION_ICON);
                    scene::draw_layout(
                        scene,
                        root,
                        &icon,
                        cell.x + (cell.w - icon.width()) * 0.5,
                        cell.y + (cell.h - icon.height()) * 0.5,
                        Some(if *enabled { pal.text } else { pal.text_dim }),
                    );
                }
                Control::Readout(value) => {
                    let label = engine.simple_layout(value, theme::role::row());
                    let line_h = scene::first_line_height(&label);
                    crate::typography::line(
                        "Control value",
                        scene,
                        root,
                        &label,
                        cell.x + (cell.w - label.width()) * 0.5,
                        cell.y + (cell.h - line_h) * 0.5,
                        theme::role::row(),
                        "theme::role::row()",
                        pal.text,
                    );
                }
            }
        }
    }

    /// Paint a card row: an inset panel — lit a shade while the row is hovered,
    /// the way a plain row is — with the badge chip and the date on its first
    /// line and a `›` at the line's end, the title and body wrapped under
    /// them, and the button at its foot. The button is the row's one control
    /// and draws from [`Menu::control_rect`], so the press and the paint agree.
    #[allow(clippy::too_many_arguments)]
    fn draw_card(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        i: usize,
        card: &MenuCard,
        on: bool,
    ) {
        let pal = theme::p();
        let Some(r) = self.card_rect(viewport, i) else {
            return;
        };
        let hovered = self.hover == Some(i) && on;
        let _inspection = inspection::card(root, r, card, i, hovered);
        scene::fill(
            scene,
            root,
            if hovered { pal.hover_tint } else { pal.chip_bg },
            &scene::rrect(r.x, r.y, r.w, r.h, CARD_RADIUS),
        );
        scene::stroke(
            scene,
            root,
            pal.panel_border,
            1.0,
            &scene::rrect(r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0, CARD_RADIUS),
        );
        let text = if on { pal.text } else { pal.text_dim };
        let inner_x = r.x + CARD_PAD;
        let inner_w = card_inner_w(self.width);
        let mut y = r.y + CARD_PAD;
        // The badge chip, the date beside it, the › at the far end.
        let badge = engine.simple_layout(&card.badge, theme::role::hint());
        let badge_w = badge.width() + 12.0;
        scene::fill(
            scene,
            root,
            pal.panel_bg,
            &scene::rrect(inner_x, y, badge_w, CARD_BADGE_H, 4.0),
        );
        crate::typography::line(
            "Card badge",
            scene,
            root,
            &badge,
            inner_x + 6.0,
            y + (CARD_BADGE_H - scene::first_line_height(&badge)) * 0.5,
            theme::role::hint(),
            "theme::role::hint()",
            pal.accent,
        );
        if !card.date.is_empty() {
            let date = engine.simple_layout(&card.date, theme::role::meta());
            crate::typography::line(
                "Card date",
                scene,
                root,
                &date,
                inner_x + badge_w + 8.0,
                y + (CARD_BADGE_H - scene::first_line_height(&date)) * 0.5,
                theme::role::meta(),
                "theme::role::meta()",
                pal.text_dim,
            );
        }
        let caret = engine.icon_layout(koru_draw::icons::CARET_RIGHT, theme::CAPTION_ICON);
        scene::draw_layout(
            scene,
            root,
            &caret,
            r.x + r.w - CARD_PAD - caret.width(),
            y + (CARD_BADGE_H - caret.height()) * 0.5,
            Some(pal.text_dim),
        );
        y += CARD_BADGE_H;
        if !card.title.is_empty() {
            y += CARD_GAP;
            let title =
                engine.simple_layout_wrapped(&card.title, theme::role::section_heading(), inner_w);
            scene::draw_layout(scene, root, &title, inner_x, y, Some(text));
            y += title.height();
        }
        if !card.body.is_empty() {
            y += CARD_GAP * 0.5;
            let body = engine.simple_layout_wrapped(&card.body, theme::role::row(), inner_w);
            scene::draw_layout(scene, root, &body, inner_x, y, Some(pal.text_dim));
        }
        let Some((label, enabled)) = &card.button else {
            return;
        };
        let Some(b) = self.card_button_rect(viewport, i) else {
            return;
        };
        let glyph = self.items[i].controls().first().and_then(|c| match c {
            Control::Button { glyph, .. } => Some(*glyph),
            Control::Readout(_) => None,
        });
        let lit = *enabled && self.control_hover == Some((i, 0));
        let _button = inspection::control(
            root,
            b,
            &Control::Button {
                glyph: glyph.unwrap_or(' '),
                enabled: *enabled,
            },
            0,
            lit,
        );
        let (fill, ink) = if *enabled {
            (pal.accent, pal.on_accent)
        } else {
            (pal.hover_tint, pal.text_dim)
        };
        scene::fill(scene, root, fill, &scene::rrect(b.x, b.y, b.w, b.h, 6.0));
        if lit {
            scene::fill(
                scene,
                root,
                pal.hover_tint,
                &scene::rrect(b.x, b.y, b.w, b.h, 6.0),
            );
        }
        let label = engine.simple_layout(label, theme::role::row());
        let icon = glyph.map(|g| engine.icon_layout(g, theme::CAPTION_ICON));
        let icon_w = icon.as_ref().map_or(0.0, |l| l.width() + 6.0);
        let mut x = b.x + (b.w - label.width() - icon_w) * 0.5;
        if let Some(icon) = &icon {
            scene::draw_layout(
                scene,
                root,
                icon,
                x,
                b.y + (b.h - icon.height()) * 0.5,
                Some(ink),
            );
            x += icon_w;
        }
        crate::typography::line(
            "Card button",
            scene,
            root,
            &label,
            x,
            b.y + (b.h - scene::first_line_height(&label)) * 0.5,
            theme::role::row(),
            "theme::role::row()",
            ink,
        );
    }

    /// Paint the menu. `enabled[i]` is whether row `i` may be chosen; a
    /// disabled row is drawn dimmed and shows no hover, the way a toolbar
    /// button that would do nothing is dimmed rather than hidden.
    pub fn draw(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        enabled: &[bool],
    ) {
        self.draw_with_inspection(
            scene,
            root,
            engine,
            viewport,
            enabled,
            KoruMenuInspection::default(),
        );
    }

    /// The ordinary painter with optional host-owned occurrence evidence.
    /// Associations describe existing identities; they never publish native
    /// accessibility nodes or retain the host callback beyond this draw.
    pub fn draw_with_inspection(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: (f32, f32),
        enabled: &[bool],
        association: KoruMenuInspection<'_>,
    ) {
        let p = self.panel(viewport);
        let pal = theme::p();
        let inspection = inspection::menu(root, p, self.align, &association);

        // Shadow, body, border — the recipe every floating surface shares, and
        // it lives in `scene` rather than here because two panels open at once
        // wearing different shadows is worse than either alone.
        scene::floating_panel(
            scene,
            root,
            p.x,
            p.y,
            p.w,
            p.h,
            &scene::Panel::popover(RADIUS),
        );

        for (i, item) in self.items.iter().enumerate() {
            let r = self.item_rect(viewport, i);
            let _inspection = inspection::row(
                root,
                r,
                item,
                i,
                enabled.get(i).copied().unwrap_or(false),
                self.hover == Some(i),
                &association,
            );
            if item.divider() {
                scene::fill(
                    scene,
                    root,
                    pal.panel_border,
                    &scene::rect(
                        r.x + 10.0,
                        r.y + (r.h - 1.0) * 0.5,
                        (r.w - 20.0).max(0.0),
                        1.0,
                    ),
                );
                continue;
            }
            let on = enabled.get(i).copied().unwrap_or(false);
            if let Some(card) = item.card() {
                self.draw_card(scene, root, engine, viewport, i, &card, on);
                continue;
            }
            if self.hover == Some(i) && on {
                scene::fill(
                    scene,
                    root,
                    pal.hover_tint,
                    &scene::rrect(p.x + 4.0, r.y, p.w - 8.0, r.h, 5.0),
                );
            }
            let label = label_layout(engine, item);
            let line_h = label
                .lines()
                .next()
                .map_or(0.0, |l| l.metrics().line_height);
            let colour = match (on, item.destructive()) {
                (false, _) => pal.text_dim,
                (true, true) => pal.close_hover,
                (true, false) => pal.text,
            };
            // A row with a leading slot — its own icon, or the column a toggle
            // row reserves — draws it first, then the label indented past it.
            let mut text_x = p.x + ROW_PAD_X;
            if leading(item).is_none()
                && let Some(token) = item.swatch()
            {
                // The rail's swatch, at the row's scale and the same accent
                // token: the square in the sidebar and the square in the
                // picker are one list.
                let fill = token
                    .and_then(theme::accent_tone)
                    .map(|tone| tone.swatch)
                    .unwrap_or(pal.text_dim);
                let _swatch = inspection::swatch(
                    root,
                    Rect::new(text_x, r.y + (r.h - SWATCH) * 0.5, SWATCH, SWATCH),
                    token,
                );
                scene::fill(
                    scene,
                    root,
                    fill,
                    &scene::rrect(
                        text_x,
                        r.y + (r.h - SWATCH) * 0.5,
                        SWATCH,
                        SWATCH,
                        SWATCH * 0.25,
                    ),
                );
                text_x += SWATCH + 8.0;
            }
            if let Some(brand) = item.brand_icon() {
                let side = theme::CAPTION_ICON;
                let _brand = inspection::brand(
                    root,
                    Rect::new(text_x, r.y + (r.h - side) * 0.5, side, side),
                    brand,
                );
                scene::draw_brand_icon(
                    scene,
                    root,
                    Rect::new(text_x, r.y + (r.h - side) * 0.5, side, side),
                    colour,
                    brand,
                );
                text_x += side + 8.0;
            } else if let Some(glyph) = leading(item) {
                let icon = if item.filled_icon() {
                    engine.icon_layout_filled(glyph, theme::CAPTION_ICON)
                } else {
                    engine.icon_layout(glyph, theme::CAPTION_ICON)
                };
                // A toggle row shows the check only while set; the slot stays,
                // so toggling never shifts the label.
                let visible = item.icon().is_some() || item.checked() == Some(true);
                if visible {
                    scene::draw_layout(
                        scene,
                        root,
                        &icon,
                        text_x,
                        r.y + (r.h - icon.height()) * 0.5,
                        Some(colour),
                    );
                }
                text_x += icon.width() + 8.0;
            }
            // **A label past the menu's ceiling ends in an ellipsis inside
            // its row** — a chat named after its first message, a document's
            // long title — rather than running out of the panel.
            let mut room = p.x + p.w - ROW_PAD_X - trailing_width(engine, item) - text_x;
            if item.unread() {
                room -= SHORTCUT_GAP + UNREAD_DOT;
            }
            let label = if label.width() > room {
                let stack = item.font_stack().unwrap_or_else(theme::ui_stack);
                let cut = engine.elided_in(item.label().as_ref(), theme::role::row(), stack, room);
                engine.simple_layout_in(&cut, theme::role::row(), stack)
            } else {
                label
            };
            crate::typography::line(
                "Menu label",
                scene,
                root,
                &label,
                text_x,
                r.y + (r.h - line_h) * 0.5,
                theme::role::row(),
                "theme::role::row()",
                colour,
            );
            // A row that opens a submenu wears a `›` where a chord would be:
            // the chord column is where "there is more to this row" already
            // lives, and a second column for one row would be a column.
            if item.has_submenu() {
                let caret = engine.icon_layout(koru_draw::icons::CARET_RIGHT, theme::CAPTION_ICON);
                scene::draw_layout(
                    scene,
                    root,
                    &caret,
                    p.x + p.w - caret.width() - ROW_PAD_X,
                    r.y + (r.h - caret.height()) * 0.5,
                    Some(colour),
                );
            } else if let Some(chord) = item.shortcut() {
                let hint = engine.simple_layout(&chord, theme::role::hint());
                let hint_h = scene::first_line_height(&hint);
                crate::typography::line(
                    "Menu shortcut",
                    scene,
                    root,
                    &hint,
                    p.x + p.w - hint.width() - ROW_PAD_X,
                    r.y + (r.h - hint_h) * 0.5,
                    theme::role::hint(),
                    "theme::role::hint()",
                    pal.text_dim,
                );
            } else if let Some(glyph) = item.trailing_icon() {
                // Where a chord would be, and as quiet as one: the row leaves
                // the app, and the glyph says so without shouting.
                let icon = engine.icon_layout(glyph, theme::CAPTION_ICON);
                scene::draw_layout(
                    scene,
                    root,
                    &icon,
                    p.x + p.w - icon.width() - ROW_PAD_X,
                    r.y + (r.h - icon.height()) * 0.5,
                    Some(pal.text_dim),
                );
            }
            if item.unread() {
                scene::fill(
                    scene,
                    root,
                    pal.accent,
                    &scene::circle(
                        p.x + p.w - ROW_PAD_X - UNREAD_DOT * 0.5,
                        r.y + r.h * 0.5,
                        UNREAD_DOT * 0.5,
                    ),
                );
            }
            self.draw_controls(scene, root, engine, viewport, i);
        }
        // This independently clamped explanation is a sibling surface. Its
        // logical owner is the disabled row, not the last painted descendant.
        drop(inspection);
        self.draw_disabled_hint(scene, root, engine, viewport, enabled, &association);
    }
}

#[cfg(test)]
mod tests;
