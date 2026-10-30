// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{Choice, Confirm, button};
use koru_draw::text::TextEngine;
use vello::Scene;

fn capture(draw: impl FnOnce(&mut Scene, &mut TextEngine)) -> inspect::Snapshot {
    let mut engine = TextEngine::new();
    let frame = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        vello::kurbo::Rect::new(0.0, 0.0, 800.0, 600.0),
    );
    draw(&mut Scene::new(), &mut engine);
    frame.finish()
}

fn ancestor_role(snapshot: &inspect::Snapshot, mut at: usize) -> Option<&str> {
    loop {
        let record = &snapshot.records[at];
        if let Some(property) = record
            .properties
            .iter()
            .find(|p| p.name == "typography role")
        {
            return Some(&property.value);
        }
        at = record.parent?;
    }
}

#[test]
fn real_dialogs_publish_text_roles_and_resolved_runs_without_labeling_artwork() {
    for choice in [false, true] {
        let snapshot = capture(|canvas, engine| {
            if choice {
                Choice::new("Review changes", "Keep this synthetic draft?")
                    .answer("Keep draft")
                    .answer("Continue")
                    .defaulting_to(0)
                    .draw(canvas, Affine::IDENTITY, engine, (800.0, 600.0));
            } else {
                Confirm::new("Review changes", "Keep this synthetic draft?", "Continue").draw(
                    canvas,
                    Affine::IDENTITY,
                    engine,
                    (800.0, 600.0),
                );
            }
        });
        for (name, role) in [
            ("Dialog title", "theme::role::section_heading()"),
            ("Dialog description", "theme::role::row()"),
            ("Button label", "theme::role::row()"),
        ] {
            let at = snapshot
                .records
                .iter()
                .position(|r| r.name == name)
                .unwrap();
            assert_eq!(ancestor_role(&snapshot, at), Some(role));
            let expected_size = if name == "Dialog title" { "14" } else { "13" };
            let expected_weight = if name == "Dialog title" { "600" } else { "500" };
            assert!(snapshot.records.iter().enumerate().any(|(i, r)| {
                r.properties
                    .iter()
                    .any(|p| p.name == "font size" && p.value == expected_size)
                    && r.properties
                        .iter()
                        .any(|p| p.name == "font weight" && p.value == expected_weight)
                    && r.properties
                        .iter()
                        .any(|p| p.name == "rendered family" && !p.value.is_empty())
                    && ancestor_role(&snapshot, i) == Some(role)
            }));
        }
        for (i, record) in snapshot.records.iter().enumerate() {
            if record.name == "fill" {
                assert_eq!(ancestor_role(&snapshot, i), None, "paint is not text");
            }
        }
        if !choice {
            assert!(
                snapshot.records.iter().enumerate().any(|(i, r)| r
                    .properties
                    .iter()
                    .any(|p| p.name == "font size")
                    && ancestor_role(&snapshot, i).is_none()),
                "warning mark must not acquire the title role"
            );
        }
    }
}

#[test]
fn decorated_button_metadata_stops_at_the_label() {
    for variant in 0..5 {
        let snapshot = capture(|canvas, engine| {
            let rect = Rect::new(10.0, 10.0, 180.0, 32.0);
            match variant {
                0 => button::draw_google_button(
                    canvas,
                    Affine::IDENTITY,
                    engine,
                    rect,
                    "Continue",
                    false,
                    false,
                ),
                1 => button::draw_help_button(canvas, Affine::IDENTITY, engine, rect, false, false),
                2 => button::ActionButton::new(
                    "Continue",
                    button::ActionStyle::Ghost,
                    Some(koru_draw::icons::ENTER),
                )
                .draw(canvas, Affine::IDENTITY, engine, rect, false),
                3 => button::ActionButton::new("Continue", button::ActionStyle::Ghost, None)
                    .draw_disabled(canvas, Affine::IDENTITY, engine, rect),
                _ => button::draw_button_enabled(
                    canvas,
                    Affine::IDENTITY,
                    engine,
                    rect,
                    "Continue",
                    button::ButtonRole::Secondary,
                    false,
                    false,
                    false,
                ),
            }
        });
        assert_eq!(
            snapshot
                .records
                .iter()
                .filter(|r| r.name == "Button label")
                .count(),
            1
        );
        for (i, r) in snapshot.records.iter().enumerate() {
            if r.name == "fill" || r.name == "icon" {
                assert_eq!(ancestor_role(&snapshot, i), None);
            }
        }
        if variant == 2 {
            assert!(
                snapshot.records.iter().any(|r| r.name == "icon"),
                "the real trailing icon must have been captured"
            );
        }
        if variant == 1 {
            assert!(
                snapshot
                    .records
                    .iter()
                    .enumerate()
                    .any(|(i, r)| r.name == "icon"
                        && r.properties
                            .iter()
                            .any(|p| p.name == "icon name" && p.value == "QUESTION")
                        && ancestor_role(&snapshot, i).is_none()),
                "Help badge is an icon outside the label's typography role"
            );
        }
    }
}

#[test]
fn migrated_roles_preserve_multilingual_measurement_and_wrapping() {
    let mut engine = TextEngine::new();
    for stack in [
        "Inter, system-ui, sans-serif",
        "Manrope, system-ui, sans-serif",
    ] {
        for text in [
            "Keep this synthetic draft for later review",
            "Änderungen verwerfen oder weiter bearbeiten",
            "مرحبا بالعالم שלום",
            "Review 보상 日本語 😀",
        ] {
            for width in [100.0, 300.0] {
                for (new, old) in [
                    (
                        theme::role::section_heading(),
                        theme::TextStyle::new(
                            theme::text_metrics::UI_BODY,
                            theme::FontWeight::SemiBold,
                        ),
                    ),
                    (
                        button::LABEL,
                        theme::TextStyle::new(theme::text_metrics::SM, theme::FontWeight::Medium),
                    ),
                ] {
                    let before = engine.simple_layout_wrapped_in(text, old, stack, width);
                    let after = engine.simple_layout_wrapped_in(text, new, stack, width);
                    assert_eq!(
                        (before.width(), before.height()),
                        (after.width(), after.height())
                    );
                    let lines = |layout: &parley::Layout<theme::Brush>| {
                        layout
                            .lines()
                            .map(|line| {
                                let m = line.metrics();
                                (m.advance, m.baseline, m.line_height)
                            })
                            .collect::<Vec<_>>()
                    };
                    assert_eq!(lines(&before), lines(&after));
                }
            }
        }
    }
}

#[test]
fn migrated_recipes_do_not_reintroduce_local_style_composition() {
    for source in [
        include_str!("../confirm.rs"),
        include_str!("../choice.rs"),
        include_str!("../button.rs"),
    ] {
        assert!(
            !source.contains("TextStyle::new("),
            "use the shared typography role in migrated dialog recipes"
        );
    }
}
