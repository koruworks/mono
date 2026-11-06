// SPDX-License-Identifier: MPL-2.0

use super::*;

const VIEWPORT: (f32, f32) = (1200.0, 800.0);

fn four() -> Launcher {
    let tile = |glyph, label: &str, letter| Tile {
        glyph,
        label: label.to_owned(),
        letter,
    };
    Launcher::open(
        "New",
        vec![
            tile('a', "Document", 'd'),
            tile('b', "Task", 't'),
            tile('c', "Agent", 'a'),
            tile('d', "Shell", 's'),
        ],
    )
}

/// The panel is centred and its tiles are inside it, a row of three then
/// the rest.
#[test]
fn the_panel_is_centred_and_holds_its_tiles() {
    let l = four();
    let panel = l.panel(VIEWPORT);
    assert!((panel.x + panel.w * 0.5 - VIEWPORT.0 * 0.5).abs() < 0.5);
    assert!((panel.y + panel.h * 0.5 - VIEWPORT.1 * 0.5).abs() < 0.5);
    for i in 0..4 {
        let r = l.tile_rect(VIEWPORT, i);
        assert!(r.x >= panel.x && r.x + r.w <= panel.x + panel.w + 0.01);
        assert!(r.y >= panel.y && r.y + r.h <= panel.y + panel.h + 0.01);
    }
    assert_eq!(
        l.tile_rect(VIEWPORT, 3).y,
        l.tile_rect(VIEWPORT, 0).y + TILE_H + GAP
    );
}

/// **A tile's letter chooses it**, case-blind, carrying the modifiers: Alt
/// asks where, Shift asks which kind.
#[test]
fn a_letter_chooses_its_tile_with_the_modifiers_held() {
    let mut l = four();
    let key = |s: &str| KeyPress::Character(s.into());
    assert_eq!(
        l.key(&key("t"), ModifiersState::empty()),
        Outcome::Chose {
            index: 1,
            alt: false,
            shift: false
        }
    );
    assert_eq!(
        l.key(&key("S"), ModifiersState::SHIFT),
        Outcome::Chose {
            index: 3,
            alt: false,
            shift: true
        }
    );
    assert_eq!(
        l.key(&key("a"), ModifiersState::ALT),
        Outcome::Chose {
            index: 2,
            alt: true,
            shift: false
        }
    );
    assert_eq!(l.key(&key("z"), ModifiersState::empty()), Outcome::Handled);
}

/// The arrows walk the grid, Enter chooses, Escape dismisses.
#[test]
fn the_arrows_walk_the_grid_and_enter_chooses() {
    let mut l = four();
    assert_eq!(
        l.key(
            &KeyPress::Named(NamedKey::ArrowDown),
            ModifiersState::empty()
        ),
        Outcome::Moved(3)
    );
    assert_eq!(
        l.key(
            &KeyPress::Named(NamedKey::ArrowRight),
            ModifiersState::empty()
        ),
        Outcome::Moved(0),
        "wraps at the end"
    );
    l.key(
        &KeyPress::Named(NamedKey::ArrowRight),
        ModifiersState::empty(),
    );
    assert_eq!(
        l.key(&KeyPress::Named(NamedKey::Enter), ModifiersState::empty()),
        Outcome::Chose {
            index: 1,
            alt: false,
            shift: false
        }
    );
    assert_eq!(
        l.key(&KeyPress::Named(NamedKey::Escape), ModifiersState::empty()),
        Outcome::Dismissed
    );
}

/// A press on a tile chooses it with its modifiers; off the tiles dismisses.
#[test]
fn a_press_chooses_a_tile_or_dismisses() {
    let l = four();
    let r = l.tile_rect(VIEWPORT, 2);
    assert_eq!(
        l.mouse_down(VIEWPORT, r.x + 5.0, r.y + 5.0, ModifiersState::ALT),
        Outcome::Chose {
            index: 2,
            alt: true,
            shift: false
        }
    );
    assert_eq!(
        l.mouse_down(VIEWPORT, 1.0, 1.0, ModifiersState::empty()),
        Outcome::Dismissed
    );
}

/// The underline lands under the letter as written, wherever it falls.
#[test]
fn the_letter_is_found_in_the_word_case_blind() {
    assert_eq!(letter_span("Document", 'd'), Some(("", "D")));
    assert_eq!(letter_span("Shell", 'l'), Some(("She", "l")));
    assert_eq!(letter_span("Tarea", 's'), None);
}

/// **A hint grows a footer** under the tiles, inside the panel, and the
/// tiles do not move.
#[test]
fn a_hint_sits_in_a_footer_under_the_tiles() {
    let bare = four();
    let hinted = four().with_hint("Alt to choose where");
    assert_eq!(bare.hint_rect(VIEWPORT), None);
    assert_eq!(hinted.panel(VIEWPORT).h, bare.panel(VIEWPORT).h + HINT_H);
    let footer = hinted.hint_rect(VIEWPORT).expect("a footer");
    let last = hinted.tile_rect(VIEWPORT, 3);
    assert!(footer.y >= last.y + last.h);
    let panel = hinted.panel(VIEWPORT);
    assert!(footer.y + footer.h <= panel.y + panel.h);
    assert_eq!(
        hinted.tile_at(VIEWPORT, footer.x + 4.0, footer.y + 4.0),
        None
    );
}

/// **Two tiles and a hint**: the panel keeps a full row's width for the
/// hint, and the pair sits in its middle.
#[test]
fn a_short_grid_with_a_hint_is_centred_in_a_full_width_panel() {
    let tile = |letter| Tile {
        glyph: 'a',
        label: "Word".into(),
        letter,
    };
    let l = Launcher::open("New", vec![tile('a'), tile('b')]).with_hint("Hold Alt");
    let panel = l.panel(VIEWPORT);
    let (a, b) = (l.tile_rect(VIEWPORT, 0), l.tile_rect(VIEWPORT, 1));
    assert!((panel.w - (theme::PANEL_PAD * 2.0 + 3.0 * TILE_W + 2.0 * GAP)).abs() < 0.01);
    let left = a.x - panel.x;
    let right = panel.x + panel.w - (b.x + b.w);
    assert!((left - right).abs() < 0.01, "{left} vs {right}");
}

/// One item per tile, named by its word, the highlight focused.
#[test]
fn semantics_name_every_tile() {
    let l = four();
    let s = l.semantics(VIEWPORT);
    assert_eq!(s.name, "New");
    assert_eq!(
        s.items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(),
        ["Document", "Task", "Agent", "Shell"]
    );
    assert_eq!(l.tile_of_instance(s.items[2].instance), Some(2));
}
