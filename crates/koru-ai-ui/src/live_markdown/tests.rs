// SPDX-License-Identifier: MPL-2.0

use koru_ai_core::{
    AiProviderId, Chat, ChatId, ChatRole, Message, MessageState, ModelSelection, ReasoningChoice,
    Timestamp,
};
use koru_ai_wire::ChatTranscript;

use super::*;
use crate::ChatSurface;
use crate::reader::Update;

const HEADING: &str = "# Top US News";
const FIRST: &str = "**1. Election eve** brings a *long* night of counting.";
const OPEN: &str = "The count began";

fn transcript() -> ChatTranscript {
    let now = Timestamp::from_millis(1);
    let chat_id = ChatId::new();
    ChatTranscript {
        sources: Vec::new(),
        chat: Chat {
            id: chat_id,
            title: "News".into(),
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

/// A reply running in `surface`, its draft so far `draft`.
fn streaming(draft: &str) -> (ChatSurface, MessageId) {
    let mut surface = ChatSurface::new();
    surface.transcript = Some(transcript());
    surface.pending_user = Some("Today's top US news?".into());
    let reply = MessageId::new();
    surface.pending_assistant_message_id = Some(reply);
    surface.draft = draft.into();
    rebuild(&mut surface);
    (surface, reply)
}

fn rebuild(surface: &mut ChatSurface) {
    let width = surface.conversation_rect().w;
    surface.rebuild_messages(width);
}

fn reply_layout(surface: &ChatSurface, reply: MessageId) -> &crate::MessageLayout {
    surface
        .message_layouts
        .iter()
        .find(|message| message.id == Some(reply))
        .unwrap()
}

fn texts(surface: &ChatSurface, reply: MessageId) -> Vec<&str> {
    reply_layout(surface, reply)
        .blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect()
}

#[test]
fn a_streaming_answer_draws_its_closed_blocks_as_markdown_and_the_rest_plain() {
    let (surface, reply) = streaming(&format!("{HEADING}\n\n{FIRST}\n\n{OPEN}"));
    let blocks = &reply_layout(&surface, reply).blocks;
    assert_eq!(blocks.len(), 3);
    // The heading and the paragraph read as Markdown: no marks left.
    assert_eq!(blocks[0].markdown, HEADING);
    assert_eq!(blocks[0].text.trim(), "Top US News");
    assert_eq!(blocks[1].markdown, FIRST);
    assert_eq!(
        blocks[1].text.trim(),
        "1. Election eve brings a long night of counting."
    );
    // The paragraph still being written is the text as it came.
    assert_eq!(blocks[2].text, OPEN);
}

#[test]
fn a_line_still_being_written_is_plain_until_it_ends() {
    let (mut surface, reply) = streaming("Intro.\n\n**Bold** head");
    assert_eq!(texts(&surface, reply), ["Intro.", "**Bold** head"]);
    surface.draft.push_str("ing\n\n");
    rebuild(&mut surface);
    assert_eq!(texts(&surface, reply), ["Intro.", "Bold heading"]);
}

#[test]
fn a_fence_in_progress_stays_plain_until_it_closes() {
    let (mut surface, reply) = streaming("Run this:\n\n```rust\nfn main() {\n");
    assert_eq!(
        texts(&surface, reply),
        ["Run this:", "```rust\nfn main() {"]
    );
    // Lines that would be Markdown outside a fence are the fence's.
    surface.draft.push_str("# not a heading\n");
    rebuild(&mut surface);
    assert_eq!(
        texts(&surface, reply),
        ["Run this:", "```rust\nfn main() {\n# not a heading"]
    );
    surface.draft.push_str("}\n```\n");
    rebuild(&mut surface);
    let blocks = &reply_layout(&surface, reply).blocks;
    assert_eq!(blocks.len(), 2);
    assert!(!blocks[1].text.contains("```"), "drawn as code now");
    assert!(blocks[1].text.contains("# not a heading"));
}

#[test]
fn a_delta_lays_out_only_the_open_block() {
    let (mut surface, reply) = streaming(&format!("{HEADING}\n\n{FIRST}\n\n{OPEN}"));
    let ids = |surface: &ChatSurface| {
        reply_layout(surface, reply)
            .blocks
            .iter()
            .map(|block| block.id)
            .collect::<Vec<_>>()
    };
    let before = ids(&surface);
    surface.draft.push_str(" at six.");
    rebuild(&mut surface);
    assert_eq!(ids(&surface), before, "the closed blocks keep their ids");
    let stats = surface.last_rebuild_stats;
    assert_eq!((stats.blocks_reused, stats.blocks_laid_out), (2, 1));

    // Closing the open block lays it out once, as Markdown, under an id
    // of its own; the open block that follows takes over the open id.
    surface.draft.push_str("\n\nMore *soon*");
    rebuild(&mut surface);
    let after = ids(&surface);
    assert_eq!(after.len(), 4);
    assert_eq!(after[..2], before[..2]);
    assert_ne!(after[2], before[2]);
    assert_eq!(after[3], before[2]);
    assert_eq!(texts(&surface, reply)[3], "More *soon*");
}

#[test]
fn the_finished_reply_draws_as_a_reply_that_never_streamed() {
    let markdown = format!(
        "{HEADING}\n\n{FIRST}\n\n- one\n- two\n\n| A | B |\n| --- | --- |\n| 1 | 2 |\n\n```\nlet x = 1;\n```\n\n{OPEN} at six."
    );
    let (mut surface, reply) = streaming(&markdown);
    let live: Vec<(String, String)> = reply_layout(&surface, reply)
        .blocks
        .iter()
        .map(|block| (block.markdown.clone(), block.text.clone()))
        .collect();

    let mut finished = surface.transcript.clone().unwrap();
    let chat = finished.chat.id;
    finished.messages.push(Message {
        id: reply,
        chat_id: chat,
        ordinal: 1,
        role: ChatRole::Assistant,
        state: MessageState::Complete,
        markdown: markdown.clone(),
        origin_message_id: None,
        provider: None,
        requested_model: None,
        resolved_model: None,
        error_class: None,
        created_at: Timestamp::from_millis(1),
        completed_at: Some(Timestamp::from_millis(2)),
    });
    finished.blocks = koru_ai_core::finalize_markdown(reply, &markdown);
    surface.apply_update(Update::Transcript(finished.clone()));
    rebuild(&mut surface);

    let mut fresh = ChatSurface::new();
    fresh.transcript = Some(finished);
    rebuild(&mut fresh);

    let drawn = |surface: &ChatSurface| {
        let message = reply_layout(surface, reply);
        (
            message.state,
            crate::message_height(message),
            message
                .blocks
                .iter()
                .map(|block| {
                    (
                        block.id,
                        block.markdown.clone(),
                        block.text.clone(),
                        block.height(),
                        block.table.is_some(),
                    )
                })
                .collect::<Vec<_>>(),
        )
    };
    let streamed = drawn(&surface);
    assert_eq!(streamed, drawn(&fresh));
    assert_eq!(streamed.0, MessageState::Complete);
    // While it streamed, every closed block read as it reads finished;
    // only the last, still open, was plain.
    let (open, closed) = live.split_last().unwrap();
    assert_eq!(open.1, format!("{OPEN} at six."));
    assert_eq!(
        closed,
        streamed.2[..closed.len()]
            .iter()
            .map(|(_, markdown, text, _, _)| (markdown.clone(), text.clone()))
            .collect::<Vec<_>>()
    );
    assert!(
        streamed.2.iter().any(|block| block.4),
        "the table is a grid"
    );
}

#[test]
fn an_answer_moved_by_a_new_call_starts_over() {
    let reply = MessageId::new();
    let mut answer = LiveAnswer::default();
    let first = answer.blocks(reply, 0, "One.\n\nTwo");
    assert_eq!(first.len(), 2);
    // A call cut the answer later in the draft: none of it is kept.
    let moved = answer.blocks(reply, 6, "Three.\n\n");
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].1, "Three.");
    assert_ne!(moved[0].0, first[0].0);
    // Text that does not continue what was closed starts over too.
    let other = answer.blocks(reply, 6, "Four.\n\n");
    assert_eq!(other.len(), 1);
    assert_eq!(other[0].1, "Four.");
}
