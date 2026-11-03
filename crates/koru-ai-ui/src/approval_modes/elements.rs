// SPDX-License-Identifier: MPL-2.0

//! **The approval-mode list as elements** (koru-ai D7, D105;
//! `koru_draw::el`): one tree gives every part of the open list its place,
//! and the same tree, painted inside the panel's `FloatingPanel` record,
//! nests everything drawn in the panel under it in the inspector.
//!
//! ```text
//! col  Approval modes                   the panel, APPROVAL_PANEL_PAD round
//!   row  Approval mode row              APPROVAL_ROW_H, a mode a row
//!     Mode icon                         IconSize::XS, APPROVAL_ICON_TOP down
//!     col  Mode words                   fill, APPROVAL_WORDS_TOP down
//!       Mode title                      row role, elided
//!       Mode help                       meta role, secondary, elided
//!     col  Check lane                   TRAILING_LANE_W, the ✓ at its end
//! ```
//!
//! A row lights on the hover plate under the pointer, and stays lit while
//! the keyboard is on it (`MODE_ROW_LIT_STATES`); *Full access* is in the
//! notice ink (`Ink::Notice`, the warning colour); the current mode has the
//! accent ✓. Nothing here is a leaf: every part is a box, text or an icon.
//! Every value is a token or a tagged metric ([`super::tokens`]); this file
//! is on the source scan's migrated list (`koru_draw::theme::ui_scan`).

use koru_draw::el::{El, Frame, FrameCx, Key, Look, StateRecipe, col, icon, row, text};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::theme::{IconSize, Ink, Radius, Surface, metrics, roles};

pub(crate) use super::tokens::*;

/// The developer names the list reads its geometry back by.
pub(crate) const PANEL: &str = "Approval modes";
pub(crate) const MODE_ROW: &str = "Approval mode row";
pub(crate) const MODE_ICON: &str = "Mode icon";
pub(crate) const MODE_WORDS: &str = "Mode words";
pub(crate) const MODE_TITLE: &str = "Mode title";
pub(crate) const MODE_HELP: &str = "Mode help";
pub(crate) const CHECK_LANE: &str = "Check lane";
pub(crate) const CHECK: &str = "Current check";

/// A mode's row, by the mode's id: the same mode gives the same key in
/// every frame, wherever it is listed.
pub(crate) fn row_key(id: &str) -> Key {
    Key::model(crate::model_picker::stable_id(id), "approval-mode")
}

/// One mode as the list draws it.
pub(crate) struct ModeSpec {
    pub(crate) key: Key,
    pub(crate) icon: char,
    pub(crate) title: String,
    pub(crate) help: Option<String>,
    /// *Full access*: the icon and the title in the notice ink.
    pub(crate) full_access: bool,
    /// The chat's mode: the ✓.
    pub(crate) current: bool,
    /// The keyboard is on it: lit whatever the pointer does.
    pub(crate) lit: bool,
}

/// **A mode row's states**: lit on the hover plate under the pointer.
pub(crate) static MODE_ROW_STATES: StateRecipe = StateRecipe {
    name: "approval mode row",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look::bg(Surface::Hover),
    pressed: Look::NONE,
    selected_hover: Look::NONE,
    selected_pressed: Look::NONE,
    disabled: Look::NONE,
    focus: None,
};

/// The row the keyboard is on: on the hover plate whatever the pointer
/// does.
pub(crate) static MODE_ROW_LIT_STATES: StateRecipe = StateRecipe {
    name: "approval mode row, the keyboard's",
    base: Look::bg(Surface::Hover),
    ..MODE_ROW_STATES
};

/// **A row of the list**: the mode's icon, its title and the line under it,
/// elided to the room left before the ✓'s lane, which is kept whether or
/// not the ✓ shows.
fn mode_row(spec: &ModeSpec) -> El {
    let ink = if spec.full_access {
        Ink::Notice
    } else {
        Ink::Primary
    };
    let mut words = col(MODE_WORDS)
        .fill()
        .pad_start(WORDS_INSET)
        .pad_top(WORDS_TOP)
        .gap(WORDS_GAP)
        .child(
            text(spec.title.clone())
                .named(MODE_TITLE)
                .role(roles::ROW)
                .ink(ink)
                .truncate(),
        );
    if let Some(help) = &spec.help {
        words = words.child(
            text(help.clone())
                .named(MODE_HELP)
                .role(roles::META)
                .ink(Ink::Secondary)
                .truncate(),
        );
    }
    let mut lane = col(CHECK_LANE)
        .w(metrics::TRAILING_LANE_W)
        .items_end()
        .align_self(koru_draw::el::Align::Centre);
    if spec.current {
        lane = lane.child(
            icon(icons::CHECK, IconSize::XS)
                .decorative()
                .ink(Ink::Accent)
                .named(CHECK),
        );
    }
    row(MODE_ROW)
        .h(ROW_HEIGHT)
        .radius(Radius::M)
        .pad_x(ROW_INSET)
        .items_start()
        .states(if spec.lit {
            &MODE_ROW_LIT_STATES
        } else {
            &MODE_ROW_STATES
        })
        .id(spec.key)
        .hit()
        .child(
            icon(spec.icon, IconSize::XS)
                .decorative()
                .ink(ink)
                .pad_top(ICON_TOP)
                .named(MODE_ICON),
        )
        .child(words)
        .child(lane)
}

/// **The list as one tree**: a row a mode, in the agent's order, inside
/// the panel's inset.
pub(crate) fn tree(modes: &[ModeSpec]) -> El {
    col(PANEL)
        .pad(PANEL_INSET)
        .children(modes.iter().map(mode_row))
}

/// `tree` laid out in `area`, the panel, under `cx`: on the engine and cache
/// the composer's panels share with the model picker.
pub(crate) fn layout(tree: &El, area: Rect, cx: FrameCx) -> Frame {
    crate::model_picker::layout_elements(tree, area, cx)
}
