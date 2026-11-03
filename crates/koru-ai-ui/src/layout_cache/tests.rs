// SPDX-License-Identifier: MPL-2.0

use koru_ai_core::{
    AiProviderId, Chat, ChatId, Message, ModelSelection, ReasoningChoice, SourceLink, Timestamp,
};
use koru_ai_wire::{ChatTranscript, MessageSources};
use parley::editing::{Cursor, Selection};
use parley::layout::Affinity;

use super::*;
use crate::{ChatSurface, SurfaceMode};

const AP: &str = "https://ap.example/";
const BBC: &str = "https://bbc.example/";

/// A finished reply: a line of links, a long paragraph, a line of links.
fn surface_with_reply() -> ChatSurface {
    let now = Timestamp::from_millis(1);
    let chat_id = ChatId::new();
    let reply = MessageId::new();
    let markdown = format!(
        "Sources: [AP]({AP}) · [Reuters](https://reuters.example/)\n\n{}\n\nSources: [BBC]({BBC})",
        "Election eve brings a long night of counting. ".repeat(12)
    );
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    surface.transcript = Some(ChatTranscript {
        sources: vec![MessageSources {
            message_id: reply,
            sources: vec![
                SourceLink {
                    url: AP.into(),
                    title: "AP".into(),
                },
                SourceLink {
                    url: BBC.into(),
                    title: "BBC".into(),
                },
            ],
        }],
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
        blocks: koru_ai_core::finalize_markdown(reply, &markdown),
        messages: vec![Message {
            id: reply,
            chat_id,
            ordinal: 0,
            role: ChatRole::Assistant,
            state: MessageState::Complete,
            markdown,
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: now,
            completed_at: Some(now),
        }],
        pinned_documents: Vec::new(),
        feedback: Vec::new(),
        usage: Vec::new(),
        reasoning_control: None,
        attachments: Vec::new(),
    });
    let width = surface.conversation_rect().w;
    surface.rebuild_messages(width);
    surface
}

/// The middle of `range` of `layout`'s text, from the layout's top-left.
fn middle_of(layout: &Layout<Brush>, range: std::ops::Range<usize>) -> (f32, f32) {
    let selection = Selection::new(
        Cursor::from_byte_index(layout, range.start, Affinity::Downstream),
        Cursor::from_byte_index(layout, range.end, Affinity::Upstream),
    );
    let (bounds, _) = selection.geometry(layout)[0];
    (
        ((bounds.x0 + bounds.x1) / 2.0) as f32,
        ((bounds.y0 + bounds.y1) / 2.0) as f32,
    )
}

/// The link each block would draw as hovered with the pointer at `(x, y)`,
/// each block asked at its own origin as `draw_messages` asks it.
fn hovered(surface: &ChatSurface, x: f32, y: f32) -> Vec<String> {
    let message = &surface.message_layouts[0];
    (0..message.blocks.len())
        .filter_map(|index| {
            let (_, (ox, oy)) = surface.response_layout(0, Some(index))?;
            message.blocks[index]
                .link_at(x - ox, y - oy)
                .map(|link| link.url.to_owned())
        })
        .collect()
}

#[test]
fn a_pointer_on_body_text_hovers_no_link_and_on_a_link_only_that_link() {
    let surface = surface_with_reply();
    let message = &surface.message_layouts[0];
    assert_eq!(message.blocks.len(), 3);
    let first = &message.blocks[0];
    let (_, (ox, oy)) = surface.response_layout(0, Some(0)).unwrap();
    let ap = first.links.iter().find(|link| link.url == AP).unwrap();
    let (link_x, link_y) = middle_of(&first.layout, ap.range.clone());

    // In the paragraph between the two lines of links, straight below the
    // first link: parley snaps a point under a layout to its last line, and
    // above one to its first, so every block used to answer with its link.
    let body = &message.blocks[1];
    let (_, (_, body_y)) = surface.response_layout(0, Some(1)).unwrap();
    let (x, y) = (ox + link_x, body_y + body.height() / 2.0);
    assert!(hovered(&surface, x, y).is_empty());
    let hit = surface.response_text_hit(x, y).unwrap();
    assert_eq!(hit.position.block, Some(1));
    assert_eq!(hit.link, None);

    // On the link itself: that link, and only it.
    let (x, y) = (ox + link_x, oy + link_y);
    assert_eq!(hovered(&surface, x, y), [AP]);
    assert_eq!(
        surface.response_text_hit(x, y).unwrap().link.as_deref(),
        Some(AP)
    );
}

#[test]
fn a_source_line_answers_only_on_a_title() {
    let surface = surface_with_reply();
    let sources = surface.message_layouts[0].sources.as_ref().unwrap();
    let ap = &sources.links[0];
    let (x, y) = middle_of(&sources.layout, ap.bytes.clone());
    assert_eq!(sources.hit(x, y), Some(AP));
    // Below the line, before the titles and after the last are not a link.
    assert_eq!(sources.hit(x, sources.layout.height() + 40.0), None);
    assert_eq!(sources.hit(2.0, y), None);
    assert_eq!(sources.hit(sources.layout.width() + 30.0, y), None);
}
