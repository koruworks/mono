// SPDX-License-Identifier: MPL-2.0

//! The document pill, the reply's menu, and *Add to chat* (D88).

use super::*;
use std::time::Duration;

use koru_ai_core::{
    AiProviderId, Chat, ChatId, ChatRole, MessageBlockId, MessageId, MessageState, ModelSelection,
    ReasoningChoice,
};
use koru_ai_wire::ChatTranscript;
use koru_ui::MenuItem;
use parley::layout::Affinity;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::reader::{Command, Reader};
use crate::{ChatSurface, ResponsePosition, ResponseSelection};

const REPLY: &str = "Koru keeps every workspace in one store, so workspaces stay together.";

fn transcript() -> ChatTranscript {
    let now = koru_ai_core::Timestamp::from_millis(1);
    let chat_id = ChatId::new();
    ChatTranscript {
        sources: Vec::new(),
        chat: Chat {
            id: chat_id,
            title: "Test chat".into(),
            private: false,
            current_model: ModelSelection {
                provider: AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
                requested_model: "gpt-test".into(),
                default_reasoning: ReasoningChoice::Default,
            },
            branched_from: None,
            agent_working_folder: None,
            agent_mode: None,
            agent_fast: None,
            pinned_at: None,
            profile_id: None,
            created_at: now,
            updated_at: now,
        },
        tool_policy: koru_ai_core::ChatToolPolicy {
            chat_id,
            mode: koru_ai_core::ToolMode::Manual,
            workspace_ids: Vec::new(),
        },
        tool_activities: Vec::new(),
        notices: Vec::new(),
        messages: Vec::new(),
        blocks: Vec::new(),
        pinned_documents: Vec::new(),
        feedback: Vec::new(),
        usage: Vec::new(),
        reasoning_control: None,
        attachments: Vec::new(),
    }
}

/// A chat on screen with one finished reply, laid out, nothing selected.
fn reply_surface() -> ChatSurface {
    let mut surface = ChatSurface::new();
    surface.lang = koru_i18n::Lang::En;
    surface.set_viewport(Rect::new(0.0, 0.0, 700.0, 520.0));
    let mut transcript = transcript();
    let message_id = MessageId::new();
    transcript.messages.push(koru_ai_core::Message {
        id: message_id,
        chat_id: transcript.chat.id,
        ordinal: 0,
        role: ChatRole::Assistant,
        state: MessageState::Complete,
        markdown: REPLY.into(),
        origin_message_id: None,
        provider: None,
        requested_model: None,
        resolved_model: None,
        error_class: None,
        created_at: koru_ai_core::Timestamp::from_millis(1),
        completed_at: Some(koru_ai_core::Timestamp::from_millis(2)),
    });
    transcript.blocks.push(koru_ai_core::MessageBlock {
        id: MessageBlockId::new(),
        message_id,
        ordinal: 0,
        kind: koru_ai_core::MessageBlockKind::Paragraph,
        markdown: REPLY.into(),
        plain_text: REPLY.into(),
    });
    surface.transcript = Some(transcript);
    surface.validated_ready = true;
    draw(&mut surface);
    surface
}

fn draw(surface: &mut ChatSurface) {
    surface.draw(&mut Scene::new(), Affine::IDENTITY);
}

/// Select `from..to` of the reply as a press in the reply would — the
/// keyboard leaves the composer — with the drag still held.
fn select(surface: &mut ChatSurface, from: usize, to: usize) {
    surface.focus = Focus::None;
    surface.composer.set_focused(false);
    surface.response_selection = Some(ResponseSelection {
        anchor: ResponsePosition {
            message: 0,
            block: Some(0),
            byte: from,
            affinity: Affinity::Downstream,
        },
        focus: ResponsePosition {
            message: 0,
            block: Some(0),
            byte: to,
            affinity: Affinity::Upstream,
        },
        dragging: true,
        pressed_link: None,
    });
}

/// Select and let go: the pill is up and drawn.
fn selected(surface: &mut ChatSurface, from: usize, to: usize) -> Rect {
    select(surface, from, to);
    assert!(surface.mouse_up());
    draw(surface);
    surface
        .add_pill
        .rect()
        .expect("letting go of a selection shows Add to chat")
}

fn ctrl_l(surface: &mut ChatSurface) -> bool {
    surface.key(&Key::Character("l".into()), Some("l"), koru_keys::PRIMARY)
}

#[test]
fn empty_strip_has_no_layout_height() {
    assert_eq!(pill_band_height(None), 0.0);
    let doc = ContextDocumentPill {
        document_id: DocumentId::new(),
        title: "Launch plan".into(),
    };
    assert_eq!(pill_band_height(Some(&doc)), PILL_H + BAND_GAP);
}

#[test]
fn context_document_pill_body_and_close_are_hit() {
    let mut engine = TextEngine::new();
    let doc = ContextDocumentPill {
        document_id: DocumentId::new(),
        title: "Launch plan".into(),
    };
    let strip = PillStrip::default();
    let band = Rect::new(0.0, 0.0, 600.0, PILL_H);
    let (part, _) = strip
        .hit(&mut engine, Some(&doc), band, band.x + 20.0, PILL_H * 0.5)
        .expect("the document pill is hit");
    assert_eq!(part, PillPart::Body);
    let pill = PillStrip::layout(&mut engine, Some(&doc), band).unwrap();
    let (part, _) = strip
        .hit(
            &mut engine,
            Some(&doc),
            band,
            pill.close.x + 2.0,
            PILL_H * 0.5,
        )
        .expect("the close target is hit");
    assert_eq!(part, PillPart::Close);
    assert!(strip.hit(&mut engine, None, band, 20.0, 10.0).is_none());
}

/// **The pill shows when a selection is let go** — just over its first
/// line, inside the transcript, the arrow under the pointer — and goes on
/// Escape, on a scroll, on a press elsewhere, and with the selection.
#[test]
fn add_to_chat_appears_over_a_finished_selection_and_hides() {
    let mut surface = reply_surface();
    draw(&mut surface);
    assert!(
        surface.add_pill.rect().is_none(),
        "nothing selected, no pill"
    );

    // A press that selects nothing shows nothing.
    select(&mut surface, 4, 4);
    surface.mouse_up();
    draw(&mut surface);
    assert!(surface.add_pill.rect().is_none());

    let pill = selected(&mut surface, 5, 20);
    let area = surface.conversation_hit_rect;
    assert!(area.contains(pill.x, pill.y) && area.contains(pill.x + pill.w, pill.y + pill.h));
    let (layout, origin) = surface.response_layout(0, Some(0)).unwrap();
    let start = Cursor::from_byte_index(layout, 5, Affinity::Downstream).geometry(layout, 1.0);
    let line_top = origin.1 + start.y0 as f32;
    assert!(
        pill.y + pill.h <= line_top || pill.y >= origin.1 + start.y1 as f32,
        "the pill does not cover the selection's first line"
    );
    surface.mouse_move(pill.x + pill.w * 0.5, pill.y + pill.h * 0.5);
    assert_eq!(
        surface.cursor_icon(),
        CursorIcon::Default,
        "no hand (ADR-215)"
    );

    assert!(surface.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty()));
    draw(&mut surface);
    assert!(surface.add_pill.rect().is_none(), "Escape hides it");
    assert!(surface.response_selection.is_some(), "the selection stays");

    selected(&mut surface, 5, 20);
    surface.scroll_by(MouseScrollDelta::LineDelta(0.0, -1.0));
    draw(&mut surface);
    assert!(surface.add_pill.rect().is_none(), "a scroll hides it");

    selected(&mut surface, 5, 20);
    surface.mouse_down(area.x + 2.0, area.y + area.h - 2.0);
    surface.mouse_up();
    draw(&mut surface);
    assert!(
        surface.add_pill.rect().is_none(),
        "a press elsewhere hides it"
    );

    selected(&mut surface, 5, 20);
    surface.response_selection = None;
    draw(&mut surface);
    assert!(surface.add_pill.rect().is_none(), "no selection, no pill");
}

/// **A press on the pill adds the selection**: a quote chip in the
/// composer, the caret under it, the keyboard there, the pill gone.
#[test]
fn clicking_add_to_chat_quotes_the_selection_into_the_composer() {
    let mut surface = reply_surface();
    let pill = selected(&mut surface, 0, 4);
    assert!(surface.mouse_down(pill.x + pill.w * 0.5, pill.y + pill.h * 0.5));
    surface.mouse_up();
    assert_eq!(surface.composer.text(), "> Koru");
    assert_eq!(surface.focus, Focus::Composer);
    assert!(surface.response_selection.is_none());
    draw(&mut surface);
    assert!(surface.add_pill.rect().is_none(), "used, it goes");
}

/// **Ctrl+L adds the selection** while the keyboard is in the transcript,
/// and moves the keyboard to the composer, where the comment is typed under
/// the chip.
#[test]
fn ctrl_l_adds_a_quote_chip_and_moves_focus_to_the_composer() {
    let mut surface = reply_surface();
    selected(&mut surface, 0, 4);
    assert!(ctrl_l(&mut surface));
    assert_eq!(surface.focus, Focus::Composer);
    assert!(surface.response_selection.is_none());
    for ch in "why?".chars() {
        let s = ch.to_string();
        surface.key(
            &Key::Character(s.as_str().into()),
            Some(&s),
            ModifiersState::empty(),
        );
    }
    assert_eq!(surface.composer.text(), "> Koru\n\nwhy?");
}

/// **The chord is the chat's while *Add to chat* is offered** (the user's
/// call, 2026-11-03) — wherever the keyboard is, the composer included,
/// whose policy refuses alignment in a message anyway. With nothing
/// selected, or the pill put away with Escape, the chat leaves it alone.
#[test]
fn ctrl_l_is_the_chats_while_the_pill_is_offered() {
    let entry = koru_keys::registry::get(koru_keys::ShortcutId::AddSelectionToChat);
    assert_eq!(entry.layer, koru_keys::Layer::Chat);
    assert_eq!(
        entry.shares_chord_with,
        Some(koru_keys::ShortcutId::AlignLeft)
    );
    assert_eq!(
        entry.chord,
        koru_keys::registry::get(koru_keys::ShortcutId::AlignLeft).chord
    );

    let mut surface = reply_surface();
    selected(&mut surface, 0, 4);
    surface.focus = Focus::Composer;
    surface.composer.set_focused(true);
    assert!(surface.add_to_chat_key_is_ours(), "in the composer too");
    assert!(ctrl_l(&mut surface));
    assert_eq!(surface.composer.text(), "> Koru");

    let mut surface = reply_surface();
    surface.focus = Focus::None;
    assert!(!surface.add_to_chat_key_is_ours(), "nothing selected");
    ctrl_l(&mut surface);
    assert!(surface.composer.is_blank());

    let mut surface = reply_surface();
    selected(&mut surface, 0, 4);
    surface.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty());
    assert!(!surface.add_to_chat_key_is_ours(), "put away");
    ctrl_l(&mut surface);
    assert!(surface.composer.is_blank());
}

/// **Ctrl+L after a real drag, as the window delivers it** (the user's
/// report, 2026-11-03): the press, the drag and the release on the reply,
/// then the Ctrl key's own press — which the shell hands the surface as a
/// key — then `L` with Windows' control character for text. The bare Ctrl
/// used to move the keyboard to the composer, and the `L` was refused.
#[test]
fn ctrl_l_after_a_mouse_selection_adds_it_as_the_window_delivers_it() {
    let mut surface = reply_surface();
    assert_eq!(surface.focus, Focus::Composer, "the keyboard starts there");
    let (layout, origin) = surface.response_layout(0, Some(0)).unwrap();
    let from = Cursor::from_byte_index(layout, 0, Affinity::Downstream).geometry(layout, 1.0);
    let to = Cursor::from_byte_index(layout, 4, Affinity::Downstream).geometry(layout, 1.0);
    let y = origin.1 + (from.y0 + from.y1) as f32 * 0.5;
    assert!(surface.mouse_down(origin.0 + from.x0 as f32 + 0.5, y));
    surface.mouse_move(origin.0 + to.x0 as f32, y);
    surface.mouse_up();
    draw(&mut surface);
    assert!(surface.add_pill.rect().is_some(), "the pill is up");

    let ctrl = koru_keys::PRIMARY;
    let modifier = if cfg!(target_os = "macos") {
        NamedKey::Super
    } else {
        NamedKey::Control
    };
    assert!(
        !surface.key(&Key::Named(modifier), None, ctrl),
        "a bare modifier is not a key the chat acts on"
    );
    assert_ne!(surface.focus, Focus::Composer, "and it moved nothing");
    assert!(surface.key(&Key::Character("l".into()), Some("\u{c}"), ctrl));
    assert_eq!(surface.composer.text(), "> Koru");
    assert_eq!(surface.focus, Focus::Composer);
}

/// **Two quotes, each with its comment, go out in order** as blockquotes
/// before the text under them.
#[test]
fn two_quotes_with_comments_send_as_blockquotes_in_order() {
    let mut surface = reply_surface();
    let (reader, commands, _updates) = Reader::test_pair();
    surface.reader = Some(reader);
    selected(&mut surface, 0, 4);
    assert!(ctrl_l(&mut surface));
    type_text(&mut surface, "something here");
    let start = REPLY.find("workspaces stay").unwrap();
    selected(&mut surface, start, REPLY.len() - 1);
    assert!(ctrl_l(&mut surface));
    type_text(&mut surface, "Another one");

    let expected = "> Koru\n\nsomething here\n\n> workspaces stay together\n\nAnother one";
    assert_eq!(surface.composer.text(), expected);
    assert!(surface.send_composer());
    let sent = std::iter::from_fn(|| commands.recv_timeout(Duration::from_millis(50)).ok())
        .find_map(|command| match command {
            Command::Send(request) => Some(request.markdown),
            _ => None,
        })
        .expect("the composer's send issues a turn");
    assert_eq!(sent, expected);
    assert!(surface.composer.is_blank());
}

fn type_text(surface: &mut ChatSurface, text: &str) {
    for ch in text.chars() {
        let s = ch.to_string();
        surface.key(
            &Key::Character(s.as_str().into()),
            Some(&s),
            ModifiersState::empty(),
        );
    }
}

/// **The reply's right-click menu offers Add to chat**, the same action.
#[test]
fn the_reply_menu_adds_the_selection_to_the_chat() {
    let mut surface = reply_surface();
    selected(&mut surface, 0, 4);
    let (layout, origin) = surface.response_layout(0, Some(0)).unwrap();
    let at = Cursor::from_byte_index(layout, 2, Affinity::Downstream).geometry(layout, 1.0);
    let (x, y) = (
        origin.0 + at.x0 as f32,
        origin.1 + (at.y0 + at.y1) as f32 * 0.5,
    );
    assert!(surface.context_menu(x, y));
    assert!(surface.response_menu.is_some());
    assert_eq!(
        ResponseMenuItem::AddToChat("Add to chat").label(),
        Cow::Borrowed("Add to chat")
    );
    draw(&mut surface);
    assert!(
        surface.add_pill.rect().is_none(),
        "the menu stands in for the pill"
    );
    assert!(surface.activate_response_menu_item(1));
    assert_eq!(surface.composer.text(), "> Koru");
    assert_eq!(surface.focus, Focus::Composer);
    assert!(surface.response_menu.is_none());
}

/// **The person's own card shows a quote behind the reply's bar**, right
/// over the comment on it; a message with no quote is untouched.
#[test]
fn a_sent_quote_reads_behind_the_bar_in_the_card() {
    assert_eq!(
        quote_bars("> first\n> line two\n\nwhy?\n\n> other\n\nAnother one"),
        "\u{2502} first\n\u{2502} line two\nwhy?\n\u{2502} other\nAnother one"
    );
    assert_eq!(quote_bars("> only a quote"), "\u{2502} only a quote");
    assert_eq!(quote_bars("a\n\nb > c"), "a\n\nb > c");
}
