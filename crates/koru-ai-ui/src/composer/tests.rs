// SPDX-License-Identifier: MPL-2.0

//! The chip composer on its own: the policy's promises, the chip, the message.

use super::*;
use koru_ai_core::DocumentId;
use winit::keyboard::NamedKey;

fn typed(composer: &mut ChatComposer, text: &str) {
    for ch in text.chars() {
        let s = ch.to_string();
        composer.key(
            &Key::Character(s.as_str().into()),
            Some(&s),
            ModifiersState::empty(),
        );
    }
}

fn press(composer: &mut ChatComposer, key: NamedKey) -> bool {
    composer.key(&Key::Named(key), None, ModifiersState::empty())
}

fn ctrl(composer: &mut ChatComposer, letter: &str) -> bool {
    composer.key(&Key::Character(letter.into()), None, koru_keys::PRIMARY)
}

/// Type `@`, answer the picker with one document, choose it: a chip.
fn chip(composer: &mut ChatComposer, title: &str) -> DocumentId {
    let document = DocumentId::new();
    typed(composer, "@");
    assert!(
        composer.picker_open(),
        "`@` at a word start opens the picker"
    );
    assert!(composer.offer(&[(document, title)]));
    assert!(press(composer, NamedKey::Enter));
    assert!(!composer.picker_open(), "choosing closes the picker");
    document
}

#[test]
fn a_chosen_document_sits_in_the_text_as_a_chip_and_leaves_as_its_link() {
    let mut composer = ChatComposer::new(None);
    typed(&mut composer, "Summarise ");
    let document = chip(&mut composer, "Budget 2026");
    // The editor leaves a space after a chip, as it does in any page.
    typed(&mut composer, "please");

    let message = composer.message();
    assert_eq!(
        message.markdown,
        format!("Summarise [[koru://doc/{document}]] please")
    );
    assert_eq!(
        message.documents,
        vec![(document, "Budget 2026".to_owned())]
    );
}

#[test]
fn at_inside_a_word_does_nothing() {
    let mut composer = ChatComposer::new(None);
    typed(&mut composer, "mail@example");
    assert!(!composer.picker_open());
    assert_eq!(composer.text(), "mail@example");
}

#[test]
fn escape_closes_the_picker_and_leaves_the_text() {
    let mut composer = ChatComposer::new(None);
    typed(&mut composer, "ask @bud");
    assert!(composer.picker_open());
    assert!(press(&mut composer, NamedKey::Escape));
    assert!(!composer.picker_open());
    assert_eq!(composer.text(), "ask @bud");
}

#[test]
fn a_chip_survives_an_edit_and_undo_and_redo() {
    let mut composer = ChatComposer::new(None);
    let document = chip(&mut composer, "Plan");
    let with_chip = composer.text();
    typed(&mut composer, "more");
    assert_eq!(composer.text(), format!("{with_chip}more"));

    assert!(ctrl(&mut composer, "z"));
    assert_eq!(composer.text(), with_chip, "undo takes the typing back");
    assert_eq!(composer.message().documents[0].0, document);

    assert!(composer.key(&Key::Character("z".into()), None, koru_keys::PRIMARY_SHIFT,));
    assert_eq!(composer.text(), format!("{with_chip}more"));
    assert_eq!(composer.message().documents[0].0, document);

    // Undo past the chip takes it out; redo puts the same document back.
    assert!(ctrl(&mut composer, "z"));
    while composer.message().documents.len() == 1 {
        if !ctrl(&mut composer, "z") {
            break;
        }
    }
    assert!(composer.message().documents.is_empty());
    assert!(composer.key(&Key::Character("z".into()), None, koru_keys::PRIMARY_SHIFT,));
    let mut guard = 0;
    while composer.message().documents.is_empty() && guard < 8 {
        composer.key(&Key::Character("z".into()), None, koru_keys::PRIMARY_SHIFT);
        guard += 1;
    }
    assert_eq!(composer.message().documents[0].0, document);
}

#[test]
fn a_multi_line_paste_lands_at_the_caret_as_plain_lines() {
    let mut composer = ChatComposer::new(None);
    typed(&mut composer, "ab");
    assert!(press(&mut composer, NamedKey::ArrowLeft));
    koru_editor::clipboard::insert_lines(&mut composer.editor, &mut composer.ws, "x\r\ny\nz");

    assert_eq!(
        composer.text(),
        "ax\ny\nzb",
        "one newline per break, at the caret"
    );
    assert!(
        composer
            .editor
            .body
            .rows
            .iter()
            .all(|row| row.kind == koru_core::node::Kind::Paragraph && row.heading.is_none()),
        "every line is a plain paragraph"
    );
    assert!(ctrl(&mut composer, "z"));
    assert_eq!(composer.text(), "ab", "the paste is one undo step");
}

#[test]
fn a_hash_at_line_start_is_not_a_heading_and_slash_opens_no_menu() {
    let mut composer = ChatComposer::new(None);
    typed(&mut composer, "# Title");
    let row = &composer.editor.body.rows[0];
    assert!(row.heading.is_none());
    assert_eq!(composer.text(), "# Title");

    let mut composer = ChatComposer::new(None);
    typed(&mut composer, "/usage");
    assert!(matches!(composer.editor.view.overlay, Overlay::None));
    typed(&mut composer, " - [] ```");
    assert_eq!(composer.text(), "/usage - [] ```");
    assert!(
        composer
            .editor
            .body
            .rows
            .iter()
            .all(|row| row.kind == koru_core::node::Kind::Paragraph)
    );
}

#[test]
fn block_chords_are_refused() {
    let mut composer = ChatComposer::new(None);
    typed(&mut composer, "words");
    // Ctrl+B (bold) and Ctrl+Enter (a block command in a document) are not a
    // message's: the key is unhandled and the text is untouched.
    assert!(!ctrl(&mut composer, "b"));
    assert!(!composer.key(&Key::Named(NamedKey::Enter), None, koru_keys::PRIMARY,));
    assert_eq!(composer.text(), "words");
    assert_eq!(composer.editor.body.rows.len(), 1);
}

#[test]
fn a_break_is_a_new_plain_paragraph_and_one_newline() {
    let mut composer = ChatComposer::new(None);
    typed(&mut composer, "one");
    assert!(composer.insert_break());
    typed(&mut composer, "two");
    assert_eq!(composer.text(), "one\ntwo");
    assert_eq!(composer.editor.body.rows.len(), 2);
}

#[test]
fn mixed_arabic_and_latin_round_trips() {
    let mut composer = ChatComposer::new(None);
    composer.set_text("مرحبا Koru، اقرأ الملف README.md");
    assert_eq!(composer.text(), "مرحبا Koru، اقرأ الملف README.md");
}

#[test]
fn a_provisional_dictation_never_enters_the_message() {
    use koru_ui::dictation::DictationTarget;
    let mut composer = ChatComposer::new(None);
    typed(&mut composer, "Note: ");
    assert!(composer.begin_composition(()));
    composer.update_composition((), "draft words");
    assert!(composer.composing());
    assert_eq!(composer.text(), "Note: ", "the partial is provisional");
    composer.cancel_composition(());
    assert!(!composer.composing());
    assert_eq!(composer.text(), "Note: ");
}

#[test]
fn clearing_starts_an_empty_message_with_no_history() {
    let mut composer = ChatComposer::new(None);
    typed(&mut composer, "sent already");
    composer.clear();
    assert!(composer.is_blank());
    ctrl(&mut composer, "z");
    assert!(composer.is_blank(), "undo does not reach a sent message");
}

/// The composer is one line tall at the bottom of the chat; its picker has to
/// float above it, over the conversation, and stay inside the room it is given.
#[test]
fn the_picker_floats_above_a_one_line_composer() {
    let mut composer = ChatComposer::new(None);
    let text = Rect::new(20.0, 600.0, 400.0, 38.0);
    let room = Rect::new(0.0, 46.0, 460.0, 592.0);
    composer.text_height(text.w - 2.0 * PAD_X, 1, 8);
    composer.place(text, room);
    typed(&mut composer, "@");
    let panel = composer.picker_rect().expect("the picker is open");
    assert!(
        panel.y + panel.h <= text.y + PAD_TOP,
        "{panel:?} over {text:?}"
    );
    assert!(
        panel.y >= room.y && panel.x >= room.x,
        "{panel:?} inside {room:?}"
    );
    assert!(composer.contains(panel.x + 4.0, panel.y + 4.0));
}

#[test]
fn the_composer_grows_line_by_line_and_stops_at_the_cap() {
    let mut composer = ChatComposer::new(None);
    let one = composer.text_height(300.0, 1, 8);
    composer.set_text("1\n2\n3");
    let three = composer.text_height(300.0, 1, 8);
    composer.set_text("1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11");
    let capped = composer.text_height(300.0, 1, 8);
    composer.set_text("1\n2\n3\n4\n5\n6\n7\n8");
    let eight = composer.text_height(300.0, 1, 8);
    assert!(three > one * 2.5);
    assert!((capped - eight).abs() < 0.5, "{capped} vs {eight}");
}

/// **The chat is not a document.** Its editor is set in the app's font and
/// size, like the messages above it, so no document font, size or preview of
/// one reaches it.
#[test]
fn the_composer_is_set_in_the_apps_face() {
    use koru_draw::theme;
    let composer = ChatComposer::new(None);
    assert_eq!(composer.editor.body.text_scale, koru_editor::TextScale::Ui);
    let metrics = composer.editor.body.metrics();
    assert_eq!(metrics, theme::DocMetrics::ui());
    assert_eq!(metrics.face, theme::TextFace::Ui);
    assert_eq!(
        metrics.scale * theme::FONT_SIZE,
        theme::role::ui().metrics.size,
        "the app's text size"
    );
    let row = composer.editor.body.first_body_row();
    assert_eq!(
        composer.editor.body.rows[row].font_choice(),
        theme::ui_font(),
        "the app font, not the document body font"
    );
    assert_eq!(line_height(), crate::chat_line_height());
}

/// **Up and Down leave the text only from its edge lines**: the chat walks
/// back through earlier messages from the first line and forward from the
/// last, and inside a longer message the arrows stay the editor's.
#[test]
fn the_caret_knows_when_it_is_on_the_first_or_the_last_line() {
    let mut composer = ChatComposer::new(None);
    assert!(composer.caret_on_first_line() && composer.caret_on_last_line());
    composer.set_text("one\ntwo\nthree");
    assert_eq!(composer.text(), "one\ntwo\nthree");
    assert!(
        composer.caret_on_last_line(),
        "a set text leaves the caret at its end"
    );
    assert!(!composer.caret_on_first_line());
    assert!(press(&mut composer, NamedKey::ArrowUp));
    assert!(!composer.caret_on_first_line() && !composer.caret_on_last_line());
    assert!(press(&mut composer, NamedKey::ArrowUp));
    assert!(composer.caret_on_first_line());
    assert!(!composer.caret_on_last_line());
}

/// A stashed message comes back whole — chip, text and caret — after the
/// composer has shown something else.
#[test]
fn a_stashed_message_comes_back_with_its_chip() {
    let mut composer = ChatComposer::new(None);
    typed(&mut composer, "about ");
    let document = chip(&mut composer, "Roadmap");
    let before = composer.text();
    let draft = composer.stash();
    assert!(composer.is_blank(), "the stash leaves an empty message");
    composer.set_text("an earlier message");
    composer.unstash(draft);
    assert_eq!(composer.text(), before);
    assert_eq!(composer.message().documents[0].0, document);
    typed(&mut composer, "x");
    assert_eq!(
        composer.text(),
        format!("{before}x"),
        "the caret is where it was"
    );
}
