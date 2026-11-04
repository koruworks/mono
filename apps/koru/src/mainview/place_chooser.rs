// SPDX-License-Identifier: MPL-2.0

//! **Where an Alt+clicked reference opens** — the window's half of the place
//! chooser.
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
//! **The cross is KoruUI's** ([`koru_ui::place_chooser`]): its layout,
//! hit-testing, hover, keys, drawing and the shape a screen reader hears.
//! It answers a [`Place`] and knows nothing of what is opening. This file is
//! the wiring: what each place opens ([`where_`]), the modal plumbing (the
//! `PlaceChooser` blocker layer, the tooltip that names a target), and the
//! split preview the highlighted arm draws over the pane it would divide
//! (`panes::draw_split_preview`), so the chooser says where the pane lands
//! before anything is chosen.
//!
//! **The window's, not the document's.** The editor only names the reference
//! (`ViewState::wants_place_chooser`); the chooser is drawn here, over the
//! whole window, so it is never clipped at a pane's edge, and the answer is
//! opened by the same door a click goes through
//! ([`MainView::open_reference`]).

use std::time::Instant;

use koru_core::schema::WorkspaceDoc;
use koru_editor::{OpenWhere, Reference, SplitSide};
use koru_ui::place_chooser::{Outcome, Place, PlaceChooser};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::Key as WKey;
use winit::window::CursorIcon;

use super::MainView;
use super::panes::{self, PanePath, SplitDir};
use super::tab::TabView;

#[cfg(test)]
mod tests;

/// Where choosing `place` opens the reference: an arm splits on its side,
/// the middle is a new tab and no split.
pub(crate) fn where_(place: Place) -> OpenWhere {
    match side(place) {
        Some(side) => OpenWhere::Split(side),
        None => OpenWhere::NewTab,
    }
}

fn side(place: Place) -> Option<SplitSide> {
    match place {
        Place::Up => Some(SplitSide::Up),
        Place::Right => Some(SplitSide::Right),
        Place::Down => Some(SplitSide::Down),
        Place::Left => Some(SplitSide::Left),
        Place::Middle => None,
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

/// One opening of the chooser, and what it was opened for.
#[derive(Clone, Debug)]
pub(crate) struct PlaceAsk {
    /// What was Alt+clicked.
    pub reference: Reference,
    /// The pane a split divides: the one the click was in.
    pub pane: PanePath,
    /// The cross itself.
    pub chooser: PlaceChooser,
}

impl MainView {
    /// Open the chooser for `reference` at `at`, over the focused pane. Any
    /// menu goes: the chooser is the newest thing asked for.
    pub(super) fn open_place_chooser(&mut self, reference: Reference, at: (f32, f32)) -> bool {
        self.nested = None;
        self.submenu = None;
        self.menu = None;
        self.tooltip.clear();
        self.place_chooser = Some(PlaceAsk {
            reference,
            pane: self.panes.focused_path().clone(),
            chooser: PlaceChooser::open(at),
        });
        true
    }

    /// An Alt+click the active document asked about, drained after the press.
    pub(super) fn drain_place_chooser(&mut self) -> bool {
        let asked = match self.active_view() {
            TabView::Leaf(leaf) => leaf.take_wants_place_chooser(),
            _ => None,
        };
        match asked {
            Some(reference) => self.open_place_chooser(reference, self.pointer),
            None => false,
        }
    }

    /// Open the reference at `place`, and close the chooser.
    ///
    /// The pane the chooser was opened over takes the focus back first, when
    /// it is still there, so a split divides the pane the person was looking
    /// at.
    pub(crate) fn choose_place(&mut self, ws: &WorkspaceDoc, place: Place) -> bool {
        let Some(ask) = self.place_chooser.take() else {
            return false;
        };
        self.tooltip.clear();
        if self.panes.contains_group(&ask.pane) {
            self.panes.set_focus(ask.pane.clone());
        }
        koru_ui::keys::debug::write(format_args!(
            "[Koru][input] view place-chooser chose={place:?}"
        ));
        self.open_reference(ws, ask.reference, where_(place));
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

    /// Act on what the chooser answered.
    fn settle_place_chooser(&mut self, ws: &WorkspaceDoc, outcome: Outcome) -> bool {
        match outcome {
            Outcome::Handled => true,
            Outcome::Moved(place) => self.set_place_highlight(place),
            Outcome::Chose(place) => self.choose_place(ws, place),
            Outcome::Dismissed => self.dismiss_place_chooser(),
        }
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
        let outcome = self
            .place_chooser
            .as_ref()?
            .chooser
            .mouse_down(self.viewport, x, y);
        Some(self.settle_place_chooser(ws, outcome))
    }

    /// The pointer moved while the chooser stands: the target under it is the
    /// highlight, and its name is the tooltip. `None` when no chooser is up.
    pub(super) fn place_chooser_mouse_move(&mut self, x: f32, y: f32) -> Option<bool> {
        let lang = crate::format::lang();
        let viewport = self.viewport;
        let chooser = &mut self.place_chooser.as_mut()?.chooser;
        let mut changed = chooser.mouse_move(viewport, x, y);
        let tip = chooser
            .place_at(viewport, x, y)
            .map(|place| chooser.tooltip(viewport, place, lang));
        changed |= self.tooltip.set_hover(tip, Instant::now());
        Some(changed)
    }

    /// A key while the chooser stands. Always handled: it is modal.
    pub(super) fn place_chooser_key(&mut self, ws: &WorkspaceDoc, key: &WKey) -> bool {
        let Some(ask) = self.place_chooser.as_mut() else {
            return false;
        };
        let outcome = ask.chooser.key(key);
        self.settle_place_chooser(ws, outcome);
        true
    }

    /// Move the highlight from the keyboard (or a screen reader's focus), and
    /// name the target at once — a key press has already said what it wants.
    pub(crate) fn set_place_highlight(&mut self, place: Place) -> bool {
        let lang = crate::format::lang();
        let viewport = self.viewport;
        let Some(ask) = self.place_chooser.as_mut() else {
            return false;
        };
        ask.chooser.set_highlight(place);
        let tip = ask.chooser.tooltip(viewport, place, lang);
        self.tooltip.set_focus(Some(tip), Instant::now());
        true
    }

    /// The pointer over the chooser: pressable over a target, the arrow
    /// elsewhere — never the document's I-beam through the panel.
    pub(super) fn place_chooser_cursor(&self) -> Option<CursorIcon> {
        let ask = self.place_chooser.as_ref()?;
        Some(
            ask.chooser
                .cursor(self.viewport, self.pointer.0, self.pointer.1),
        )
    }

    /// The split the highlighted arm would make, then the cross over it.
    pub(super) fn draw_place_chooser(&mut self, scene: &mut Scene, root: Affine) {
        let Some(ask) = self.place_chooser.clone() else {
            return;
        };
        if let Some(dir) = side(ask.chooser.highlight()).map(split_dir)
            && let Some(view_rect) = self.panes.view_rect_for(self.content(), &ask.pane)
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
        ask.chooser
            .draw(scene, root, &mut self.engine, self.viewport);
    }
}
