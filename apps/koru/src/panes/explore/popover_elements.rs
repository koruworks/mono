// SPDX-License-Identifier: MPL-2.0

//! **The switcher's two pages as elements, on the app's real types** — the
//! Workspaces page, the canonical fixture of ELEMENTS.final §3.2, since
//! ELEMENTS-PLAN P4, and the Profiles page ([`profiles`]) since P5.
//!
//! One frame answers everything a page does: its paint, where a press
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
use vello::peniko::ImageBrush;

pub(crate) mod profiles;

/// The page's shaped-text cache, shared by its paint and by the layout that
/// answers a press, a key, a scroll and the accessibility tree, so each
/// name is shaped once and not once per question.
#[derive(Default)]
pub(crate) struct PageLayout {
    cache: TextCache,
}

/// The engine input layouts shape with: paint is handed the window's, and a
/// press, a key and the accessibility tree arrive without one.
use crate::input_engine::with as with_input_engine;

/// The popover's object, as its accessibility identities name it.
pub(crate) const SURFACE: &str = "explore-popover";
/// A row's primary control: the model's own root part, so its locator is
/// the one the row has always had.
pub(crate) const PRIMARY: &str = "root";
/// A row's gear.
pub(crate) const MANAGE: &str = "manage";
pub(crate) const PIN: &str = "pin";

pub(crate) const PAGE: Key = Key::surface(SURFACE, "popover");
pub(crate) const HEADING: Key = Key::surface(SURFACE, "heading");

pub(crate) fn primary(id: WorkspaceId) -> Key {
    Key::model(id.as_uuid().as_u128(), PRIMARY)
}

pub(crate) fn manage(id: WorkspaceId) -> Key {
    Key::model(id.as_uuid().as_u128(), MANAGE)
}

pub(crate) fn pin(id: WorkspaceId) -> Key {
    Key::model(id.as_uuid().as_u128(), PIN)
}

struct PinLane {
    pinned: bool,
    pending: bool,
    disabled: bool,
}

impl Leaf for PinLane {
    fn measure(&self, _: Option<f32>) -> (f32, f32) {
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
        if !self.pinned && !self.pending && !state.hovered && !state.focused && !state.pressed {
            return;
        }
        let glyph = if self.pending {
            koru_draw::icons::CLOCK
        } else {
            koru_draw::icons::PUSH_PIN
        };
        let size = koru_draw::theme::IconSize::L.px(koru_draw::theme::Profile::Desktop);
        let shaped = if self.pinned && !self.pending {
            engine.icon_layout_filled(glyph, size)
        } else {
            engine.icon_layout(glyph, size)
        };
        let ink = if self.disabled {
            Ink::Hint
        } else if self.pinned {
            Ink::Primary
        } else {
            Ink::Secondary
        };
        koru_draw::scene::draw_layout(
            scene,
            root,
            &shaped,
            at.x + (at.w - shaped.width()) * 0.5,
            at.y + (at.h - shaped.height()) * 0.5,
            Some(ink.of(cx.palette())),
        );
    }
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
    focus: None,
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
/// gear's node, name and actions are the element's, always there. A
/// workspace row's and a Profile row's alike.
pub(crate) struct TrailingAdapter {
    pub current: bool,
    /// The gear's own key, so the lane can tell the gear being lit from the
    /// row it is in being lit.
    pub key: Key,
}

impl Leaf for TrailingAdapter {
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
    pub(crate) fn menu_tooltip(
        &self,
        area: Rect,
        viewport: (f32, f32),
        x: f32,
        y: f32,
    ) -> Option<(Rect, String, Option<koru_keys::ShortcutId>)> {
        self.with_page(area, viewport, |frame| {
            if let Some(node) = [
                profiles::MenuAction::Sync.key(),
                super::workspace_menu_elements::MenuAction::Sync.key(),
                super::workspace_menu_elements::MenuAction::Cloud.key(),
            ]
            .into_iter()
            .find_map(|key| frame.node(key).filter(|node| node.rect.contains(x, y)))
            {
                return Some((node.rect, self.sync_control_name(), None));
            }
            if let Some((rect, description)) = self.profiles.iter().find_map(|profile| {
                let rect = frame.node(profiles::primary(profile.id))?.rect;
                rect.contains(x, y)
                    .then(|| (rect, self.profile_description(profile)))
            }) {
                return Some((rect, description, None));
            }
            self.workspaces.iter().find_map(|workspace| {
                let rect = frame.node(pin(workspace.id))?.rect;
                if !rect.contains(x, y) {
                    return None;
                }
                let (pinned, _, reason) = self.workspace_pin_state(workspace.id);
                let words = reason.unwrap_or_else(|| {
                    koru_i18n::t(
                        crate::format::lang(),
                        if pinned {
                            koru_i18n::Key::UnpinFromRail
                        } else {
                            koru_i18n::Key::PinToRail
                        },
                    )
                    .to_owned()
                });
                Some((rect, words, None))
            })
        })
        .flatten()
    }

    pub(crate) fn element_page_height(&self, kind: PopoverKind) -> Option<f32> {
        let tree = self.page_tree(kind, 0.0, None)?;
        let frame = with_input_engine(|engine| {
            tree.layout(
                engine,
                &mut self.page_layout.borrow_mut().cache,
                Rect::new(0.0, 0.0, super::POPOVER_W, super::SWITCHER_MAX_H),
                FrameCx::root(),
            )
        });
        let (list, content, _) = list_of(&frame)?;
        let allocated: f32 = frame
            .nodes
            .iter()
            .filter(|node| node.parent == Some(0))
            .map(|node| node.rect.h)
            .sum();
        Some(allocated - list.h + content.max(metrics::ROW_MIN.value) + super::POPOVER_PAD * 2.0)
    }

    /// The Workspaces page as an element tree: the heading, then the list
    /// that scrolls under it, a row per workspace in the order the page
    /// lists them.
    pub(crate) fn workspaces_page(&self, scroll: f32) -> El {
        let rows = self.rows_for(PopoverKind::Summary);
        let mut list = Vec::new();
        let mut previous_pin = false;
        for row in rows {
            if let PopoverRow::Workspace(i) = row {
                if let Some(w) = self.workspaces.get(i) {
                    if previous_pin && !w.pinned {
                        list.push(super::workspace_menu_elements::rule());
                    }
                    list.push(self.workspace_row(
                        w.id,
                        &w.name,
                        w.icon.clone(),
                        w.color.clone(),
                        self.current == Some(w.id),
                    ));
                    previous_pin = w.pinned;
                }
            }
        }
        // An empty Profile (root-catalog §3.10) says so where its rows would
        // be; *New workspace* in the foot is the one way out of it.
        if list.is_empty() {
            list.push(self.workspaces_empty_line());
        }
        let mut page = col("Workspaces page")
            .id(PAGE)
            .semantics(Role::Menu)
            .name(koru_i18n::Key::Workspaces)
            .pad_y(alias::PANEL_PAD)
            .child(self.workspace_menu_header())
            .child(super::workspace_menu_elements::rule());
        if let Some(group) = self.unassigned_group() {
            page = page.child(group);
        }
        page.child(self.workspace_menu_heading())
            .child(
                scroll_y(LIST)
                    .fill()
                    .min_h(metrics::ROW_MIN)
                    .offset(scroll)
                    .children(list),
            )
            .child(self.workspace_menu_footer())
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
        let pin = pin(id);
        let (pinned, pending, reason) = self.workspace_pin_state(id);
        let lang = crate::format::lang();
        let pin_name = koru_i18n::fill(
            koru_i18n::t(
                lang,
                if pinned {
                    koru_i18n::Key::UnpinNamedFromRail
                } else {
                    koru_i18n::Key::PinNamedToRail
                },
            ),
            &[name],
        );
        // **The organization reading, where a row already shows state**
        // (root-catalog Plan 2.2): a word after the name while the Workspace
        // is pending, paused, blocked or held, and the sentence behind it in
        // the row's accessible name. Up to date says nothing extra.
        let organization = self.workspace_organization_status(id);
        let row_name = match organization {
            Some(status) => {
                let word = koru_i18n::t(lang, status.key());
                match status.hint() {
                    Some(hint) => format!("{name}, {word}. {}", koru_i18n::t(lang, hint)),
                    None => format!("{name}, {word}"),
                }
            }
            None => name.to_string(),
        };
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
                    .activity([primary, pin, gear])
                    .child(
                        row("Workspace primary action")
                            .fill()
                            .gap(Space::S2)
                            .id(primary)
                            .focusable()
                            .action(Action::Click)
                            .action(Action::Focus)
                            .semantics(Role::MenuItem)
                            .name_text(row_name)
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
                            )
                            .children(organization.map(|status| {
                                text(koru_i18n::t(lang, status.key()))
                                    .named(ORGANIZATION)
                                    .role(roles::ROW)
                                    .ink(Ink::Hint)
                                    .truncate()
                                    .ink_centred()
                            })),
                    )
                    .child(
                        leaf(GEAR, TrailingAdapter { current, key: gear })
                            .semantic()
                            .w(metrics::TRAILING_LANE_W)
                            .id(gear)
                            .activity([primary, pin])
                            .hit()
                            .focusable()
                            .action(Action::Click)
                            .action(Action::Focus)
                            .semantics(Role::MenuItem)
                            .name(koru_i18n::Key::ManageWorkspace),
                    )
                    .child(
                        leaf(
                            "Workspace pin",
                            PinLane {
                                pinned,
                                pending,
                                disabled: reason.is_some(),
                            },
                        )
                        .semantic()
                        .w(metrics::TRAILING_LANE_W)
                        .id(pin)
                        .activity([primary, gear])
                        .hit()
                        .focusable()
                        .action(Action::Click)
                        .action(Action::Focus)
                        .semantics(Role::MenuItem)
                        .name_text(pin_name)
                        .disabled(reason.is_some()),
                    ),
            )
    }

    /// The key the lit row lights — its primary, or its gear while the
    /// pointer or the keyboard is on the gear — for the row at `hover`.
    pub(crate) fn lit_key(&self, hover: Option<usize>) -> Option<Key> {
        let rows = self.popover_rows();
        let on_gear = self.popover_part == Some(PopoverPart::Gear);
        match hover.and_then(|i| rows.get(i).copied())? {
            PopoverRow::Workspace(i) => self.workspaces.get(i).map(|w| {
                if on_gear {
                    manage(w.id)
                } else if self.popover_part == Some(PopoverPart::Pin) {
                    pin(w.id)
                } else {
                    primary(w.id)
                }
            }),
            PopoverRow::Profile(i) => self.profiles.get(i).map(|profile| {
                if on_gear {
                    profiles::manage(profile.id)
                } else {
                    profiles::primary(profile.id)
                }
            }),
            PopoverRow::ProfileAction(action) => Some(action.key()),
            PopoverRow::Help(action) => Some(action.key()),
            PopoverRow::ProfilesHeading => Some(profiles::MANAGE_PROFILES),
            PopoverRow::WorkspaceAction(action) => Some(action.key()),
            PopoverRow::WorkspacesHeading => Some(super::workspace_menu_elements::MANAGE),
            _ => None,
        }
    }

    /// The host's pointer and keyboard state, by element key, while the
    /// page takes input. Read by the tests' frames; the app's own paint and
    /// input lay the page out through their own paths.
    #[cfg(test)]
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
    /// page shows or the popover is closing. Test-only, like
    /// [`Self::popover_interaction`].
    #[cfg(test)]
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

    /// A page drawn from its elements: the Workspaces page or the Profiles
    /// page, scrolled by `scroll`. `None` for a popover that is not one.
    pub(crate) fn page_tree(
        &self,
        kind: PopoverKind,
        scroll: f32,
        avatar: Option<&ImageBrush>,
    ) -> Option<El> {
        match kind {
            PopoverKind::Summary => Some(self.workspaces_page(scroll)),
            PopoverKind::Profiles => Some(self.profiles_page(scroll, avatar)),
            PopoverKind::Help => Some(self.help_page(scroll)),
            PopoverKind::LibraryActions => None,
        }
    }

    /// The settled page's frame, open or closing, answering `f`: its
    /// geometry, scroll extent and reveal. `None` while the popover is not an
    /// element page. Input that must not reach a closing popover checks
    /// `live` itself.
    pub(crate) fn with_page<R>(
        &self,
        area: Rect,
        viewport: (f32, f32),
        f: impl FnOnce(&Frame) -> R,
    ) -> Option<R> {
        let popover = self.popover.as_ref()?;
        let panel = self.settled_panel(area, viewport)?;
        let tree = self.page_tree(popover.kind, popover.scroll, None)?;
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

    /// A page laid out for paint in `panel` — where the popover is this
    /// frame, mid-slide or settled — scrolled by `scroll`, with the row at
    /// `hover` lit, on the window's engine.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn page_at(
        &self,
        engine: &mut TextEngine,
        kind: PopoverKind,
        panel: Rect,
        scroll: f32,
        hover: Option<usize>,
        avatar: Option<&ImageBrush>,
    ) -> Option<(El, Frame)> {
        let tree = self.page_tree(kind, scroll, avatar)?;
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
        Some((tree, frame))
    }

    /// The open page's rows in the order `popover_rows` lists them, each
    /// with its border box: the heading, a row per workspace or Profile,
    /// and the Profiles page's foot.
    pub(crate) fn page_rows(&self, frame: &Frame) -> Vec<(PopoverRow, Rect)> {
        let Some(kind) = self.popover.as_ref().map(|p| p.kind) else {
            return Vec::new();
        };
        let mut workspaces = row_rects(frame).into_iter();
        let mut profiles = frame.named(profiles::ROW).map(|node| node.rect);
        let unassigned = self.unassigned_rows();
        self.rows_for(kind)
            .into_iter()
            .filter_map(|row| match row {
                PopoverRow::WorkspaceAction(action) => {
                    frame.node(action.key()).map(|node| (row, node.rect))
                }
                PopoverRow::WorkspaceNotice => frame
                    .node(super::workspace_menu_elements::NOTICE)
                    .map(|node| (row, node.rect)),
                PopoverRow::WorkspacesEmpty => frame
                    .node(super::workspace_menu_elements::EMPTY)
                    .map(|node| (row, node.rect)),
                PopoverRow::Unassigned(i)
                | PopoverRow::UnassignedMove(i)
                | PopoverRow::UnassignedRestore(i) => {
                    let id = crate::manage::workspaces::app_workspace(unassigned.get(i)?.id);
                    let key = match row {
                        PopoverRow::Unassigned(_) => super::workspace_menu_elements::unassigned(id),
                        PopoverRow::UnassignedMove(_) => {
                            super::workspace_menu_elements::unassigned_move(id)
                        }
                        _ => super::workspace_menu_elements::unassigned_restore(id),
                    };
                    frame.node(key).map(|node| (row, node.rect))
                }
                PopoverRow::WorkspacesHeading | PopoverRow::ProfilesHeading => {
                    frame.node(HEADING).map(|n| (row, n.rect))
                }
                PopoverRow::Workspace(i) if self.workspaces.get(i).is_some() => {
                    workspaces.next().map(|r| (row, r))
                }
                PopoverRow::Profile(i) if self.profiles.get(i).is_some() => {
                    profiles.next().map(|r| (row, r))
                }
                PopoverRow::ProfileAction(action) => {
                    frame.node(action.key()).map(|n| (row, n.rect))
                }
                PopoverRow::Help(action) => frame.node(action.key()).map(|n| (row, n.rect)),
                _ => None,
            })
            .collect()
    }

    /// The row and part a point presses, by the frame's targets: a row's
    /// primary or its gear, one of the foot's items; the heading's words,
    /// which press nothing but are still the heading, and the foot between
    /// its items, which is the foot; `None` anywhere else.
    pub(crate) fn page_part_at(
        &self,
        frame: &Frame,
        x: f32,
        y: f32,
    ) -> Option<(usize, PopoverRow, PopoverPart)> {
        let rows = self.popover_rows();
        let heading = match self.popover.as_ref()?.kind {
            PopoverKind::Profiles => PopoverRow::ProfilesHeading,
            _ => PopoverRow::WorkspacesHeading,
        };
        let (row, part) = match frame.hit_at(x, y) {
            Some(key) => row_part_of(self, key)?,
            None if frame.node(HEADING)?.rect.contains(x, y) => (heading, PopoverPart::Row),
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
            pin: frame.rect_of("Workspace pin")?,
        })
    }
}

/// The scroll viewport's rect, content extent and offset, as laid out:
/// either page's list.
pub(crate) fn list_of(frame: &Frame) -> Option<(Rect, f32, f32)> {
    let list = frame
        .named(LIST)
        .next()
        .or_else(|| frame.named(profiles::LIST).next())
        .or_else(|| frame.named(super::help::LIST).next())?;
    let (content, offset) = list.scroll?;
    Some((list.rect, content, offset))
}

/// Each row's border box in list order, beside the key of its primary.
pub(crate) fn row_rects(frame: &Frame) -> Vec<Rect> {
    frame.named(ROW).map(|node| node.rect).collect()
}

/// The developer names the host reads geometry by.
pub(crate) const ROW: &str = "Workspace row";
/// The organization reading's word after a row's name (Plan 2.2), present
/// only while the Workspace is not up to date.
pub(crate) const ORGANIZATION: &str = "Workspace organization status";
pub(crate) const LIST: &str = "Workspace list";
pub(crate) const NAME: &str = "Workspace name";
pub(crate) const TILE: &str = "Workspace tile";
pub(crate) const GEAR: &str = "Manage gear";

/// The popover row and part an element key stands for: what a press or an
/// accessibility request on that key chooses.
pub(crate) fn row_part_of(view: &ExploreView, key: Key) -> Option<(PopoverRow, PopoverPart)> {
    if let Some(action) = super::help::Action::from_key(key) {
        return Some((PopoverRow::Help(action), PopoverPart::Row));
    }
    if key == super::workspace_menu_elements::MANAGE {
        return Some((PopoverRow::WorkspacesHeading, PopoverPart::Gear));
    }
    if let Some(action) = super::workspace_menu_elements::MenuAction::from_key(key) {
        return Some((PopoverRow::WorkspaceAction(action), PopoverPart::Row));
    }
    if key == profiles::MANAGE_PROFILES {
        return Some((PopoverRow::ProfilesHeading, PopoverPart::Gear));
    }
    if let Some(action) = profiles::MenuAction::from_key(key) {
        return Some((PopoverRow::ProfileAction(action), PopoverPart::Row));
    }
    let el::Object::Model(id) = key.object else {
        return None;
    };
    // The Unassigned group's rows and their two actions, keyed by workspace.
    if let Some(i) = view
        .unassigned_rows()
        .iter()
        .position(|row| row.id.as_uuid().as_u128() == id)
    {
        let row = match key.part {
            super::workspace_menu_elements::UNASSIGNED => PopoverRow::Unassigned(i),
            super::workspace_menu_elements::UNASSIGNED_MOVE => PopoverRow::UnassignedMove(i),
            super::workspace_menu_elements::UNASSIGNED_RESTORE => PopoverRow::UnassignedRestore(i),
            _ => return None,
        };
        return Some((row, PopoverPart::Row));
    }
    let part = match key.part {
        MANAGE => PopoverPart::Gear,
        PIN => PopoverPart::Pin,
        PRIMARY => PopoverPart::Row,
        _ => return None,
    };
    let row = if let Some(i) = view
        .workspaces
        .iter()
        .position(|w| w.id.as_uuid().as_u128() == id)
    {
        PopoverRow::Workspace(i)
    } else {
        let i = view
            .profiles
            .iter()
            .position(|p| p.id.as_uuid().as_u128() == id)?;
        PopoverRow::Profile(i)
    };
    Some((row, part))
}

#[cfg(test)]
mod tests;
