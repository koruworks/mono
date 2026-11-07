// SPDX-License-Identifier: MPL-2.0

//! The rail's element tree. Its frame supplies paint, targets and keyboard
//! order; drag animation changes only the painted allocations.

use super::apps_rail::{Hit, Rects};
use koru_draw::el::{
    Action, El, ElState, Frame, FrameCx, Interaction, Key, Leaf, Look, Ring, Role, StateRecipe,
    TextCache, boxed, col, divider, leaf, spacer,
};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme::{Ink, Line, Radius, Space, Surface, metrics};
use vello::{Scene, kurbo::Affine};

pub(crate) const SURFACE: &str = "rail";

pub(crate) fn key(hit: Hit) -> Key {
    match hit {
        Hit::Workspace(id) => Key::model(id.as_uuid().as_u128(), "rail"),
        Hit::WaitingWorkspace => Key::surface(SURFACE, "waiting-workspace"),
        Hit::App(mode) => Key::surface(SURFACE, mode.token()),
        Hit::Overflow => Key::surface(SURFACE, "overflow"),
        Hit::Settings => Key::surface(SURFACE, "settings"),
        Hit::Help => Key::surface(SURFACE, "help"),
        Hit::Profile => Key::surface(SURFACE, "profile"),
    }
}

pub(crate) static RAIL_SQUARE_STATES: StateRecipe = StateRecipe {
    name: "rail square",
    base: Look::NONE,
    selected: Look::bg(Surface::Selected),
    hover: Look::bg(Surface::Hover),
    pressed: Look::bg(Surface::Hover),
    selected_hover: Look::bg(Surface::Selected),
    selected_pressed: Look::bg(Surface::Selected),
    disabled: Look {
        bg: None,
        ink: Some(Ink::Hint),
    },
    focus: Some(Ring::Line(Line::Focus)),
};

#[derive(Clone)]
pub(crate) enum Contents {
    Tile {
        name: String,
        icon: Option<String>,
        color: Option<String>,
    },
    Glyph(char),
}

#[derive(Clone)]
pub(crate) struct Item {
    pub hit: Hit,
    pub name: String,
    pub contents: Contents,
    pub selected: bool,
    pub dimmed: bool,
}

/// Dimming is presentation only: a waiting tile still opens its recovery menu.
const WAITING_TILE_VEIL: f32 = 0.5;

struct SquareContents {
    contents: Contents,
    dimmed: bool,
}

impl Leaf for SquareContents {
    fn measure(&self, _: Option<f32>) -> (f32, f32) {
        (metrics::RAIL_SQUARE.value, metrics::RAIL_SQUARE.value)
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
        match &self.contents {
            Contents::Tile { name, icon, color } => {
                let side = metrics::WORKSPACE_TILE.value;
                let tile = Rect::new(
                    at.x + (at.w - side) * 0.5,
                    at.y + (at.h - side) * 0.5,
                    side,
                    side,
                );
                koru_draw::tile::Tile {
                    name,
                    icon: icon.as_deref(),
                    color: color.as_deref(),
                }
                .draw_in(scene, root, engine, tile, cx.palette());
                if self.dimmed {
                    koru_draw::scene::fill(
                        scene,
                        root,
                        Surface::Ground
                            .resolve(&cx.theme, cx.contrast)
                            .multiply_alpha(WAITING_TILE_VEIL),
                        &koru_draw::scene::rect(tile.x, tile.y, tile.w, tile.h),
                    );
                }
            }
            Contents::Glyph(glyph) => {
                let shaped = if state.selected {
                    engine.icon_layout_filled(*glyph, metrics::RAIL_GLYPH.value)
                } else {
                    engine.icon_layout(*glyph, metrics::RAIL_GLYPH.value)
                };
                let ink = if state.disabled {
                    Ink::Hint
                } else if state.selected {
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
    }
}

fn square(item: &Item) -> El {
    boxed("Rail square")
        .id(key(item.hit))
        .w(metrics::RAIL_SQUARE)
        .h(metrics::RAIL_SQUARE)
        .hit()
        .focusable()
        .action(Action::Click)
        .semantics(Role::Button)
        .name_text(&item.name)
        .selected(item.selected)
        .radius(Radius::L)
        .states(&RAIL_SQUARE_STATES)
        .child(
            leaf(
                "Rail contents",
                SquareContents {
                    contents: item.contents.clone(),
                    dimmed: item.dimmed,
                },
            )
            .decorative()
            .fill()
            .activity([key(item.hit)])
            .selected(item.selected),
        )
}

pub(crate) struct Rail {
    pub workspaces: Vec<Item>,
    pub views: Vec<Item>,
    pub foot: Vec<Item>,
}

impl Rail {
    /// Capacity from the same tokens as the element tree, reserving the
    /// workspace run, divider and every fixed foot control.
    pub fn view_capacity(&self, height: f32) -> usize {
        let gap = Space::S1.px(koru_draw::theme::Profile::Desktop);
        let square = metrics::RAIL_SQUARE.value;
        let run = |count: usize| count as f32 * (square + gap) - gap;
        let workspace = if self.workspaces.is_empty() {
            0.0
        } else {
            run(self.workspaces.len()) + 2.0 * gap + 1.0 + 2.0 * gap
        };
        let fixed = Space::S2_5.px(koru_draw::theme::Profile::Desktop)
            + Space::S1_5.px(koru_draw::theme::Profile::Desktop)
            + workspace
            + run(self.foot.len()).max(0.0)
            + 2.0 * gap;
        ((height - fixed + gap) / (square + gap)).floor().max(2.0) as usize
    }

    pub fn tree(&self) -> El {
        let mut tree = col("Rail")
            .items_center()
            .pad_top(Space::S2_5)
            .pad_bottom(Space::S1_5)
            .gap(Space::S1);
        if !self.workspaces.is_empty() {
            tree = tree
                .child(
                    col("Workspaces")
                        .id(Key::surface(SURFACE, "workspaces"))
                        .semantics(Role::Group)
                        .name(koru_i18n::Key::Workspaces)
                        .gap(Space::S1)
                        .children(self.workspaces.iter().map(square)),
                )
                .child(
                    col("Rail divider")
                        .w(metrics::RAIL_RULE_W)
                        .pad_y(Space::S1)
                        .child(divider(Line::Divider)),
                );
        }
        tree.child(
            col("Views")
                .id(Key::surface(SURFACE, "views"))
                .semantics(Role::Group)
                .name(koru_i18n::Key::A11yNavigatorViews)
                .gap(Space::S1)
                .children(self.views.iter().map(square)),
        )
        .child(spacer())
        .child(
            col("Rail foot")
                .gap(Space::S1)
                .children(self.foot.iter().map(square)),
        )
    }

    pub fn layout(
        &self,
        engine: &mut TextEngine,
        cache: &mut TextCache,
        at: Rect,
        interaction: Interaction,
    ) -> Layout {
        let tree = self.tree();
        let frame = tree.layout(
            engine,
            cache,
            at,
            FrameCx::root().with_interaction(interaction),
        );
        Layout { tree, frame }
    }

    pub fn hit(&self, frame: &Frame, x: f32, y: f32) -> Option<Hit> {
        let target = frame.hit_at(x, y)?;
        self.items()
            .find(|item| key(item.hit) == target)
            .map(|item| item.hit)
    }

    pub fn controls(&self, frame: &Frame) -> Vec<Hit> {
        frame
            .nodes
            .iter()
            .filter(|node| node.focusable)
            .filter_map(|node| {
                self.items()
                    .find(|item| Some(key(item.hit)) == node.key)
                    .map(|item| item.hit)
            })
            .collect()
    }

    fn items(&self) -> impl Iterator<Item = &Item> {
        self.workspaces.iter().chain(&self.views).chain(&self.foot)
    }

    pub fn rectangles(&self, frame: &Frame, band: Rect) -> Rects {
        let mut rects = Rects {
            band,
            apps: Vec::new(),
            overflow: Rect::new(band.x, band.y, 0.0, 0.0),
            workspaces: Vec::new(),
            settings: Rect::new(band.x, band.y, 0.0, 0.0),
            help: Rect::new(band.x, band.y, 0.0, 0.0),
            profile: Rect::new(band.x, band.y, 0.0, 0.0),
        };
        for item in self.items() {
            let Some(node) = frame.node(key(item.hit)) else {
                continue;
            };
            let r = if band.w > 0.0 {
                node.rect
            } else {
                Rect::new(band.x, band.y, 0.0, 0.0)
            };
            match item.hit {
                Hit::Workspace(id) => rects.workspaces.push((Some(id), r)),
                Hit::WaitingWorkspace => rects.workspaces.push((None, r)),
                Hit::App(mode) => rects.apps.push((mode, r)),
                Hit::Overflow => rects.overflow = r,
                Hit::Settings => rects.settings = r,
                Hit::Help => rects.help = r,
                Hit::Profile => rects.profile = r,
            }
        }
        rects
    }
}

pub(crate) struct Layout {
    pub tree: El,
    pub frame: Frame,
}

#[cfg(test)]
mod tests;

impl Layout {
    pub fn paint(
        mut self,
        rail: &Rail,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        offsets: &[(Key, f32)],
        carried: Option<(Key, Rect)>,
    ) {
        for (key, offset) in offsets {
            let Some(index) = self
                .frame
                .nodes
                .iter()
                .position(|node| node.key == Some(*key))
            else {
                continue;
            };
            let mut descendants = vec![index];
            for i in index + 1..self.frame.nodes.len() {
                if self.frame.nodes[i]
                    .parent
                    .is_some_and(|parent| descendants.contains(&parent))
                {
                    descendants.push(i);
                }
            }
            for i in descendants {
                self.frame.nodes[i].rect.y += offset;
            }
        }
        if let Some((key, _)) = carried {
            if let Some(node) = self
                .frame
                .nodes
                .iter_mut()
                .find(|node| node.key == Some(key))
            {
                node.clip = Some(Rect::new(node.rect.x, node.rect.y, 0.0, 0.0));
            }
        }
        self.frame.paint(&self.tree, scene, root, engine);
        if let Some((key, at)) = carried {
            if let Some(item) = rail.items().find(|item| key == self::key(item.hit)) {
                let tree = square(item);
                let frame = tree.layout(engine, &mut TextCache::default(), at, self.frame.cx);
                frame.paint(&tree, scene, root, engine);
            }
        }
    }
}
