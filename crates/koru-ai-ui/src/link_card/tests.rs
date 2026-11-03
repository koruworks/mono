// SPDX-License-Identifier: MPL-2.0

use std::time::{Duration, Instant};

use koru_ai_core::{
    AiProviderId, Chat, ChatId, ChatRole, Message, MessageId, MessageState, ModelSelection,
    ReasoningChoice, Timestamp,
};
use koru_ai_wire::ChatTranscript;
use koru_draw::geometry::Rect;
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};
use winit::window::CursorIcon;

use super::*;
use crate::{ChatSurface, SurfaceMode};

const DOCUMENT: &str = "koru://doc/0b9c1f1e-6a0e-4b7a-9f53-2d8f0c6a7e11";

/// A reply with a link in every kind of block a reply has: a paragraph, a
/// list item, a quote, a heading, bold and italic, an autolink, an address
/// written bare, and a table whose cells hold a web link and a document.
fn every_kind_of_link() -> String {
    format!(
        "Intro [para](https://k.test/p) text\n\n\
         - an item [list](https://k.test/l)\n\n\
         > quoted [quote](https://k.test/q)\n\n\
         ## Heading [head](https://k.test/h)\n\n\
         **bold [bold](https://k.test/b)** and *[italic](https://k.test/i)*\n\n\
         <https://k.test/auto>\n\n\
         Bare https://k.test/bare here.\n\n\
         | Video | Where |\n\
         | --- | --- |\n\
         | Intro | [Walkthrough](https://youtu.test/1) |\n\
         | Plan | [The plan]({DOCUMENT}) |\n\
         | Raw | https://k.test/cell |\n"
    )
}

/// A finished reply holding `markdown`, laid out in a window tall enough
/// that none of it scrolls.
fn surface_with(markdown: &str) -> ChatSurface {
    let now = Timestamp::from_millis(1);
    let chat_id = ChatId::new();
    let reply = MessageId::new();
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    surface.lang = koru_i18n::Lang::En;
    surface.set_viewport(Rect::new(0.0, 0.0, 1000.0, 3000.0));
    surface.transcript = Some(ChatTranscript {
        sources: Vec::new(),
        chat: Chat {
            id: chat_id,
            title: "Videos".into(),
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
        blocks: koru_ai_core::finalize_markdown(reply, markdown),
        messages: vec![Message {
            id: reply,
            chat_id,
            ordinal: 0,
            role: ChatRole::Assistant,
            state: MessageState::Complete,
            markdown: markdown.to_owned(),
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
    surface.validated_ready = true;
    let width = surface.conversation_rect().w;
    surface.rebuild_messages(width);
    surface
}

/// A point in the window on a link to `url`, found by sweeping each block
/// of the reply the way the surface asks it.
fn point_on(surface: &ChatSurface, url: &str) -> (f32, f32) {
    let message = &surface.message_layouts[0];
    for (index, block) in message.blocks.iter().enumerate() {
        let (_, (ox, oy)) = surface.response_layout(0, Some(index)).unwrap();
        let mut y = 1.0;
        while y < block.height() {
            let mut x = 1.0;
            while x < block.width() {
                if block.link_at(x, y).is_some_and(|link| link.url == url) {
                    return (ox + x, oy + y);
                }
                x += 2.0;
            }
            y += 2.0;
        }
    }
    panic!("no block draws a link to {url}")
}

fn opened() -> Vec<String> {
    crate::tests::OPENED.with(|opened| opened.borrow().clone())
}

fn copied() -> Vec<String> {
    crate::tests::COPIED.with(|copied| copied.borrow().clone())
}

/// Rest on the link at `point` past the wait, and draw so the card has its
/// place.
fn show_card(surface: &mut ChatSurface, point: (f32, f32)) -> Rect {
    surface.mouse_move(point.0, point.1);
    assert!(
        surface.link_card_buttons().is_empty(),
        "no card before the wait"
    );
    let due = surface.link_card.deadline().expect("the wait is scheduled");
    assert!(
        due <= Instant::now() + theme::motion::tooltip().hover_delay + Duration::from_millis(5)
    );
    surface.link_card.rest(Duration::from_secs(1));
    assert!(
        surface.link_card.maintain(Instant::now()),
        "a frame to show it"
    );
    assert!(!surface.link_card.maintain(Instant::now()), "and only one");
    surface.draw(&mut Scene::new(), Affine::IDENTITY);
    surface
        .link_card
        .card_rect(Instant::now())
        .expect("the card is up")
}

fn middle(rect: Rect) -> (f32, f32) {
    (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5)
}

/// **Every link in a reply has a hit region** (D95): in a paragraph, a
/// list item, a quote, a heading, bold and italic, an autolink, a bare
/// address, and a table's cell — each found, pressed and pointed at as a
/// link, with the arrow (ADR-215).
#[test]
fn a_link_in_every_kind_of_block_is_a_link() {
    let mut surface = surface_with(&every_kind_of_link());
    for url in [
        "https://k.test/p",
        "https://k.test/l",
        "https://k.test/q",
        "https://k.test/h",
        "https://k.test/b",
        "https://k.test/i",
        "https://k.test/auto",
        "https://k.test/bare",
        "https://youtu.test/1",
        DOCUMENT,
        "https://k.test/cell",
    ] {
        let (x, y) = point_on(&surface, url);
        assert_eq!(
            surface.conversation_link_at(x, y).map(|link| link.url),
            Some(url.to_owned()),
            "{url}"
        );
        assert_eq!(
            surface.response_text_hit(x, y).and_then(|hit| hit.link),
            Some(url.to_owned()),
            "{url}"
        );
        surface.pointer = (x, y);
        assert_eq!(surface.cursor_icon(), CursorIcon::Default, "{url}");
    }
}

/// **A click on a link in a table's cell opens it** (D95): a web link goes
/// to the browser, a document to the host — what a link in a sentence does.
#[test]
fn clicking_a_link_in_a_table_cell_opens_it() {
    let mut surface = surface_with(&every_kind_of_link());
    let (x, y) = point_on(&surface, "https://youtu.test/1");
    assert!(surface.mouse_down(x, y));
    surface.mouse_up();
    assert_eq!(opened(), vec!["https://youtu.test/1".to_owned()]);

    let (x, y) = point_on(&surface, DOCUMENT);
    assert!(surface.mouse_down(x, y));
    surface.mouse_up();
    assert!(matches!(
        surface.take_open_document_request(),
        Some(koru_link::KoruLink::Document { .. })
    ));
    assert_eq!(opened().len(), 1, "a document is not the browser's");
}

/// A press that slides from one cell's link to another's opens neither.
#[test]
fn a_press_let_go_over_another_link_opens_nothing() {
    let mut surface = surface_with(&every_kind_of_link());
    let (x, y) = point_on(&surface, "https://youtu.test/1");
    let (x2, y2) = point_on(&surface, "https://k.test/cell");
    assert!(surface.mouse_down(x, y));
    surface.mouse_move(x2, y2);
    surface.mouse_up();
    assert!(opened().is_empty());
}

/// **A reply's link shows the document's card** (D95), under the link,
/// with copy and *open* — no pencil, a reply cannot be edited. Open does
/// what the click does; copy copies the address.
#[test]
fn a_reply_link_shows_the_card_with_copy_and_open() {
    let mut surface = surface_with(&every_kind_of_link());
    let point = point_on(&surface, "https://youtu.test/1");
    let link = surface.conversation_link_at(point.0, point.1).unwrap();
    assert_eq!(link.label(), "[Walkthrough](https://youtu.test/1)");
    let card = show_card(&mut surface, point);
    assert!(
        card.y >= link.anchor.y + link.anchor.h - 4.0 - 0.01,
        "below the link"
    );

    let buttons = surface.link_card_buttons();
    let names = buttons.iter().map(|(_, name)| *name).collect::<Vec<_>>();
    assert_eq!(names, vec!["Copy link", "Open link"]);

    // The pointer crosses onto the card and the card stays.
    let (open, _) = buttons[1];
    let (ox, oy) = middle(open);
    surface.mouse_move(ox, oy);
    assert_eq!(surface.cursor_icon(), CursorIcon::Default);
    assert_eq!(surface.link_card_buttons().len(), 2);
    assert!(surface.mouse_down(ox, oy));
    surface.mouse_up();
    assert_eq!(opened(), vec!["https://youtu.test/1".to_owned()]);
    assert!(surface.link_card_buttons().is_empty(), "the card goes");

    let card = show_card(&mut surface, point);
    let (copy, _) = surface.link_card_buttons()[0];
    assert!(card.contains(middle(copy).0, middle(copy).1));
    let (cx, cy) = middle(copy);
    surface.mouse_move(cx, cy);
    assert!(surface.mouse_down(cx, cy));
    surface.mouse_up();
    assert_eq!(copied(), vec!["https://youtu.test/1".to_owned()]);
    assert_eq!(surface.copy_feedback.label(), Some("Link copied"));
    assert_eq!(opened().len(), 1, "copy opens nothing");
}

/// A bare address's card says the address alone.
#[test]
fn a_bare_address_card_says_only_the_address() {
    let surface = surface_with(&every_kind_of_link());
    let (x, y) = point_on(&surface, "https://k.test/bare");
    let link = surface.conversation_link_at(x, y).unwrap();
    assert_eq!(link.label(), "https://k.test/bare");
}

/// The card goes when the pointer leaves the link, on Escape (which it
/// spends), and on a scroll.
#[test]
fn leaving_escape_and_a_scroll_let_the_card_go() {
    let mut surface = surface_with(&every_kind_of_link());
    let point = point_on(&surface, "https://k.test/p");
    let card = show_card(&mut surface, point);
    surface.mouse_move(card.x + card.w + 200.0, card.y + card.h + 200.0);
    assert!(surface.link_card_buttons().is_empty());

    show_card(&mut surface, point);
    assert!(surface.key(
        &WinitKey::Named(NamedKey::Escape),
        None,
        ModifiersState::empty()
    ));
    assert!(surface.link_card_buttons().is_empty());

    show_card(&mut surface, point);
    surface.scroll_by(MouseScrollDelta::LineDelta(0.0, -1.0));
    assert!(surface.link_card_buttons().is_empty());
}

#[test]
fn the_label_is_markdown_unless_the_title_is_the_address() {
    let link = |title: &str| ConversationLink {
        url: "https://koru.test/a".into(),
        title: title.into(),
        anchor: Rect::new(0.0, 0.0, 1.0, 1.0),
    };
    assert_eq!(link("Koru").label(), "[Koru](https://koru.test/a)");
    assert_eq!(link("https://koru.test/a").label(), "https://koru.test/a");
    assert_eq!(link("  ").label(), "https://koru.test/a");
}
