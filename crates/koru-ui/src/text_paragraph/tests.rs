// SPDX-License-Identifier: MPL-2.0

use super::*;

fn engine() -> TextEngine {
    TextEngine::new()
}

#[test]
fn search_highlights_keep_original_unicode_boundaries() {
    let text = "Ärger español português İ";
    let matched: Vec<_> = highlight_ranges(text, "ÄRG ESPAÑ PORTUGUÊ İ")
        .into_iter()
        .map(|range| &text[range])
        .collect();
    assert_eq!(matched, ["Ärg", "españ", "portuguê", "İ"]);
    assert_eq!(highlight_ranges("İ", "i"), vec![0..2]);
    assert!(highlight_ranges(text, " ").is_empty());
}

#[test]
fn measure_is_the_wrapped_layouts_height() {
    let mut engine = engine();
    let paragraph = KoruTextParagraph::new(
        "Koru keeps your work on your devices. Your account only handles sync, sharing and downloads — never your content.",
        theme::role::row(),
        "theme::role::row()",
        Color::BLACK,
    );
    let narrow = paragraph.measure(&mut engine, 200.0);
    let wide = paragraph.measure(&mut engine, 900.0);
    assert!(narrow > wide, "a narrower wrap wraps to more lines");
}

#[test]
fn a_drawn_paragraph_owns_its_lines_as_children() {
    let mut engine = engine();
    let mut scene_out = Scene::new();
    let root = Affine::IDENTITY;
    let paragraph = KoruTextParagraph::new(
        "Your work stays on your devices. Start locally now, or sign in to connect devices.",
        theme::role::row(),
        "theme::role::row()",
        Color::BLACK,
    );
    let capture = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, 900.0, 650.0));
    let used = paragraph.draw(
        &mut scene_out,
        root,
        &mut engine,
        Rect::new(10.0, 20.0, 180.0, 0.0),
    );
    let snapshot = capture.finish();
    let paragraph_index = snapshot
        .records
        .iter()
        .position(|r| r.name == "KoruTextParagraph")
        .unwrap();
    assert_eq!(
        snapshot.records[paragraph_index].rect(),
        scene::rect(10.0, 20.0, 180.0, used)
    );
    let lines: Vec<_> = snapshot
        .records
        .iter()
        .filter(|r| r.name == "text line")
        .collect();
    assert!(
        lines.len() >= 2,
        "the copy wraps to more than one line at 180 px"
    );
    for record in &snapshot.records {
        if record.name == "text line" || record.name == "Text" {
            let mut ancestor = record.parent;
            let mut inside = false;
            while let Some(index) = ancestor {
                if index == paragraph_index {
                    inside = true;
                    break;
                }
                ancestor = snapshot.records[index].parent;
            }
            assert!(inside, "{} escapes the paragraph boundary", record.name);
        }
    }
}

#[test]
fn a_single_line_carries_its_typography_role() {
    let mut engine = engine();
    let mut scene_out = Scene::new();
    let root = Affine::IDENTITY;
    let capture = inspect::Frame::begin(2, root, scene::rect(0.0, 0.0, 900.0, 650.0));
    draw_line(
        &mut scene_out,
        root,
        &mut engine,
        "Display name",
        24.0,
        174.0,
        theme::role::meta(),
        "theme::role::meta()",
        Color::BLACK,
    );
    let snapshot = capture.finish();
    // One visual line is one node: the declared role and the actual Parley
    // line box are properties of the same record, not two nested records that
    // the inspector would show under the same "Text line" name.
    let lines: Vec<_> = snapshot
        .records
        .iter()
        .filter(|r| r.name == "Text line")
        .collect();
    assert_eq!(lines.len(), 1);
    let line = lines[0];
    assert!(
        line.properties
            .iter()
            .any(|p| p.name == "typography role" && p.value == "theme::role::meta()")
    );
    assert!(
        line.properties
            .iter()
            .any(|p| p.name == "requested size / line height" && p.value == "12 / 16")
    );
    assert!(
        line.properties
            .iter()
            .any(|p| p.name == "line height" && p.value == "16")
    );
    assert!(
        !snapshot
            .records
            .iter()
            .any(|r| r.name == "label line" || r.name == "text line"),
        "a second line node nested under the first is the doubling this collapses"
    );
    // Its children are the glyph runs and nothing else.
    let at = snapshot
        .records
        .iter()
        .position(|r| r.name == "Text line")
        .unwrap();
    for (i, record) in snapshot.records.iter().enumerate() {
        if record.parent == Some(at) {
            assert_eq!(record.name, "text run", "unexpected child {record:?}");
            assert!(snapshot.records[i].parent.is_some());
        }
    }
}
