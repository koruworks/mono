// SPDX-License-Identifier: MPL-2.0

//! The keyboard, mostly. This dialog exists because of what Enter means.

use super::*;

fn engine() -> TextEngine {
    TextEngine::new()
}

const VIEWPORT: (f32, f32) = (1200.0, 800.0);

fn confirm() -> Confirm {
    Confirm::new(
        "Move 24 items to Trash?",
        "\u{201C}Q3 Launch\u{201D} contains 23 nested items. They will be moved \
         together and can be restored from Trash.",
        "Move to Trash",
    )
}

fn key(k: NamedKey) -> Key {
    Key::Named(k)
}

// ------------------------------------------------------------ the keyboard

/// **The load-bearing one.** Escape cancels, and so does Enter — because the
/// button in the corner destroys twenty-four documents, and a modal that
/// appears under somebody's hands must not read the next keystroke as consent.
/// This is the whole reason this type exists rather than a third `ErrorDialog`.
#[test]
fn escape_cancels_and_so_does_enter() {
    let mut c = confirm();
    assert_eq!(c.key(&key(NamedKey::Escape)), Outcome::Cancel);

    let mut c = confirm();
    assert_eq!(c.key(&key(NamedKey::Enter)), Outcome::Cancel);

    let mut c = confirm();
    assert_eq!(c.key(&key(NamedKey::Space)), Outcome::Cancel);
}

/// The destructive answer is reachable — it just has to be asked for.
#[test]
fn tab_moves_the_focus_and_then_enter_confirms() {
    let mut c = confirm();
    assert_eq!(c.key(&key(NamedKey::Tab)), Outcome::Keep);
    assert_eq!(c.key(&key(NamedKey::Enter)), Outcome::Confirm);
}

#[test]
fn the_focus_cycles_back_to_cancel() {
    let mut c = confirm();
    c.key(&key(NamedKey::Tab));
    c.key(&key(NamedKey::Tab));
    assert_eq!(c.key(&key(NamedKey::Enter)), Outcome::Cancel);
}

#[test]
fn the_arrows_move_the_focus_too() {
    let mut c = confirm();
    assert_eq!(c.key(&key(NamedKey::ArrowRight)), Outcome::Keep);
    assert_eq!(c.key(&key(NamedKey::Enter)), Outcome::Confirm);
}

// ------------------------------------------------- the declared default (ADR-265)

/// **The default is declared, and this dialog declares Cancel.**
///
/// Not a special case preserving ADR-71 — it is row three of DESIGN's table: an
/// answering button that destroys something declares *a safe, non-destructive
/// default*. Which is why the two tests above still read exactly as they did.
#[test]
fn a_confirmation_declares_the_safe_answer_by_default() {
    assert_eq!(confirm().default_action(), Some(Outcome::Cancel));
}

/// **Row two: a reversible action the person explicitly invoked.** Archiving a
/// workspace is the first, in `koru-workspaces-ui`. Enter accepts — and Escape
/// still cancels, because that is independent of the declaration.
#[test]
fn a_reversible_confirmation_may_declare_its_primary() {
    let mut c = confirm().defaulting_to_confirm();
    assert_eq!(c.default_action(), Some(Outcome::Confirm));
    assert_eq!(c.key(&key(NamedKey::Enter)), Outcome::Confirm);

    let mut c = confirm().defaulting_to_confirm();
    assert_eq!(
        c.key(&key(NamedKey::Escape)),
        Outcome::Cancel,
        "Escape cancels wherever closing is safe, whatever was declared"
    );
}

/// **Row four: no default at all.** Enter answers nothing until a button is
/// deliberately focused — which is what an irreversible action would declare.
/// Koru has none today, and `koru-workspaces-ui` pins that absence.
#[test]
fn a_modal_declaring_none_leaves_enter_inert_until_something_is_focused() {
    let mut c = confirm().declaring_no_default();
    assert_eq!(c.default_action(), None);
    assert_eq!(c.key(&key(NamedKey::Enter)), Outcome::Keep);
    assert_eq!(c.key(&key(NamedKey::Space)), Outcome::Keep);
    assert_eq!(
        c.key(&key(NamedKey::Escape)),
        Outcome::Cancel,
        "declaring no default is not declaring no way out"
    );

    let mut c = confirm().declaring_no_default();
    c.key(&key(NamedKey::Tab));
    assert_eq!(c.key(&key(NamedKey::Enter)), Outcome::Confirm);
}

/// **A deliberately focused button outranks the declaration**, in both
/// directions — the last row of DESIGN's table, and what stops a declaration
/// making the visible focus ring lie.
#[test]
fn a_focused_button_wins_over_whatever_was_declared() {
    // Declared Confirm, focus moved to Confirm and back to Cancel.
    let mut c = confirm().defaulting_to_confirm();
    c.key(&key(NamedKey::Tab));
    assert_eq!(c.default_action(), Some(Outcome::Confirm));
    c.key(&key(NamedKey::Tab));
    assert_eq!(
        c.default_action(),
        Some(Outcome::Cancel),
        "the focus ring is on Cancel, so Enter cancels"
    );
    assert_eq!(c.key(&key(NamedKey::Enter)), Outcome::Cancel);
}

/// Everything else is swallowed, for `ErrorDialog`'s reason: a modal that let
/// keystrokes through would take dictation into the document behind it.
#[test]
fn everything_else_is_swallowed() {
    let mut c = confirm();
    for k in ["a", "y", "n", " x"] {
        assert_eq!(
            c.key(&Key::Character(k.into())),
            Outcome::Keep,
            "{k:?} must not reach the document"
        );
    }
    assert_eq!(c.key(&key(NamedKey::ArrowDown)), Outcome::Keep);
    assert_eq!(c.key(&key(NamedKey::Backspace)), Outcome::Keep);
}

// ------------------------------------------------------------- the mouse

#[test]
fn every_button_hit_tests_back_to_where_it_is_drawn() {
    let mut e = engine();
    let mut c = confirm();
    for (hit, r) in c.targets(&mut e, VIEWPORT) {
        let (x, y) = (r.x + r.w * 0.5, r.y + r.h * 0.5);
        let want = super::answer(hit);
        assert_eq!(c.mouse_down(&mut e, VIEWPORT, x, y), want, "{hit:?}");
    }
}

/// The destructive answer owns the corner, and both buttons stay on the panel.
#[test]
fn the_destructive_answer_owns_the_corner() {
    let mut e = engine();
    let c = confirm();
    let panel = c.panel(&mut e, VIEWPORT);
    let targets = c.targets(&mut e, VIEWPORT);

    let (_, confirm_r) = targets[1];
    assert!((confirm_r.x + confirm_r.w - (panel.x + panel.w - PAD)).abs() < 0.5);
    for (hit, r) in targets {
        assert!(r.x >= panel.x, "{hit:?} ran off the left");
        assert!(r.y + r.h <= panel.y + panel.h, "{hit:?} ran off the bottom");
    }
}

/// A stray click must not be able to answer a question about deleting somebody's
/// work — in **either** direction.
#[test]
fn a_press_outside_does_not_dismiss() {
    let mut e = engine();
    let mut c = confirm();
    assert_eq!(c.mouse_down(&mut e, VIEWPORT, 4.0, 4.0), Outcome::Keep);
    let panel = c.panel(&mut e, VIEWPORT);
    assert_eq!(
        c.mouse_down(&mut e, VIEWPORT, panel.x + 10.0, panel.y + 10.0),
        Outcome::Keep,
        "even inside the panel, off the buttons"
    );
}

// ------------------------------------------------------------ the panel

#[test]
fn the_panel_grows_with_a_longer_body() {
    let mut e = engine();
    let short = Confirm::new("Empty Trash?", "This cannot be undone.", "Empty Trash");
    let long = confirm();
    assert!(
        long.panel(&mut e, VIEWPORT).h > short.panel(&mut e, VIEWPORT).h,
        "the body wraps and the panel follows it"
    );
}

/// **A `\n` in the body is a line break**, which is what lets a caller give
/// the dialog a list (the voice consent step's one line per model): the same
/// short words on three lines stand three lines tall, not one.
#[test]
fn a_line_break_in_the_body_starts_a_new_line() {
    let mut e = engine();
    let one = Confirm::new("Set up?", "one two three", "Download");
    let three = Confirm::new("Set up?", "one\ntwo\nthree", "Download");
    let line = BODY.metrics.line_height;
    let grew = three.panel(&mut e, VIEWPORT).h - one.panel(&mut e, VIEWPORT).h;
    assert!(
        (grew - 2.0 * line).abs() < 1.0,
        "two more lines, {grew} px taller"
    );
}

#[test]
fn the_panel_is_centred_and_fits_a_small_window() {
    let mut e = engine();
    let c = confirm();
    let tiny = (300.0, 240.0);
    let p = c.panel(&mut e, tiny);
    assert!(p.x >= 0.0 && p.x + p.w <= tiny.0, "fits across");
    assert!(p.y >= 0.0 && p.y + p.h <= tiny.1, "fits down");
    assert!((p.x - (tiny.0 - p.w) * 0.5).abs() < 0.5, "centred");
}

// ------------------------------------------------ the three-answer question

fn three_answers() -> Confirm {
    Confirm::new(
        "Move simple-brook to the Trash?",
        "It produced 2 results.",
        "Keep the results",
    )
    .with_alternative("Delete the results")
}

/// **Three buttons, and the alternative between the other two.** Cancel is
/// leftmost, the corner answer is in the corner, and the second answer sits
/// where a person reads it before reaching for either.
#[test]
fn a_three_answer_question_lays_the_alternative_between_cancel_and_the_corner() {
    let mut e = engine();
    let c = three_answers();
    let panel = c.panel(&mut e, VIEWPORT);
    let targets = c.targets(&mut e, VIEWPORT);
    assert_eq!(targets.len(), 3);
    assert_eq!(targets[0].0, Hit::Cancel);
    assert_eq!(targets[1].0, Hit::Alternative);
    assert_eq!(targets[2].0, Hit::Confirm);
    assert!((targets[2].1.x + targets[2].1.w - (panel.x + panel.w - PAD)).abs() < 0.5);
    for (hit, r) in targets {
        assert!(r.x >= panel.x, "{hit:?} ran off the left");
    }
}

/// Each button answers what it is, by pointer and by keyboard alike.
#[test]
fn every_answer_of_a_three_answer_question_hit_tests_back_to_itself() {
    let mut e = engine();
    let mut c = three_answers();
    for (hit, r) in c.targets(&mut e, VIEWPORT) {
        let (x, y) = (r.x + r.w * 0.5, r.y + r.h * 0.5);
        assert_eq!(
            c.mouse_down(&mut e, VIEWPORT, x, y),
            super::answer(hit),
            "{hit:?}"
        );
    }
}

/// **Tab walks three.** And Enter still answers the declared default — Cancel
/// — until a button is deliberately focused, which is the whole of why this
/// type exists.
#[test]
fn tab_walks_all_three_and_enter_still_starts_on_cancel() {
    let mut c = three_answers();
    assert_eq!(c.default_action(), Some(Outcome::Cancel));
    assert_eq!(c.key(&key(NamedKey::Tab)), Outcome::Keep);
    assert_eq!(c.default_action(), Some(Outcome::Alternative));
    assert_eq!(c.key(&key(NamedKey::Tab)), Outcome::Keep);
    assert_eq!(c.default_action(), Some(Outcome::Confirm));
    assert_eq!(c.key(&key(NamedKey::Tab)), Outcome::Keep);
    assert_eq!(c.default_action(), Some(Outcome::Cancel), "and round again");
    assert_eq!(c.key(&key(NamedKey::Escape)), Outcome::Cancel);
}

/// **A question with two answers never produces the third.** So a caller that
/// did not ask for an alternative can leave that arm with the ones that
/// change nothing.
#[test]
fn a_two_answer_question_never_answers_with_the_alternative() {
    let mut e = engine();
    let mut c = confirm();
    assert_eq!(c.targets(&mut e, VIEWPORT).len(), 2);
    for _ in 0..6 {
        assert_ne!(c.key(&key(NamedKey::Tab)), Outcome::Alternative);
        assert_ne!(c.default_action(), Some(Outcome::Alternative));
    }
}

// ------------------------------------------------------------ the inspector

fn drawn(c: &Confirm) -> Vec<koru_draw::inspect::Record> {
    use koru_draw::inspect;
    let mut engine = engine();
    let mut canvas = Scene::new();
    let root = Affine::IDENTITY;
    let capture = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, VIEWPORT.0, VIEWPORT.1));
    c.draw(&mut canvas, root, &mut engine, VIEWPORT);
    capture.finish().records
}

/// The title, the description and the buttons are the panel's contents, so
/// the inspector shows them under `FloatingPanel`, not beside it.
#[test]
fn the_contents_nest_under_the_floating_panel() {
    for c in [
        confirm(),
        Confirm::asking("Share usage data?", "Body.", "Share", "Don't"),
    ] {
        let records = drawn(&c);
        let panel = records
            .iter()
            .position(|r| r.name == "FloatingPanel")
            .expect("the panel");
        for name in ["Dialog title", "Dialog description"] {
            let mut at = records.iter().position(|r| r.name == name).expect(name);
            while records[at].parent != Some(panel) {
                at = records[at]
                    .parent
                    .unwrap_or_else(|| panic!("{name} escaped the panel"));
            }
        }
    }
}

/// A question without the warning badge spends no gutter on it: its text
/// spans the panel between the paddings.
#[test]
fn text_without_a_badge_uses_the_full_width() {
    let mut engine = engine();
    let c = Confirm::asking("Share usage data?", "Body.", "Share", "Don't");
    let panel = c.panel(&mut engine, VIEWPORT);
    let records = drawn(&c);
    let title = records
        .iter()
        .find(|r| r.name == "Dialog title")
        .unwrap()
        .rect();
    assert_eq!(title.x0 as f32, panel.x + PAD);
    assert_eq!(title.width() as f32, panel.w - PAD * 2.0);

    let badged = drawn(&confirm());
    let title = badged
        .iter()
        .find(|r| r.name == "Dialog title")
        .unwrap()
        .rect();
    assert_eq!(title.x0 as f32, panel.x + PAD + BADGE + 14.0);
}
