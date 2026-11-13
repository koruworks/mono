// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::layout_cache::{self, MessageInput};
use crate::{ChatRole, MessageBlockId, MessageId, MessageState, render_chat_block};
use koru_draw::text::TextEngine;

const SOURCE: &str = "flowchart TD\n A[Boot up] --> B{Coffee available?}\n B -- Yes --> C[Become unstoppable]\n B -- No --> D[Enter power-saving mode]\n C --> E{Found a bug?}\n E -- Yes --> F[Call it a feature]\n E -- No --> G[Become suspicious]\n F --> H[Take a snack break]\n G --> H\n D --> H\n H --> B\n";

fn markdown() -> String {
    format!("```mermaid\n{SOURCE}```")
}

fn rebuild(surface: &mut ChatSurface, id: MessageBlockId, markdown: &str, width: f32) {
    let input = MessageInput {
        id: MessageId::from_uuid(id.as_uuid()),
        role: ChatRole::Assistant,
        state: MessageState::Complete,
        markdown: markdown.into(),
        display: markdown.into(),
        links: Vec::new(),
        wrap_width: width,
        usage_text: None,
        sources: Vec::new(),
        blocks: vec![(id, markdown.into())],
        attachments: Vec::new(),
    };
    let (mut messages, stats) = layout_cache::reconcile(
        &mut surface.engine,
        std::mem::take(&mut surface.message_layouts),
        vec![input],
        render_chat_block,
    );
    surface.diagrams.sync(&mut messages);
    surface.message_layouts = messages;
    surface.last_rebuild_stats = stats;
}

fn prepared() -> Arc<PreparedDiagram> {
    let measure: Arc<dyn koru_diagram::TextMeasure> = Arc::new(koru_draw::diagram::InterMeasure);
    let scene = koru_diagram::layout(
        SOURCE,
        &measure,
        &koru_diagram::Limits::default(),
        &koru_diagram::Cancel::new(),
    )
    .expect("the robot flowchart draws with the document engine");
    Arc::new(PreparedDiagram::new(Arc::new(scene)))
}

#[test]
fn closed_mermaid_uses_document_geometry_and_keeps_original_markdown() {
    let mut surface = ChatSurface::new();
    let id = MessageBlockId::new();
    let markdown = markdown();
    rebuild(&mut surface, id, &markdown, 260.0);
    let requests = surface.take_diagram_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(&*requests[0].text, SOURCE.trim_end());
    assert!(!surface.message_layouts[0].blocks[0].diagram_drawn());
    let prepared = prepared();
    assert!(surface.diagram_result(
        requests[0].node,
        requests[0].text_hash,
        Ok(prepared.clone())
    ));
    rebuild(&mut surface, id, &markdown, 260.0);
    assert_eq!(surface.last_rebuild_stats.messages_laid_out, 0);
    let block = &surface.message_layouts[0].blocks[0];
    assert!(block.diagram_drawn());
    assert!(block.width() <= 260.0);
    assert!(block.height() > 0.0);
    assert_eq!(block.markdown, markdown);
    assert!(surface.response_fragments().is_empty());
    let height = block.height();
    rebuild(&mut surface, id, &markdown, 130.0);
    assert!(surface.message_layouts[0].blocks[0].height() < height);
    assert!(surface.take_diagram_requests().is_empty());
    surface.invalidate();
    rebuild(&mut surface, id, &markdown, 260.0);
    assert!(Arc::ptr_eq(
        surface.message_layouts[0].blocks[0]
            .diagram
            .as_ref()
            .unwrap()
            .prepared
            .as_ref()
            .unwrap(),
        &prepared
    ));
    assert!(surface.take_diagram_requests().is_empty());
}

#[test]
fn failures_stay_readable_and_are_not_retried_on_resize() {
    let mut surface = ChatSurface::new();
    let id = MessageBlockId::new();
    let markdown = "```mermaid\nnot a diagram\n```";
    rebuild(&mut surface, id, markdown, 260.0);
    let request = surface.take_diagram_requests().pop().unwrap();
    assert!(surface.diagram_result(
        request.node,
        request.text_hash,
        Err(Status::NotParsed {
            line: 1,
            message: "Invalid test diagram".into(),
            code: "test.invalid".into()
        })
    ));
    rebuild(&mut surface, id, markdown, 130.0);
    let block = &surface.message_layouts[0].blocks[0];
    assert!(!block.diagram_drawn());
    assert!(block.text.contains("not a diagram"));
    assert!(surface.take_diagram_requests().is_empty());
    assert!(!surface.response_fragments().is_empty());
}

#[test]
fn replies_for_removed_or_changed_blocks_are_ignored() {
    let mut surface = ChatSurface::new();
    let id = MessageBlockId::new();
    rebuild(&mut surface, id, &markdown(), 260.0);
    let old = surface.take_diagram_requests().pop().unwrap();
    rebuild(&mut surface, id, "ordinary text", 260.0);
    assert!(!surface.diagram_result(old.node, old.text_hash, Ok(prepared())));
    rebuild(&mut surface, id, &markdown(), 260.0);
    assert!(!surface.diagram_result(old.node, old.text_hash, Ok(prepared())));
    assert_ne!(surface.take_diagram_requests()[0].node, old.node);
}

#[test]
fn only_mermaid_code_is_a_diagram_and_streaming_waits_for_closure() {
    let mut engine = TextEngine::new();
    let id = MessageBlockId::new();
    for markdown in [
        SOURCE.to_owned(),
        format!("```rust\n{SOURCE}```"),
        format!("```Mermaid\n{SOURCE}```"),
    ] {
        assert!(
            render_chat_block(&mut engine, id, &markdown, 300.0)
                .diagram
                .is_none()
        );
    }
    assert!(
        render_chat_block(&mut engine, id, &format!("~~~mermaid\n{SOURCE}~~~"), 300.0)
            .diagram
            .is_some()
    );
    let mut live = crate::live_markdown::LiveAnswer::default();
    let message = MessageId::new();
    let open = format!("```mermaid\n{SOURCE}");
    let blocks = live.blocks(message, 0, &open);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].0, live.tail_id());
    assert!(
        crate::live_markdown::plain_block(&mut engine, blocks[0].0, &blocks[0].1, 300.0)
            .diagram
            .is_none()
    );
    let closed = live.blocks(message, 0, &format!("{open}```\n\n"));
    assert_ne!(closed[0].0, live.tail_id());
    assert!(
        render_chat_block(&mut engine, closed[0].0, &closed[0].1, 300.0)
            .diagram
            .is_some()
    );
}
