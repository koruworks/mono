// SPDX-License-Identifier: MPL-2.0

//! **The frame contract** (ELEMENTS.final §3.5–§3.8): identity, targets,
//! state, clipping, accessibility and per-subtree themes, on small trees.
//! The canonical workspace row on the app's real types is in
//! `apps/koru/src/panes/explore/popover_elements/tests.rs`.

use super::*;
use crate::inspect;
use crate::theme::{Contrast, metrics, roles, themes};

struct Fixed(f32, f32);

impl Leaf for Fixed {
    fn measure(&self, _width: Option<f32>) -> (f32, f32) {
        (self.0, self.1)
    }
}

const PLATE: StateRecipe = StateRecipe {
    name: "test plate",
    base: Look::NONE,
    selected: Look {
        bg: Some(Surface::Chip),
        ink: Some(Ink::Primary),
    },
    hover: Look::bg(Surface::Hover),
    pressed: Look::bg(Surface::Raised),
    selected_hover: Look::NONE,
    selected_pressed: Look::NONE,
    disabled: Look {
        bg: None,
        ink: Some(Ink::Hint),
    },
};

fn state(hovered: bool, pressed: bool, selected: bool, disabled: bool) -> ElState {
    ElState {
        hovered,
        pressed,
        focused: false,
        selected,
        disabled,
    }
}

/// §3.7's order: selected, then hover, then pressed over it; disabled
/// suppresses hover and press; focus changes nothing.
#[test]
fn state_rules_resolve_in_the_specified_order() {
    let (look, rule) = PLATE.resolve(state(true, true, false, false));
    assert_eq!((look.bg, rule), (Some(Surface::Raised), "pressed"));
    let (look, rule) = PLATE.resolve(state(true, false, true, false));
    assert_eq!(look.bg, Some(Surface::Hover), "hover over selected's fill");
    assert_eq!(look.ink, Some(Ink::Primary), "selected's ink survives");
    assert_eq!(rule, "hover");
    let (look, rule) = PLATE.resolve(state(true, true, true, true));
    assert_eq!(
        (look.bg, look.ink, rule),
        (None, Some(Ink::Hint), "disabled")
    );
    let focused = ElState {
        focused: true,
        ..ElState::default()
    };
    assert_eq!(PLATE.resolve(focused), (Look::NONE, "base"));
}

fn lay(tree: &El, area: Rect, cx: FrameCx) -> Frame {
    let mut engine = TextEngine::new();
    let mut cache = TextCache::default();
    tree.layout(&mut engine, &mut cache, area, cx)
}

const PRIMARY: &str = "primary";
const MANAGE: &str = "manage";

/// A menu of rows: each a neutral row lit by either of its two targets,
/// holding a primary and a trailing gear.
fn menu(ids: &[u128]) -> El {
    col("Menu")
        .id(Key::surface("menu", "root"))
        .semantics(Role::Menu)
        .name_text("Workspaces")
        .child(
            text("WORKSPACES")
                .named("Heading")
                .id(Key::surface("menu", "heading"))
                .semantics(Role::Heading)
                .name_text("Workspaces")
                .min_h(metrics::ROW_MIN),
        )
        .children(ids.iter().map(|&id| {
            let primary = Key::model(id, PRIMARY);
            let manage = Key::model(id, MANAGE);
            row("Row")
                .min_h(metrics::ROW_MIN)
                .states(&PLATE)
                .activity([primary, manage])
                .child(
                    row("Primary")
                        .fill()
                        .id(primary)
                        .hit()
                        .focusable()
                        .action(Action::Click)
                        .action(Action::Focus)
                        .semantics(Role::MenuItem)
                        .name_text(format!("Workspace {id}"))
                        .child(text(format!("Workspace {id}")).role(roles::ROW).fill()),
                )
                .child(
                    leaf("Gear", Fixed(20.0, 20.0))
                        .semantic()
                        .w(metrics::TRAILING_LANE_W)
                        .id(manage)
                        .hit()
                        .focusable()
                        .action(Action::Click)
                        .action(Action::Focus)
                        .semantics(Role::MenuItem)
                        .name(koru_i18n::Key::Back),
                )
        }))
}

fn area() -> Rect {
    Rect::new(10.0, 20.0, 258.0, 200.0)
}

/// **A key is the model's, not the position's.** Reordered, the same key
/// is the same row's target; deleted, it is gone, and nothing inherits it.
#[test]
fn a_target_survives_reorder_and_is_invalidated_by_deletion() {
    let before = lay(&menu(&[1, 2, 3]), area(), FrameCx::root());
    let after = lay(&menu(&[3, 1, 2]), area(), FrameCx::root());
    let key = Key::model(2, PRIMARY);
    let was = before.node(key).unwrap().rect;
    let now = after.node(key).unwrap().rect;
    assert_ne!(was.y, now.y, "it moved");
    let (x, y) = (now.x + 4.0, now.y + 4.0);
    assert_eq!(
        after.hit_at(x, y),
        Some(key),
        "and its target moved with it"
    );
    let gone = lay(&menu(&[1, 3]), area(), FrameCx::root());
    assert!(!gone.contains(key));
    assert_ne!(gone.hit_at(was.x + 4.0, was.y + 4.0), Some(key));
}

/// The primary and the gear are separate targets in one row; the row and
/// the heading are none.
#[test]
fn the_primary_and_the_gear_are_separate_targets_and_the_heading_is_none() {
    let frame = lay(&menu(&[7]), area(), FrameCx::root());
    let gear = frame.node(Key::model(7, MANAGE)).unwrap().rect;
    let primary = frame.node(Key::model(7, PRIMARY)).unwrap().rect;
    assert_eq!(
        frame.hit_at(gear.x + gear.w * 0.5, gear.y + gear.h * 0.5),
        Some(Key::model(7, MANAGE))
    );
    assert_eq!(
        frame.hit_at(primary.x + 2.0, primary.y + 2.0),
        Some(Key::model(7, PRIMARY))
    );
    let heading = frame.rect_of("Heading").unwrap();
    assert_eq!(frame.hit_at(heading.x + 2.0, heading.y + 2.0), None);
    let keys: Vec<_> = frame.hits().into_iter().map(|h| h.key).collect();
    assert_eq!(keys, [Key::model(7, PRIMARY), Key::model(7, MANAGE)]);
}

/// Where targets overlap, the one painted last is the one pressed.
#[test]
fn the_foremost_target_wins() {
    let under = Key::surface("t", "under");
    let over = Key::surface("t", "over");
    let tree = col("Stack")
        .child(row("Under").id(under).hit().h(Length::Px(40.0)))
        .child(row("Over").id(over).hit().h(Length::Px(40.0)));
    let mut frame = lay(&tree, Rect::new(0.0, 0.0, 100.0, 80.0), FrameCx::root());
    // Make them overlap: the second painted over the first.
    frame.nodes[2].hit = Some(Rect::new(0.0, 0.0, 100.0, 40.0));
    assert_eq!(frame.hit_at(50.0, 20.0), Some(over));
}

/// **A clipped row is pressed only where it shows**, and a row scrolled
/// out of view keeps its rectangle so it can be revealed.
#[test]
fn clipped_targets_stay_inside_the_viewport_and_hidden_rows_are_revealable() {
    let ids: Vec<u128> = (1..=12).collect();
    let tree = scroll_y("List")
        .h(Length::Runtime(104.0, "band"))
        .offset(13.0)
        .children(ids.iter().map(|&id| {
            row("Row")
                .min_h(metrics::ROW_MIN)
                .id(Key::model(id, PRIMARY))
                .hit()
        }));
    let frame = lay(&tree, Rect::new(0.0, 100.0, 258.0, 104.0), FrameCx::root());
    let first = frame.node(Key::model(1, PRIMARY)).unwrap();
    assert_eq!(first.rect.y, 87.0, "half above the viewport");
    assert_eq!(
        first.hit_region().unwrap(),
        Rect::new(0.0, 100.0, 258.0, 13.0)
    );
    assert_eq!(frame.hit_at(10.0, 95.0), None, "not where it is hidden");
    let last = frame.node(Key::model(12, PRIMARY)).unwrap();
    assert!(last.hit_region().unwrap().h <= 0.0, "fully out of view");
    assert_eq!(
        frame.reveal(Key::model(12, PRIMARY)).map(|(_, o)| o),
        Some(12.0 * 26.0 - 104.0),
        "revealing the last row scrolls to its end"
    );
    assert_eq!(
        frame.reveal(Key::model(1, PRIMARY)).map(|(_, o)| o),
        Some(0.0)
    );
    assert_eq!(frame.reveal(Key::model(3, PRIMARY)), None, "already whole");
}

/// **Structural boxes are transparent**: the row has no node, so the
/// primary and the gear are sibling items of the menu, beside the heading.
#[test]
fn the_semantic_tree_skips_neutral_rows() {
    let frame = lay(&menu(&[1, 2]), area(), FrameCx::root());
    let tree = frame.semantics();
    assert_eq!(tree.len(), 1);
    let menu = &tree[0];
    assert_eq!(menu.role, Role::Menu);
    let items: Vec<_> = menu.children.iter().map(|c| (c.role, c.key)).collect();
    assert_eq!(
        items,
        [
            (Role::Heading, Some(Key::surface("menu", "heading"))),
            (Role::MenuItem, Some(Key::model(1, PRIMARY))),
            (Role::MenuItem, Some(Key::model(1, MANAGE))),
            (Role::MenuItem, Some(Key::model(2, PRIMARY))),
            (Role::MenuItem, Some(Key::model(2, MANAGE))),
        ]
    );
    assert_eq!(menu.children[2].name, Some(Name::Key(koru_i18n::Key::Back)));
    assert!(frame.diagnostics.is_empty(), "{:?}", frame.diagnostics);
}

/// The row is lit by its gear as well as its primary, and only the lit
/// row; the gear's own state says which part.
#[test]
fn a_neutral_row_is_lit_by_either_of_its_targets() {
    for lit in [PRIMARY, MANAGE] {
        let cx = FrameCx::root().with_interaction(Interaction {
            hot: Some(Key::model(2, lit)),
            ..Interaction::default()
        });
        let frame = lay(&menu(&[1, 2]), area(), cx);
        let rows: Vec<_> = frame.named("Row").collect();
        assert!(!rows[0].state.hovered && rows[1].state.hovered, "{lit}");
        assert_eq!(rows[1].look.bg, Some(Surface::Hover));
        let gear = frame.node(Key::model(2, MANAGE)).unwrap();
        assert_eq!(gear.state.hovered, lit == MANAGE);
    }
}

/// Composition errors are said by element, never repaired.
#[test]
fn the_final_tree_is_validated() {
    let tree = col("Page")
        .child(row("Twice").id(Key::surface("p", "x")))
        .child(row("Again").id(Key::surface("p", "x")))
        .child(row("Keyless").semantics(Role::Button).name_text("Go"))
        .child(
            row("Nameless")
                .id(Key::surface("p", "n"))
                .semantics(Role::MenuItem),
        )
        .child(leaf("Picture", Fixed(4.0, 4.0)).decorative().hit());
    let frame = lay(&tree, Rect::new(0.0, 0.0, 100.0, 100.0), FrameCx::root());
    let said: Vec<_> = frame
        .diagnostics
        .iter()
        .map(|d| (d.element, d.message))
        .collect();
    assert!(said.contains(&("Again", "a key is used twice in one frame")));
    assert!(said.contains(&("Keyless", "a node, target or action needs a stable key")));
    assert!(said.contains(&("Nameless", "a semantic node needs a name")));
    assert!(
        frame.rect_of("Picture").is_some() && frame.hits().is_empty(),
        "a decorative leaf is never a target"
    );
}

/// **Two themes in one frame.** A subtree that carries its own theme
/// resolves and paints in it, beside one that does not; each element's
/// record names the theme, appearance and contrast it painted under.
#[test]
fn two_differently_themed_subtrees_paint_in_their_own_themes() {
    let tree = row("Split")
        .child(
            col("Light half")
                .fill()
                .bg(Surface::Raised)
                .child(text("Light").named("Light words").role(roles::ROW)),
        )
        .child(
            col("Fixture half")
                .fill()
                .theme(&themes::FIXTURE_THEME)
                .bg(Surface::Raised)
                .child(text("Fixture").named("Fixture words").role(roles::ROW)),
        );
    let light = FrameCx::new(&themes::LIGHT_THEME, Contrast::Standard);
    let mut engine = TextEngine::new();
    let mut cache = TextCache::default();
    let frame = tree.layout(
        &mut engine,
        &mut cache,
        Rect::new(0.0, 0.0, 400.0, 40.0),
        light,
    );
    let mut scene = vello::Scene::new();
    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        crate::scene::rect(0.0, 0.0, 400.0, 40.0),
    );
    frame.paint(&tree, &mut scene, Affine::IDENTITY, &mut engine);
    let records = capture.finish().records;
    let prop = |name: &str, key: &str| {
        let i = records.iter().position(|r| r.name == name).expect(name);
        records[i]
            .properties
            .iter()
            .find(|p| p.name == key)
            .map(|p| p.value.clone())
    };
    assert_eq!(prop("Light half", "theme").as_deref(), Some("light"));
    assert_eq!(prop("Fixture half", "theme").as_deref(), Some("fixture"));
    assert_eq!(prop("Fixture words", "theme").as_deref(), Some("fixture"));
    assert_eq!(
        prop("Fixture words", "appearance").as_deref(),
        Some("light")
    );
    assert_eq!(
        prop("Fixture words", "contrast").as_deref(),
        Some("standard")
    );
    let fills: Vec<String> = records
        .iter()
        .filter(|r| r.name == "fill")
        .flat_map(|r| r.properties.iter().filter(|p| p.name == "color"))
        .map(|p| p.value.clone())
        .collect();
    assert_eq!(
        fills,
        [
            inspect::color(Surface::Raised.of(themes::LIGHT_THEME.standard)),
            inspect::color(Surface::Raised.of(themes::FIXTURE_THEME.standard)),
        ]
    );
    let inks: Vec<String> = records
        .iter()
        .flat_map(|r| r.properties.iter().filter(|p| p.name == "foreground"))
        .map(|p| p.value.clone())
        .collect();
    assert_eq!(
        inks,
        [
            inspect::color(Ink::Primary.of(themes::LIGHT_THEME.standard)),
            inspect::color(Ink::Primary.of(themes::FIXTURE_THEME.standard)),
        ]
    );
}
