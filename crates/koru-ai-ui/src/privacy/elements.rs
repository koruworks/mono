// SPDX-License-Identifier: MPL-2.0

//! **The workspace-access panel as elements** (koru-ai D106;
//! `koru_draw::el`): one tree gives every part of the open panel its place,
//! and the same tree, painted inside the panel's `FloatingPanel` record,
//! nests everything drawn in the panel under it in the inspector.
//!
//! ```text
//! col  Workspace access                 the panel, ACCESS_PANEL_PAD round
//!   row  Access heading                 ACCESS_HEADING_H
//!     Access heading words · Access count      meta role, secondary
//!   col  Workspace filter row           over five workspaces only
//!     Workspace filter                  ACCESS_FILTER_H, the shared field
//!   scroll  Workspace list              fill, a row a step, the scrollbar
//!     row  Workspace access row         ACCESS_ROW_H: name · ✓ lane
//!   col  No workspaces                  in the list's place when none show
//! ```
//!
//! **The filter field goes in as a named adapter** (ELEMENTS.final §3.1;
//! QUESTIONS M7): the model picker's `SearchLeaf`, which draws the shared
//! `TextInput` in the allocation the frame gives it. A chosen workspace's
//! row is on `Surface::Selected`, a row under the pointer on
//! `Surface::Hover`; the chosen ones carry the accent ✓. The list is the
//! one scroll element every list uses (M1): whole rows, the shared
//! scrollbar. Every value is a token or a tagged metric ([`super::tokens`]);
//! this file is on the source scan's migrated list
//! (`koru_draw::theme::ui_scan`).

use koru_ai_core::WorkspaceId;
use koru_draw::el::{
    El, Frame, FrameCx, Key, Look, StateRecipe, col, icon, leaf, row, scroll_y, text,
};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::theme::{IconSize, Ink, Radius, Surface, roles};

pub(crate) use super::tokens::*;
use crate::model_picker::{SearchLeaf, SearchPaint};

/// The developer names the panel reads its geometry back by.
pub(crate) const PANEL: &str = "Workspace access";
pub(crate) const HEADING: &str = "Access heading";
pub(crate) const HEADING_WORDS: &str = "Access heading words";
pub(crate) const COUNT: &str = "Access count";
pub(crate) const FILTER_ROW: &str = "Workspace filter row";
pub(crate) const FILTER: &str = "Workspace filter";
pub(crate) const LIST: &str = "Workspace list";
pub(crate) const ROW: &str = "Workspace access row";
pub(crate) const ROW_NAME: &str = "Workspace name";
pub(crate) const CHECK_LANE: &str = "Check lane";
pub(crate) const CHECK: &str = "Chosen check";
pub(crate) const EMPTY: &str = "No workspaces";
pub(crate) const EMPTY_NOTE: &str = "No workspaces note";

/// The panel, as its keys name it.
pub(crate) const SURFACE: &str = "workspace-access";
pub(crate) const FILTER_KEY: Key = Key::surface(SURFACE, "filter");
pub(crate) const LIST_KEY: Key = Key::surface(SURFACE, "list");

/// A workspace's row, by the workspace's id: never its place in the list,
/// which a filter changes.
pub(crate) fn row_key(workspace: WorkspaceId) -> Key {
    Key::model(workspace.as_uuid().as_u128(), "access")
}

/// One workspace as the panel draws it.
pub(crate) struct AccessRow {
    pub(crate) key: Key,
    pub(crate) label: String,
    /// The chat may reach it: the selected plate and the ✓.
    pub(crate) chosen: bool,
}

/// What the panel shows, as far as its layout and its declarations go.
pub(crate) struct Spec {
    /// *WORKSPACE ACCESS*, and the count after it: chosen of all.
    pub(crate) heading: String,
    pub(crate) count: String,
    /// The filter field, over five workspaces.
    pub(crate) filter: bool,
    /// Every workspace the filter lets through, and the first one shown.
    pub(crate) rows: Vec<AccessRow>,
    pub(crate) offset: usize,
    /// What shows in the list's place while it has no row.
    pub(crate) empty: Option<String>,
}

/// **What the adapters draw** beyond the spec: the filter field's input.
/// None of it when the tree is laid out only for its geometry.
#[derive(Default)]
pub(crate) struct Painted {
    pub(crate) filter: Option<SearchPaint>,
}

/// **A workspace row's states**: a chosen workspace on the selected plate,
/// whatever the pointer does; any other on the hover plate under the
/// pointer.
pub(crate) static ROW_STATES: StateRecipe = StateRecipe {
    name: "workspace access row",
    base: Look::NONE,
    selected: Look::bg(Surface::Selected),
    hover: Look::bg(Surface::Hover),
    pressed: Look::NONE,
    selected_hover: Look::bg(Surface::Selected),
    selected_pressed: Look::NONE,
    disabled: Look::NONE,
    focus: None,
};

/// **A row**: the workspace's name, elided to the room before the ✓'s lane,
/// which is kept whether or not the ✓ shows.
fn access_row(spec: &AccessRow) -> El {
    let mut lane = col(CHECK_LANE).w(CHECK_LANE_W).items_end();
    if spec.chosen {
        lane = lane.child(
            icon(icons::CHECK, IconSize::S)
                .decorative()
                .ink(Ink::Accent)
                .named(CHECK),
        );
    }
    row(ROW)
        .h(ROW_HEIGHT)
        .radius(Radius::Metric(ROW_RADIUS))
        .pad_x(ROW_INSET)
        .items_center()
        .states(&ROW_STATES)
        .selected(spec.chosen)
        .id(spec.key)
        .hit()
        .child(
            text(spec.label.clone())
                .named(ROW_NAME)
                .role(roles::ROW)
                .ink(Ink::Primary)
                .fill()
                .truncate(),
        )
        .child(lane)
}

/// **The panel as one tree.** `painted` is what its adapter draws; a tree
/// laid out only for its geometry hands it nothing.
pub(crate) fn tree(spec: &Spec, painted: Painted) -> El {
    let heading = row(HEADING)
        .h(HEADING_HEIGHT)
        .pad_start(HEADING_INSET)
        .pad_top(HEADING_TOP)
        .gap(COUNT_GAP)
        .items_start()
        .child(
            text(spec.heading.clone())
                .named(HEADING_WORDS)
                .role(roles::META)
                .ink(Ink::Secondary),
        )
        .child(
            text(spec.count.clone())
                .named(COUNT)
                .role(roles::META)
                .ink(Ink::Secondary),
        );
    let mut panel = col(PANEL)
        .pad_x(PANEL_INSET)
        .pad_bottom(PANEL_INSET)
        .child(heading);
    if spec.filter {
        panel = panel.child(
            col(FILTER_ROW)
                .pad_x(FILTER_INSET)
                .pad_bottom(FILTER_GAP)
                .child(
                    leaf(FILTER, SearchLeaf(painted.filter))
                        .semantic()
                        .id(FILTER_KEY)
                        .h(FILTER_HEIGHT),
                ),
        );
    }
    match &spec.empty {
        Some(note) => panel.child(
            col(EMPTY)
                .fill()
                .pad_x(EMPTY_INSET)
                .pad_top(EMPTY_TOP)
                .child(
                    text(note.clone())
                        .named(EMPTY_NOTE)
                        .role(roles::ROW)
                        .ink(Ink::Secondary)
                        .wrap(),
                ),
        ),
        None => panel.child(
            scroll_y(LIST)
                .id(LIST_KEY)
                .fill()
                .row_step(ROW_HEIGHT)
                .scrollbar()
                .offset(spec.offset as f32 * ROW_HEIGHT.value)
                .children(spec.rows.iter().map(access_row)),
        ),
    }
}

/// `tree` laid out in `area`, the panel, under `cx`: on the engine and cache
/// the composer's panels share with the model picker.
pub(crate) fn layout(tree: &El, area: Rect, cx: FrameCx) -> Frame {
    crate::model_picker::layout_elements(tree, area, cx)
}
