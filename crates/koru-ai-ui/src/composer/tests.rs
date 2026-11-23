// SPDX-License-Identifier: MPL-2.0

//! The chip composer on its own: the policy's promises, the chip, the message.

use super::*;
use koru_ai_core::DocumentId;
use winit::keyboard::NamedKey;

static CARET_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn the_empty_composer_placeholder_cannot_paint_over_the_document_caret() {
    use koru_draw::inspect;
    let _lock = CARET_TEST_LOCK.lock().unwrap();

    let mut composer = ChatComposer::new(None);
    let text = Rect::new(20.0, 80.0, 400.0, 60.0);
    composer.text_height(text.w - 2.0 * PAD_X, 1, 8);
    composer.place(text, Rect::new(0.0, 0.0, 500.0, 500.0));
    composer.set_focused(true);
    let capture = inspect::Frame::begin(
        1,
        Affine::IDENTITY,
        koru_draw::scene::rect(0.0, 0.0, 500.0, 500.0),
    );
    composer.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        true,
        (-1.0, -1.0),
        "Continue the conversation...",
    );
    let records = capture.finish().records;
    let color = inspect::color(theme::caret_color().color());
    let caret = records
        .iter()
        .find(|record| {
            record.name == "fill"
                && (record.rect().width() - f64::from(theme::caret_w())).abs() < 0.01
                && record
                    .properties
                    .iter()
                    .any(|p| p.name == "color" && p.value == color)
        })
        .expect("the composer draws the shared document caret");
    let placeholder = records
        .iter()
        .find(|record| record.name == "text line" && record.rect().width() > 20.0)
        .expect("the empty composer draws its placeholder");
    assert!(
        caret.visible().area() > 0.0,
        "the caret is not clipped away"
    );
    assert!(
        caret.z > placeholder.z,
        "the placeholder must sit behind the caret, not recolor or cover it"
    );
}

#[test]
fn typed_chat_surface_uses_live_document_caret_preferences() {
    use koru_draw::inspect;
    let _lock = CARET_TEST_LOCK.lock().unwrap();
    struct Restore(theme::CaretColor, theme::CaretWidth, theme::CaretHeight);
    impl Drop for Restore {
        fn drop(&mut self) {
            theme::set_caret_color(self.0);
            theme::set_caret_width(self.1);
            theme::set_caret_height(self.2);
        }
    }
    let _restore = Restore(
        theme::caret_color(),
        theme::caret_width(),
        theme::caret_height(),
    );
    let mut surface = crate::ChatSurface::new();
    let (reader, _commands, _updates) = crate::reader::Reader::test_pair();
    surface.reader = Some(reader);
    surface.setup_open = false;
    surface.validated_ready = true;
    surface.focus = crate::Focus::Composer;
    typed(&mut surface.composer, "asdfasdasdf asdfs");
    surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 600.0));
    let mut heights = Vec::new();
    for (color, width, height) in [
        (
            theme::CaretColor::Blue,
            theme::CaretWidth::Thin,
            theme::CaretHeight::Short,
        ),
        (
            theme::CaretColor::Violet,
            theme::CaretWidth::Thick,
            theme::CaretHeight::Tall,
        ),
    ] {
        theme::set_caret_color(color);
        theme::set_caret_width(width);
        theme::set_caret_height(height);
        let capture = inspect::Frame::begin(
            1,
            Affine::IDENTITY,
            koru_draw::scene::rect(0.0, 0.0, 900.0, 600.0),
        );
        surface.draw_with_caret_phase(&mut Scene::new(), Affine::IDENTITY, true);
        let records = capture.finish().records;
        let composer_text = records
            .iter()
            .position(|r| r.name == "ComposerText")
            .expect("the real chat composer shell is painted");
        let inside_composer = |record: &inspect::Record| {
            let mut parent = record.parent;
            while let Some(index) = parent {
                if index == composer_text {
                    return true;
                }
                parent = records[index].parent;
            }
            false
        };
        let expected_color = inspect::color(color.color());
        let caret = records
            .iter()
            .find(|record| {
                record.name == "fill"
                    && inside_composer(record)
                    && (record.rect().width() - f64::from(width.px())).abs() < 0.01
                    && record
                        .properties
                        .iter()
                        .any(|p| p.name == "color" && p.value == expected_color)
            })
            .expect("typed composer paints the current document caret color and width");
        assert_eq!(caret.visible(), caret.rect(), "the full caret is visible");
        heights.push(caret.rect().height());
        let capture = inspect::Frame::begin(
            2,
            Affine::IDENTITY,
            koru_draw::scene::rect(0.0, 0.0, 900.0, 600.0),
        );
        surface.draw_with_caret_phase(&mut Scene::new(), Affine::IDENTITY, false);
        assert!(
            !capture.finish().records.iter().any(|record| {
                record.name == "fill"
                    && record.bounds == caret.bounds
                    && record
                        .properties
                        .iter()
                        .any(|p| p.name == "color" && p.value == expected_color)
            }),
            "the host clock's off phase hides the caret"
        );
        assert!(
            surface.composer.editor.view.active,
            "blink does not remove input focus"
        );
        assert_eq!(surface.composer.message().markdown, "asdfasdasdf asdfs");
    }
    assert!(
        heights[1] > heights[0],
        "the existing composer follows live caret height changes"
    );
}

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
fn single_line_ink_is_centered_for_empty_and_typed_drafts() {
    let mut composer = ChatComposer::new(None);
    for draft in ["", "Hello", ""] {
        composer.set_text(draft);
        let h = PAD_TOP + line_height() + PAD_BOTTOM;
        let text = Rect::new(20.0, 30.0, 500.0, h);
        composer.place(text, Rect::new(0.0, 0.0, 600.0, 500.0));
        let row = &composer.editor.body.rows[0];
        let ink_center = composer.origin.1 + koru_editor::rowlayout::first_line_mid_y(row, row.y);
        assert!((ink_center - (text.y + text.h * 0.5)).abs() < 0.5);
    }
}

#[test]
fn bot_mentions_keep_the_plain_composer_height_and_baseline() {
    let mut composer = ChatComposer::new(None);
    for width in [360.0, 720.0] {
        composer.set_text("Ask CFO what is your role?");
        let plain_height = composer.text_height(width - PAD_X * 2.0, 1, 8);
        let plain_baseline = composer.editor.body.rows[0]
            .layout
            .get(0)
            .unwrap()
            .metrics()
            .baseline;
        composer.set_message(
            &format!("Ask [[koru://bot/{CFO}#CFO]] what is your role?"),
            |_| String::new(),
        );
        let height = composer.text_height(width - PAD_X * 2.0, 1, 8);
        assert!(
            (height - plain_height).abs() < 0.01,
            "mentions must not enlarge a single line: {height} vs {plain_height}"
        );
        let text = Rect::new(20.0, 30.0, width, PAD_TOP + height + PAD_BOTTOM);
        composer.place(text, Rect::new(0.0, 0.0, 800.0, 500.0));
        let row = &composer.editor.body.rows[0];
        assert!(!row.pills.is_empty());
        assert_eq!(row.layout.len(), 1);
        let baseline = row.layout.get(0).unwrap().metrics().baseline;
        assert!((baseline - plain_baseline).abs() < 0.01);
        let center = composer.origin.1 + koru_editor::rowlayout::first_line_mid_y(row, row.y);
        assert!((center - (text.y + text.h * 0.5)).abs() < 0.5);
        koru_editor::rowlayout::visit_pills(
            row,
            &composer.editor.view.engine.pills,
            |_, _, visual, _, y, _, h| {
                assert!(
                    (composer.origin.1 + row.y + y + h * 0.5 - center).abs() < 0.5,
                    "avatar and mention bounds share the surrounding text's center"
                );
                assert!(visual.icon_slot <= h);
                assert!((h - plain_height).abs() < 0.01);
            },
        );
    }
}

#[test]
fn context_pages_share_the_plain_line_height_and_wrap_normally() {
    let mut composer = ChatComposer::new(None);
    for width in [360.0, 720.0] {
        composer.set_text("Top News Today please read this");
        let plain = composer.text_height(width - PAD_X * 2.0, 1, 8);
        let baseline = composer.editor.body.rows[0]
            .layout
            .get(0)
            .unwrap()
            .metrics()
            .baseline;
        composer.set_text("");
        chip(&mut composer, "Top News Today");
        typed(&mut composer, "please read this");
        let height = composer.text_height(width - PAD_X * 2.0, 1, 8);
        assert!(
            (height - plain).abs() < 0.01,
            "a context page must not grow the line"
        );
        assert!(
            (composer.editor.body.rows[0]
                .layout
                .get(0)
                .unwrap()
                .metrics()
                .baseline
                - baseline)
                .abs()
                < 0.01
        );
        composer.place(
            Rect::new(20.0, 30.0, width, PAD_TOP + height + PAD_BOTTOM),
            Rect::new(0.0, 0.0, 800.0, 500.0),
        );
        let row = &composer.editor.body.rows[0];
        let center = composer.origin.1 + koru_editor::rowlayout::first_line_mid_y(row, row.y);
        koru_editor::rowlayout::visit_pills(
            row,
            &composer.editor.view.engine.pills,
            |_, _, _, _, y, _, h| {
                assert!((composer.origin.1 + row.y + y + h * 0.5 - center).abs() < 0.5);
            },
        );
        typed(
            &mut composer,
            " and explain the details of this page in a few paragraphs with the important facts included",
        );
        assert!(
            composer.text_height(width - PAD_X * 2.0, 1, 8) > plain,
            "long input still grows when it wraps"
        );
    }
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
fn an_inline_document_pill_is_a_control_under_the_pointer() {
    let mut composer = ChatComposer::new(None);
    assert!(composer.insert_document(DocumentId::new(), "Top Priorities"));
    let text = Rect::new(20.0, 80.0, 400.0, 50.0);
    composer.text_height(text.w - 2.0 * PAD_X, 1, 8);
    composer.place(text, Rect::new(0.0, 0.0, 500.0, 500.0));
    let row = &composer.editor.body.rows[0];
    let step = row.type_step();
    let style = koru_draw::pill::PillTextStyle::new(step.size, step.size * step.line_height)
        .with_stack(row.font_choice().stack());
    assert_eq!(style.size, koru_draw::theme::role::ui().metrics.size);
    assert_eq!(style.stack, koru_draw::theme::chat_stack());
    let info = row.pills.values().next().expect("document pill");
    assert!(
        composer
            .editor
            .view
            .engine
            .pills
            .get_with_style(info.kind, &info.label, info.url.as_deref(), style)
            .is_some()
    );
    let x = composer.origin.0 + composer.editor.view.content_x + row.text_x() + 6.0;
    let y = composer.origin.1 + row.y - composer.editor.view.scroll + row.height * 0.5;
    composer.mouse_move(x, y);
    assert!(
        composer.over_inline_pill(),
        "{:?}",
        composer.editor.view.hover
    );
}

#[test]
fn alt_click_on_an_inline_document_pill_asks_for_a_place() {
    let mut composer = ChatComposer::new(None);
    let document = DocumentId::new();
    assert!(composer.insert_document(document, "Top Priorities"));
    let text = Rect::new(20.0, 80.0, 400.0, 50.0);
    composer.text_height(text.w - 2.0 * PAD_X, 1, 8);
    composer.place(text, Rect::new(0.0, 0.0, 500.0, 500.0));
    let row = &composer.editor.body.rows[0];
    let x = composer.origin.0 + composer.editor.view.content_x + row.text_x() + 6.0;
    let y = composer.origin.1 + row.y - composer.editor.view.scroll + row.height * 0.5;

    composer.set_mods(ModifiersState::ALT);
    assert!(composer.mouse_down(x, y));
    assert_eq!(
        composer.take_place_request(),
        Some(koru_link::KoruLink::Document {
            document: document.as_uuid(),
            node: None,
        })
    );
    assert_eq!(composer.take_open_request(), None);
}

#[test]
fn selected_task_keeps_its_node_link_and_containing_document_context() {
    let document = DocumentId::new();
    let node = NodeId::new();
    let mut composer = ChatComposer::new(None);
    typed(&mut composer, "@ship");
    assert!(composer.editor.set_entity_search_results(
        vec![DocumentChoice {
            target: DocumentTarget {
                document: koru_core::DocumentId::from_uuid(document.as_uuid()),
                node: Some(node)
            },
            title: "Ship café".into(),
            breadcrumb: "Workspace / Plan".into(),
            is_file: false,
        }],
        false
    ));
    assert!(press(&mut composer, NamedKey::Enter));
    let message = composer.message();
    assert_eq!(
        message.markdown,
        format!("[[koru://doc/{document}#{node}]] ")
    );
    assert_eq!(message.documents, vec![(document, "Ship café".into())]);
    let mut recalled = ChatComposer::new(None);
    recalled.set_message(&message.markdown, |_| "Ship café".into());
    assert_eq!(recalled.message().markdown, message.markdown);
    assert_eq!(recalled.message().documents, message.documents);
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
fn chat_picker_requests_documents_when_this_window_has_no_recents() {
    let current = koru_ai_core::WorkspaceId::new();
    let mut composer = ChatComposer::new(None);
    composer.set_context(Some(current), None);
    typed(&mut composer, "@");
    assert!(composer.picker_open());
    assert!(
        composer
            .search
            .as_ref()
            .is_some_and(|search| search.pending())
    );
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
fn the_picker_floats_above_the_whole_multiline_composer() {
    let mut composer = ChatComposer::new(None);
    let text = Rect::new(20.0, 520.0, 400.0, 118.0);
    let room = Rect::new(0.0, 46.0, 460.0, 592.0);
    composer.place(text, room);
    typed(&mut composer, "@");
    let panel = composer.picker_rect().expect("the picker is open");
    assert!(
        panel.y + panel.h <= text.y - 4.0,
        "{panel:?} above {text:?}"
    );
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

// ----------------------------------------------- the host's `@` choices

use koru_editor::components::entity_picker::ExtraChoice;

const CFO: &str = "0b8f6f0e-3c5e-4d0a-9a51-7d5b7c1f2a11";
const LEGAL: &str = "7c1d2e3f-4a5b-4c6d-8e7f-901a2b3c4d5e";

/// What a Bot's tab feeds: two Bots, the second paused.
fn bots() -> ExtraSection {
    ExtraSection {
        label: "Bot".into(),
        role: koru_core::attributes::ROLE_BOT.into(),
        icon: Some(koru_draw::icons::ROBOT),
        choices: vec![
            ExtraChoice {
                id: CFO.into(),
                title: "CFO".into(),
                note: None,
            },
            ExtraChoice {
                id: LEGAL.into(),
                title: "Legal".into(),
                note: Some("Paused".into()),
            },
        ],
    }
}

fn picker(composer: &ChatComposer) -> &koru_editor::components::entity_picker::EntityPicker {
    let Overlay::Entity(picker) = &composer.editor.view.overlay else {
        panic!("the @ picker is open");
    };
    picker
}

/// **`@` in a fed composer lists the host's choices above the pages**, and
/// picking one puts its chip where the `@` was: `@CFO` on screen, a link
/// carrying the id and the name in the message, and nothing else — no
/// document, no skill.
#[test]
fn bot_mention_picked_from_the_picker_is_a_chip_stored_as_its_link() {
    let mut composer = ChatComposer::new(None);
    composer.set_bot_faces(&std::collections::HashMap::from([(
        uuid::Uuid::parse_str(CFO).unwrap(),
        4,
    )]));
    assert!(
        !composer.set_extras(&bots()),
        "nothing is drawn differently yet"
    );
    let page = DocumentId::new();
    typed(&mut composer, "Ask @");
    assert!(composer.picker_open());
    assert!(composer.offer(&[(page, "Budget 2026")]));
    {
        let picker = picker(&composer);
        assert_eq!(picker.lead(), 2, "the two Bots");
        assert_eq!(picker.extra_shown[0].title, "CFO");
        assert_eq!(picker.extra_shown[1].note.as_deref(), Some("Paused"));
        assert_eq!(picker.hits.len(), 1, "then the page");
        assert_eq!(picker.sel, 0, "the first Bot is highlighted");
    }
    assert!(press(&mut composer, NamedKey::Enter));
    assert!(!composer.picker_open(), "choosing closes the picker");
    typed(&mut composer, "about Q3");

    let message = composer.message();
    assert_eq!(
        message.markdown,
        format!("Ask [[koru://bot/{CFO}#CFO]] about Q3")
    );
    assert!(message.documents.is_empty(), "a Bot is not a page");
    assert!(message.skills.is_empty());

    // The chip reads `@CFO`, and never shows the id.
    let row = &composer.editor.body.rows[0];
    let info = row.pills.values().next().expect("the mention chip");
    assert_eq!(info.kind, koru_draw::pill::PillKind::Bot(4));
    assert_eq!(info.label, "@CFO");
    assert!(!row.text.contains(CFO));

    // Backspace takes the chip back whole.
    for _ in 0.."about Q3".len() + 1 {
        press(&mut composer, NamedKey::Backspace);
    }
    assert!(press(&mut composer, NamedKey::Backspace));
    assert_eq!(composer.message().markdown, "Ask ");
}

/// What is typed after `@` filters the Bots; the arrows walk Bots then
/// pages; a page picked under them is still a document chip.
#[test]
fn bot_mention_picker_filters_and_walks_bots_then_pages() {
    let mut composer = ChatComposer::new(None);
    composer.set_extras(&bots());
    let page = DocumentId::new();
    typed(&mut composer, "@le");
    assert!(composer.offer(&[(page, "Ledger")]));
    {
        let picker = picker(&composer);
        assert_eq!(picker.lead(), 1);
        assert_eq!(picker.extra_shown[0].title, "Legal");
    }
    // Down: off the Bot, onto the page.
    assert!(press(&mut composer, NamedKey::ArrowDown));
    assert_eq!(picker(&composer).sel, 1);
    assert!(press(&mut composer, NamedKey::Enter));
    let message = composer.message();
    assert_eq!(message.markdown, format!("[[koru://doc/{page}]] "));
    assert_eq!(message.documents, vec![(page, "Ledger".to_owned())]);

    // Up from the page is the Bot again; Enter picks it.
    typed(&mut composer, "@le");
    assert!(composer.offer(&[(page, "Ledger")]));
    press(&mut composer, NamedKey::ArrowDown);
    press(&mut composer, NamedKey::ArrowUp);
    assert_eq!(picker(&composer).sel, 0);
    assert!(press(&mut composer, NamedKey::Enter));
    assert_eq!(
        composer.message().markdown,
        format!("[[koru://doc/{page}]] [[koru://bot/{LEGAL}#Legal]] ")
    );
}

/// **Escape closes the picker and the keyboard is the composer's again**:
/// what was typed stays, nothing was picked, and the next keys type.
#[test]
fn bot_mention_escape_closes_the_picker_and_returns_to_the_composer() {
    let mut composer = ChatComposer::new(None);
    composer.set_extras(&bots());
    typed(&mut composer, "ask @c");
    assert!(composer.picker_open());
    assert_eq!(picker(&composer).lead(), 1);
    assert!(press(&mut composer, NamedKey::Escape));
    assert!(!composer.picker_open());
    assert_eq!(composer.text(), "ask @c", "nothing was picked");
    typed(&mut composer, "fo");
    assert_eq!(composer.text(), "ask @cfo", "the composer has the keyboard");
    assert!(composer.editor.view.active);
}

/// A section that arrives while the picker is open shows in it at once; a
/// section taken away leaves the pages.
#[test]
fn bot_mention_section_fed_to_an_open_picker_shows_at_once() {
    let mut composer = ChatComposer::new(None);
    typed(&mut composer, "@");
    assert_eq!(picker(&composer).lead(), 0);
    assert!(composer.set_extras(&bots()), "the open picker changed");
    assert_eq!(picker(&composer).lead(), 2);
    assert!(!composer.set_extras(&bots()), "the same section again");
    assert!(composer.set_extras(&ExtraSection::default()));
    assert_eq!(picker(&composer).lead(), 0);
}

/// **A composer nobody feeds is the composer there was**: its picker has
/// no row of the host's, and choosing gives what it always gave.
#[test]
fn bot_mention_unfed_composer_lists_pages_only() {
    let mut composer = ChatComposer::new(None);
    assert!(composer.extras().choices.is_empty());
    let page = DocumentId::new();
    typed(&mut composer, "@");
    assert!(composer.offer(&[(page, "Plan")]));
    {
        let picker = picker(&composer);
        assert_eq!(picker.lead(), 0);
        assert!(picker.extra_shown.is_empty());
        assert_eq!(picker.row_count(), 2, "the page and the add row");
    }
    assert!(press(&mut composer, NamedKey::Enter));
    assert_eq!(
        composer.message().markdown,
        format!("[[koru://doc/{page}]] ")
    );
}

/// **A recalled message brings its mention back as the chip it was**, and
/// leaves again spelled exactly as it was sent; words that only look like
/// a mention stay words.
#[test]
fn bot_mention_recalled_message_brings_the_chip_back() {
    let sent = format!(
        r"Ask [[koru://bot/{CFO}#Head%20of%20Finance]] and \[[koru://bot/{LEGAL}#Legal]] by hand"
    );
    let mut composer = ChatComposer::new(None);
    composer.set_message(&sent, |_| String::new());
    assert_eq!(composer.message().markdown, sent);
    let row = &composer.editor.body.rows[0];
    let labels: Vec<&str> = row.pills.values().map(|info| info.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["@Head of Finance"],
        "one chip; the typed one is text"
    );
    let attrs = composer.ws.read(row.id).unwrap().content.attrs().to_vec();
    let chip = attrs
        .iter()
        .find(|attr| attr.role == koru_core::attributes::ROLE_BOT)
        .expect("the mention's record");
    assert_eq!(chip.value, AttrValue::Text(CFO.to_owned()), "by id");
}
