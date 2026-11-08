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
fn chat_picker_shows_window_recents_and_keeps_query_when_scope_widens() {
    let current = koru_ai_core::WorkspaceId::new();
    let other = koru_core::WorkspaceId::new();
    let profile = koru_core::ProfileId::new();
    let document = koru_core::DocumentId::new();
    let mut composer = ChatComposer::new(None);
    composer.set_context(Some(current), None);
    composer.workspaces = vec![
        WorkspaceChoice {
            profile,
            profile_name: String::new(),
            id: koru_core::WorkspaceId::from_uuid(current.as_uuid()),
            name: "Current".into(),
        },
        WorkspaceChoice {
            profile,
            profile_name: String::new(),
            id: other,
            name: "Other".into(),
        },
    ];
    composer.set_recent_documents(vec![(document, "Recent plan".into())]);
    typed(&mut composer, "@");
    let Overlay::Entity(picker) = &composer.editor.view.overlay else {
        panic!("@ picker open");
    };
    assert_eq!(picker.hits[0].target.document, document);
    assert_eq!(
        picker.selected_workspaces,
        [koru_core::WorkspaceId::from_uuid(current.as_uuid())]
    );

    typed(&mut composer, "plan");
    assert!(composer.key(&Key::Named(NamedKey::Space), None, ModifiersState::CONTROL));
    let Overlay::Entity(picker) = &composer.editor.view.overlay else {
        panic!("@ picker remains open");
    };
    assert_eq!(picker.query, "plan");
    assert_eq!(picker.selected_workspaces.len(), 2);
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

/// **The chat is not a document.** Its editor is set in the chat font — the
/// app font until chosen — at the app's size, like the messages above it, so
/// no document font, size or preview of one reaches it.
#[test]
fn the_composer_is_set_in_the_chats_face() {
    use koru_draw::theme;
    let composer = ChatComposer::new(None);
    assert_eq!(
        composer.editor.body.text_scale,
        koru_editor::TextScale::Chat
    );
    let metrics = composer.editor.body.metrics();
    assert_eq!(metrics, theme::DocMetrics::chat());
    assert_eq!(metrics.face, theme::TextFace::Chat);
    assert_eq!(
        metrics.scale * theme::FONT_SIZE,
        theme::role::ui().metrics.size,
        "the app's text size"
    );
    let row = composer.editor.body.first_body_row();
    assert_eq!(
        composer.editor.body.rows[row].font_choice(),
        theme::chat_font(),
        "the chat font, not the document body font"
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

/// **A sent message comes back with its chips** — a document's and a
/// heading's, on any line — captioned by the title it is given, and leaves
/// again spelled exactly as it was sent. A token that is not a document's
/// stays text.
#[test]
fn a_sent_message_comes_back_with_its_chips() {
    let mut composer = ChatComposer::new(None);
    let budget = DocumentId::new();
    let plan = DocumentId::new();
    let heading = NodeId::new();
    let sent = format!(
        "Compare [[koru://doc/{budget}]] with\n[[koru://doc/{plan}#{heading}]] and [[koru://chat/{}]]",
        uuid::Uuid::new_v4()
    );
    composer.set_message(&sent, |document| {
        if document == budget { "Budget" } else { "Plan" }.to_owned()
    });

    let message = composer.message();
    let canonical = sent.replace("[[koru://chat/", r"\[[koru://chat/");
    assert_eq!(message.markdown, canonical, "a text link stays literal");
    assert_eq!(
        message.documents,
        vec![(budget, "Budget".to_owned()), (plan, "Plan".to_owned())]
    );
    typed(&mut composer, "!");
    assert_eq!(
        composer.text(),
        format!("{canonical}!"),
        "edited at its end"
    );

    // Undo takes the whole recall back in one step.
    assert!(ctrl(&mut composer, "z"));
    assert!(ctrl(&mut composer, "z"));
    assert!(composer.is_blank());
}

#[test]
fn selected_agent_pills_and_literal_marker_recall_without_reclassification() {
    let mut composer = ChatComposer::new(None);
    let sent = r"literal $review-pr and \[[koru://skill/review-pr]] then [[koru://skill/review-pr]] with [[koru://command/issue%2342]]";
    composer.set_message(sent, |_| String::new());
    let message = composer.message();
    assert_eq!(message.markdown, sent);
    assert_eq!(message.skills, vec!["review-pr"]);
    let row = composer.ws.children(Parent::Node(composer.root)).unwrap()[0];
    let node = composer.ws.read(row).unwrap();
    let roles: Vec<_> = node
        .content
        .attrs()
        .iter()
        .map(|attr| attr.role.as_str())
        .collect();
    assert_eq!(
        roles
            .iter()
            .filter(|role| **role == koru_core::attributes::ROLE_SKILL)
            .count(),
        1,
    );
    assert_eq!(
        roles
            .iter()
            .filter(|role| **role == koru_core::attributes::ROLE_COMMAND)
            .count(),
        1,
    );
}

// ----- quotes (koru-ai D88) ---------------------------------------------------

/// **A quote is a chip on its own line, the caret under it**: two quotes,
/// each followed by its comment, interleave in the order they were added
/// and go out as blockquotes before the text under them.
#[test]
fn quotes_interleave_with_their_comments_and_send_as_blockquotes() {
    let mut composer = ChatComposer::new(None);
    assert!(composer.insert_quote("g work, so \"workspaces\"\nstay"));
    typed(&mut composer, "something here");
    assert!(composer.insert_quote("one line"));
    typed(&mut composer, "Another one");
    assert_eq!(
        composer.text(),
        "> g work, so \"workspaces\"\n> stay\n\nsomething here\n\n> one line\n\nAnother one"
    );
    assert!(!composer.is_blank());
    assert!(
        !composer.insert_quote(" \n "),
        "an empty passage is not a quote"
    );
}

/// **A quote lands at the caret**: text before it stays above the chip,
/// text after it moves under, the caret at its start.
#[test]
fn a_quote_splits_the_line_at_the_caret() {
    let mut composer = ChatComposer::new(None);
    typed(&mut composer, "before after");
    for _ in 0.."after".len() {
        press(&mut composer, NamedKey::ArrowLeft);
    }
    assert!(composer.insert_quote("passage"));
    typed(&mut composer, "X");
    assert_eq!(composer.text(), "before \n\n> passage\n\nXafter");
}

/// **Backspace removes a quote** as it removes a document chip, and undo
/// brings it back.
#[test]
fn backspace_removes_a_quote_chip() {
    let mut composer = ChatComposer::new(None);
    assert!(composer.insert_quote("passage"));
    // The caret is on the empty line under the chip: one Backspace joins
    // the lines, the next takes the chip.
    assert!(press(&mut composer, NamedKey::Backspace));
    assert!(press(&mut composer, NamedKey::Backspace));
    assert!(composer.is_blank(), "{:?}", composer.text());
    assert!(ctrl(&mut composer, "z"));
    assert!(composer.text().contains("> passage"));
}

/// **A recalled message brings its quotes back as chips** and leaves again
/// spelled exactly as it was sent.
#[test]
fn a_recalled_message_brings_its_quotes_back() {
    let mut composer = ChatComposer::new(None);
    let sent = "intro\n\n> first\n>\n> passage\n\nsomething here\n\n> other\n\nAnother one";
    composer.set_message(sent, |_| String::new());
    assert_eq!(composer.text(), sent);
    let quotes = composer
        .ws
        .children(Parent::Node(composer.root))
        .unwrap()
        .into_iter()
        .filter(|row| {
            let node = composer.ws.read(*row).unwrap();
            node.content
                .attrs()
                .iter()
                .any(|attr| attr.role == koru_core::attributes::ROLE_QUOTE)
        })
        .count();
    assert_eq!(quotes, 2, "each blockquote is a chip again");

    let mut composer = ChatComposer::new(None);
    composer.set_message("> only a quote", |_| String::new());
    typed(&mut composer, "and a comment");
    assert_eq!(composer.text(), "> only a quote\n\nand a comment");
}
