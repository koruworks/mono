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

/// The element record named `name` in the paint of `tree`, as its
/// properties.
fn record_of(tree: &El, frame: &Frame, name: &str) -> Vec<(String, String)> {
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
        .find(|record| record.name == name)
        .map(|record| {
            record
                .properties
                .into_iter()
                .map(|p| (p.name.to_string(), p.value))
                .collect()
        })
        .unwrap_or_default()
}

/// **A box can sit on the window's ground, rounded at its start only**
/// (M2): the fill is the theme's ground — the person's base colour when
/// they chose one, which `theme/doc_size_tests.rs` holds — and the
/// inspector names the two corners it rounds.
#[test]
fn a_rail_sits_on_the_ground_rounded_at_its_start() {
    let mut engine = engine();
    let mut cache = TextCache::default();
    let tree = col("Rail")
        .bg(Surface::Ground)
        .radius_start(Radius::L)
        .child(boxed("Mark").h(metrics::BUTTON_MIN));
    for theme in themes::registered() {
        let cx = FrameCx::new(theme, themes::Contrast::Standard);
        let frame = tree.layout(
            &mut engine,
            &mut cache,
            Rect::new(0.0, 0.0, 44.0, 300.0),
            cx,
        );
        assert_eq!(frame.nodes[0].look.bg, Some(Surface::Ground));
        let props = record_of(&tree, &frame, "Rail");
        let value = |name: &str| {
            props
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            value("fill colour"),
            Some(format!(
                "Surface::Ground -> {}",
                inspect::color(cx.surface(Surface::Ground))
            ))
        );
        assert_eq!(value("radius top-start").as_deref(), Some("Radius::L -> 8"));
        assert_eq!(
            value("radius bottom-start").as_deref(),
            Some("Radius::L -> 8")
        );
        assert_eq!(value("radius top-end"), None);
        assert_eq!(value("radius"), None, "the corners differ");
    }
    let uniform = boxed("Plate").radius(Radius::M).bg(Surface::Hover);
    let cx = FrameCx::root();
    let frame = uniform.layout(&mut engine, &mut cache, Rect::new(0.0, 0.0, 30.0, 30.0), cx);
    let props = record_of(&uniform, &frame, "Plate");
    assert!(
        props
            .iter()
            .any(|(n, v)| n == "radius" && v == "Radius::M -> 6")
    );
}

const LIST_KEY: Key = Key::surface("test", "list");
const STEP: Metric = Metric::new("TEST_ROW_H", 30.0);

fn row_key(index: usize) -> Key {
    Key::model(index as u128 + 1, "row")
}

/// `count` rows of 30 in a list that scrolls a row at a time, with the
/// shared scrollbar.
fn list(count: usize, offset: f32) -> El {
    scroll_y("List")
        .id(LIST_KEY)
        .fill()
        .row_step(STEP)
        .scrollbar()
        .offset(offset)
        .children((0..count).map(|index| {
            row("Row")
                .h(STEP)
                .id(row_key(index))
                .hit()
                .child(text(format!("Row {index}")).fill().truncate())
        }))
}

fn laid(tree: &El, area: Rect, interaction: Interaction) -> Frame {
    let mut engine = engine();
    let mut cache = TextCache::default();
    tree.layout(
        &mut engine,
        &mut cache,
        area,
        FrameCx::new(&themes::LIGHT_THEME, themes::Contrast::Standard)
            .with_interaction(interaction),
    )
}

/// **A row step shows whole rows and scrolls by them** (M1): a 200-unit
/// viewport of 30-unit rows shows six, from its top; the offset is a
/// whole number of rows, the last row reachable at the foot; a reveal
/// and a thumb drag land on whole rows too.
#[test]
fn a_row_step_shows_whole_rows_and_scrolls_by_them() {
    let area = Rect::new(10.0, 20.0, 240.0, 200.0);
    let frame = laid(&list(10, 40.0), area, Interaction::default());
    let node = frame.node(LIST_KEY).unwrap();
    assert_eq!(node.viewport, Some(Rect::new(10.0, 20.0, 240.0, 180.0)));
    assert_eq!(node.scroll, Some((300.0, 30.0)), "40 is a row and a third");
    let first = frame.node(row_key(1)).unwrap();
    assert_eq!(first.rect.y, 20.0, "row 1 at the top");
    // Past the end: the last six rows.
    let frame = laid(&list(10, 500.0), area, Interaction::default());
    assert_eq!(frame.node(LIST_KEY).unwrap().scroll, Some((300.0, 120.0)));
    // A row below the viewport is revealed a whole row at a time.
    let frame = laid(&list(10, 0.0), area, Interaction::default());
    assert_eq!(frame.reveal(row_key(7)).map(|(_, at)| at), Some(60.0));
    assert_eq!(frame.reveal(row_key(2)), None, "already whole");
    // Rows outside the whole-row viewport are not targets.
    assert_eq!(frame.hit_at(30.0, 20.0 + 6.0 * 30.0 + 5.0), None);
    // A drag lands on whole rows.
    let bar = frame.scrollbar(LIST_KEY).unwrap();
    let travel = bar.track_bottom - bar.track_top - bar.thumb_h;
    let offset = frame
        .drag_offset(LIST_KEY, bar.track_top + travel * 0.55, 0.0)
        .unwrap();
    assert_eq!(offset % 30.0, 0.0);
    assert!((0.0..=120.0).contains(&offset));
}

/// **The scrollbar is the shared one** (M1): `koru_draw::scrollbar`'s
/// geometry over the rows shown, absent while everything fits; its gutter
/// presses the list over the rows under it; its thumb lit while the
/// pointer is in the gutter or it is dragged.
#[test]
fn the_scrollbar_is_the_shared_bar_and_its_gutter_wins_over_the_rows() {
    let area = Rect::new(0.0, 0.0, 240.0, 200.0);
    let fits = laid(&list(4, 0.0), area, Interaction::default());
    assert_eq!(fits.scrollbar(LIST_KEY), None);

    let frame = laid(&list(10, 0.0), area, Interaction::default());
    let bar = frame.scrollbar(LIST_KEY).unwrap();
    assert_eq!(
        Some(bar),
        crate::scrollbar::ScrollbarGeom::for_content(Rect::new(0.0, 0.0, 240.0, 180.0), 300.0, 0.0)
    );
    let gutter = bar.gutter();
    let y = 45.0;
    assert_eq!(frame.hit_at(gutter.x + 1.0, y), Some(LIST_KEY));
    assert_eq!(frame.hit_at(gutter.x - 20.0, y), Some(row_key(1)));

    let thumb = |interaction: Interaction| {
        let tree = list(10, 0.0);
        let frame = laid(&tree, area, interaction);
        painted(&tree, &frame)
            .into_iter()
            .filter(|(name, _)| name == "fill")
            .map(|(_, colour)| colour)
            .last()
            .unwrap()
    };
    let palette = themes::LIGHT_THEME.standard;
    let rest = inspect::color(crate::theme::scrollbar_thumb_color_in(palette, false));
    let lit = inspect::color(crate::theme::scrollbar_thumb_color_in(palette, true));
    assert_eq!(thumb(Interaction::default()), rest);
    let hot = Interaction {
        hot: Some(LIST_KEY),
        ..Interaction::default()
    };
    assert_eq!(thumb(hot), lit, "the pointer in its gutter");
    let dragged = Interaction {
        pressed: Some(LIST_KEY),
        ..Interaction::default()
    };
    assert_eq!(thumb(dragged), lit, "dragged");
}

/// A scrollbar without a key cannot be pressed, and says so.
#[test]
fn a_scrollbar_needs_its_viewports_key() {
    let tree = scroll_y("List")
        .fill()
        .scrollbar()
        .children((0..10).map(|_| boxed("Row").h(STEP)));
    let frame = laid(
        &tree,
        Rect::new(0.0, 0.0, 100.0, 100.0),
        Interaction::default(),
    );
    assert!(
        frame
            .diagnostics
            .iter()
            .any(|d| d.element == "List" && d.message.contains("scrollbar"))
    );
}

/// **An icon has a filled form** (M1): the inspector names it.
#[test]
fn an_icon_can_be_filled() {
    let tree = row("Mark").child(
        icon(crate::icons::STAR, IconSize::L)
            .decorative()
            .filled()
            .named("Star"),
    );
    let frame = laid(
        &tree,
        Rect::new(0.0, 0.0, 40.0, 40.0),
        Interaction::default(),
    );
    let props = record_of(&tree, &frame, "Star");
    assert!(props.iter().any(|(n, v)| n == "icon form" && v == "filled"));
}
