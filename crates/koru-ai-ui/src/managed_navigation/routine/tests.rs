// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::managed_surface::tests::surface;
use koru_ai_core::bots::{BotId, BotReplyPreview};

fn card() -> wire::Card {
    let reply = Uuid::new_v4();
    wire::Card {
        id: Uuid::new_v4(),
        sequence: 2,
        message: Uuid::new_v4(),
        kind: "thread".into(),
        title: "Weekly digest".into(),
        thread: Some(ChatId::new()),
        bot: Some(BotId::mint()),
        bot_name: "Scout".into(),
        workspace: None,
        document: None,
        messages: 99,
        outputs: 12,
        state: "active".into(),
        dispatches: Vec::new(),
        routine: Some(wire::RoutineResult {
            routine: Uuid::new_v4(),
            occurrence: Uuid::new_v4(),
            at_ms: 60_000,
            manual: false,
            reply,
            preview: BotReplyPreview::new(
                MessageId::from_uuid(reply),
                "**Actual result**\n\nRead [the source](https://example.com).",
            ),
        }),
    }
}

#[test]
fn repeated_firings_keep_their_own_reply_order_and_keyboard_thread_link() {
    let (mut view, commands, _) = surface();
    view.managed
        .as_mut()
        .unwrap()
        .snapshot
        .conversation
        .label
        .purpose = koru_ai_core::bots::BotChatPurpose::Additional;
    let first = card();
    let mut second = first.clone();
    second.id = Uuid::new_v4();
    second.message = Uuid::new_v4();
    second.sequence = 4;
    let result = second.routine.as_mut().unwrap();
    result.occurrence = Uuid::new_v4();
    result.reply = Uuid::new_v4();
    result.preview = BotReplyPreview::new(MessageId::from_uuid(result.reply), "A later result");
    let expected_replies = [first.routine.as_ref().unwrap().reply, result.reply];
    view.composer.set_text("keep my draft");
    view.managed.as_mut().unwrap().navigation = State::new(Page {
        // Feed pages sort by UUID, not chronology; placement uses sequence.
        items: vec![second.clone(), first.clone()],
        ..Default::default()
    });
    view.rebuild_managed_navigation(420.0);
    view.place_managed_navigation(None, 0.0);
    let results: Vec<_> = view
        .navigation()
        .rows
        .iter()
        .filter(|row| row.routine)
        .collect();
    assert_eq!(
        results.len(),
        2,
        "one persistent thread must not merge its firings"
    );
    assert_eq!([results[0].id, results[1].id], [first.id, second.id]);
    assert_eq!(
        [
            results[0].replies[0].preview.message.as_uuid(),
            results[1].replies[0].preview.message.as_uuid()
        ],
        expected_replies
    );
    assert!(results[1].top >= results[0].top + results[0].height);
    assert!(results.iter().all(|row| row.label == "Open conversation"));
    let first_index = view
        .navigation()
        .rows
        .iter()
        .position(|row| row.id == first.id)
        .unwrap();
    view.navigation_mut().cursor = first_index;
    view.focus = Focus::ManagedNavigation;
    assert_eq!(
        view.managed_navigation_key(&Key::Named(NamedKey::Enter), ModifiersState::empty()),
        Some(true)
    );
    assert_eq!(
        view.take_open_bot_conversation(),
        Some((
            first.bot.unwrap(),
            first.bot_name.clone(),
            first.thread.unwrap()
        ))
    );
    assert_eq!(
        view.managed_navigation_key(&Key::Named(NamedKey::Tab), ModifiersState::empty()),
        Some(true)
    );
    assert_eq!(
        view.navigation().rows[view.navigation().cursor].id,
        second.id
    );
    assert_eq!(
        view.managed_navigation_key(&Key::Named(NamedKey::Space), ModifiersState::empty()),
        Some(true)
    );
    assert_eq!(
        view.take_open_bot_conversation(),
        Some((
            second.bot.unwrap(),
            second.bot_name.clone(),
            second.thread.unwrap()
        ))
    );
    assert_eq!(view.composer.text(), "keep my draft");
    assert!(
        commands.try_recv().is_err(),
        "opening a result never sends or joins"
    );
}

#[test]
fn main_chat_keeps_result_anchors_without_transcript_rows() {
    let (mut view, _, _) = surface();
    let result = card();
    view.managed.as_mut().unwrap().navigation = State::new(Page {
        items: vec![result.clone()],
        ..Default::default()
    });
    view.rebuild_managed_navigation(420.0);
    assert!(!view.navigation().rows.iter().any(|row| row.id == result.id));
    assert!(
        view.navigation()
            .items
            .iter()
            .any(|item| item.id == result.id)
    );
}

#[test]
fn routine_preview_requires_the_exact_recorded_reply_and_missing_text_is_not_invented() {
    let mut card = card();
    card.routine
        .as_mut()
        .unwrap()
        .preview
        .as_mut()
        .unwrap()
        .message = MessageId::new();
    let wrong = row(&card, None, koru_i18n::Lang::En).unwrap();
    assert!(wrong.replies.is_empty());
    assert!(matches!(wrong.action, Some(Action::Thread(..))));
    card.routine.as_mut().unwrap().preview = None;
    assert!(
        row(&card, None, koru_i18n::Lang::En)
            .unwrap()
            .replies
            .is_empty()
    );
    card.routine.as_mut().unwrap().manual = true;
    assert_eq!(
        row(&card, None, koru_i18n::Lang::En).unwrap().body,
        "Run now"
    );
}

#[test]
fn routine_reply_and_link_share_one_measured_parent_at_narrow_widths() {
    let (mut view, _, _) = surface();
    let mut card = card();
    card.title = "An especially long routine name that wraps in a narrow window".into();
    card.routine
        .as_mut()
        .unwrap()
        .preview
        .as_mut()
        .unwrap()
        .truncated = true;
    for width in [180.0, 420.0] {
        let mut entry = row(&card, None, view.lang).unwrap();
        reply::prepare(&mut entry, &mut view.engine, width);
        entry.height = entry.compact_height(&mut view.engine, &view.bot_faces, width);
        let tree = entry.compact_tree(&mut view.engine, &view.bot_faces, width);
        let frame = questions::elements::layout(
            &tree,
            Rect::new(0.0, 0.0, width, entry.height),
            koru_draw::el::FrameCx::root(),
        );
        let parent = frame
            .nodes
            .iter()
            .position(|node| node.name == "Routine result")
            .unwrap();
        let result = frame.rect_of("Routine result").unwrap();
        for name in ["Routine heading", "Bot reply", "Routine conversation"] {
            let child = frame.nodes.iter().find(|node| node.name == name).unwrap();
            assert_eq!(child.parent, Some(parent));
            assert!(
                child.rect.y >= result.y
                    && child.rect.y + child.rect.h <= result.y + result.h + 0.01
            );
            assert!(child.rect.x >= 0.0 && child.rect.x + child.rect.w <= width + 0.01);
        }
        assert_eq!(entry.replies[0].blocks[0].text, "Actual result");
        assert_eq!(
            entry.replies[0].blocks[1].links[0].url,
            "https://example.com"
        );
        let key = koru_draw::el::Key::model(entry.id.as_u128(), "bot-thread");
        let link = frame.node(key).unwrap().rect;
        assert_eq!(
            frame.hit_at(link.x + link.w / 2.0, link.y + link.h / 2.0),
            Some(key)
        );
        assert!(
            frame
                .node(koru_draw::el::Key::model(
                    entry.replies[0].preview.message.as_uuid().as_u128(),
                    "exchange-reply-more"
                ))
                .is_none(),
            "one honest thread link, including truncated results"
        );
    }
}
