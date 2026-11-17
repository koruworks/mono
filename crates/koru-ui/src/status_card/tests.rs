// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_draw::el::{FrameCx, Name, TextCache};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;

const SURFACE: &str = "status-card-test";

fn keys() -> StatusCardKeys {
    StatusCardKeys {
        card: Key::surface(SURFACE, "card"),
        title: Key::surface(SURFACE, "title"),
        status: Key::surface(SURFACE, "status"),
        note: Key::surface(SURFACE, "note"),
    }
}

fn lay(tree: &El) -> koru_draw::el::Frame {
    let mut engine = TextEngine::new();
    let mut cache = TextCache::default();
    tree.layout(
        &mut engine,
        &mut cache,
        Rect::new(0.0, 0.0, 360.0, 400.0),
        FrameCx::root(),
    )
}

fn card(tone: StatusTone, note: Option<&str>) -> StatusCard {
    StatusCard {
        name: "Request to Scout".into(),
        title: "Asked Scout".into(),
        status: "Scout is working".into(),
        tone,
        note: note.map(str::to_owned),
    }
}

/// **The card is the host's words, laid out where the host finds them**:
/// a group named by the host, its title and status at the host's keys, the
/// note only when there is one, and the actions under the lines in the
/// host's order. Nothing the host did not hand in is drawn.
#[test]
fn a_status_card_lays_out_the_hosts_words_at_the_hosts_keys() {
    let action = Key::surface(SURFACE, "action");
    let tree = card(StatusTone::Plain, None).el(
        keys(),
        None,
        vec![
            text("Open conversation")
                .id(action)
                .focusable()
                .semantics(Role::Button),
        ],
    );
    let frame = lay(&tree);
    let group = frame.node(keys().card).expect("the card");
    assert_eq!(group.role, Some(Role::Group));
    assert!(matches!(&group.label, Some(Name::Text(name)) if name == "Request to Scout"));
    let title = frame.node(keys().title).expect("the title").rect;
    let status = frame.node(keys().status).expect("the status").rect;
    let pressed = frame.node(action).expect("the action").rect;
    assert!(
        status.y >= title.y + title.h,
        "the status is under the title"
    );
    assert!(
        pressed.y >= status.y + status.h,
        "the actions are under the lines"
    );
    assert!(frame.node(keys().note).is_none(), "no note was handed in");
    for key in [keys().title, keys().status, action] {
        let inside = frame.node(key).expect("a part").rect;
        assert!(
            inside.x >= group.rect.x && inside.x + inside.w <= group.rect.x + group.rect.w + 0.5,
            "every part sits inside the card"
        );
    }
}

/// **The note and the lead are drawn only when handed in**, and a card with
/// no action has no action row.
#[test]
fn a_status_card_draws_its_note_and_lead_only_when_given() {
    let lead = Key::surface(SURFACE, "lead");
    let tree =
        card(StatusTone::Settled, Some("Paused")).el(keys(), Some(text("●").id(lead)), Vec::new());
    let frame = lay(&tree);
    let note = frame.node(keys().note).expect("the note").rect;
    let status = frame.node(keys().status).expect("the status").rect;
    assert!(
        note.y >= status.y + status.h,
        "the note is under the status"
    );
    let lead = frame.node(lead).expect("the lead").rect;
    let title = frame.node(keys().title).expect("the title").rect;
    assert!(lead.x < title.x, "the lead comes before the title");
    assert!(
        !frame
            .nodes
            .iter()
            .any(|node| node.name == "Status card actions"),
        "no action, no action row"
    );
}

/// **The attention tone is a different ink and the same words**: the tone
/// never replaces the sentence.
#[test]
fn the_attention_tone_changes_the_ink_and_never_the_words() {
    assert_ne!(StatusTone::Attention.ink(), StatusTone::Plain.ink());
    assert_ne!(StatusTone::Settled.ink(), StatusTone::Plain.ink());
    let plain = lay(&card(StatusTone::Plain, None).el(keys(), None, Vec::new()));
    let warned = lay(&card(StatusTone::Attention, None).el(keys(), None, Vec::new()));
    assert_eq!(
        plain.node(keys().status).and_then(|node| node.lines()),
        warned.node(keys().status).and_then(|node| node.lines()),
        "the same sentence either way"
    );
}
