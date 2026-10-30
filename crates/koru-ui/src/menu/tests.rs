// SPDX-License-Identifier: MPL-2.0

//! Geometry, over a row type that is a label and nothing else.

use super::*;

enum Row {
    Item(&'static str),
    Rule,
    Toggle(&'static str, bool),
    /// A label and a `−  value  +` band, the shape the window menu's zoom row
    /// wears. `floor` dims the `−`, which is what the end of a range looks
    /// like here.
    Stepper {
        label: &'static str,
        value: &'static str,
        floor: bool,
    },
    /// A release card, with or without its button, and whether the button
    /// can be pressed.
    Card {
        title: &'static str,
        button: Option<bool>,
    },
    /// A row that is nothing but `n` buttons — the social strip.
    Strip(usize),
}

impl MenuItem for Row {
    fn label(&self) -> Cow<'_, str> {
        match self {
            Row::Item(label) | Row::Toggle(label, _) | Row::Stepper { label, .. } => {
                (*label).into()
            }
            Row::Card { title, .. } => (*title).into(),
            Row::Rule | Row::Strip(_) => "".into(),
        }
    }

    fn selectable(&self) -> bool {
        !matches!(self, Row::Rule | Row::Stepper { .. } | Row::Strip(_))
    }

    fn card(&self) -> Option<MenuCard> {
        let Row::Card { title, button } = self else {
            return None;
        };
        Some(MenuCard {
            badge: "STABLE · 1.0.1".into(),
            date: "24 Oct".into(),
            title: (*title).into(),
            body: "A line under the title.".into(),
            button: button.map(|on| ("Download".into(), on)),
        })
    }

    fn controls(&self) -> Vec<Control> {
        match self {
            Row::Stepper { value, floor, .. } => vec![
                Control::Button {
                    glyph: '-',
                    enabled: !floor,
                },
                Control::Readout((*value).into()),
                Control::Button {
                    glyph: '+',
                    enabled: true,
                },
            ],
            Row::Card {
                button: Some(on), ..
            } => vec![Control::Button {
                glyph: 'v',
                enabled: *on,
            }],
            Row::Strip(n) => (0..*n)
                .map(|_| Control::Button {
                    glyph: 'o',
                    enabled: true,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn divider(&self) -> bool {
        matches!(self, Row::Rule)
    }

    fn checked(&self) -> Option<bool> {
        match self {
            Row::Toggle(_, set) => Some(*set),
            _ => None,
        }
    }
}

fn menu(items: Vec<Row>) -> Menu<Row> {
    Menu::at_width((100.0, 100.0), items, Anchor::Cursor, 200.0)
}

#[test]
fn the_panel_anchors_at_the_point_and_clamps_to_the_viewport() {
    let m = menu(vec![Row::Item("one"), Row::Item("two")]);
    let p = m.panel((1280.0, 720.0));
    assert_eq!(p.x, 100.0);
    assert_eq!(p.y, 100.0);
    assert_eq!(p.w, 200.0);

    let edge = Menu::<Row>::at_width(
        (1270.0, 700.0),
        vec![Row::Item("one")],
        Anchor::Cursor,
        200.0,
    );
    let p = edge.panel((1280.0, 720.0));
    assert!(p.x + p.w <= 1280.0 - EDGE_MARGIN);
    assert!(p.y + p.h <= 720.0 - EDGE_MARGIN);
}

#[test]
fn a_button_anchored_menu_hangs_back_under_the_button() {
    let m = Menu::at_width(
        (300.0, 40.0),
        vec![Row::Item("one")],
        Anchor::ButtonRight,
        200.0,
    );
    let p = m.panel((1280.0, 720.0));
    assert_eq!(p.x + p.w, 300.0, "the point is the menu's right edge");
}

#[test]
fn item_at_hits_rows_and_misses_dividers_and_padding() {
    let m = menu(vec![Row::Item("one"), Row::Rule, Row::Item("two")]);
    let vp = (1280.0, 720.0);
    let first = m.item_rect(vp, 0);
    let second = m.item_rect(vp, 2);
    assert!(second.y > first.y, "later rows sit lower");
    assert_eq!(
        m.item_at(vp, first.x + 10.0, first.y + 5.0),
        Some(0),
        "the first row"
    );
    let divider = m.item_rect(vp, 1);
    assert_eq!(
        m.item_at(vp, divider.x + 10.0, divider.y + divider.h * 0.5),
        None,
        "a divider is never a hit"
    );
    assert_eq!(m.item_at(vp, 1.0, 1.0), None, "outside the panel");
}

#[test]
fn step_skips_dividers_and_wraps() {
    let mut m = menu(vec![Row::Rule, Row::Item("one"), Row::Item("two")]);
    let enabled = [true, true, true];
    m.step(&enabled, 1);
    assert_eq!(m.hover, Some(1), "the divider is not a stop");
    m.step(&enabled, 1);
    assert_eq!(m.hover, Some(2));
    m.step(&enabled, 1);
    assert_eq!(m.hover, Some(1), "wraps at the end");
    m.step(&enabled, -1);
    assert_eq!(m.hover, Some(2), "and back");
}

#[test]
fn step_skips_disabled_rows() {
    let mut m = menu(vec![Row::Item("one"), Row::Item("two"), Row::Item("three")]);
    let enabled = [true, false, true];
    m.step(&enabled, 1);
    m.step(&enabled, 1);
    assert_eq!(m.hover, Some(2), "the dimmed row is stepped over");
}

/// A toggle row is still measured with its check column, set or not — the
/// label must not shift when the state flips under an open menu.
#[test]
fn a_toggle_row_reserves_its_check_column_either_way() {
    let set = Row::Toggle("Keyboard hints", true);
    let unset = Row::Toggle("Keyboard hints", false);
    assert_eq!(leading(&set), leading(&unset));
    assert!(leading(&unset).is_some());
}

// --------------------------------------------------------------- the stepper

fn stepper_menu(floor: bool) -> Menu<Row> {
    menu(vec![
        Row::Item("Settings"),
        Row::Stepper {
            label: "Zoom",
            value: "100%",
            floor,
        },
    ])
}

const VIEWPORT: (f32, f32) = (1280.0, 720.0);

/// The cells sit inside their own row, end to end, right-aligned to the same
/// trailing pad a chord uses — so a control is always where its glyph is.
#[test]
fn a_steppers_cells_tile_the_right_hand_end_of_its_row() {
    let m = stepper_menu(false);
    let row = m.item_rect(VIEWPORT, 1);
    let cells: Vec<Rect> = (0..3)
        .map(|c| m.control_rect(VIEWPORT, 1, c).expect("three cells"))
        .collect();
    assert_eq!(
        cells[2].x + cells[2].w,
        row.x + row.w - ROW_PAD_X,
        "the band ends at the row's trailing pad"
    );
    for pair in cells.windows(2) {
        assert_eq!(
            pair[0].x + pair[0].w,
            pair[1].x,
            "cells tile, never overlap"
        );
    }
    for cell in &cells {
        assert_eq!((cell.y, cell.h), (row.y, row.h), "full row height to press");
        assert!(cell.x >= row.x, "and inside the panel");
    }
    assert!(m.control_rect(VIEWPORT, 1, 3).is_none(), "no fourth cell");
    assert!(
        m.control_rect(VIEWPORT, 0, 0).is_none(),
        "a plain row has none"
    );
}

/// The readout keeps its width whatever it says, so `+` does not slide out
/// from under a pointer that is stepping with it.
#[test]
fn the_readout_is_a_fixed_width_whatever_the_value_reads() {
    let wide = menu(vec![Row::Stepper {
        label: "Zoom",
        value: "200%",
        floor: false,
    }]);
    let narrow = menu(vec![Row::Stepper {
        label: "Zoom",
        value: "50%",
        floor: false,
    }]);
    for c in 0..3 {
        assert_eq!(
            wide.control_rect(VIEWPORT, 0, c),
            narrow.control_rect(VIEWPORT, 0, c),
            "cell {c} moved when the number did"
        );
    }
}

/// The pointer lands on a control, and never on the row carrying it: a stepper
/// row is not a thing you can choose.
#[test]
fn the_pointer_finds_the_control_and_not_the_row_under_it() {
    let m = stepper_menu(false);
    let plus = m.control_rect(VIEWPORT, 1, 2).expect("the + cell");
    let (x, y) = (plus.x + plus.w * 0.5, plus.y + plus.h * 0.5);
    assert_eq!(m.control_at(VIEWPORT, x, y), Some((1, 2)));
    assert_eq!(m.item_at(VIEWPORT, x, y), None, "the row is not selectable");
    let settings = m.item_rect(VIEWPORT, 0);
    let inside = (settings.x + 4.0, settings.y + settings.h * 0.5);
    assert_eq!(m.item_at(VIEWPORT, inside.0, inside.1), Some(0));
    assert_eq!(m.control_at(VIEWPORT, inside.0, inside.1), None);
}

/// The readout is not a button, and a button at the end of its range is found
/// but refuses — the caller needs the first answer to swallow the press and
/// the second to decide whether to act on it.
#[test]
fn a_readout_and_a_dimmed_button_are_found_but_not_pressable() {
    let m = stepper_menu(true);
    for (cell, pressable) in [(0, false), (1, false), (2, true)] {
        let r = m.control_rect(VIEWPORT, 1, cell).expect("cell");
        let (x, y) = (r.x + r.w * 0.5, r.y + r.h * 0.5);
        assert_eq!(m.control_at(VIEWPORT, x, y), Some((1, cell)), "cell {cell}");
        assert_eq!(
            m.pressable_control_at(VIEWPORT, x, y).is_some(),
            pressable,
            "cell {cell}"
        );
    }
}

/// Keyboard stepping walks past a stepper the way it walks past a divider:
/// there is no such thing as choosing "Zoom".
#[test]
fn step_skips_a_stepper_row() {
    let mut m = menu(vec![
        Row::Item("Settings"),
        Row::Stepper {
            label: "Zoom",
            value: "100%",
            floor: false,
        },
        Row::Item("Updates"),
    ]);
    let enabled = [true, true, true];
    m.step(&enabled, 1);
    assert_eq!(m.hover, Some(0));
    m.step(&enabled, 1);
    assert_eq!(m.hover, Some(2), "straight past the stepper");
}

/// Re-anchoring moves the panel and leaves the rows alone — what a zoom step
/// under an open menu needs, and nothing more.
#[test]
fn set_anchor_moves_the_panel_without_touching_the_rows() {
    let mut m = stepper_menu(false);
    let before = m.panel(VIEWPORT);
    m.set_anchor((300.0, 220.0));
    let after = m.panel(VIEWPORT);
    assert_eq!((after.x, after.y), (300.0, 220.0));
    assert_eq!((after.w, after.h), (before.w, before.h));
    assert_eq!(m.items().len(), 2);
}

// ------------------------------------------------------------- card rows

/// Without a font a card is `CARD_FALLBACK_H` tall; the rows after it sit
/// under it, the panel grows by it, and its button — its one control — is at
/// the card's foot, inset like the card is from the row.
#[test]
fn a_card_row_is_its_fallback_height_with_its_button_at_its_foot() {
    let m = menu(vec![
        Row::Item("What's new"),
        Row::Card {
            title: "Faster sync",
            button: Some(true),
        },
        Row::Item("Full changelog"),
    ]);
    let card_row = m.item_rect(VIEWPORT, 1);
    assert_eq!(card_row.h, CARD_FALLBACK_H);
    assert_eq!(
        m.item_rect(VIEWPORT, 2).y,
        card_row.y + CARD_FALLBACK_H,
        "the next row sits under the card"
    );
    assert_eq!(m.height(), PAD * 2.0 + ITEM_H * 2.0 + CARD_FALLBACK_H);
    let card = m.card_rect(VIEWPORT, 1).expect("a card");
    assert_eq!(
        (card.x, card.y, card.w, card.h),
        (
            card_row.x + CARD_INSET,
            card_row.y + CARD_INSET,
            card_row.w - CARD_INSET * 2.0,
            card_row.h - CARD_INSET * 2.0
        )
    );
    assert!(m.card_rect(VIEWPORT, 0).is_none(), "a plain row is no card");
    let button = m.control_rect(VIEWPORT, 1, 0).expect("the card's button");
    assert_eq!(button.x, card.x + CARD_PAD, "at the card's left pad");
    assert_eq!(
        button.y + button.h,
        card.y + card.h - CARD_PAD,
        "and its bottom pad"
    );
    assert_eq!((button.w, button.h), (CARD_BUTTON_MIN_W, CARD_BUTTON_H));
    assert!(m.control_rect(VIEWPORT, 1, 1).is_none(), "one control only");
    // The press finds the button where it is drawn, and the row elsewhere.
    let (bx, by) = button.centre();
    assert_eq!(m.control_at(VIEWPORT, bx, by), Some((1, 0)));
    assert_eq!(m.pressable_control_at(VIEWPORT, bx, by), Some((1, 0)));
    let (tx, ty) = (card.x + card.w * 0.5, card.y + CARD_PAD + 4.0);
    assert_eq!(m.control_at(VIEWPORT, tx, ty), None, "the title is the row");
    assert_eq!(m.item_at(VIEWPORT, tx, ty), Some(1), "chosen as a whole");
}

/// A dimmed button is found and refused; a card without one has no control
/// and no button rect, and keeps its height.
#[test]
fn a_cards_button_is_found_but_not_pressable_when_dimmed_and_absent_when_none() {
    let dimmed = menu(vec![Row::Card {
        title: "Up to date",
        button: Some(false),
    }]);
    let b = dimmed.control_rect(VIEWPORT, 0, 0).expect("drawn dim");
    let (x, y) = b.centre();
    assert_eq!(dimmed.control_at(VIEWPORT, x, y), Some((0, 0)));
    assert_eq!(dimmed.pressable_control_at(VIEWPORT, x, y), None);
    let none = menu(vec![Row::Card {
        title: "Up to date",
        button: None,
    }]);
    assert!(none.control_rect(VIEWPORT, 0, 0).is_none());
    assert_eq!(none.item_rect(VIEWPORT, 0).h, CARD_FALLBACK_H);
}

/// A strip — a row that is only its buttons — spreads them across the row
/// in equal cells between the pads, so six logos read as one line rather
/// than a huddle in the chord column; the keyboard steps past it like a
/// stepper.
#[test]
fn a_strip_spreads_its_buttons_across_the_row() {
    let mut m = menu(vec![
        Row::Item("Report a problem"),
        Row::Strip(6),
        Row::Item("After"),
    ]);
    let row = m.item_rect(VIEWPORT, 1);
    let cells: Vec<Rect> = (0..6)
        .map(|c| m.control_rect(VIEWPORT, 1, c).expect("six cells"))
        .collect();
    assert_eq!(cells[0].x, row.x + ROW_PAD_X, "from the leading pad");
    assert!(
        (cells[5].x + cells[5].w - (row.x + row.w - ROW_PAD_X)).abs() < 0.01,
        "to the trailing pad"
    );
    for pair in cells.windows(2) {
        assert!(
            (pair[0].x + pair[0].w - pair[1].x).abs() < 0.01,
            "cells tile"
        );
        assert!((pair[0].w - pair[1].w).abs() < 0.01, "and are equal");
    }
    for (c, cell) in cells.iter().enumerate() {
        let (x, y) = cell.centre();
        assert_eq!(m.control_at(VIEWPORT, x, y), Some((1, c)));
    }
    let enabled = [true, true, true];
    m.step(&enabled, 1);
    m.step(&enabled, 1);
    assert_eq!(m.hover, Some(2), "straight past the strip");
}
