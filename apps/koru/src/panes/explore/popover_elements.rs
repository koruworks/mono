// SPDX-License-Identifier: MPL-2.0

//! **The Workspaces page as elements, on the app's real types** — the
//! canonical fixture of ELEMENTS.final §3.2, and since ELEMENTS-PLAN P4 the
//! page itself.
//!
//! One frame answers everything the page does: its paint, where a press
//! lands, the rows the keyboard walks, how far the list scrolls and what it
//! reveals, and the accessibility tree. The host keeps its policies (below)
//! and the band chrome a scrolling list wears; nothing here repeats a row's
//! arithmetic.
//!
//! **Values are today's, each through a token** (QUESTIONS E4): the row's
//! content starts 8 in because the lit plate is `Space::S1` inside the row
//! and the content another `Space::S1` inside the plate; the plate's corner
//! is the tagged `WORKSPACE_GEAR_RADIUS`; the heading is `ROW_MIN` high in
//! `Ink::Hint`.
//!
//! **Host policies stay the host's.** Input is measured against the settled
//! page (`settled_panel`) and a closing popover answers nothing (`live`);
//! paint follows the panel as it slides and fades, exactly as before.

use std::cell::RefCell;

use koru_core::WorkspaceId;
use koru_draw::el::{
    self, Action, Align, El, ElState, Frame, FrameCx, Interaction, Key, Leaf, Look, Role,
    StateRecipe, TextCache, col, leaf, row, scroll_y, text,
};
use koru_draw::geometry::Rect;
use koru_draw::theme::{Ink, Radius, Space, Surface, alias, metrics, roles};
use vello::Scene;
use vello::kurbo::Affine;

use super::{
    ExploreView, PopoverKind, PopoverPart, PopoverRow, WorkspaceRowParts, draw_trailing, label_fit,
};
use koru_draw::text::TextEngine;

/// The page's shaped-text cache, shared by its paint and by the layout that
/// answers a press, a key, a scroll and the accessibility tree, so each
/// name is shaped once and not once per question.
#[derive(Default)]
pub(crate) struct PageLayout {
    cache: TextCache,
}

thread_local! {
    /// The engine input layouts shape with. Paint is handed the window's;
    /// a press, a key and the accessibility tree arrive without one. One per
    /// UI thread, made on first use, so a second window or a reopened pane
    /// does not pay for a font context again.
    static INPUT_ENGINE: RefCell<Option<TextEngine>> = const { RefCell::new(None) };
}

/// `f` with the thread's input-layout engine.
fn with_input_engine<R>(f: impl FnOnce(&mut TextEngine) -> R) -> R {
    INPUT_ENGINE.with_borrow_mut(|engine| f(engine.get_or_insert_with(TextEngine::new)))
}

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
        // Uppercased with its count, as the Profiles page is headed (S1).
        let heading = format!(
            "{} · {count}",
            koru_i18n::t(crate::format::lang(), koru_i18n::Key::Workspaces).to_uppercase()
        );
        col("Workspaces page")
            .id(PAGE)
            .semantics(Role::Menu)
            .name(koru_i18n::Key::FloorChipAction)
            .pad_y(alias::PANEL_PAD)
            .child(
                text(heading.clone())
                    .named("Workspaces heading")
                    .id(HEADING)
                    .semantics(Role::Heading)
                    .name_text(heading)
                    .role(roles::POPOVER_HEADING)
                    .ink(Ink::Hint)
                    .pad_x(alias::NAV_PAD)
                    .min_h(metrics::ROW_MIN),
            )
            .child(
                scroll_y(LIST)
                    .fill()
                    .offset(scroll)
                    .children(rows.into_iter().filter_map(|row| match row {
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
                    })),
            )
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
        row(ROW)
            .min_h(metrics::ROW_MIN)
            .pad_x(alias::ROW_PLATE_INSET)
            .hit_as(primary)
            .child(
                row("Workspace plate")
                    .fill()
                    .pad_x(alias::ROW_PLATE_INSET)
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
                                    TILE,
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
                                    .named(NAME)
                                    .role(roles::ROW)
                                    .ink(if current {
                                        Ink::Primary
                                    } else {
                                        Ink::Secondary
                                    })
                                    .emphasis(current)
                                    .fill()
                                    .truncate()
                                    .ink_centred(),
                            ),
                    )
                    .child(
                        leaf(GEAR, WorkspaceTrailingAdapter { current, key: gear })
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

    /// The key the lit row lights — its primary, or its gear while the
    /// pointer or the keyboard is on the gear — for the row at `hover`.
    pub(crate) fn lit_key(&self, hover: Option<usize>) -> Option<Key> {
        let rows = self.popover_rows();
        match hover.and_then(|i| rows.get(i).copied())? {
            PopoverRow::Workspace(i) => self.workspaces.get(i).map(|w| {
                if self.popover_part == Some(PopoverPart::Gear) {
                    manage(w.id)
                } else {
                    primary(w.id)
                }
            }),
            _ => None,
        }
    }

    /// The host's pointer and keyboard state, by element key, while the
    /// page takes input.
    pub(crate) fn popover_interaction(&self) -> Interaction {
        let Some(popover) = self.live() else {
            return Interaction::default();
        };
        Interaction {
            hot: self.lit_key(popover.hover),
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

    /// The settled Workspaces page's frame, open or closing, answering `f`:
    /// its geometry, scroll extent and reveal. `None` while another page is
    /// up. Input that must not reach a closing popover checks `live` itself.
    pub(crate) fn with_workspaces<R>(
        &self,
        area: Rect,
        viewport: (f32, f32),
        f: impl FnOnce(&Frame) -> R,
    ) -> Option<R> {
        let popover = self.popover.as_ref()?;
        if popover.kind != PopoverKind::Summary {
            return None;
        }
        let panel = self.settled_panel(area, viewport)?;
        let tree = self.workspaces_page(popover.scroll);
        let interaction = Interaction {
            hot: self.lit_key(popover.hover),
            ..Interaction::default()
        };
        let frame = with_input_engine(|engine| {
            tree.layout(
                engine,
                &mut self.page_layout.borrow_mut().cache,
                panel,
                FrameCx::root().with_interaction(interaction),
            )
        });
        Some(f(&frame))
    }

    /// The Workspaces page laid out for paint in `panel` — where the
    /// popover is this frame, mid-slide or settled — scrolled by `scroll`,
    /// with the row at `hover` lit, on the window's engine.
    pub(crate) fn workspaces_at(
        &self,
        engine: &mut TextEngine,
        panel: Rect,
        scroll: f32,
        hover: Option<usize>,
    ) -> (El, Frame) {
        let tree = self.workspaces_page(scroll);
        let interaction = Interaction {
            hot: self.lit_key(hover),
            ..Interaction::default()
        };
        let mut layout = self.page_layout.borrow_mut();
        let frame = tree.layout(
            engine,
            &mut layout.cache,
            panel,
            FrameCx::root().with_interaction(interaction),
        );
        (tree, frame)
    }

    /// The page's rows in the order `popover_rows` lists them, each with
    /// its border box: the heading, then a row per workspace.
    pub(crate) fn workspaces_rows(&self, frame: &Frame) -> Vec<(PopoverRow, Rect)> {
        let mut rows = row_rects(frame).into_iter();
        self.rows_for(PopoverKind::Summary)
            .into_iter()
            .filter_map(|row| match row {
                PopoverRow::WorkspacesHeading => frame.node(HEADING).map(|n| (row, n.rect)),
                PopoverRow::Workspace(i) if self.workspaces.get(i).is_some() => {
                    rows.next().map(|r| (row, r))
                }
                _ => None,
            })
            .collect()
    }

    /// The row and part a point presses, by the frame's targets: a row's
    /// primary or its gear; the heading's words, which press nothing but
    /// are still the heading; `None` anywhere else.
    pub(crate) fn workspaces_part_at(
        &self,
        frame: &Frame,
        x: f32,
        y: f32,
    ) -> Option<(usize, PopoverRow, PopoverPart)> {
        let rows = self.popover_rows();
        let (row, part) = match frame.hit_at(x, y) {
            Some(key) => row_part_of(self, key)?,
            None if frame.node(HEADING)?.rect.contains(x, y) => {
                (PopoverRow::WorkspacesHeading, PopoverPart::Row)
            }
            None => return None,
        };
        let index = rows.iter().position(|r| *r == row)?;
        Some((index, row, part))
    }

    /// Workspace `i`'s row element laid out in `r`, read back as its parts.
    pub(crate) fn workspace_parts_in(&self, i: usize, r: Rect) -> Option<WorkspaceRowParts> {
        let w = self.workspaces.get(i)?;
        let tree = self.workspace_row(
            w.id,
            &w.name,
            w.icon.clone(),
            w.color.clone(),
            self.current == Some(w.id),
        );
        let frame = with_input_engine(|engine| {
            tree.layout(
                engine,
                &mut self.page_layout.borrow_mut().cache,
                r,
                FrameCx::root(),
            )
        });
        Some(WorkspaceRowParts {
            chip: frame.rect_of(TILE)?,
            name: frame.rect_of(NAME)?,
            trailing: frame.rect_of(GEAR)?,
        })
    }
}

/// The scroll viewport's rect, content extent and offset, as laid out.
pub(crate) fn list_of(frame: &Frame) -> Option<(Rect, f32, f32)> {
    let list = frame.named(LIST).next()?;
    let (content, offset) = list.scroll?;
    Some((list.rect, content, offset))
}

/// Each row's border box in list order, beside the key of its primary.
pub(crate) fn row_rects(frame: &Frame) -> Vec<Rect> {
    frame.named(ROW).map(|node| node.rect).collect()
}

/// The developer names the host reads geometry by.
pub(crate) const ROW: &str = "Workspace row";
pub(crate) const LIST: &str = "Workspace list";
pub(crate) const NAME: &str = "Workspace name";
pub(crate) const TILE: &str = "Workspace tile";
pub(crate) const GEAR: &str = "Manage gear";

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
