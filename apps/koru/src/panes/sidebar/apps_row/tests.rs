// SPDX-License-Identifier: MPL-2.0

use super::*;

fn items(n: usize, label_w: f32) -> Vec<Item> {
    NavigatorMode::ALL
        .into_iter()
        .take(n)
        .map(|mode| Item { mode, label_w })
        .collect()
}

fn band(w: f32) -> Rect {
    Rect::new(8.0, 50.0, w - 16.0, SQUARE)
}

/// Icon-only chips at the row's pitch, the manage square at the end, one
/// line, and every chip hit-tests back to its own app.
#[test]
fn chips_sit_at_the_rows_pitch_and_hit_test_back_to_their_app() {
    // Four squares, one labelled chip (104 px) and the manage square: 254 px
    // of chips and gaps in a 240 px band would wrap, so measure at 300.
    let l = solve(band(300.0), &items(5, 60.0), NavigatorMode::Files, false);
    assert_eq!(l.lines, 1);
    assert_eq!(l.height, SQUARE);
    for (i, c) in l.chips.iter().enumerate() {
        assert_eq!(c.rect.h, SQUARE);
        if i > 0 {
            let prev = l.chips[i - 1].rect;
            assert_eq!(c.rect.x, prev.x + prev.w + GAP, "chip {i} at the pitch");
        }
        let (x, y) = c.rect.centre();
        assert_eq!(chip_at(&l, x, y), Some(c.mode));
    }
    let last = l.chips.last().unwrap().rect;
    assert_eq!(
        l.manage.x,
        last.x + last.w + GAP,
        "manage follows the last chip"
    );
    assert_eq!(l.manage.w, SQUARE);
    let (x, y) = l.manage.centre();
    assert_eq!(chip_at(&l, x, y), None, "manage is not an app");
}

/// Only the active app wears its label below `LABELS_FROM`; a label whose
/// width is unknown lays out as a square.
#[test]
fn only_the_active_app_is_labelled_in_a_narrow_pane() {
    let l = solve(band(256.0), &items(5, 60.0), NavigatorMode::Files, false);
    let files = l
        .chips
        .iter()
        .find(|c| c.mode == NavigatorMode::Files)
        .unwrap();
    assert!(files.labelled);
    assert_eq!(files.rect.w, SQUARE + LABEL_GAP + 60.0 + LABEL_PAD_RIGHT);
    for c in l.chips.iter().filter(|c| c.mode != NavigatorMode::Files) {
        assert!(!c.labelled, "{:?}", c.mode);
        assert_eq!(c.rect.w, SQUARE);
    }
    let unmeasured = solve(band(256.0), &items(5, 0.0), NavigatorMode::Files, false);
    assert!(
        unmeasured
            .chips
            .iter()
            .all(|c| !c.labelled && c.rect.w == SQUARE)
    );
}

/// From 400 px every app wears its label — when every label fits on one
/// line. When they do not, labels give way before anything wraps.
#[test]
fn from_four_hundred_every_app_is_labelled_when_the_labels_fit() {
    // Four chips at 28 + 6 + 40 + 10 = 84 px, three gaps and the manage
    // square: 370 px, inside the 384 px band.
    let l = solve(band(400.0), &items(4, 40.0), NavigatorMode::Documents, true);
    assert_eq!(l.lines, 1);
    assert!(l.chips.iter().all(|c| c.labelled), "all labelled at 400");

    // Eight apps with 60 px labels do not fit one line at 400: labels give
    // way (only the active one stays) and the squares still fit one line.
    let l = solve(band(400.0), &items(8, 60.0), NavigatorMode::Documents, true);
    assert_eq!(l.lines, 1, "labels gave way before wrapping");
    assert_eq!(l.chips.iter().filter(|c| c.labelled).count(), 1);
}

/// When the squares themselves do not fit, the row wraps; nothing is
/// clipped, and the manage square ends the last line.
#[test]
fn a_row_that_does_not_fit_wraps_and_clips_nothing() {
    // 9 squares + manage at 256 px: (10 × 28 + 9 × 2) = 298 > 240 → two lines.
    let l = solve(band(256.0), &items(9, 0.0), NavigatorMode::Documents, false);
    assert_eq!(l.lines, 2);
    assert_eq!(l.height, SQUARE * 2.0 + GAP);
    let right = band(256.0).x + band(256.0).w;
    for c in &l.chips {
        assert_eq!(c.rect.w, SQUARE, "{:?} is whole", c.mode);
        assert!(c.rect.x + c.rect.w <= right, "{:?} inside the band", c.mode);
    }
    let second_line_y = band(256.0).y + SQUARE + GAP;
    assert_eq!(l.manage.y, second_line_y, "manage ends the last line");
    assert!(l.manage.x + l.manage.w <= right);
    // A labelled active chip wraps whole too.
    let l = solve(band(256.0), &items(9, 60.0), NavigatorMode::Socials, false);
    let socials = l
        .chips
        .iter()
        .find(|c| c.mode == NavigatorMode::Socials)
        .unwrap();
    assert!(socials.labelled);
    assert!(socials.rect.x + socials.rect.w <= right);
    assert_eq!(socials.rect.w, SQUARE + LABEL_GAP + 60.0 + LABEL_PAD_RIGHT);
}

/// An empty row is the manage square alone, and a zero-width band yields
/// zero-width chips rather than negative ones.
#[test]
fn edge_bands_stay_finite() {
    let l = solve(band(256.0), &[], NavigatorMode::Documents, false);
    assert!(l.chips.is_empty());
    assert_eq!(l.manage.x, band(256.0).x);
    let shut = Rect::new(0.0, 0.0, 0.0, SQUARE);
    let l = solve(shut, &items(3, 0.0), NavigatorMode::Documents, false);
    assert!(l.chips.iter().all(|c| c.rect.w == 0.0));
    assert_eq!(
        chip_at(&l, 0.0, 0.0),
        None,
        "a shut row answers for nothing"
    );
}
