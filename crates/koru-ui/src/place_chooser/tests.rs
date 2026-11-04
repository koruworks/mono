// SPDX-License-Identifier: MPL-2.0

//! The cross on its own: its geometry, its keys and pointer, what a screen
//! reader hears and what the inspector sees — with nothing to open.

use super::*;

const VIEWPORT: (f32, f32) = (1200.0, 700.0);

#[test]
fn every_target_has_a_name() {
    for place in Place::ALL {
        assert!(!place.label(Lang::En).is_empty(), "{place:?}");
    }
    assert!(!t(Lang::En, Key::PlaceChooser).is_empty());
}

/// The arrows move across the cross it draws: out from the middle, back to
/// it from the arm opposite, round to a neighbour perpendicular — and never
/// off the edge.
#[test]
fn the_arrows_walk_the_cross() {
    use NamedKey::{ArrowDown as D, ArrowLeft as L, ArrowRight as R, ArrowUp as U};
    for (from, key, to) in [
        (Place::Middle, U, Place::Up),
        (Place::Middle, R, Place::Right),
        (Place::Middle, D, Place::Down),
        (Place::Middle, L, Place::Left),
        (Place::Up, D, Place::Middle),
        (Place::Up, U, Place::Up),
        (Place::Up, L, Place::Left),
        (Place::Up, R, Place::Right),
        (Place::Right, L, Place::Middle),
        (Place::Right, R, Place::Right),
        (Place::Right, D, Place::Down),
        (Place::Down, U, Place::Middle),
        (Place::Left, R, Place::Middle),
        (Place::Left, U, Place::Up),
    ] {
        assert_eq!(from.step(key), to, "{from:?} {key:?}");
    }
    assert_eq!(
        Place::Left.step(NamedKey::Home),
        Place::Left,
        "not an arrow"
    );
}

/// **Never clipped at the window's edge.** Opened in a corner the whole
/// cross stays inside the window; in the open, the middle is under the
/// press.
#[test]
fn the_chooser_stays_inside_the_window() {
    let inset = theme::overlay::popover().viewport_inset;
    for at in [(0.0, 0.0), (1200.0, 700.0), (1199.0, 3.0), (2.0, 698.0)] {
        let chooser = PlaceChooser::open(at);
        let panel = chooser.panel(VIEWPORT);
        assert!(panel.x >= inset && panel.y >= inset, "{at:?}: {panel:?}");
        assert!(
            panel.x + panel.w <= VIEWPORT.0 - inset && panel.y + panel.h <= VIEWPORT.1 - inset,
            "{at:?}: {panel:?}"
        );
        for place in Place::ALL {
            let cell = chooser.cell_rect(VIEWPORT, place);
            assert!(
                cell.x >= panel.x
                    && cell.x + cell.w <= panel.x + panel.w
                    && cell.y >= panel.y
                    && cell.y + cell.h <= panel.y + panel.h,
                "{place:?} outside the panel"
            );
        }
    }
    let chooser = PlaceChooser::open((600.0, 350.0));
    assert_eq!(chooser.at(), (600.0, 350.0));
    assert_eq!(chooser.highlight(), Place::Middle, "opens on the middle");
    assert_eq!(
        chooser.place_at(VIEWPORT, 600.0, 350.0),
        Some(Place::Middle),
        "a second press where the first was chooses the middle"
    );
}

/// The arms are where they say: above, left, right and below the middle,
/// and no two overlap.
#[test]
fn the_arms_lie_around_the_middle() {
    let chooser = PlaceChooser::open((600.0, 350.0));
    let cell = |place| chooser.cell_rect(VIEWPORT, place);
    let middle = cell(Place::Middle);
    assert!(cell(Place::Up).y + cell(Place::Up).h <= middle.y);
    assert!(cell(Place::Down).y >= middle.y + middle.h);
    assert!(cell(Place::Left).x + cell(Place::Left).w <= middle.x);
    assert!(cell(Place::Right).x >= middle.x + middle.w);
    for place in Place::ALL {
        let (x, y) = cell(place).centre();
        assert_eq!(chooser.place_at(VIEWPORT, x, y), Some(place));
    }
    let panel = chooser.panel(VIEWPORT);
    assert_eq!(
        chooser.place_at(VIEWPORT, panel.x + 1.0, panel.y + 1.0),
        None,
        "a corner of the panel is no target"
    );
}

#[test]
fn keys_move_choose_dismiss_and_swallow_the_rest() {
    let mut chooser = PlaceChooser::open((600.0, 350.0));
    assert_eq!(
        chooser.key(&KeyPress::Named(NamedKey::ArrowLeft)),
        Outcome::Moved(Place::Left)
    );
    assert_eq!(chooser.highlight(), Place::Left);
    assert_eq!(
        chooser.key(&KeyPress::Character("x".into())),
        Outcome::Handled,
        "a key it does not use is still swallowed"
    );
    assert_eq!(chooser.highlight(), Place::Left);
    assert_eq!(
        chooser.key(&KeyPress::Named(NamedKey::Enter)),
        Outcome::Chose(Place::Left)
    );
    assert_eq!(
        chooser.key(&KeyPress::Named(NamedKey::Space)),
        Outcome::Chose(Place::Left)
    );
    assert_eq!(
        chooser.key(&KeyPress::Character(" ".into())),
        Outcome::Chose(Place::Left)
    );
    assert_eq!(
        chooser.key(&KeyPress::Named(NamedKey::Escape)),
        Outcome::Dismissed
    );
}

#[test]
fn a_press_on_a_target_chooses_it_and_anywhere_else_dismisses() {
    let chooser = PlaceChooser::open((400.0, 300.0));
    let up = chooser.cell_rect(VIEWPORT, Place::Up);
    assert_eq!(
        chooser.mouse_down(VIEWPORT, up.x + up.w * 0.5, up.y + up.h * 0.5),
        Outcome::Chose(Place::Up)
    );
    let panel = chooser.panel(VIEWPORT);
    assert_eq!(
        chooser.mouse_down(VIEWPORT, panel.x + panel.w + 40.0, panel.y),
        Outcome::Dismissed
    );
}

/// Hover is the highlight; off the targets it stays, so Enter always has
/// somewhere to go — and the pointer is pressable over a target, never the
/// hand (ADR-215 is the cursor table's).
#[test]
fn hovering_a_target_highlights_it_and_the_highlight_stays_off_them() {
    let mut chooser = PlaceChooser::open((400.0, 300.0));
    let down = chooser.cell_rect(VIEWPORT, Place::Down);
    assert!(chooser.mouse_move(VIEWPORT, down.x + 2.0, down.y + 2.0));
    assert_eq!(chooser.highlight(), Place::Down);
    assert!(
        !chooser.mouse_move(VIEWPORT, down.x + 3.0, down.y + 3.0),
        "the same target is no change"
    );
    assert!(!chooser.mouse_move(VIEWPORT, 5.0, 5.0));
    assert_eq!(chooser.highlight(), Place::Down, "kept off the targets");
    assert_eq!(
        chooser.cursor(VIEWPORT, down.x + 2.0, down.y + 2.0),
        crate::cursor::PRESS
    );
    assert_ne!(
        chooser.cursor(VIEWPORT, down.x + 2.0, down.y + 2.0),
        CursorIcon::Pointer
    );
    assert_eq!(chooser.cursor(VIEWPORT, 5.0, 5.0), crate::cursor::IDLE);
    let tip = chooser.tooltip(VIEWPORT, Place::Down, Lang::En);
    assert_eq!(tip.anchor, chooser.panel(VIEWPORT), "under the whole cross");
    assert_eq!(tip.label, Place::Down.label(Lang::En));
}

/// A menu of five, in reading order, each placed where it is drawn, with
/// ids from the opening's own block — and a second opening's block is
/// another one, so an id from the first names nothing in it.
#[test]
fn the_semantics_are_a_menu_of_five_with_ids_of_its_own() {
    let mut chooser = PlaceChooser::open((400.0, 300.0));
    chooser.set_highlight(Place::Right);
    let semantics = chooser.semantics(VIEWPORT, Lang::En);
    assert_eq!(semantics.name, t(Lang::En, Key::PlaceChooser));
    assert_eq!(semantics.bounds, chooser.panel(VIEWPORT));
    assert_eq!(semantics.focus, Place::Right);
    assert_eq!(
        semantics.items.iter().map(|i| i.place).collect::<Vec<_>>(),
        Place::ALL
    );
    for item in &semantics.items {
        assert_eq!(item.rect, chooser.cell_rect(VIEWPORT, item.place));
        assert_eq!(item.label, item.place.label(Lang::En));
        assert_eq!(chooser.place_of_instance(item.instance), Some(item.place));
        assert_ne!(item.instance, chooser.instance());
    }
    assert_eq!(chooser.place_of_instance(chooser.instance()), None);
    let next = PlaceChooser::open((400.0, 300.0));
    for place in Place::ALL {
        assert_eq!(
            next.place_of_instance(chooser.place_instance(place)),
            None,
            "{place:?}: a closed opening's id names nothing in the next"
        );
    }
}

/// The inspector sees one KoruPlaceChooser around a popover surface holding
/// the five icon buttons.
#[test]
fn it_draws_as_one_component_of_five_icon_buttons() {
    let frame = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        vello::kurbo::Rect::new(0.0, 0.0, f64::from(VIEWPORT.0), f64::from(VIEWPORT.1)),
    );
    let mut canvas = Scene::new();
    let mut engine = TextEngine::new();
    PlaceChooser::open((400.0, 300.0)).draw(&mut canvas, Affine::IDENTITY, &mut engine, VIEWPORT);
    let snapshot = frame.finish();
    let count = |name: &str| snapshot.records.iter().filter(|r| r.name == name).count();
    assert_eq!(count("KoruPlaceChooser"), 1);
    assert_eq!(count("Popover surface"), 1);
    assert_eq!(count("KoruIconButton"), Place::ALL.len());
}
