// SPDX-License-Identifier: MPL-2.0

//! **KoruPlaceChooser — where something opens, asked at the pointer.**
//!
//! Five targets laid out as a cross on a popover: the four arms name a side
//! (a split above, right, below or left of the host's pane) and the middle
//! names *no split* (the host's new tab). The chooser knows nothing of what
//! is being opened or what a side means; it answers a [`Place`], and the
//! host does the rest.
//!
//! Consumers: Alt+click on a reference in a document, and on a document or
//! chat row in the workspace navigator (ADR-58's amendment). The host
//! owns the modal plumbing (the blocker layer, the split preview over the
//! pane an arm would divide, the tooltip it names a target in) and what each
//! answer opens.
//!
//! **Composed, not drawn anew.** The surface is [`PopoverShell`], each
//! target an [`icon_button`](crate::icon_button) with its hover state, and
//! the arms wear the carets the split preview already wears.
//!
//! | | |
//! | --- | --- |
//! | Geometry | Centred on the press and kept inside the window by the popover recipe's inset; the middle sits under the press, so a second press without moving chooses it. Draw, hit-test, the tooltip anchor and the semantics' rects all come from [`PlaceChooser::panel`] and [`PlaceChooser::cell_rect`]. |
//! | Keys | The highlight starts on the middle. The arrows move it across the cross — from an arm, the opposite arrow comes back to the middle and the perpendicular ones turn to the neighbouring arms; Enter or Space chooses; Escape dismisses. Every other key is swallowed ([`Outcome::Handled`]): it is modal while it stands. |
//! | Pointer | Hovering a target highlights it; off the targets the highlight stays, so Enter always has somewhere to go. A press on a target chooses it; a press anywhere else dismisses. The pointer is [`cursor::PRESS`](crate::cursor::PRESS) over a target and the arrow elsewhere — never the hand (ADR-215). |
//! | Accessibility | [`PlaceChooser::semantics`]: a menu named *Where to open*, one item per target in reading order named by what it does; the highlight is the published focus. Plain data — the host projects it, with ids from the block [`PlaceChooser::instance`] reserves. |
//! | Words | Every label is a `koru-i18n` key (`PlaceChooser`, `PlaceSplitUp/Right/Down/Left`, `PlaceNewTab`). |

use std::sync::atomic::{AtomicU64, Ordering};

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, inspect, scene, theme};
use koru_i18n::{Key, Lang, t};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key as KeyPress, NamedKey};
use winit::window::CursorIcon;

use crate::icon_button::{self, Metrics};
use crate::{PopoverShell, TooltipContent};

#[cfg(test)]
mod tests;

/// One target of the cross.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Place {
    Up,
    Left,
    Middle,
    Right,
    Down,
}

impl Place {
    /// Reading order — the cross row by row, which is the order a screen
    /// reader lists the targets in and the order they are numbered in.
    pub const ALL: [Place; 5] = [
        Place::Up,
        Place::Left,
        Place::Middle,
        Place::Right,
        Place::Down,
    ];

    /// What the target is called — its tooltip and its accessible name.
    pub fn label(self, lang: Lang) -> &'static str {
        t(
            lang,
            match self {
                Place::Up => Key::PlaceSplitUp,
                Place::Right => Key::PlaceSplitRight,
                Place::Down => Key::PlaceSplitDown,
                Place::Left => Key::PlaceSplitLeft,
                Place::Middle => Key::PlaceNewTab,
            },
        )
    }

    /// Where an arrow key moves the highlight from here.
    ///
    /// From the middle an arrow goes to that arm. From an arm, the arrow
    /// pointing back across the cross returns to the middle, the outward one
    /// stays (there is nothing further out), and a perpendicular one turns to
    /// the neighbouring arm on that side — so every target is one or two
    /// presses from every other, and none is a dead end. Any other key stays.
    pub fn step(self, arrow: NamedKey) -> Place {
        let toward = match arrow {
            NamedKey::ArrowUp => Place::Up,
            NamedKey::ArrowDown => Place::Down,
            NamedKey::ArrowLeft => Place::Left,
            NamedKey::ArrowRight => Place::Right,
            _ => return self,
        };
        if self == Place::Middle || toward == self {
            return toward;
        }
        if toward == self.opposite() {
            return Place::Middle;
        }
        toward
    }

    fn opposite(self) -> Place {
        match self {
            Place::Up => Place::Down,
            Place::Down => Place::Up,
            Place::Left => Place::Right,
            Place::Right => Place::Left,
            Place::Middle => Place::Middle,
        }
    }

    fn glyph(self) -> char {
        match self {
            Place::Up => icons::CARET_UP,
            Place::Right => icons::CARET_RIGHT,
            Place::Down => icons::CARET_DOWN,
            Place::Left => icons::CARET_LEFT,
            Place::Middle => icons::TABS,
        }
    }

    /// Column and row in the 3×3 grid the cross is drawn on.
    fn cell(self) -> (f32, f32) {
        match self {
            Place::Up => (1.0, 0.0),
            Place::Left => (0.0, 1.0),
            Place::Middle => (1.0, 1.0),
            Place::Right => (2.0, 1.0),
            Place::Down => (1.0, 2.0),
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }
}

/// What the host does after an input the chooser handled. Every input
/// reaching a standing chooser is handled: it is modal.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Swallowed; nothing changed.
    Handled,
    /// The keyboard moved the highlight here. The host names the target at
    /// once — a key press has already said what it wants.
    Moved(Place),
    /// Open there, and take the chooser down.
    Chose(Place),
    /// Take the chooser down with nothing opened.
    Dismissed,
}

/// The chooser as a screen reader should hear it, in window coordinates.
/// Plain data: the host projects it into its own tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Semantics {
    /// The menu's name, *Where to open*.
    pub name: String,
    /// The whole cross.
    pub bounds: Rect,
    /// One per target, in [`Place::ALL`] order.
    pub items: Vec<SemanticItem>,
    /// The highlighted target — what Enter would choose, and the focus.
    pub focus: Place,
}

/// One target a screen reader can press.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticItem {
    pub place: Place,
    pub label: String,
    pub rect: Rect,
    /// This target's id in the block [`PlaceChooser::instance`] reserved.
    pub instance: u64,
}

/// Each target's icon control: the caption glyph size, with a panel pad of
/// clear space on every side — a target the pointer can land on without
/// aiming, and no inset, because the chooser has no strip to sit in.
pub const TARGET: Metrics = Metrics {
    icon_size: theme::CAPTION_ICON,
    padding_x: theme::PANEL_PAD,
    padding_y: theme::PANEL_PAD,
    inset_top: 0.0,
    inset_bottom: 0.0,
    radius: Metrics::COMPACT.radius,
};

/// The key a target's tooltip goes by, clear of a window's other tooltips;
/// the five targets take this and the four after it.
pub const TOOLTIP_KEY: u64 = 900;

/// The width and height of one cell of the grid.
fn cell_side() -> f32 {
    TARGET.control_width()
}

/// The whole chooser's side: three cells, the gaps between them and the
/// panel's padding round them.
fn panel_side() -> f32 {
    theme::PANEL_PAD * 2.0 + cell_side() * 3.0 + theme::TAB_GAP * 2.0
}

/// One opening of the chooser: where it was asked for, and its highlight.
#[derive(Clone, Debug, PartialEq)]
pub struct PlaceChooser {
    at: (f32, f32),
    highlight: Place,
    instance: u64,
}

impl PlaceChooser {
    /// Open at the press `at`, in window coordinates, highlighting the
    /// middle.
    pub fn open(at: (f32, f32)) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            at,
            highlight: Place::Middle,
            instance: NEXT.fetch_add(Place::ALL.len() as u64 + 1, Ordering::Relaxed),
        }
    }

    /// The press it was opened at.
    pub fn at(&self) -> (f32, f32) {
        self.at
    }

    /// The target Enter would choose: the keyboard's position and the
    /// pointer's hover, which are one highlight, the way a menu's are.
    pub fn highlight(&self) -> Place {
        self.highlight
    }

    /// Move the highlight — the keyboard's arrows, a screen reader's focus.
    pub fn set_highlight(&mut self, place: Place) {
        self.highlight = place;
    }

    /// A block of ids for an accessibility tree: this, then one per target
    /// in [`Place::ALL`] order. Never reused, so a request queued against a
    /// closed chooser cannot land in the next one.
    pub fn instance(&self) -> u64 {
        self.instance
    }

    /// A target's own id in the block.
    pub fn place_instance(&self, place: Place) -> u64 {
        self.instance + 1 + place.index() as u64
    }

    /// Which target an id names, if it is one of this opening's.
    pub fn place_of_instance(&self, id: u64) -> Option<Place> {
        let offset = id.checked_sub(self.instance + 1)?;
        Place::ALL.get(usize::try_from(offset).ok()?).copied()
    }

    /// The surface, centred on the press and kept inside the window by the
    /// popover recipe's inset, so it is never clipped at an edge.
    pub fn panel(&self, viewport: (f32, f32)) -> Rect {
        let side = panel_side();
        let inset = theme::overlay::popover().viewport_inset;
        let clamp = |centre: f32, extent: f32| {
            (centre - side * 0.5).clamp(inset, (extent - side - inset).max(inset))
        };
        let (x, y) = self.at;
        Rect::new(clamp(x, viewport.0), clamp(y, viewport.1), side, side)
    }

    /// Where one target is drawn and hit.
    pub fn cell_rect(&self, viewport: (f32, f32), place: Place) -> Rect {
        let panel = self.panel(viewport);
        let (col, row) = place.cell();
        let step = cell_side() + theme::TAB_GAP;
        Rect::new(
            panel.x + theme::PANEL_PAD + col * step,
            panel.y + theme::PANEL_PAD + row * step,
            cell_side(),
            cell_side(),
        )
    }

    /// The target under a point, if any.
    pub fn place_at(&self, viewport: (f32, f32), x: f32, y: f32) -> Option<Place> {
        Place::ALL
            .into_iter()
            .find(|place| self.cell_rect(viewport, *place).contains(x, y))
    }

    /// A target's tooltip: its name, under the whole cross rather than
    /// under the target, where it would cover the target below it.
    pub fn tooltip(&self, viewport: (f32, f32), place: Place, lang: Lang) -> TooltipContent {
        TooltipContent::new(
            TOOLTIP_KEY + place.index() as u64,
            self.panel(viewport),
            place.label(lang),
        )
    }

    /// A press: on a target it chooses that target, anywhere else it
    /// dismisses. Either way the press goes no further.
    pub fn mouse_down(&self, viewport: (f32, f32), x: f32, y: f32) -> Outcome {
        match self.place_at(viewport, x, y) {
            Some(place) => Outcome::Chose(place),
            None => Outcome::Dismissed,
        }
    }

    /// The pointer moved: the target under it is the highlight. Off the
    /// targets the highlight stays where it was. `true` when it changed.
    pub fn mouse_move(&mut self, viewport: (f32, f32), x: f32, y: f32) -> bool {
        match self.place_at(viewport, x, y) {
            Some(place) if place != self.highlight => {
                self.highlight = place;
                true
            }
            _ => false,
        }
    }

    /// A key. Escape dismisses, Enter or Space chooses the highlight, an
    /// arrow moves it; anything else is swallowed.
    pub fn key(&mut self, key: &KeyPress) -> Outcome {
        match key {
            KeyPress::Named(NamedKey::Escape) => Outcome::Dismissed,
            KeyPress::Named(NamedKey::Enter | NamedKey::Space) => Outcome::Chose(self.highlight),
            KeyPress::Character(c) if c.as_str() == " " => Outcome::Chose(self.highlight),
            KeyPress::Named(
                arrow @ (NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::ArrowLeft
                | NamedKey::ArrowRight),
            ) => {
                self.highlight = self.highlight.step(*arrow);
                Outcome::Moved(self.highlight)
            }
            _ => Outcome::Handled,
        }
    }

    /// The pointer over the chooser: pressable over a target, the arrow
    /// elsewhere — never the I-beam of whatever is underneath.
    pub fn cursor(&self, viewport: (f32, f32), x: f32, y: f32) -> CursorIcon {
        match self.place_at(viewport, x, y) {
            Some(_) => crate::cursor::PRESS,
            None => crate::cursor::IDLE,
        }
    }

    /// What a screen reader should hear, in `lang`.
    pub fn semantics(&self, viewport: (f32, f32), lang: Lang) -> Semantics {
        Semantics {
            name: t(lang, Key::PlaceChooser).to_owned(),
            bounds: self.panel(viewport),
            items: Place::ALL
                .into_iter()
                .map(|place| SemanticItem {
                    place,
                    label: place.label(lang).to_owned(),
                    rect: self.cell_rect(viewport, place),
                    instance: self.place_instance(place),
                })
                .collect(),
            focus: self.highlight,
        }
    }

    /// Paint the cross: the popover and its five targets, the highlighted
    /// one hovered. Anything the host draws for an arm (a split preview) is
    /// the host's, drawn before this.
    #[track_caller]
    pub fn draw(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: (f32, f32),
    ) {
        let panel = self.panel(viewport);
        let _component = inspect::scope(
            "KoruPlaceChooser",
            "component",
            root,
            scene::rect(panel.x, panel.y, panel.w, panel.h),
            |r| {
                r.property(
                    "implementation",
                    "koru_ui::place_chooser::PlaceChooser",
                    "shared implementation",
                );
                r.property(
                    "highlight",
                    format!("{:?}", self.highlight),
                    "instance state",
                );
                r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
            },
        );
        let shell = PopoverShell {
            rect: panel,
            radius: theme::overlay::popover().radius,
        };
        shell.draw_containing(canvas, root, |canvas, root| {
            for place in Place::ALL {
                icon_button::draw_with(
                    canvas,
                    root,
                    engine,
                    self.cell_rect(viewport, place),
                    place.glyph(),
                    icon_button::State {
                        hovered: place == self.highlight,
                        active: false,
                        disabled: false,
                    },
                    TARGET,
                );
            }
        });
    }
}
