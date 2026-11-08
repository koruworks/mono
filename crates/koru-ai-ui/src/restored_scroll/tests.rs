// SPDX-License-Identifier: MPL-2.0

use koru_ai_core::{
    AiProviderId, ChatRole, MessageState, ModelSelection, ReasoningChoice, Timestamp,
};
use koru_ai_wire::ChatTranscript;
use koru_draw::geometry::Rect;
use vello::Scene;
use vello::kurbo::Affine;

use super::*;
use crate::reader::Update;
use crate::unmade_transcript;

/// A chat the service holds, long enough to scroll.
fn chat(title: &str) -> ChatTranscript {
    let mut transcript = unmade_transcript(ModelSelection {
        provider: AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
        requested_model: "gpt-test".into(),
        default_reasoning: ReasoningChoice::Default,
    });
    transcript.chat.id = ChatId::new();
    transcript.chat.title = title.into();
    transcript.tool_policy.chat_id = transcript.chat.id;
    for ordinal in 0..24u32 {
        transcript.messages.push(koru_ai_core::Message {
            id: koru_ai_core::MessageId::new(),
            chat_id: transcript.chat.id,
            ordinal,
            role: if ordinal % 2 == 0 {
                ChatRole::User
            } else {
                ChatRole::Assistant
            },
            state: MessageState::Complete,
            markdown: format!("{title}, message {ordinal}"),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: Timestamp::from_millis(ordinal as i64 + 1),
            completed_at: Some(Timestamp::from_millis(ordinal as i64 + 1)),
        });
    }
    transcript
}

fn laid_out(surface: &mut ChatSurface) {
    surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);
}

fn bottom(surface: &mut ChatSurface) -> f32 {
    (surface.conversation_extent - surface.conversation_rect().h).max(0.0)
}

/// **A scroll restored before its chat has loaded waits for it**, wins over
/// the end a loaded chat opens at, and is what the surface reports in the
/// meantime — a host reopening a chat tab its history named.
#[test]
fn a_restored_scroll_waits_for_its_chat_and_wins_over_the_end() {
    let a = chat("A");
    let mut surface = ChatSurface::new();
    surface.set_viewport(Rect::new(0.0, 0.0, 480.0, 360.0));
    surface.open_link(a.chat.id, None);
    surface.restore_conversation_scroll(120.0);
    assert_eq!(
        surface.conversation_scroll(),
        120.0,
        "reported while waiting"
    );

    surface.scroll_loaded_chat_to_bottom = true;
    surface.apply_update(Update::Transcript(a));
    laid_out(&mut surface);
    assert!(bottom(&mut surface) > 120.0, "the chat scrolls past it");
    assert_eq!(surface.conversation_scroll, 120.0);
    assert_eq!(surface.conversation_scroll(), 120.0);
}

/// **A scroll owed to another chat is not taken** by the one laid out.
#[test]
fn a_restored_scroll_belongs_to_its_chat() {
    let (a, b) = (chat("A"), chat("B"));
    let mut surface = ChatSurface::new();
    surface.set_viewport(Rect::new(0.0, 0.0, 480.0, 360.0));
    surface.open_link(a.chat.id, None);
    surface.restore_conversation_scroll(120.0);
    surface.apply_update(Update::Transcript(b));
    assert_eq!(surface.take_restored_scroll(), None, "B owes nothing");
    surface.apply_update(Update::Transcript(a));
    assert_eq!(surface.take_restored_scroll(), Some(120.0));
    assert_eq!(surface.take_restored_scroll(), None, "taken once");
}
