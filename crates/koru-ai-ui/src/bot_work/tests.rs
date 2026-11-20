// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::bot_presentation::tests::bot_surface;

#[test]
fn exact_reply_keeps_discovery_full_copy_and_does_not_follow_newer_work() {
    let (mut surface, reply, call) = bot_surface();
    let output = "retained output\n".repeat(300);
    let transcript = surface.transcript.as_mut().unwrap();
    transcript.tool_activities[0].agent_label = Some("ToolSearch".into());
    transcript.tool_activities[0].agent_output = Some(output.clone());
    let width = surface.conversation_rect().w;
    surface.rebuild_messages(width);
    assert!(surface.open_bot_work(reply));
    let open = surface.bot_work.as_ref().unwrap();
    assert!(open.calls.is_empty());
    assert!(
        open.layout
            .as_ref()
            .unwrap()
            .lines
            .iter()
            .all(|line| line.copy_output.is_none())
    );
    assert!(
        open.layout
            .as_ref()
            .unwrap()
            .lines
            .iter()
            .any(|line| line.hit == Some(WorkHit::Call(call))),
        "discovery remains a collapsed, expandable call"
    );
    assert!(surface.bot_work_action(WorkHit::Call(call)));
    surface.prepare_bot_work();
    assert!(
        surface
            .bot_work
            .as_ref()
            .unwrap()
            .layout
            .as_ref()
            .unwrap()
            .lines
            .iter()
            .any(|line| line.copy_output == Some(call))
    );
    let mut unrelated = surface.transcript.as_ref().unwrap().tool_activities[0].clone();
    unrelated.id = ToolInvocationId::new();
    unrelated.assistant_message_id = MessageId::new();
    unrelated.agent_output = Some("other reply".into());
    surface
        .transcript
        .as_mut()
        .unwrap()
        .tool_activities
        .push(unrelated);
    surface.messages_dirty = true;
    surface.prepare_bot_work();
    assert_eq!(surface.bot_work.as_ref().unwrap().reply, reply);
    crate::tests::COPIED.with(|copied| copied.borrow_mut().clear());
    assert!(surface.bot_work_action(WorkHit::CopyOutput(call)));
    crate::tests::COPIED.with(|copied| assert_eq!(*copied.borrow(), [output]));
    assert_eq!(
        surface.bot_work.as_ref().unwrap().calls.len(),
        1,
        "expanding one call does not expand its neighbors"
    );
    assert!(
        !surface
            .bot_work_semantics()
            .unwrap()
            .details
            .contains("other reply")
    );
    surface.close_bot_work();
    assert!(surface.open_bot_work(reply));
    assert!(
        surface.bot_work.as_ref().unwrap().calls.is_empty(),
        "each opening starts collapsed"
    );
}

#[test]
fn modal_focus_scroll_and_dismissal_do_not_touch_conversation_or_draft() {
    let (mut surface, reply, call) = bot_surface();
    surface.transcript.as_mut().unwrap().tool_activities[0].agent_output =
        Some("line\n".repeat(200));
    surface.set_viewer_lift(Some(ViewerLift {
        origin: (100.0, 70.0),
        size: (900.0, 300.0),
    }));
    let width = surface.conversation_rect().w;
    surface.rebuild_messages(width);
    surface.conversation_scroll = 17.0;
    surface.follow_tail = false;
    assert!(surface.open_bot_work(reply));
    assert!(surface.bot_work_action(WorkHit::Call(call)));
    surface.prepare_bot_work();
    let count = surface.bot_work.as_ref().unwrap().controls.len() + 1;
    for _ in 0..count {
        assert!(surface.key(&Key::Named(NamedKey::Tab), None, ModifiersState::empty()));
    }
    assert!(
        surface.bot_work.as_ref().unwrap().focus.is_none(),
        "Tab wraps to Close"
    );
    surface.bot_work_scroll(MouseScrollDelta::LineDelta(0.0, -20.0));
    assert!(surface.bot_work.as_ref().unwrap().scroll > 0.0);
    assert_eq!(surface.conversation_scroll, 17.0);
    assert!(!surface.follow_tail);
    assert!(surface.key(
        &Key::Character("x".into()),
        Some("x"),
        ModifiersState::empty()
    ));
    assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::CONTROL));
    assert!(
        surface.bot_work_open(),
        "modified Enter cannot activate or send"
    );
    assert!(surface.bot_work_pointer(0.0, 0.0, true));
    assert!(!surface.bot_work_open());
    assert!(
        surface.bot_work_release(),
        "dismissal consumes the release too"
    );
    assert_eq!(surface.work_focus(), Some(WorkHit::Header(reply)));
}

#[test]
fn disappearing_source_closes_before_input_and_rejects_old_copy() {
    let (mut surface, reply, call) = bot_surface();
    let width = surface.conversation_rect().w;
    surface.rebuild_messages(width);
    surface.open_bot_work(reply);
    surface.transcript.as_mut().unwrap().tool_activities.clear();
    surface.messages_dirty = true;
    assert!(surface.bot_work_key(&Key::Named(NamedKey::Tab), ModifiersState::empty()));
    assert!(!surface.bot_work_open());
    assert!(!surface.bot_work_activate(key(WorkHit::CopyOutput(call))));
}

#[test]
fn ordinary_chat_has_no_bot_modal_and_bot_navigation_closes_it() {
    let (mut surface, reply, _) = bot_surface();
    let width = surface.conversation_rect().w;
    surface.rebuild_messages(width);
    surface.open_bot_work(reply);
    let mut owned = surface.owned.clone().unwrap();
    owned.chat = ChatId::new();
    surface.bind_owned(owned);
    assert!(!surface.bot_work_open());
    let mut ordinary = ChatSurface::new();
    let (reply, _) = crate::work_view::tests::finished(&mut ordinary);
    assert!(!ordinary.open_bot_work(reply));
}

#[test]
fn both_bot_roles_share_narrow_cap_and_hug_while_ordinary_layout_is_unchanged() {
    let (mut surface, _, _) = bot_surface();
    for viewport in [240.0, 1400.0] {
        surface.set_viewport(Rect::new(0.0, 0.0, viewport, 600.0));
        let area = surface.conversation_rect();
        assert!(area.w <= CONTENT_MAX_W * 0.8);
        surface.rebuild_messages(area.w);
        for message in &surface.message_layouts {
            let card = message_card(area, 0.0, 100.0, message);
            assert!(card.w <= area.w * 0.8 + 0.01);
            assert!(message.wrap_width <= area.w * 0.8);
            if message.role == ChatRole::User {
                assert!((card.x + card.w - area.x - area.w).abs() < 0.01);
            } else {
                assert_eq!(card.x, area.x);
            }
        }
        let short = surface.user_card("Yes", vec![], area.w);
        assert!(message_card(area, 0.0, 30.0, &short).w < area.w * 0.8);
    }
    let mut ordinary = ChatSurface::new();
    ordinary.set_viewport(Rect::new(0.0, 0.0, 1400.0, 600.0));
    assert_eq!(ordinary.content_rect().w, CONTENT_MAX_W);
    let (_, _) = crate::work_view::tests::finished(&mut ordinary);
    let area = ordinary.conversation_rect();
    ordinary.rebuild_messages(area.w);
    let assistant = ordinary
        .message_layouts
        .iter()
        .find(|message| message.role == ChatRole::Assistant)
        .unwrap();
    assert_eq!(message_card(area, 0.0, 100.0, assistant).w, area.w);
}

#[test]
fn pointer_hover_reuses_detail_and_frame() {
    let (mut surface, reply, _) = bot_surface();
    let width = surface.conversation_rect().w;
    surface.rebuild_messages(width);
    surface.open_bot_work(reply);
    let layout = Rc::clone(surface.bot_work.as_ref().unwrap().layout.as_ref().unwrap());
    let frame = surface.bot_work.as_ref().unwrap().frame.as_ref().unwrap() as *const Frame;
    for x in 0..20 {
        surface.bot_work_pointer(x as f32, 40.0, false);
    }
    assert!(Rc::ptr_eq(
        &layout,
        surface.bot_work.as_ref().unwrap().layout.as_ref().unwrap()
    ));
    assert_eq!(
        frame,
        surface.bot_work.as_ref().unwrap().frame.as_ref().unwrap() as *const Frame
    );
}

#[test]
fn consecutive_empty_tool_replies_have_separate_reachable_anchors() {
    let (mut surface, first, _) = bot_surface();
    let transcript = surface.transcript.as_mut().unwrap();
    transcript.messages[1].markdown.clear();
    transcript.blocks.clear();
    let mut second = transcript.messages[1].clone();
    second.id = MessageId::new();
    second.ordinal += 1;
    let second_id = second.id;
    transcript.messages.push(second);
    let mut call = transcript.tool_activities[0].clone();
    call.id = ToolInvocationId::new();
    call.assistant_message_id = second_id;
    transcript.tool_activities.push(call);
    let area = surface.conversation_rect();
    surface.rebuild_messages(area.w);
    let one = surface.bot_work_anchor(first).unwrap();
    let two = surface.bot_work_anchor(second_id).unwrap();
    assert!(one.y + one.h <= two.y);
    assert!(two.y + two.h <= area.y + surface.conversation_content_extent());
    for (reply, anchor) in [(first, one), (second_id, two)] {
        assert_eq!(
            surface.bot_work_anchor_at(anchor.x + 1.0, anchor.y + 1.0),
            Some(reply)
        );
        assert!(surface.mouse_down(anchor.x + 1.0, anchor.y + 1.0));
        assert_eq!(surface.bot_work.as_ref().unwrap().reply, reply);
        surface.close_bot_work();
        surface.bot_work_release();
    }
}
