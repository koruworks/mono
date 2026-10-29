// SPDX-License-Identifier: MPL-2.0

use super::*;

fn bounds() -> Rect {
    Rect::new(0.0, 0.0, 900.0, 600.0)
}

/// Every point of the pane that a press would land a choice on.
fn presses(offer: &mut Offer, engine: &mut TextEngine) -> Vec<(Choice, f32, f32)> {
    let mut out = Vec::new();
    for y in (0..600).step_by(3) {
        for x in (0..900).step_by(4) {
            offer.mouse_down(engine, Lang::En, bounds(), x as f32, y as f32);
            if let Some(choice) = offer.take_choice() {
                out.push((choice, x as f32, y as f32));
            }
        }
    }
    out
}

#[test]
fn an_agent_that_can_continue_offers_resume_first_and_start_new_beside_it() {
    let mut engine = TextEngine::new();
    let mut offer = Offer::new("Claude".into(), Some("D:/work".into()), true);
    let pressed = presses(&mut offer, &mut engine);
    let resume: Vec<_> = pressed
        .iter()
        .filter(|(c, ..)| *c == Choice::Resume)
        .collect();
    let start: Vec<_> = pressed
        .iter()
        .filter(|(c, ..)| *c == Choice::StartNew)
        .collect();
    assert!(!resume.is_empty(), "Resume can be pressed");
    assert!(!start.is_empty(), "Start new can be pressed");
    let resume_right = resume.iter().map(|(_, x, _)| *x).fold(f32::MIN, f32::max);
    let start_left = start.iter().map(|(_, x, _)| *x).fold(f32::MAX, f32::min);
    assert!(
        resume_right < start_left,
        "Resume sits first, Start new after it"
    );
}

#[test]
fn an_agent_with_no_continue_form_offers_only_start_new() {
    let mut engine = TextEngine::new();
    let mut offer = Offer::new("Muse".into(), None, false);
    let pressed = presses(&mut offer, &mut engine);
    assert!(pressed.iter().all(|(c, ..)| *c == Choice::StartNew));
    assert!(!pressed.is_empty());
}

#[test]
fn enter_takes_the_first_button_and_other_keys_are_swallowed() {
    let mut with = Offer::new("Claude".into(), None, true);
    assert!(with.key(&Key::Named(NamedKey::Escape)));
    assert_eq!(with.take_choice(), None, "Escape chooses nothing");
    with.key(&Key::Named(NamedKey::Enter));
    assert_eq!(with.take_choice(), Some(Choice::Resume));

    let mut without = Offer::new("Muse".into(), None, false);
    without.key(&Key::Named(NamedKey::Enter));
    assert_eq!(without.take_choice(), Some(Choice::StartNew));
    assert_eq!(without.take_choice(), None, "a choice is taken once");
}

#[test]
fn a_press_beside_the_buttons_is_the_panels_and_chooses_nothing() {
    let mut engine = TextEngine::new();
    let mut offer = Offer::new("Claude".into(), None, true);
    assert!(offer.mouse_down(&mut engine, Lang::En, bounds(), 2.0, 2.0));
    assert_eq!(offer.take_choice(), None);
}

#[test]
fn the_pointer_is_the_plain_arrow_over_a_button_and_nowhere_else_claims_one() {
    let mut engine = TextEngine::new();
    let mut offer = Offer::new("Claude".into(), None, true);
    let pressed = presses(&mut offer, &mut engine);
    let (_, x, y) = pressed[0];
    assert!(offer.mouse_move(&mut engine, Lang::En, bounds(), x, y));
    assert_eq!(offer.cursor(), koru_ui::cursor::PRESS);
    offer.mouse_move(&mut engine, Lang::En, bounds(), 2.0, 2.0);
    assert_eq!(offer.cursor(), CursorIcon::Default);
}
