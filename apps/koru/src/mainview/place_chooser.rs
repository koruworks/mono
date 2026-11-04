// SPDX-License-Identifier: MPL-2.0

//! **Where an Alt+clicked reference opens** — the place chooser.
//!
//! A click on a reference in a document follows it in this tab, Ctrl+click
//! opens it in a background tab and Shift+click in a new window. Alt+click
//! asks where instead: five targets at the pointer, laid out as a cross. The
//! four arms put the reference in a new pane split off this one on that side;
//! the middle opens it in a new tab here, with no split. The same for every
//! reference the editor reads — an `@` document pill, an older document pill,
//! a `koru://` document link, a `koru://chat/…` link
//! ([`koru_editor::Reference`]).
//!
//! **The window's, not the document's.** The editor only names the reference
//! (`ViewState::wants_place_chooser`); the chooser is drawn here, over the
//! whole window, so it is never clipped at a pane's edge and is kept inside
//! the window's, and the answer is opened by the same door a click goes
//! through ([`MainView::open_reference`]).
//!
//! **Composed, not a new component.** The surface is `koru_ui`'s
//! [`PopoverShell`], each target an icon button drawn by
//! `koru_ui::icon_button` with its hover state, the arms wear the carets the
//! split preview already wears, and the highlighted arm shows that preview
//! over the pane it would divide (`panes::draw_split_preview`), so the
//! chooser says where the pane lands before anything is chosen.
//!
//! | | |
//! | --- | --- |
//! | Keys | The highlight starts on the middle. The arrows move it across the cross — from an arm, the opposite arrow comes back to the middle and the perpendicular ones turn to the neighbouring arms; Enter or Space opens; Escape closes with nothing opened. Every other key is swallowed: it is modal while it stands. |
//! | Pointer | Hovering a target highlights it; pressing one opens there; a press anywhere else closes the chooser, opens nothing, and goes no further. |
//! | Accessibility | A menu named *Where to open*, one menu item per target named by what it does; the highlight is the published focus. |

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use koru_core::schema::WorkspaceDoc;
use koru_draw::geometry::Rect;
use koru_draw::{icons, theme};
use koru_editor::{OpenWhere, Reference, SplitSide};
use koru_i18n::{Key, Lang, t};
use koru_ui::icon_button::{self, Metrics};
use koru_ui::{PopoverShell, TooltipContent};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key as WKey, NamedKey};
use winit::window::CursorIcon;

use super::MainView;
use super::panes::{self, PanePath, SplitDir};
use super::tab::TabView;

#[cfg(test)]
mod tests;

/// One target of the cross.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum Place {
    Up,
    Left,
    Middle,
    Right,
    Down,
}

impl Place {
    /// Reading order — the cross row by row, which is the order a screen
    /// reader lists the targets in and the order they are numbered in.
    pub(crate) const ALL: [Place; 5] = [
        Place::Up,
        Place::Left,
        Place::Middle,
        Place::Right,
        Place::Down,
    ];

    /// Where choosing this target opens the reference.
    pub(crate) fn where_(self) -> OpenWhere {
        match self.side() {
            Some(side) => OpenWhere::Split(side),
            None => OpenWhere::NewTab,
        }
    }

    fn side(self) -> Option<SplitSide> {
        match self {
            Place::Up => Some(SplitSide::Up),
            Place::Right => Some(SplitSide::Right),
            Place::Down => Some(SplitSide::Down),
            Place::Left => Some(SplitSide::Left),
            Place::Middle => None,
        }
    }

    /// The split this arm makes, for the preview drawn while it is
    /// highlighted. `None` for the middle, which splits nothing.
    pub(crate) fn split_dir(self) -> Option<SplitDir> {
        self.side().map(split_dir)
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

    /// What the target is called — its tooltip and its accessible name.
    pub(crate) fn label(self, lang: Lang) -> &'static str {
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

    /// Where an arrow key moves the highlight from here.
    ///
    /// From the middle an arrow goes to that arm. From an arm, the arrow
    /// pointing back across the cross returns to the middle, the outward one
    /// stays (there is nothing further out), and a perpendicular one turns to
    /// the neighbouring arm on that side — so every target is one or two
    /// presses from every other, and none is a dead end.
    pub(crate) fn step(self, arrow: NamedKey) -> Place {
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
}

/// The pane split a [`SplitSide`] names.
pub(crate) fn split_dir(side: SplitSide) -> SplitDir {
    match side {
        SplitSide::Up => SplitDir::Top,
        SplitSide::Right => SplitDir::Right,
        SplitSide::Down => SplitDir::Bottom,
        SplitSide::Left => SplitDir::Left,
    }
}

/// Each target's icon control: the caption glyph size, with a panel pad of
/// clear space on every side — a target the pointer can land on without
/// aiming, and no inset, because the chooser has no strip to sit in.
const TARGET: Metrics = Metrics {
    icon_size: theme::CAPTION_ICON,
    padding_x: theme::PANEL_PAD,
    padding_y: theme::PANEL_PAD,
    inset_top: 0.0,
    inset_bottom: 0.0,
    radius: Metrics::COMPACT.radius,
};

/// The width and height of one cell of the grid.
fn cell_side() -> f32 {
    TARGET.control_width()
}

/// The whole chooser's side: three cells, the gaps between them and the
/// panel's padding round them.
fn panel_side() -> f32 {
    theme::PANEL_PAD * 2.0 + cell_side() * 3.0 + theme::TAB_GAP * 2.0
}

/// The key a target's tooltip goes by, clear of the window's other tooltips.
const PLACE_TIP: u64 = 900;

/// One opening of the chooser.
#[derive(Clone, Debug)]
pub(crate) struct PlaceChooser {
    /// What was Alt+clicked.
    pub reference: Reference,
    /// The pane the click was in — the one a split divides.
    pub pane: PanePath,
    /// The press, in window coordinates. The middle target sits under it, so
    /// a second press without moving opens a new tab.
    pub at: (f32, f32),
    /// The target Enter would open: the keyboard's position and the pointer's
    /// hover, which are one highlight, the way a menu's are.
    pub highlight: Place,
    /// A block of ids for the accessibility tree: this, then one per target
    /// in [`Place::ALL`] order. Never reused, so a request queued against a
    /// closed chooser cannot land in the next one.
    pub instance: u64,
}

impl PlaceChooser {
    pub(crate) fn new(reference: Reference, pane: PanePath, at: (f32, f32)) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            reference,
            pane,
            at,
            highlight: Place::Middle,
            instance: NEXT.fetch_add(Place::ALL.len() as u64 + 1, Ordering::Relaxed),
        }
    }

    /// The surface, centred on the press and kept inside the window by the
    /// popover recipe's inset, so it is never clipped at an edge.
    pub(crate) fn panel(&self, viewport: (f32, f32)) -> Rect {
        let side = panel_side();
        let inset = theme::overlay::popover().viewport_inset;
        let clamp = |centre: f32, extent: f32| {
            (centre - side * 0.5).clamp(inset, (extent - side - inset).max(inset))
        };
        Rect::new(
            clamp(self.at.0, viewport.0),
            clamp(self.at.1, viewport.1),
            side,
            side,
        )
    }

    /// Where one target is drawn and hit.
    pub(crate) fn cell_rect(&self, viewport: (f32, f32), place: Place) -> Rect {
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
    pub(crate) fn place_at(&self, viewport: (f32, f32), x: f32, y: f32) -> Option<Place> {
        Place::ALL
            .into_iter()
            .find(|place| self.cell_rect(viewport, *place).contains(x, y))
    }

    /// A target's own accessibility id.
    pub(crate) fn place_instance(&self, place: Place) -> u64 {
        self.instance + 1 + place.index() as u64
    }

    /// Which target an accessibility id names, if it is one of this
    /// opening's.
    pub(crate) fn place_of_instance(&self, id: u64) -> Option<Place> {
        let offset = id.checked_sub(self.instance + 1)?;
        Place::ALL.get(usize::try_from(offset).ok()?).copied()
    }

    fn tooltip(&self, viewport: (f32, f32), place: Place, lang: Lang) -> TooltipContent {
        // Under the whole cross rather than under the target, where it would
        // cover the target below it.
        TooltipContent::new(
            PLACE_TIP + place.index() as u64,
            self.panel(viewport),
            place.label(lang),
        )
    }
}

impl MainView {
    /// Open the chooser for `reference`, at the press, over the pane it was
    /// pressed in. Any menu goes: the chooser is the newest thing asked for.
    pub(super) fn open_place_chooser(&mut self, reference: Reference) -> bool {
        self.nested = None;
        self.submenu = None;
        self.menu = None;
        self.tooltip.clear();
        self.place_chooser = Some(PlaceChooser::new(
            reference,
            self.panes.focused_path().clone(),
            self.pointer,
        ));
        true
    }

    /// An Alt+click the active document asked about, drained after the press.
    pub(super) fn drain_place_chooser(&mut self) -> bool {
        let asked = match self.active_view() {
            TabView::Leaf(leaf) => leaf.take_wants_place_chooser(),
            _ => None,
        };
        match asked {
            Some(reference) => self.open_place_chooser(reference),
            None => false,
        }
    }

    /// Open the reference at `place`, and close the chooser.
    ///
    /// The pane the click was in takes the focus back first, when it is
    /// still there, so a split divides the pane the person was looking at.
    pub(crate) fn choose_place(&mut self, ws: &WorkspaceDoc, place: Place) -> bool {
        let Some(chooser) = self.place_chooser.take() else {
            return false;
        };
        self.tooltip.clear();
        if self.panes.contains_group(&chooser.pane) {
            self.panes.set_focus(chooser.pane.clone());
        }
        koru_ui::keys::debug::write(format_args!(
            "[Koru][input] view place-chooser chose={place:?}"
        ));
        self.open_reference(ws, chooser.reference, place.where_());
        true
    }

    /// Close the chooser with nothing opened.
    pub(crate) fn dismiss_place_chooser(&mut self) -> bool {
        let had = self.place_chooser.take().is_some();
        if had {
            self.tooltip.clear();
        }
        had
    }

    /// A press while the chooser stands: on a target it opens there, anywhere
    /// else it closes the chooser. Either way the press goes no further.
    /// `None` when no chooser is up.
    pub(super) fn place_chooser_mouse_down(
        &mut self,
        ws: &WorkspaceDoc,
        x: f32,
        y: f32,
    ) -> Option<bool> {
        let chooser = self.place_chooser.as_ref()?;
        Some(match chooser.place_at(self.viewport, x, y) {
            Some(place) => self.choose_place(ws, place),
            None => self.dismiss_place_chooser(),
        })
    }

    /// The pointer moved while the chooser stands: the target under it is the
    /// highlight, and its name is the tooltip. Off the targets the highlight
    /// stays where it was, so Enter always has somewhere to go. `None` when
    /// no chooser is up.
    pub(super) fn place_chooser_mouse_move(&mut self, x: f32, y: f32) -> Option<bool> {
        let lang = crate::format::lang();
        let viewport = self.viewport;
        let chooser = self.place_chooser.as_mut()?;
        let over = chooser.place_at(viewport, x, y);
        let mut changed = false;
        if let Some(place) = over
            && place != chooser.highlight
        {
            chooser.highlight = place;
            changed = true;
        }
        let tip = over.map(|place| chooser.tooltip(viewport, place, lang));
        changed |= self.tooltip.set_hover(tip, Instant::now());
        Some(changed)
    }

    /// A key while the chooser stands. Always handled: it is modal.
    pub(super) fn place_chooser_key(&mut self, ws: &WorkspaceDoc, key: &WKey) -> bool {
        let Some(highlight) = self.place_chooser.as_ref().map(|c| c.highlight) else {
            return false;
        };
        match key {
            WKey::Named(NamedKey::Escape) => {
                self.dismiss_place_chooser();
            }
            WKey::Named(NamedKey::Enter | NamedKey::Space) => {
                self.choose_place(ws, highlight);
            }
            WKey::Character(c) if c.as_str() == " " => {
                self.choose_place(ws, highlight);
            }
            WKey::Named(
                arrow @ (NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::ArrowLeft
                | NamedKey::ArrowRight),
            ) => {
                self.set_place_highlight(highlight.step(*arrow));
            }
            _ => {}
        }
        true
    }

    /// Move the highlight from the keyboard (or a screen reader's focus), and
    /// name the target at once — a key press has already said what it wants.
    pub(crate) fn set_place_highlight(&mut self, place: Place) -> bool {
        let lang = crate::format::lang();
        let viewport = self.viewport;
        let Some(chooser) = self.place_chooser.as_mut() else {
            return false;
        };
        chooser.highlight = place;
        let tip = chooser.tooltip(viewport, place, lang);
        self.tooltip.set_focus(Some(tip), Instant::now());
        true
    }

    /// The pointer over the chooser: pressable over a target, the arrow
    /// elsewhere — never the document's I-beam through the panel.
    pub(super) fn place_chooser_cursor(&self) -> Option<CursorIcon> {
        let chooser = self.place_chooser.as_ref()?;
        Some(
            match chooser.place_at(self.viewport, self.pointer.0, self.pointer.1) {
                Some(_) => koru_ui::cursor::PRESS,
                None => CursorIcon::Default,
            },
        )
    }

    /// The chooser, and the split its highlighted arm would make.
    pub(super) fn draw_place_chooser(&mut self, scene: &mut Scene, root: Affine) {
        let Some(chooser) = self.place_chooser.clone() else {
            return;
        };
        if let Some(dir) = chooser.highlight.split_dir()
            && let Some(view_rect) = self.panes.view_rect_for(self.content(), &chooser.pane)
        {
            panes::draw_split_preview(
                scene,
                &mut self.engine,
                root,
                panes::preview_rect(view_rect, dir),
                dir,
                panes::PREVIEW_TINT_ALPHA,
            );
        }
        let viewport = self.viewport;
        let shell = PopoverShell {
            rect: chooser.panel(viewport),
            radius: theme::overlay::popover().radius,
        };
        let engine = &mut self.engine;
        shell.draw_containing(scene, root, |scene, root| {
            for place in Place::ALL {
                icon_button::draw_with(
                    scene,
                    root,
                    engine,
                    chooser.cell_rect(viewport, place),
                    place.glyph(),
                    icon_button::State {
                        hovered: place == chooser.highlight,
                        active: false,
                        disabled: false,
                    },
                    TARGET,
                );
            }
        });
    }
}
