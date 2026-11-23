// SPDX-License-Identifier: MPL-2.0

use super::*;
use winit::event::Ime;

fn fixture(text: &str) -> (TextField, TextEngine) {
    let mut engine = TextEngine::new();
    let field = TextField::with_text(&mut engine, theme::role::row(), text);
    (field, engine)
}

#[test]
fn native_preedit_is_visible_but_authored_reads_and_history_stay_clean() {
    let (mut field, mut engine) = fixture("keep");
    field.select_all(&mut engine);
    field.ime(&mut engine, &Ime::Preedit("日本".into(), Some((3, 3))));
    assert_eq!(field.raw_text(), "keep");
    assert_eq!(field.visible_text(), "日本");
    assert_eq!(field.caret(), 3);
    field.ime(&mut engine, &Ime::Preedit("日本".into(), Some((0, 3))));
    assert_eq!(field.editor.raw_selection().text_range(), 0..3);
    field.ime(&mut engine, &Ime::Preedit(String::new(), None));
    assert_eq!(field.visible_text(), "keep");
    field.ime(&mut engine, &Ime::Commit("日本語".into()));
    assert_eq!(field.raw_text(), "日本語");
    field.history.undo(&mut field.editor, &mut engine);
    assert_eq!(field.raw_text(), "keep");
    assert!(field.history.past.is_empty());
}

#[test]
fn native_cancel_and_ordinary_input_restore_authored_selection() {
    let (mut field, mut engine) = fixture("one old end");
    field
        .editor
        .driver(&mut engine.font_cx, &mut engine.layout_cx)
        .select_byte_range(7, 4);
    field.ime(&mut engine, &Ime::Preedit("仮".into(), Some((3, 3))));
    field.ime_cancel(&mut engine);
    assert_eq!(field.editor.raw_selection().anchor().index(), 7);
    assert_eq!(field.editor.raw_selection().focus().index(), 4);
    assert!(field.history.past.is_empty());
    field.ime(&mut engine, &Ime::Preedit("仮".into(), Some((3, 3))));
    field.key(
        &mut engine,
        &Key::Character("X".into()),
        Some("X"),
        ModifiersState::empty(),
    );
    assert_eq!(field.raw_text(), "one X end");
    field.history.undo(&mut field.editor, &mut engine);
    assert_eq!(field.raw_text(), "one old end");
}

#[test]
fn native_empty_commit_and_source_exclusion_preserve_voice_behavior() {
    let (mut field, mut engine) = fixture("keep");
    field.select_all(&mut engine);
    field.ime(&mut engine, &Ime::Commit(String::new()));
    assert_eq!(field.raw_text(), "keep");
    field.ime(&mut engine, &Ime::Preedit("仮".into(), None));
    field.commit_composition(&mut engine, "voice");
    field.cancel_composition(&mut engine);
    assert_eq!(field.visible_text(), "仮");
    field.ime_cancel(&mut engine);
    assert!(field.begin_composition(&mut engine));
    field.update_composition(&mut engine, "voice");
    assert!(!field.ime(&mut engine, &Ime::Commit("native".into())));
    assert!(!field.ime_cancel(&mut engine));
    field.commit_composition(&mut engine, "voice");
    assert_eq!(field.raw_text(), "voice");
}

#[test]
fn native_cursor_geometry_tracks_scroll_and_hidden_invalid_cursors() {
    let (mut field, mut engine) = fixture("long long long long long");
    let rect = koru_draw::geometry::Rect::new(120.0, 80.0, 90.0, 32.0);
    field.ime(&mut engine, &Ime::Preedit("日本".into(), Some((3, 3))));
    field.prepare(&mut engine, rect.w - 2.0 * PAD, true);
    let area = field.ime_cursor_area(rect).unwrap();
    assert!(area.x >= rect.x && area.x <= rect.x + rect.w);
    assert!(area.y >= rect.y && area.y < rect.y + rect.h);
    field.ime(&mut engine, &Ime::Preedit("日本".into(), None));
    assert!(field.ime_cursor_area(rect).is_none());
    field.ime(
        &mut engine,
        &Ime::Preedit("日本".into(), Some((1, usize::MAX))),
    );
    assert!(field.ime_cursor_area(rect).is_none());
    field.ime(&mut engine, &Ime::Commit("日本語".into()));
    assert!(field.raw_text().ends_with("日本語"));
}
