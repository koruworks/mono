// SPDX-License-Identifier: MPL-2.0

//! The Explore heading's compact actions menu.
//!
//! The pane owns popover lifetime; this module owns the mode-dependent action
//! vocabulary and the one button that opens it. Keeping those together means a
//! new heading action cannot be drawn without also being reachable.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, theme};
use koru_ui::icon_button::{self, Metrics, State};
use vello::Scene;
use vello::kurbo::Affine;

use super::NavigatorMode;

/// The `…` buttons' geometry — the heading's, the floor's and a row's: the
/// caption's glyph size, the chip the slot itself (no tab insets, which
/// `Metrics::STANDARD` carries for the title bar's buttons). One constant
/// so the three dots read as one control wherever they are (the review's
/// note, E76).
pub(super) const DOTS: Metrics = Metrics {
    icon_size: theme::CAPTION_ICON,
    padding_x: 0.0,
    padding_y: 0.0,
    inset_top: 0.0,
    inset_bottom: 0.0,
    radius: 6.0,
};

/// A `…` in `slot`, as the shared icon button: `KoruIconButton` in the
/// inspector, Phosphor's `dots-three`, the component's own hover chip.
pub(super) fn draw_dots(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    slot: Rect,
    hovered: bool,
) {
    if slot.w <= 0.0 {
        return;
    }
    icon_button::draw_with(
        scene,
        root,
        engine,
        slot,
        icons::DOTS_THREE,
        State {
            hovered,
            active: false,
            disabled: false,
        },
        DOTS,
    );
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Action {
    Import,
    CollapseAll,
    ExpandAll,
}

/// **Empty is a real answer**, and the tasks rail is what makes it one: there
/// is nothing to import into a queue and nothing to fold in it, so the mode
/// offers no actions and [`button`] draws none. A menu that opened on nothing
/// would be a control advertising something that is not there — the same
/// argument that keeps the context column's toggle hidden while its sections
/// are empty.
pub(super) fn available(mode: NavigatorMode) -> &'static [Action] {
    match mode {
        NavigatorMode::Documents => &[Action::Import, Action::CollapseAll, Action::ExpandAll],
        NavigatorMode::Files => &[Action::Import],
        NavigatorMode::Tasks => &[],
        // No Import: the Agents projection is read-only (Q-E4). The folds are
        // the pane's own furniture, so those two stay.
        NavigatorMode::Agents => &[Action::CollapseAll, Action::ExpandAll],
        // The artifacts column offers nothing at the workspace level: what
        // it can do is on its rows, and a menu of nothing is a dead end.
        NavigatorMode::Artifacts => &[],
        // Home's nav has no folds and nothing imports into it.
        NavigatorMode::Home => &[],
        // Nor does the Chats view: *New chat* heads its own list, and a menu
        // holding it a second time would be two doors to one room.
        NavigatorMode::Chats | NavigatorMode::Socials => &[],
        // Nor Canvases: nothing imports as a canvas, and a flat list has no
        // folds to collapse or expand.
        NavigatorMode::Canvases => &[],
    }
}

pub(super) fn glyph(action: Action) -> char {
    match action {
        Action::Import => icons::IMPORT,
        Action::CollapseAll => icons::FOLD_ALL,
        Action::ExpandAll => icons::UNFOLD_ALL,
    }
}

/// The stable keyword this action is *identified* by — never its label.
///
/// Read by the accessibility projection, which puts it on a locator an external
/// client can search for. *Import…* is renamed and translated;
/// `import` is neither. The same argument and the same shape as
/// [`NavigatorMode::token`](super::NavigatorMode::token).
pub(super) fn token(action: Action) -> &'static str {
    match action {
        Action::Import => "import",
        Action::CollapseAll => "collapse-all",
        Action::ExpandAll => "expand-all",
    }
}

pub(super) fn label(action: Action, _mode: NavigatorMode) -> &'static str {
    match action {
        // One word in every mode: the row opens the one Import list
        // (`context_menu::import_menu_items`), files and content both.
        Action::Import => "Import…",
        Action::CollapseAll => "Collapse all",
        Action::ExpandAll => "Expand all",
    }
}
