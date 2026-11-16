// SPDX-License-Identifier: MPL-2.0

//! The clock and the targets. What the chip *says* is the caller's; that it
//! stops asking for frames is not.

use super::*;

fn engine() -> TextEngine {
    TextEngine::new()
}

fn toast(now: Instant) -> Toast {
    Toast::new(
        "Moved \u{201C}Spec\u{201D} to Trash".into(),
        Some("Undo"),
        now,
    )
}

const OVER: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 900.0,
    h: 600.0,
};

// ------------------------------------------------------------- the clock

/// **The one that stops the 625 wasted frames.** A chip holding still is not an
/// animation; only the fade is. Someone will be tempted to simplify
/// `animating` back to "is there a toast", and this is what catches it.
#[test]
fn it_asks_for_frames_only_during_the_fade() {
    let t0 = Instant::now();
    let t = toast(t0);

    assert!(!t.animating(t0), "not the moment it appears");
    assert!(!t.animating(t0 + HOLD / 2), "not while it is holding still");
    assert!(t.animating(t0 + HOLD + FADE / 2), "yes while it fades");
}

/// It must return to `false` on its own (ADR-26), and the way it does that is by
/// expiring: `alpha` goes to `None` and the owner drops it.
#[test]
fn a_toast_fades_out_and_stops_asking_for_frames() {
    let t0 = Instant::now();
    let t = toast(t0);

    assert_eq!(t.alpha(t0), Some(1.0));
    assert_eq!(t.alpha(t0 + HOLD), Some(1.0), "full until the hold is up");

    let mid = t.alpha(t0 + HOLD + FADE / 2).unwrap();
    assert!(mid > 0.0 && mid < 1.0, "part way through: {mid}");

    assert_eq!(t.alpha(t0 + HOLD + FADE), None, "expired");
    assert!(!t.animating(t0 + HOLD + FADE), "and stops asking");
    assert!(!t.animating(t0 + HOLD + FADE * 4), "and stays stopped");
}

#[test]
fn determinate_progress_stays_visible_without_animating() {
    let t0 = Instant::now();
    let t =
        Toast::new("Importing 25%".into(), None, t0).with_progress(ProgressMode::Determinate(0.25));
    let later = t0 + HOLD + FADE * 20;

    assert_eq!(t.alpha(later), Some(1.0));
    assert!(!t.animating(later));
}

#[test]
fn indeterminate_progress_stays_visible_and_keeps_moving() {
    let t0 = Instant::now();
    let t = Toast::new("Importing".into(), None, t0).with_progress(ProgressMode::Indeterminate);
    let later = t0 + HOLD + FADE * 20;

    assert_eq!(t.alpha(later), Some(1.0));
    assert!(t.animating(later));
}

#[test]
fn progress_action_sits_after_the_bar_and_can_be_removed_in_place() {
    let mut e = engine();
    let mut t = Toast::new(
        "Importing content\u{2026} Inspecting".into(),
        Some("Stop"),
        Instant::now(),
    )
    .with_progress(ProgressMode::Indeterminate);
    let r = t.rect(&mut e, OVER, 600.0);
    let (bar, action) = t.progress_row(&mut e, r).unwrap();
    let action = action.expect("Stop");

    assert!(bar.x + bar.w < action.x, "Stop follows the progress bar");
    assert_eq!(t.targets(&mut e, OVER, 600.0)[0], (Hit::Action, action));

    t.set_action(None);
    assert!(t.progress_row(&mut e, r).unwrap().1.is_none());
    assert_eq!(
        t.targets(&mut e, OVER, 600.0).len(),
        1,
        "only close remains"
    );
}

/// A progress toast keeps one width as its stage label changes (Converting →
/// Reading media), so the chip does not jump between updates.
#[test]
fn a_progress_toast_keeps_one_width_as_its_label_changes() {
    let mut e = engine();
    let mut t = Toast::new(
        "Importing content\u{2026} Converting 40%".into(),
        Some("Stop"),
        Instant::now(),
    )
    .with_progress(ProgressMode::Determinate(0.4));
    let converting = t.rect(&mut e, OVER, 600.0);

    t.set_progress(
        "Importing content\u{2026} Reading media".into(),
        ProgressMode::Indeterminate,
        Instant::now(),
    );
    let reading = t.rect(&mut e, OVER, 600.0);

    assert_eq!(converting.w, reading.w, "width is stable across stages");
}

// ------------------------------------------------------------ the targets

/// Both controls are pressable exactly where they are drawn, and neither
/// escapes the chip.
#[test]
fn undo_and_the_close_box_are_pressable_where_they_are_drawn() {
    let mut e = engine();
    let mut t = toast(Instant::now());
    let r = t.rect(&mut e, OVER, 600.0);

    let targets = t.targets(&mut e, OVER, 600.0);
    assert_eq!(targets.len(), 2);
    for (hit, target) in targets {
        assert!(
            target.x >= r.x && target.x + target.w <= r.x + r.w,
            "{hit:?} escaped the chip"
        );
        let (x, y) = (target.x + target.w * 0.5, target.y + target.h * 0.5);
        let want = match hit {
            Hit::Action => Outcome::Action,
            Hit::Secondary => Outcome::Secondary,
            Hit::Close => Outcome::Dismiss,
        };
        assert_eq!(t.mouse_down(&mut e, OVER, 600.0, x, y), want, "{hit:?}");
    }
}

/// It is not modal. A press beside it is not its business, and must fall through
/// to whatever is underneath.
#[test]
fn a_press_beside_it_is_a_miss() {
    let mut e = engine();
    let mut t = toast(Instant::now());
    assert_eq!(t.mouse_down(&mut e, OVER, 600.0, 5.0, 5.0), Outcome::Miss);
    assert!(!t.contains(&mut e, OVER, 600.0, 5.0, 5.0));
}

/// Centred on what it is *over*, not on the window, so it does not sit half
/// under the explore column.
#[test]
fn it_is_centred_on_the_content_rather_than_the_window() {
    let mut e = engine();
    let t = toast(Instant::now());
    let content = Rect::new(260.0, 0.0, 640.0, 600.0);
    let r = t.rect(&mut e, content, 600.0);

    let centre = r.x + r.w * 0.5;
    assert!(
        (centre - (content.x + content.w * 0.5)).abs() < 0.5,
        "centred on the content"
    );
    assert!(r.x > content.x, "and clear of the column beside it");
}

/// The floor is passed in rather than taken from the viewport, because an update
/// banner pushes the chip up off it.
#[test]
fn the_floor_moves_it_rather_than_resizing_it() {
    let mut e = engine();
    let t = toast(Instant::now());
    let low = t.rect(&mut e, OVER, 600.0);
    let high = t.rect(&mut e, OVER, 520.0);

    assert_eq!(low.y - high.y, 80.0);
    assert_eq!(low.w, high.w);
    assert_eq!(low.y + low.h, 600.0 - MARGIN);
    assert_eq!(high.y + high.h, 520.0 - MARGIN);
}

#[test]
fn a_toast_with_no_action_has_only_a_close_box() {
    let mut e = engine();
    let t = Toast::new(
        "\u{201C}Spec\u{201D} was restored".into(),
        None,
        Instant::now(),
    );
    let targets = t.targets(&mut e, OVER, 600.0);
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].0, Hit::Close);
}

#[test]
fn hovering_a_control_reports_a_change_exactly_once() {
    let mut e = engine();
    let mut t = toast(Instant::now());
    let close = t.targets(&mut e, OVER, 600.0)[1].1;
    let (x, y) = (close.x + close.w * 0.5, close.y + close.h * 0.5);

    assert!(t.mouse_move(&mut e, OVER, 600.0, x, y), "arrived");
    assert!(!t.mouse_move(&mut e, OVER, 600.0, x, y), "still there");
    assert!(t.mouse_move(&mut e, OVER, 600.0, 5.0, 5.0), "left");
}

/// A toast you are pointing at does not fade or expire under the pointer.
#[test]
fn hovering_the_chip_keeps_it_visible_past_the_hold() {
    let mut e = engine();
    let t0 = Instant::now();
    let mut t = toast(t0);
    let r = t.rect(&mut e, OVER, 600.0);
    assert!(t.mouse_move(&mut e, OVER, 600.0, r.centre().0, r.centre().1));
    assert!(t.over_chip());
    let later = t0 + HOLD + FADE * 2;
    assert_eq!(t.alpha(later), Some(1.0), "still solid while hovered");
    assert!(!t.animating(later), "and not fading");
}

/// The whole chip is the arrow — body, Show / Undo and × alike, because Koru
/// does not use the hand cursor. What the test is really pinning is that none
/// of it is the I-beam of the document underneath: the chip claims the pointer
/// wherever it is over it, and gives it back when the pointer leaves.
#[test]
fn the_whole_chip_is_the_arrow_and_never_the_ibeam() {
    let mut e = engine();
    let mut t = toast(Instant::now());
    let r = t.rect(&mut e, OVER, 600.0);
    let action = t.targets(&mut e, OVER, 600.0)[0].1;

    assert!(t.mouse_move(&mut e, OVER, 600.0, r.x + 20.0, r.centre().1));
    assert!(t.over_chip());
    assert_eq!(t.cursor(), CursorIcon::Default, "body of the chip");

    assert!(t.mouse_move(
        &mut e,
        OVER,
        600.0,
        action.x + action.w * 0.5,
        action.y + action.h * 0.5
    ));
    assert_eq!(t.cursor(), crate::cursor::PRESS, "Show / Undo");
    assert_ne!(t.cursor(), CursorIcon::Pointer, "and never the hand");

    assert!(t.mouse_move(&mut e, OVER, 600.0, 5.0, 5.0));
    assert!(!t.over_chip());
    assert_eq!(t.cursor(), CursorIcon::Default, "off the chip");
}

/// A chip cannot grow wider than [`MAX_W`] (or the content band), however long
/// the message — it wraps and grows taller instead.
#[test]
fn a_very_long_message_caps_width_and_grows_taller() {
    let mut e = engine();
    let short = Toast::new("ok".into(), None, Instant::now());
    let long = Toast::new("x".repeat(4000), None, Instant::now());
    let short_r = short.rect(&mut e, OVER, 600.0);
    let long_r = long.rect(&mut e, OVER, 600.0);
    assert!(
        long_r.w <= MAX_W + 0.5,
        "width capped at {MAX_W}, got {}",
        long_r.w
    );
    assert!(long_r.w <= OVER.w - MARGIN * 2.0);
    assert!(long_r.x >= OVER.x);
    assert!(
        long_r.h > short_r.h,
        "long copy should wrap taller ({} vs {})",
        long_r.h,
        short_r.h
    );
}

/// An action never makes the message disposable. A narrow confirmation grows
/// tall enough for the complete copy rather than replacing its tail with an
/// ellipsis.
#[test]
fn actionable_confirmation_wraps_when_it_needs_to() {
    let mut e = engine();
    let t = Toast::new(
        "Import succeeded.".into(),
        Some("View Notebook"),
        Instant::now(),
    );

    let wide = t.rect(&mut e, OVER, 600.0);
    let ordinary = t.rect(&mut e, Rect::new(OVER.x, OVER.y, 433.0, OVER.h), 600.0);
    let narrow = t.rect(&mut e, Rect::new(OVER.x, OVER.y, 280.0, OVER.h), 600.0);
    assert_eq!(wide.h, H_MIN, "the natural width remains compact");
    assert_eq!(ordinary.h, H_MIN, "roomy copy must not wrap unnecessarily");
    assert!(wide.w < MAX_W, "short copy uses its natural width");
    assert!(narrow.h > H_MIN, "narrow copy wraps instead of eliding");
}

#[test]
fn duplicate_file_message_and_action_fit_on_one_line() {
    let mut e = engine();
    let t = Toast::new(
        "File is already known.".into(),
        Some("View file"),
        Instant::now(),
    );
    let r = t.rect(&mut e, OVER, 600.0);
    let action = t.targets(&mut e, OVER, 600.0)[0].1;

    assert_eq!(r.h, H_MIN);
    assert!(r.w < MAX_W);
    assert!(
        action.x < r.x + r.w - PAD_X - CLOSE,
        "action is before close"
    );
}

// ------------------------------------------------- a line under the message

const LINE: &str = "412 pictures and scanned PDFs can be found by the words in them. Koru reads them on this computer. One-time download, 24 MB.";

fn summary(now: Instant) -> Toast {
    Toast::new("Import succeeded.".into(), Some("View Notebook"), now)
        .with_line(LINE.into(), vec!["Turn on", "Not now"])
}

/// **The line sits under the message, inside the chip**, its answers after
/// its sentence and before nothing else; the message's own action is where it
/// was; each answer is pressable exactly where it is drawn, and a press on it
/// is the line's, not the action's.
#[test]
fn a_line_sits_under_the_message_and_its_answers_hit_test() {
    let mut e = engine();
    let t = summary(Instant::now());
    let r = t.rect(&mut e, OVER, 600.0);
    let action = t.action_rect(&mut e, OVER, 600.0).expect("View Notebook");
    let answers = t.line_rects(&mut e, OVER, 600.0);
    assert_eq!(answers.len(), 2);
    assert_eq!(r.w, MAX_W, "a toast with a line takes the room it may");
    assert!(r.h > H_MIN, "two paragraphs are taller than one line");
    for answer in &answers {
        assert!(
            answer.y > action.y + action.h,
            "under the message: {answer:?}"
        );
        assert!(
            answer.x >= r.x && answer.x + answer.w <= r.x + r.w,
            "{answer:?} in {r:?}"
        );
        assert!(answer.y + answer.h <= r.y + r.h, "{answer:?} in {r:?}");
    }
    assert!(
        answers[1].x > answers[0].x + answers[0].w,
        "Not now after Turn on"
    );
    for (index, answer) in answers.iter().enumerate() {
        let (x, y) = answer.centre();
        assert_eq!(t.line_action_at(&mut e, OVER, 600.0, x, y), Some(index));
        let mut pressed = summary(Instant::now());
        assert_eq!(
            pressed.mouse_down(&mut e, OVER, 600.0, x, y),
            Outcome::Miss,
            "the action and the close box are not under it"
        );
    }
    let (x, y) = action.centre();
    assert_eq!(t.line_action_at(&mut e, OVER, 600.0, x, y), None);
}

/// **The keyboard on the line holds the chip**, as the pointer does, and an
/// index past the answers is no focus at all.
#[test]
fn the_keyboard_on_the_line_holds_the_chip() {
    let t0 = Instant::now();
    let mut t = summary(t0);
    t.set_line_focus(Some(1));
    assert_eq!(t.line_focus(), Some(1));
    assert_eq!(
        t.alpha(t0 + HOLD + FADE * 2),
        Some(1.0),
        "held while answered"
    );
    assert!(!t.animating(t0 + HOLD + FADE / 2));
    t.set_line_focus(Some(2));
    assert_eq!(t.line_focus(), None, "there is no third answer");
    assert_eq!(t.alpha(t0 + HOLD + FADE), None, "and without it, it goes");

    // Held past its hold, then left: a full hold again, not a vanishing.
    let long_ago = Instant::now() - HOLD * 3;
    let mut held = summary(long_ago);
    held.set_line_focus(Some(0));
    held.set_line_focus(None);
    assert_eq!(held.alpha(Instant::now()), Some(1.0));
}

/// The answer given, the line goes and the summary stays; a line with no
/// answers is a sentence alone; a progress toast carries none.
#[test]
fn a_line_can_be_taken_away_or_have_nothing_to_press() {
    let mut e = engine();
    let mut t = summary(Instant::now());
    t.set_line_focus(Some(0));
    t.clear_line();
    assert!(t.line_actions().is_empty());
    assert_eq!(t.line_sentence(), None);
    assert_eq!(t.line_focus(), None);
    assert_eq!(t.action(), Some("View Notebook"));
    assert_eq!(t.rect(&mut e, OVER, 600.0).h, H_MIN, "back to one line");

    let quiet = Toast::new(
        "Import succeeded.".into(),
        Some("View Notebook"),
        Instant::now(),
    )
    .with_line(LINE.into(), Vec::new());
    assert!(quiet.line_rects(&mut e, OVER, 600.0).is_empty());
    assert!(
        quiet.rect(&mut e, OVER, 600.0).h > H_MIN,
        "the sentence is drawn"
    );

    let progress = Toast::new("Downloading".into(), Some("Stop"), Instant::now())
        .with_progress(ProgressMode::Indeterminate)
        .with_line(LINE.into(), vec!["Turn on"]);
    assert!(progress.line_actions().is_empty());
}

/// Hovering an answer is a change worth a redraw, and the pointer says it
/// can press.
#[test]
fn hovering_an_answer_is_a_change_and_a_press_cursor() {
    let mut e = engine();
    let mut t = summary(Instant::now());
    let answer = t.line_rects(&mut e, OVER, 600.0)[0];
    let (x, y) = answer.centre();
    assert!(t.mouse_move(&mut e, OVER, 600.0, x, y));
    assert_eq!(t.cursor(), crate::cursor::PRESS);
    assert!(
        !t.mouse_move(&mut e, OVER, 600.0, x, y),
        "no change, no redraw"
    );
}
