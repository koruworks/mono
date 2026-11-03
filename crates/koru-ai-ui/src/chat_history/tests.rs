// SPDX-License-Identifier: MPL-2.0

use std::sync::mpsc::Receiver;

use koru_ai_core::{
    AiProviderId, ChatRole, InferenceAttemptId, MessageState, ModelSelection, ReasoningChoice,
    Timestamp,
};
use koru_ai_wire::ChatTranscript;
use koru_draw::geometry::Rect;
use vello::Scene;
use vello::kurbo::Affine;

use super::*;
use crate::reader::{Reader, Update};
use crate::{SurfaceMode, unmade_transcript};

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

/// The standalone window's surface, on `first`, with a reader whose
/// commands the test reads.
fn window_on(first: &ChatTranscript) -> (ChatSurface, Receiver<Command>) {
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    let (reader, commands, updates) = Reader::test_pair();
    std::mem::forget(updates);
    surface.reader = Some(reader);
    surface.set_viewport(Rect::new(0.0, 0.0, 480.0, 360.0));
    surface.apply_update(Update::Transcript(first.clone()));
    laid_out(&mut surface);
    (surface, commands)
}

fn laid_out(surface: &mut ChatSurface) {
    surface.draw_messages(&mut Scene::new(), Affine::IDENTITY);
}

fn bottom(surface: &mut ChatSurface) -> f32 {
    (surface.conversation_extent - surface.conversation_rect().h).max(0.0)
}

/// The chats this surface was asked to read, oldest first; anything else it
/// sent is named so a test can see it.
fn sent(commands: &Receiver<Command>) -> Vec<String> {
    let mut sent = Vec::new();
    while let Ok(command) = commands.try_recv() {
        sent.push(match command {
            Command::Read(chat) => format!("read {chat}"),
            _ => "another command".to_owned(),
        });
    }
    sent
}

/// Read `next` into the window the way a row, a step or a link does, and
/// let the service answer.
fn move_to(surface: &mut ChatSurface, next: &ChatTranscript) {
    assert!(surface.open_link(next.chat.id, None));
    surface.apply_update(Update::Transcript(next.clone()));
    laid_out(surface);
}

/// **Back and forward walk the chats read in place**, each read back at the
/// scroll it was left at, and stop at either end.
#[test]
fn back_and_forward_walk_the_chats_read_in_place_at_their_scroll() {
    let (a, b) = (chat("A"), chat("B"));
    let (mut surface, commands) = window_on(&a);
    let left_at = bottom(&mut surface) * 0.3;
    surface.conversation_scroll = left_at;
    move_to(&mut surface, &b);
    sent(&commands);

    assert!(surface.step_chat_history(-1), "back to A");
    assert_eq!(sent(&commands), vec![format!("read {}", a.chat.id)]);
    surface.apply_update(Update::Transcript(a.clone()));
    laid_out(&mut surface);
    assert_eq!(surface.current_chat_id(), Some(a.chat.id));
    assert!(
        (surface.conversation_scroll - left_at).abs() < 0.5,
        "A comes back where it was left, not at its end: {} vs {left_at}",
        surface.conversation_scroll
    );
    assert!(!surface.step_chat_history(-1), "nothing before A");

    assert!(surface.step_chat_history(1), "forward to B");
    assert_eq!(sent(&commands), vec![format!("read {}", b.chat.id)]);
    surface.apply_update(Update::Transcript(b.clone()));
    laid_out(&mut surface);
    assert!(!surface.step_chat_history(1), "nothing after B");
}

/// **A new move drops the forward branch** — the browser rule, as the
/// workspace window's history keeps it.
#[test]
fn a_new_move_after_going_back_drops_the_way_forward() {
    let (a, b, c) = (chat("A"), chat("B"), chat("C"));
    let (mut surface, commands) = window_on(&a);
    move_to(&mut surface, &b);
    assert!(surface.step_chat_history(-1));
    surface.apply_update(Update::Transcript(a.clone()));
    laid_out(&mut surface);
    move_to(&mut surface, &c);
    sent(&commands);

    assert!(!surface.step_chat_history(1), "B is no longer ahead");
    assert!(surface.step_chat_history(-1));
    assert_eq!(sent(&commands), vec![format!("read {}", a.chat.id)]);
}

/// **A chat deleted since is stepped over**, not stopped on: the walk goes
/// on in the same direction.
#[test]
fn a_chat_the_listing_no_longer_holds_is_skipped() {
    let (a, b, c) = (chat("A"), chat("B"), chat("C"));
    let (mut surface, commands) = window_on(&a);
    move_to(&mut surface, &b);
    move_to(&mut surface, &c);
    surface.chats = vec![a.chat.clone(), c.chat.clone()];
    sent(&commands);

    assert!(surface.step_chat_history(-1));
    assert_eq!(sent(&commands), vec![format!("read {}", a.chat.id)]);
}

/// **A step never stops a running reply.** It reads the other chat and
/// sends nothing else — no cancel — so the reply goes on in the service and
/// the chat's status says so where it is listed.
#[test]
fn a_step_away_from_a_running_reply_sends_no_cancel() {
    let (a, b) = (chat("A"), chat("B"));
    let (mut surface, commands) = window_on(&a);
    move_to(&mut surface, &b);
    surface.attempt = Some(InferenceAttemptId::new());
    assert!(surface.is_working());
    sent(&commands);

    assert!(surface.step_chat_history(-1));
    assert_eq!(sent(&commands), vec![format!("read {}", a.chat.id)]);
}

/// **A surface in a tab keeps no list of its own**: the workspace window's
/// history holds its tabs, and its chat tabs never read another chat into
/// their place.
#[test]
fn a_surface_hosted_in_a_tab_records_and_walks_nothing() {
    let (a, b) = (chat("A"), chat("B"));
    let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
    let (reader, commands, updates) = Reader::test_pair();
    std::mem::forget(updates);
    surface.reader = Some(reader);
    surface.apply_update(Update::Transcript(a));
    move_to(&mut surface, &b);
    sent(&commands);
    assert!(!surface.step_chat_history(-1));
    assert!(sent(&commands).is_empty());
}

/// **A scroll restored before its chat has loaded waits for it**, wins over
/// the end a loaded chat opens at, and is what the surface reports in the
/// meantime — a host reopening a chat tab its history named.
#[test]
fn a_restored_scroll_waits_for_its_chat_and_wins_over_the_end() {
    let a = chat("A");
    let mut surface = ChatSurface::new(SurfaceMode::ContextPane);
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
