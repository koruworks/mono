// SPDX-License-Identifier: MPL-2.0

//! **The Workspaces page as elements, on the app's real types** — the
//! canonical fixture of ELEMENTS.final §3.2, compiled against the host
//! (ELEMENTS-PLAN P2).
//!
//! It is not drawn yet: the page still paints through `draw_page`, and P4
//! moves it over. What is here is the host adapter that move needs — the
//! keys, the tile and trailing-lane leaves, the row's state recipe, and the
//! frame laid out where input is measured — proved against today's page by
//! the tests beside it.
//!
//! **Values are today's, each through a token** (QUESTIONS E4): the row's
//! content starts 8 in because the lit plate is `Space::S1` inside the row
//! and the content another `Space::S1` inside the plate; the plate's corner
//! is the tagged `WORKSPACE_GEAR_RADIUS`; the heading is `ROW_MIN` high in
//! `Ink::Hint`.
//!
//! **Host policies stay the host's.** Input is measured against the settled
//! page (`settled_panel`) and a closing popover answers nothing (`live`);
//! the frame is laid out there and only then, exactly as `popover_rects`
//! decides today.

// Built and proved here; drawn from in P4, when the page moves over.
#![cfg_attr(not(test), allow(dead_code))]

use koru_core::WorkspaceId;
use koru_draw::el::{
    self, Action, Align, El, ElState, Frame, FrameCx, Interaction, Key, Leaf, Look, Role,
    StateRecipe, TextCache, col, leaf, row, scroll_y, text,
};
use koru_draw::geometry::Rect;
use koru_draw::theme::{Ink, Radius, Space, Surface, alias, metrics, roles};
use vello::Scene;
use vello::kurbo::Affine;

use super::{ExploreView, PopoverKind, PopoverPart, PopoverRow, draw_trailing, label_fit};
use koru_draw::text::TextEngine;

/// The popover's object, as its accessibility identities name it.
pub(crate) const SURFACE: &str = "explore-popover";
/// A row's primary control: the model's own root part, so its locator is
/// the one the row has always had.
pub(crate) const PRIMARY: &str = "root";
/// A row's gear.
pub(crate) const MANAGE: &str = "manage";

pub(crate) const PAGE: Key = Key::surface(SURFACE, "popover");
pub(crate) const HEADING: Key = Key::surface(SURFACE, "heading");

pub(crate) fn primary(id: WorkspaceId) -> Key {
    Key::model(id.as_uuid().as_u128(), PRIMARY)
}

pub(crate) fn manage(id: WorkspaceId) -> Key {
    Key::model(id.as_uuid().as_u128(), MANAGE)
}

/// The workspace row's states, extracted from `draw_page`: the lit plate is
/// the one change, and it lights for the pointer and the keyboard alike.
/// Current is not a fill — it is the name's weight and ink, and the tick.
pub(crate) static WORKSPACE_ROW_STATES: StateRecipe = StateRecipe {
    name: "workspace row",
    base: Look::NONE,
    selected: Look::NONE,
    hover: Look::bg(Surface::Hover),
    pressed: Look::NONE,
    selected_hover: Look::NONE,
    selected_pressed: Look::NONE,
    disabled: Look::NONE,
};

/// A workspace's tile, painted in the theme of the subtree it is in.
pub(crate) struct TileLeaf {
    pub icon: Option<String>,
    pub color: Option<String>,
    pub name: String,
}

impl Leaf for TileLeaf {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        let side = metrics::WORKSPACE_TILE.value;
        (side, side)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        _state: ElState,
    ) {
        koru_draw::tile::Tile {
            icon: self.icon.as_deref(),
            color: self.color.as_deref(),
            name: &self.name,
        }
        .draw_in(scene, root, engine, at, cx.palette());
    }
}

/// **The trailing lane: one allocation in every state** (§3.7). The gear
/// while the row is lit, the tick on the current row otherwise, nothing
/// else; the gear's own plate when the gear itself is hot. Paint-only — the
/// gear's node, name and actions are the element's, always there.
pub(crate) struct WorkspaceTrailingAdapter {
    pub current: bool,
    /// The gear's own key, so the lane can tell the gear being lit from the
    /// row it is in being lit.
    pub key: Key,
}

impl Leaf for WorkspaceTrailingAdapter {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (metrics::TRAILING_LANE_W.value, 0.0)
    }

    fn paint(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        cx: &FrameCx,
        state: ElState,
    ) {
        let reveal = state.hovered || state.focused || state.pressed;
        let on_gear = [
            cx.interaction.hot,
            cx.interaction.focused,
            cx.interaction.pressed,
        ]
        .contains(&Some(self.key));
        draw_trailing(
            scene,
            root,
            engine,
            at,
            self.current,
            reveal,
            on_gear.then_some(PopoverPart::Gear),
            cx.palette(),
        );
    }
}

impl ExploreView {
    /// The Workspaces page as an element tree: the heading, then the list
    /// that scrolls under it, a row per workspace in the order the page
    /// lists them.
    pub(crate) fn workspaces_page(&self, scroll: f32) -> El {
        let rows = self.rows_for(PopoverKind::Summary);
        let count = rows
            .iter()
            .filter(|row| matches!(row, PopoverRow::Workspace(_)))
            .count();
        // Today's words, kept until S1's translated heading lands in P4.
        let heading = format!("WORKSPACES · {count}");
        col("Workspaces page")
            .id(PAGE)
            .semantics(Role::Menu)
            .name(koru_i18n::Key::FloorChipAction)
            .pad_y(alias::PANEL_PAD.token)
            .child(
                text(heading.clone())
                    .named("Workspaces heading")
                    .id(HEADING)
                    .semantics(Role::Heading)
                    .name_text(heading)
                    .role(roles::POPOVER_HEADING)
                    .ink(Ink::Hint)
                    .pad_x(alias::NAV_PAD.token)
                    .min_h(metrics::ROW_MIN),
            )
            .child(scroll_y("Workspace list").fill().offset(scroll).children(
                rows.into_iter().filter_map(|row| match row {
                    PopoverRow::Workspace(i) => self.workspaces.get(i).map(|w| {
                        self.workspace_row(
                            w.id,
                            &w.name,
                            w.icon.clone(),
                            w.color.clone(),
                            self.current == Some(w.id),
                        )
                    }),
                    _ => None,
                }),
            ))
    }

    /// One workspace row: a neutral row whose whole width presses the
    /// primary, the lit plate inside it, and in the plate the primary (tile,
    /// name) and the gear as two sibling targets.
    pub(crate) fn workspace_row(
        &self,
        id: WorkspaceId,
        name: &str,
        icon: Option<String>,
        color: Option<String>,
        current: bool,
    ) -> El {
        let primary = primary(id);
        let gear = manage(id);
        row("Workspace row")
            .min_h(metrics::ROW_MIN)
            .pad_x(alias::ROW_PLATE_INSET.token)
            .hit_as(primary)
            .child(
                row("Workspace plate")
                    .fill()
                    .pad_x(alias::ROW_PLATE_INSET.token)
                    .gap(Space::S1)
                    .radius(Radius::Metric(metrics::WORKSPACE_GEAR_RADIUS))
                    .states(&WORKSPACE_ROW_STATES)
                    .activity([primary, gear])
                    .child(
                        row("Workspace primary action")
                            .fill()
                            .gap(Space::S2)
                            .id(primary)
                            .focusable()
                            .action(Action::Click)
                            .action(Action::Focus)
                            .semantics(Role::MenuItem)
                            .name_text(name)
                            .selected(current)
                            .child(
                                leaf(
                                    "Workspace tile",
                                    TileLeaf {
                                        icon,
                                        color,
                                        name: name.to_string(),
                                    },
                                )
                                .decorative()
                                .align_self(Align::Centre),
                            )
                            .child(
                                text(label_fit(name))
                                    .named("Workspace name")
                                    .role(roles::ROW)
                                    .ink(if current {
                                        Ink::Primary
                                    } else {
                                        Ink::Secondary
                                    })
                                    .emphasis(current)
                                    .fill()
                                    .truncate(),
                            ),
                    )
                    .child(
                        leaf(
                            "Manage gear",
                            WorkspaceTrailingAdapter { current, key: gear },
                        )
                        .semantic()
                        .w(metrics::TRAILING_LANE_W)
                        .id(gear)
                        .activity([primary])
                        .hit()
                        .focusable()
                        .action(Action::Click)
                        .action(Action::Focus)
                        .semantics(Role::MenuItem)
                        .name(koru_i18n::Key::ManageWorkspace),
                    ),
            )
    }

    /// The host's pointer and keyboard state, by element key: the lit row's
    /// primary, or its gear while the pointer is on the gear.
    pub(crate) fn popover_interaction(&self) -> Interaction {
        let Some(popover) = self.live() else {
            return Interaction::default();
        };
        let rows = self.popover_rows();
        let hot = popover
            .hover
            .and_then(|i| rows.get(i).copied())
            .and_then(|row| match row {
                PopoverRow::Workspace(i) => self.workspaces.get(i).map(|w| {
                    if self.popover_part == Some(PopoverPart::Gear) {
                        manage(w.id)
                    } else {
                        primary(w.id)
                    }
                }),
                _ => None,
            });
        Interaction {
            hot,
            ..Interaction::default()
        }
    }

    /// The Workspaces page laid out where its input is measured — the
    /// settled panel — and only while it takes input: `None` while another
    /// page shows or the popover is closing.
    pub(crate) fn workspaces_frame(
        &self,
        area: Rect,
        viewport: (f32, f32),
        engine: &mut TextEngine,
        cache: &mut TextCache,
        cx: FrameCx,
    ) -> Option<(El, Frame)> {
        let popover = self.live()?;
        if popover.kind != PopoverKind::Summary {
            return None;
        }
        let panel = self.settled_panel(area, viewport)?;
        let tree = self.workspaces_page(popover.scroll);
        let frame = tree.layout(
            engine,
            cache,
            panel,
            cx.with_interaction(self.popover_interaction()),
        );
        Some((tree, frame))
    }
}

/// The popover row and part an element key stands for: what a press or an
/// accessibility request on that key chooses.
pub(crate) fn row_part_of(view: &ExploreView, key: Key) -> Option<(PopoverRow, PopoverPart)> {
    let el::Object::Model(id) = key.object else {
        return None;
    };
    let index = view
        .workspaces
        .iter()
        .position(|w| w.id.as_uuid().as_u128() == id)?;
    let part = match key.part {
        MANAGE => PopoverPart::Gear,
        PRIMARY => PopoverPart::Row,
        _ => return None,
    };
    Some((PopoverRow::Workspace(index), part))
}

#[cfg(test)]
mod tests;
