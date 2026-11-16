// SPDX-License-Identifier: MPL-2.0

use super::*;

/// Every size a face is drawn at today.
const SIZES: [f32; 6] = [28.0, 36.0, 44.0, 46.0, 56.0, 96.0];

fn finite(affine: Affine) -> bool {
    affine.as_coeffs().iter().all(|c| c.is_finite())
}

#[test]
fn the_eight_eye_paths_parse_and_each_has_an_outline() {
    for index in 0..EYES.len() as u8 {
        let eye = eye(index);
        let bounds = eye.path.bounding_box();
        assert!(bounds.width() > 1.0 && bounds.height() > 1.0, "eye {index}");
        // Drawn inside the 16-unit cell the design gives an eye.
        assert!(bounds.x0 >= 0.0 && bounds.x1 <= 16.5, "eye {index}");
        assert!(bounds.y0 >= 0.0 && bounds.y1 <= 16.5, "eye {index}");
        assert!(eye.mid_y > bounds.y0 && eye.mid_y < bounds.y1);
    }
    // No two shapes are the same shape.
    for a in 0..EYES.len() {
        for b in a + 1..EYES.len() {
            assert_ne!(EYES[a], EYES[b]);
        }
    }
}

#[test]
fn every_row_names_a_tone_and_an_eye_that_exist_and_no_two_rows_are_the_same() {
    assert_eq!(ROWS.len(), usize::from(FACE_COUNT));
    let tones = theme::p().accents.len();
    assert_eq!(tones, theme::ACCENT_TOKENS.len());
    let mut used = vec![false; tones];
    for (index, row) in ROWS.iter().enumerate() {
        assert!(usize::from(row.accent) < tones, "row {index}");
        assert!(usize::from(row.eyes) < EYES.len(), "row {index}");
        used[usize::from(row.accent)] = true;
        for (other_index, other) in ROWS.iter().enumerate().skip(index + 1) {
            assert_ne!(row, other, "rows {index} and {other_index}");
        }
    }
    // The design's nine pairs stand on the nine tones, one each.
    assert!(used.iter().all(|used| *used), "{used:?}");
    // The first row is the design's first face, number for number.
    assert_eq!(ROWS[0], row(SKY, 4, 1.49, 2.67, -3.12, -0.11, -14.64));
    assert_eq!(ROWS[23], row(GREEN, 6, 0.09, 3.6, -2.45, 0.48, -0.9));
}

#[test]
fn every_face_builds_geometry_inside_its_tile_at_every_size() {
    for face in 0..FACE_COUNT {
        for size in SIZES {
            let g = geometry(face, size, FacePose::STILL);
            let side = f64::from(size);
            let tile = g.tile.rect();
            assert_eq!((tile.x0, tile.y0), (0.0, 0.0));
            assert!((tile.x1 - side).abs() < 1e-6 && (tile.y1 - side).abs() < 1e-6);
            let radii = g.tile.radii();
            assert!((radii.top_left - side * 0.28).abs() < 1e-6, "face {face}");
            assert!(finite(g.left_eye) && finite(g.right_eye) && finite(g.head));
            let left = (g.left_eye * g.eye.clone()).bounding_box();
            let right = (g.right_eye * g.eye.clone()).bounding_box();
            for (name, eye) in [("left", left), ("right", right)] {
                assert!(
                    eye.area() > 0.0,
                    "face {face} at {size}: {name} eye is empty"
                );
                assert!(
                    eye.x0 >= 0.0 && eye.y0 >= 0.0 && eye.x1 <= side && eye.y1 <= side,
                    "face {face} at {size}: {name} eye leaves the tile: {eye:?}"
                );
            }
            assert!(
                left.center().x < right.center().x,
                "face {face} at {size}: the eyes are crossed"
            );
            // The mouth sits inside the tile too, below the eyes' tops.
            let mouth = g.head * g.mouth;
            assert!(mouth.x > 0.0 && mouth.x < side && mouth.y > 0.0 && mouth.y < side);
            assert!(mouth.y > left.y0.min(right.y0), "face {face} at {size}");
            assert!((f64::from(g.mouth_size) - side * 0.2).abs() < 1e-4);
        }
    }
}

#[test]
fn the_right_eye_is_the_left_one_mirrored_about_the_head() {
    for face in 0..FACE_COUNT {
        // Without the tilt and offset of the head, in the head's own units.
        let g = geometry(face, 70.0, FacePose::STILL);
        let head = g.head.inverse();
        let left = (head * g.left_eye * g.eye.clone()).bounding_box();
        let right = (head * g.right_eye * g.eye.clone()).bounding_box();
        assert!((left.x0 - (44.0 - right.x1)).abs() < 1e-6, "face {face}");
        assert!((left.x1 - (44.0 - right.x0)).abs() < 1e-6, "face {face}");
        assert!((left.y0 - right.y0).abs() < 1e-6 && (left.y1 - right.y1).abs() < 1e-6);
    }
}

#[test]
fn distinct_rows_place_their_eyes_or_mouth_differently() {
    let signature = |face: u8| {
        let g = geometry(face, 70.0, FacePose::STILL);
        let left = (g.left_eye * g.eye.clone()).bounding_box();
        let mouth = g.head * g.mouth;
        let row = face_row(face);
        (
            row.accent,
            [left.x0, left.y0, left.x1, left.y1, mouth.x, mouth.y]
                .map(|v| (v * 1000.0).round() as i64),
        )
    };
    for a in 0..FACE_COUNT {
        for b in a + 1..FACE_COUNT {
            assert_ne!(
                signature(a),
                signature(b),
                "faces {a} and {b} look the same"
            );
        }
    }
}

#[test]
fn geometry_scales_with_the_size_and_an_index_past_the_table_wraps() {
    let small = geometry(5, 35.0, FacePose::STILL);
    let large = geometry(5, 70.0, FacePose::STILL);
    let a = (small.left_eye * small.eye.clone()).bounding_box();
    let b = (large.left_eye * large.eye.clone()).bounding_box();
    assert!((a.x0 * 2.0 - b.x0).abs() < 1e-6 && (a.y1 * 2.0 - b.y1).abs() < 1e-6);
    assert_eq!(face_row(FACE_COUNT + 3), face_row(3));
    assert_eq!(face_row(u8::MAX), face_row(u8::MAX % FACE_COUNT));
}

#[test]
fn a_still_pose_is_the_row_as_designed() {
    assert_eq!(FacePose::default(), FacePose::STILL);
    let row = face_row(0);
    let g = geometry(0, 70.0, FacePose::STILL);
    // translate(13 16) translate(head_x 0) rotate(tilt 22 22), on the pivot:
    // the pivot does not move under the rotation.
    let pivot = g.head * Point::new(22.0, 22.0);
    assert!((pivot.x - (13.0 + f64::from(row.head_x) + 22.0)).abs() < 1e-6);
    assert!((pivot.y - 38.0).abs() < 1e-6);
    // A point to the right of the pivot swings up for a negative tilt.
    let right = g.head * Point::new(32.0, 22.0);
    assert!(row.tilt < 0.0 && right.y < pivot.y);
    let expected = pivot.y + 10.0 * f64::from(row.tilt).to_radians().sin();
    assert!((right.y - expected).abs() < 1e-6);
}

#[test]
fn a_pose_is_a_pure_bounded_function_of_the_frame_step() {
    for face in 0..FACE_COUNT {
        let mut tilted = false;
        let mut blinked = false;
        for step in 0..(2 * BLINK_EVERY_MS / FACE_FRAME_MS) {
            let at = step * FACE_FRAME_MS;
            let pose = FacePose::at(at, face);
            assert_eq!(pose, FacePose::at(at, face));
            // Any instant inside the step is the step.
            assert_eq!(pose, FacePose::at(at + FACE_FRAME_MS - 1, face));
            assert!(pose.tilt.abs() <= SWAY_DEG + 1e-4, "face {face} at {at}");
            assert!((BLINK_SHUT..=1.0).contains(&pose.eyes_open));
            tilted |= pose.tilt.abs() > SWAY_DEG * 0.9;
            blinked |= pose.eyes_open < 0.5;
        }
        assert!(tilted, "face {face} never sways");
        assert!(blinked, "face {face} never blinks");
    }
    // Neighbours are out of step, so a roster does not nod in unison.
    assert_ne!(FacePose::at(1600, 0).tilt, FacePose::at(1600, 1).tilt);
    // The sway eases in: the first step of any face is close to rest.
    for face in 0..FACE_COUNT {
        assert!(FacePose::at(0, face).tilt.abs() < 1e-4);
    }
}

#[test]
fn a_moving_pose_changes_two_transforms_and_no_size() {
    let still = geometry(7, 46.0, FacePose::STILL);
    let moving = geometry(
        7,
        46.0,
        FacePose {
            tilt: 4.0,
            eyes_open: 0.12,
        },
    );
    assert_eq!(still.tile, moving.tile);
    assert_eq!(still.mouth, moving.mouth);
    assert_eq!(still.mouth_size, moving.mouth_size);
    assert_ne!(still.head, moving.head);
    let open = (still.left_eye * still.eye.clone()).bounding_box();
    let shut = (moving.left_eye * moving.eye.clone()).bounding_box();
    assert!(shut.height() < open.height() * 0.5);
    assert!(shut.area() > 0.0);
}

#[test]
fn one_clock_gives_every_face_one_deadline_and_only_working_faces_move() {
    let t0 = Instant::now();
    let mut clock = FaceClock::new();
    // Nothing is working: still, and the loop sleeps.
    assert!(!clock.animating());
    assert_eq!(clock.deadline(t0), None);
    assert_eq!(clock.pose(3, true, t0), FacePose::STILL);

    assert!(clock.set_working(true, t0));
    // Saying it again is not a change, and does not restart the clock.
    let later = t0 + Duration::from_millis(1730);
    assert!(!clock.set_working(true, later));
    assert!(clock.animating());
    // A working face takes the pose of the elapsed time; an idle one beside
    // it stays still.
    assert_eq!(clock.pose(3, true, later), FacePose::at(1730, 3));
    assert_ne!(clock.pose(3, true, later), FacePose::STILL);
    assert_eq!(clock.pose(3, false, later), FacePose::STILL);
    // One deadline, on the frame grid, whichever face asks and however many.
    assert_eq!(
        clock.deadline(later),
        Some(t0 + Duration::from_millis(1750))
    );
    let on_the_step = t0 + Duration::from_millis(1750);
    assert_eq!(
        clock.deadline(on_the_step),
        Some(t0 + Duration::from_millis(1800))
    );
    // A late frame still names a deadline in the future.
    let late = t0 + Duration::from_millis(60_010);
    assert!(clock.deadline(late).unwrap() > late);
    assert!(clock.deadline(late).unwrap() <= late + Duration::from_millis(FACE_FRAME_MS));

    // Work ends: still again, and nothing more is asked for.
    assert!(clock.set_working(false, late));
    assert!(!clock.set_working(false, late));
    assert!(!clock.animating());
    assert_eq!(clock.deadline(late), None);
    assert_eq!(clock.pose(3, true, late), FacePose::STILL);
    // Work begins again from the first step, easing in.
    assert!(clock.set_working(true, late));
    assert_eq!(clock.pose(3, true, late), FacePose::at(0, 3));
}

#[test]
fn reduced_motion_holds_every_face_still_and_asks_for_no_frames() {
    let t0 = Instant::now();
    let mut clock = FaceClock::new();
    assert!(clock.set_reduced_motion(true));
    assert!(!clock.set_reduced_motion(true));
    assert!(clock.set_working(true, t0));
    let later = t0 + Duration::from_millis(1730);
    assert!(!clock.animating());
    assert_eq!(clock.deadline(later), None);
    assert_eq!(clock.pose(3, true, later), FacePose::STILL);
    // Turning the setting off mid-work starts the motion.
    assert!(clock.set_reduced_motion(false));
    assert!(clock.animating());
    assert!(clock.deadline(later).is_some());
}

#[test]
fn the_initial_is_the_first_character_upper_cased_where_there_is_case() {
    for (name, expected) in [
        ("scout", "S"),
        ("  élan ", "É"),
        ("Scout", "S"),
        ("", ""),
        ("   ", ""),
        ("7 of 9", "7"),
        // No case: as written.
        ("日本語のボット", "日"),
        ("مساعد", "م"),
        ("бот", "Б"),
        ("ωmega", "Ω"),
        // One letter stays one letter.
        ("ßeta", "ß"),
        // A decomposed accent stays with its letter.
        ("e\u{301}lan", "E\u{301}"),
        // An emoji keeps its variation selector and its skin tone; a flag
        // keeps both halves.
        ("\u{2764}\u{FE0F} helper", "\u{2764}\u{FE0F}"),
        ("\u{1F44D}\u{1F3FD}bot", "\u{1F44D}\u{1F3FD}"),
        ("\u{1F1E8}\u{1F1ED} bot", "\u{1F1E8}\u{1F1ED}"),
    ] {
        assert_eq!(initial(name), expected, "{name:?}");
    }
}

#[test]
fn every_face_paints_at_every_size_with_and_without_a_name() {
    let mut engine = TextEngine::new();
    for face in 0..FACE_COUNT {
        for size in SIZES {
            let at = Rect::new(3.0, 5.0, size, size);
            let mut nameless = Scene::new();
            Face::new(face, "").draw(&mut nameless, Affine::IDENTITY, &mut engine, at);
            let tags = nameless.encoding().path_tags.len();
            assert!(tags > 0, "face {face} at {size}");
            // A name adds the mouth; a moving pose adds nothing.
            let mut named = Scene::new();
            Face::new(face, "Scout").draw(&mut named, Affine::IDENTITY, &mut engine, at);
            let mut moving = Scene::new();
            Face::new(face, "Scout")
                .posed(FacePose::at(1700, face))
                .draw(&mut moving, Affine::IDENTITY, &mut engine, at);
            assert_eq!(
                named.encoding().path_tags.len(),
                moving.encoding().path_tags.len()
            );
            assert!(named.encoding().n_paths >= nameless.encoding().n_paths);
        }
    }
    // Nothing is drawn into no room.
    let mut empty = Scene::new();
    Face::new(0, "Scout").draw(
        &mut empty,
        Affine::IDENTITY,
        &mut engine,
        Rect::new(0.0, 0.0, 0.0, 0.0),
    );
    assert!(empty.encoding().path_tags.is_empty());
}

#[test]
fn a_face_takes_its_two_colors_from_the_palette_it_is_given() {
    // The same row is a different pair in each theme: nothing is hard-coded.
    let row = face_row(4);
    let light = &theme::LIGHT.accents[usize::from(row.accent)];
    let dark = &theme::DARK.accents[usize::from(row.accent)];
    assert_ne!(light.fill, dark.fill);
    assert_ne!(light.ink, dark.ink);
    let mut engine = TextEngine::new();
    for table in [&theme::LIGHT, &theme::DARK] {
        // Ink is never its own fill, in either theme.
        for row in &ROWS {
            let tone = &table.accents[usize::from(row.accent)];
            assert_ne!(tone.fill, tone.ink);
        }
        let mut scene = Scene::new();
        Face::new(4, "Scout").draw_in(
            &mut scene,
            Affine::IDENTITY,
            &mut engine,
            Rect::new(0.0, 0.0, 46.0, 46.0),
            table,
        );
        assert!(!scene.encoding().path_tags.is_empty());
    }
}
