// SPDX-License-Identifier: MPL-2.0

//! **What the chat's model picker needed of the layer** (QUESTIONS M1–M3,
//! approved by the user 2026-11-03), on small trees: the selected plate and
//! a recipe's focus ring.

use super::*;
use crate::inspect;
use crate::theme::{metrics, themes};

fn engine() -> TextEngine {
    TextEngine::new()
}

/// Every record the paint of `tree` makes, by name, with its colour.
fn painted(tree: &El, frame: &Frame) -> Vec<(String, String)> {
    let mut engine = engine();
    let mut scene = vello::Scene::new();
    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        crate::scene::rect(0.0, 0.0, 800.0, 600.0),
    );
    frame.paint(tree, &mut scene, Affine::IDENTITY, &mut engine);
    capture
        .finish()
        .records
        .into_iter()
        .flat_map(|record| {
            record
                .properties
                .into_iter()
                .filter(|p| p.name == "color")
                .map(move |p| (record.name.to_string(), p.value))
        })
        .collect()
}

static MARK: StateRecipe = StateRecipe {
    name: "mark",
    base: Look::NONE,
    selected: Look::bg(Surface::Selected),
    hover: Look::bg(Surface::Hover),
    pressed: Look::NONE,
    selected_hover: Look::bg(Surface::Selected),
    selected_pressed: Look::NONE,
    disabled: Look::NONE,
    focus: Some(Ring::Accent(Accent::Base)),
};

const MARK_KEY: Key = Key::surface("test", "mark");

fn mark(selected: bool) -> El {
    boxed("Mark")
        .w(metrics::BUTTON_MIN)
        .h(metrics::BUTTON_MIN)
        .radius(Radius::M)
        .states(&MARK)
        .id(MARK_KEY)
        .hit()
        .selected(selected)
}

/// **`Surface::Selected` is the palette's `row_selected`** in every
/// registered theme and contrast (M3).
#[test]
fn the_selected_surface_is_row_selected_in_every_theme() {
    for theme in themes::registered() {
        for contrast in themes::Contrast::ALL {
            let palette = theme.palette(contrast);
            assert_eq!(Surface::Selected.of(palette), palette.row_selected);
            assert_eq!(Surface::Selected.field(), "row_selected");
        }
    }
}

/// **A recipe's focus ring is decoration** (§3.7, M3): drawn only while
/// the element is focused, in its role's colour, without moving anything;
/// the selected plate stays under the pointer.
#[test]
fn a_focus_ring_is_drawn_only_while_focused_and_moves_nothing() {
    let mut engine = engine();
    let mut cache = TextCache::default();
    let area = Rect::new(0.0, 0.0, 32.0, 32.0);
    let cx = FrameCx::new(&themes::LIGHT_THEME, themes::Contrast::Standard);
    let tree = mark(true);
    let rest = tree.layout(&mut engine, &mut cache, area, cx);
    let focused = tree.layout(
        &mut engine,
        &mut cache,
        area,
        cx.with_interaction(Interaction {
            hot: Some(MARK_KEY),
            pressed: None,
            focused: Some(MARK_KEY),
        }),
    );
    assert_eq!(rest.nodes[0].ring, None);
    assert_eq!(focused.nodes[0].ring, Some(Ring::Accent(Accent::Base)));
    assert_eq!(rest.nodes[0].rect, focused.nodes[0].rect);
    assert_eq!(focused.nodes[0].look.bg, Some(Surface::Selected));
    assert_eq!(focused.nodes[0].rule, "selected+hover");

    let palette = cx.palette();
    let ring = inspect::color(Accent::Base.of(palette));
    let strokes = |records: Vec<(String, String)>| {
        records
            .into_iter()
            .filter(|(name, colour)| name == "stroke" && *colour == ring)
            .count()
    };
    assert_eq!(strokes(painted(&tree, &rest)), 0);
    assert_eq!(strokes(painted(&tree, &focused)), 1);
}
