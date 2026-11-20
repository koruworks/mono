// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::bot_presentation::tests::bot_surface;
use crate::reader::Reader;

#[test]
fn bot_details_shares_the_bottom_action_row_and_its_tooltip_hit_target() {
    let (mut surface, reply, _) = bot_surface();
    let area = surface.conversation_rect();
    surface.rebuild_messages(area.w);
    let index = surface
        .message_layouts
        .iter()
        .position(|m| m.id == Some(reply))
        .unwrap();
    surface.response_message_hover = Some(index);
    let message = &surface.message_layouts[index];
    let actions: Vec<_> = surface.reply_actions(message).collect();
    assert_eq!(
        actions,
        [
            ReplyAction::Copy,
            ReplyAction::Store,
            ReplyAction::Link,
            ReplyAction::Details
        ]
    );
    let card = message_card(
        area,
        area.y + message.top,
        surface.shown_message_height(message),
        message,
    );
    let rects: Vec<_> = actions
        .iter()
        .enumerate()
        .map(|(slot, _)| surface.reply_action_rect(message, card, slot))
        .collect();
    assert!(rects[0].y >= card.y + message.padding() + message_content_height(message));
    for (slot, action) in actions.iter().enumerate() {
        let at = rects[slot];
        assert_eq!(at.y, rects[0].y);
        if slot > 0 {
            assert!(rects[slot - 1].x + rects[slot - 1].w < at.x);
        }
        let point = (at.x + at.w * 0.5, at.y + at.h * 0.5);
        let hint = surface.reply_tooltip_at(point.0, point.1).unwrap();
        assert_eq!(hint.label, t(surface.lang, action.words()));
        assert_eq!(hint.anchor, at);
        assert_eq!(
            surface.response_action_hover_at(point.0, point.1),
            Some((index, *action))
        );
    }
    let anchor = surface.bot_work_anchor(reply).unwrap();
    assert_eq!(anchor, *rects.last().unwrap());
    assert!(surface.mouse_down(anchor.x + 1.0, anchor.y + 1.0));
    assert!(surface.bot_work_open());
    surface.close_bot_work();
    surface.focus_work(work_view::WorkHit::Header(reply));
    assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()));
    assert!(surface.bot_work_open());
}

#[test]
fn legacy_bot_refuses_branch_commands_but_ordinary_chat_still_branches() {
    let (mut surface, reply, _) = bot_surface();
    let chat = surface.current_chat_id().unwrap();
    let (reader, commands, _) = Reader::test_pair();
    surface.reader = Some(reader);
    let request = BranchChatRequest {
        branch_chat_id: ChatId::new(),
        through_message_id: reply,
        title: "branch".into(),
    };
    assert!(!surface.perform_response_action(ResponseAction::Branch(reply)));
    assert!(!surface.owned_admits(&Command::Branch(chat, request.clone())));
    assert!(!surface.queue_command(Command::Branch(chat, request.clone())));
    assert!(commands.try_recv().is_err());
    surface.owned = None;
    surface.working_bot = None;
    assert!(!surface.bot_chat_flow());
    let area = surface.conversation_rect();
    surface.rebuild_messages(area.w);
    let message = surface
        .message_layouts
        .iter()
        .find(|m| m.id == Some(reply))
        .unwrap();
    assert_eq!(
        surface.reply_actions(message).collect::<Vec<_>>(),
        [
            ReplyAction::Copy,
            ReplyAction::Branch,
            ReplyAction::Store,
            ReplyAction::Link
        ]
    );
    assert!(surface.queue_command(Command::Branch(chat, request)));
    assert!(matches!(commands.try_recv().unwrap(), Command::Branch(id, _) if id == chat));
}

#[test]
fn native_actions_keep_capability_limits_and_observer_only_has_details() {
    let (mut surface, reply, _) = bot_surface();
    let (native, _, _) = crate::managed_surface::tests::surface();
    surface.managed = native.managed;
    let area = surface.conversation_rect();
    surface.rebuild_messages(area.w);
    let message = surface
        .message_layouts
        .iter()
        .find(|m| m.id == Some(reply))
        .unwrap();
    assert_eq!(
        surface.reply_actions(message).collect::<Vec<_>>(),
        [ReplyAction::Copy, ReplyAction::Details]
    );
    let state = surface.managed.as_mut().unwrap();
    state.snapshot.joined = false;
    state.snapshot.origin = Some(koru_ai_wire::bot_threads::Origin {
        thread: ChatId::new(),
        bot: uuid::Uuid::new_v4(),
        name: "CFO".into(),
        title: "Main".into(),
        main: true,
    });
    assert!(surface.observing_bot_thread());
    let message = surface
        .message_layouts
        .iter()
        .find(|m| m.id == Some(reply))
        .unwrap();
    assert_eq!(
        surface.reply_actions(message).collect::<Vec<_>>(),
        [ReplyAction::Details]
    );
}

#[test]
fn retry_has_its_own_localized_tooltip_and_never_takes_the_branch_label() {
    let mut surface = ChatSurface::new();
    let (reply, _) = crate::work_view::tests::finished(&mut surface);
    surface
        .transcript
        .as_mut()
        .unwrap()
        .messages
        .iter_mut()
        .find(|m| m.id == reply)
        .unwrap()
        .state = MessageState::Failed;
    let area = surface.conversation_rect();
    surface.rebuild_messages(area.w);
    let index = surface
        .message_layouts
        .iter()
        .position(|m| m.id == Some(reply))
        .unwrap();
    surface.response_message_hover = Some(index);
    let message = &surface.message_layouts[index];
    assert_eq!(
        surface.reply_actions(message).collect::<Vec<_>>(),
        [ReplyAction::Retry]
    );
    let card = message_card(
        area,
        area.y + message.top,
        surface.shown_message_height(message),
        message,
    );
    let at = surface.reply_action_rect(message, card, 0);
    assert_eq!(
        surface
            .reply_tooltip_at(at.x + 1.0, at.y + 1.0)
            .unwrap()
            .label,
        "Retry reply"
    );
}
