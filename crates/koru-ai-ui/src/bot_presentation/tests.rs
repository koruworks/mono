// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::work_view::tests::finished;
use koru_ai_core::Timestamp;
use koru_ai_wire::bot_threads as wire;

#[test]
fn bot_activity_frames_repaint_cached_text_and_stop_for_reduced_motion_or_completion() {
    let (mut surface, _, _) = bot_surface();
    surface.transcript.as_mut().unwrap().messages[1].state = MessageState::Pending;
    surface.rebuild_messages(420.0);
    surface.messages_dirty = false;
    let header = Rc::clone(&surface.work_layouts[0].header);
    let now = Instant::now();
    surface.work_ticked_at = Some(now);
    let next = now + koru_chats_ui::status::FRAME;
    assert_eq!(surface.work_tick_deadline(), Some(next));
    assert!(surface.maintain(next));
    assert!(
        !surface.messages_dirty,
        "animation never rebuilds the transcript"
    );
    assert!(Rc::ptr_eq(&header, &surface.work_layouts[0].header));
    surface.set_reduced_motion(true);
    assert!(
        surface.work_tick_deadline().is_none(),
        "Bot status has no elapsed counter to tick"
    );
    surface.set_reduced_motion(false);
    surface.transcript.as_mut().unwrap().messages[1].state = MessageState::Complete;
    surface.rebuild_messages(420.0);
    assert!(surface.work_tick_deadline().is_none());
    assert_eq!(surface.work_layouts[0].height, 0.0);
}

#[test]
fn bot_activity_follows_acceptance_reasoning_web_and_text_without_guessing_intent() {
    use crate::reader::Update;
    use koru_ai_wire::{PublicAiEvent as Event, SendTurnAccepted};
    let (mut surface, reply, _) = bot_surface();
    let transcript = surface.transcript.as_mut().unwrap();
    transcript.tool_activities.clear();
    transcript.messages[1].state = MessageState::Pending;
    transcript.messages[1].markdown.clear();
    transcript.blocks.clear();
    let attempt = InferenceAttemptId::new();
    let user = transcript.messages[0].id;
    surface.apply_update(Update::Accepted(SendTurnAccepted {
        attempt_id: attempt,
        user_message_id: user,
        assistant_message_id: reply,
        replayed: false,
    }));
    let status = |surface: &mut ChatSurface| {
        surface.rebuild_messages(420.0);
        surface.work_layouts[0].header_text.clone()
    };
    assert_eq!(status(&mut surface), "Waiting to start…");
    surface.observe_bot_progress(&Event::ReasoningDelta {
        attempt_id: attempt,
        text: "private reasoning".into(),
        at: Timestamp::from_millis(1),
    });
    assert_eq!(status(&mut surface), "Thinking…");
    surface.open_searches.insert("search".into());
    assert_eq!(status(&mut surface), "Searching the web…");
    surface.open_searches.clear();
    surface.observe_bot_progress(&Event::TextDelta {
        attempt_id: attempt,
        text: "Here is the answer".into(),
        at: Timestamp::from_millis(2),
    });
    assert_eq!(status(&mut surface), "Typing…");
    surface.observe_bot_progress(&Event::ReasoningDelta {
        attempt_id: InferenceAttemptId::new(),
        text: "stale turn".into(),
        at: Timestamp::from_millis(3),
    });
    assert_eq!(
        status(&mut surface),
        "Typing…",
        "another attempt cannot change this status"
    );
    surface.observe_bot_progress(&Event::Completed {
        attempt_id: attempt,
        stop_reason: None,
        at: Timestamp::from_millis(4),
    });
    assert!(surface.bot_progress.is_none());
}

pub(crate) fn bot_surface() -> (ChatSurface, MessageId, ToolInvocationId) {
    let mut surface = ChatSurface::new();
    surface.set_viewport(Rect::new(0.0, 0.0, 640.0, 600.0));
    let (reply, [call, _]) = finished(&mut surface);
    let opened = crate::managed::tests::opened();
    surface.owned = Some(OwnedConversation {
        chat: surface.transcript.as_ref().unwrap().chat.id,
        destination: WorkspaceId::new(),
        provider: opened.snapshot.conversation.chat.current_model.provider,
        offers: OwnedOffers::default(),
        refusals: &[],
    });
    surface.working_bot = Some(WorkingBot {
        face: 3,
        name: "Scout".into(),
        pose: Default::default(),
    });
    (surface, reply, call)
}

#[test]
fn bot_prose_survives_tools_and_details_follow_the_actual_message() {
    let (mut surface, reply, call) = bot_surface();
    surface.transcript.as_mut().unwrap().tool_activities[0].agent_output =
        Some("{\"answer\":42}".into());
    surface.rebuild_messages(420.0);
    let message = surface
        .message_layouts
        .iter()
        .find(|m| m.id == Some(reply))
        .unwrap();
    assert_eq!(message.display, message.markdown);
    assert!(message.display.starts_with("I'll look at the file."));
    let work = &surface.work_layouts[0];
    assert!(work.bot && !work.live && !work.open && work.lines.is_empty());
    assert_eq!(work.height, 0.0, "finished work has no persistent header");
    assert_eq!(
        work.top,
        message.top + surface.shown_message_height(message) + CARD_GAP
    );
    surface.toggle_work(WorkHit::Header(reply));
    assert!(surface.bot_work_activate(koru_draw::el::Key::model(
        call.as_uuid().as_u128(),
        "bot-work-call"
    )));
    surface.rebuild_messages(420.0);
    assert!(
        surface.work_layouts[0].lines.iter().all(|line| !line.said),
        "no duplicated commentary in Details"
    );
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
    crate::tests::COPIED.with(|copied| copied.borrow_mut().clear());
    assert!(surface.toggle_work(WorkHit::CopyOutput(call)));
    crate::tests::COPIED.with(|copied| assert_eq!(*copied.borrow(), ["{\"answer\":42}"]));
}

#[test]
fn live_status_is_quiet_and_the_separate_anchor_opens_by_pointer_and_keyboard() {
    let (mut surface, reply, _) = bot_surface();
    let transcript = surface.transcript.as_mut().unwrap();
    transcript.messages[1].state = MessageState::Pending;
    transcript.tool_activities[0].status = ToolActivityStatus::Running;
    let area = surface.conversation_rect();
    surface.rebuild_messages(area.w);
    assert!(surface.work_layouts[0].live && !surface.work_layouts[0].open);
    let work = &surface.work_layouts[0];
    let rect = status_rect(
        work,
        area.x + PROSE_PAD,
        area.y + work.top,
        area.w - PROSE_PAD * 2.0,
    );
    surface.mouse_move(rect.x + 1.0, rect.y + 1.0);
    assert!(
        !surface.opened_work.contains(&reply),
        "hover never opens work"
    );
    surface.mouse_down(rect.x + 1.0, rect.y + 1.0);
    assert!(
        !surface.bot_work_open(),
        "activity words are not a disclosure"
    );
    let anchor = surface.bot_work_anchor(reply).unwrap();
    assert!(surface.mouse_down(anchor.x + 1.0, anchor.y + 1.0));
    assert!(surface.bot_work_open());
    assert!(surface.key(&Key::Named(NamedKey::Escape), None, ModifiersState::empty()));
    assert!(!surface.bot_work_open());
    surface.focus_work(WorkHit::Header(reply));
    assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()));
    assert!(surface.bot_work_open());
    surface.close_bot_work();
    let transcript = surface.transcript.as_mut().unwrap();
    transcript.messages[1].state = MessageState::Complete;
    transcript.tool_activities[0].status = ToolActivityStatus::Complete;
    surface.rebuild_messages(420.0);
    assert!(!surface.work_layouts[0].live && !surface.work_layouts[0].open);
    assert_eq!(surface.work_layouts[0].height, 0.0);
    assert!(surface.work_tick_deadline().is_none());
}

#[test]
fn actual_discovery_then_named_read_updates_the_bot_status_without_exposing_plumbing() {
    let (mut surface, reply, _) = bot_surface();
    let transcript = surface.transcript.as_mut().unwrap();
    transcript.messages[1].state = MessageState::Pending;
    transcript.tool_activities.truncate(1);
    let call = &mut transcript.tool_activities[0];
    call.status = ToolActivityStatus::Running;
    call.tool_name = "agent.other".into();
    call.agent_label = Some("ToolSearch".into());
    call.agent_input = Some(r#"{"query":"private query"}"#.into());
    surface.rebuild_messages(420.0);
    assert_eq!(surface.work_layouts.len(), 1);
    assert_eq!(surface.work_layouts[0].header_text, "Finding tools…");
    surface.toggle_work(WorkHit::Header(reply));
    surface.rebuild_messages(420.0);
    assert!(
        surface.work_layouts[0].lines.is_empty(),
        "discovery stays out of expanded history"
    );

    let document = DocumentId::new();
    surface
        .document_titles
        .insert(document, "Top News Today".into());
    let call = &mut surface.transcript.as_mut().unwrap().tool_activities[0];
    call.agent_label = Some("mcp__koru__read_document".into());
    call.agent_input = Some(serde_json::json!({"document_id":document}).to_string());
    surface.rebuild_messages(420.0);
    assert_eq!(
        surface.work_layouts[0].header_text,
        "Reading Top News Today…"
    );
    assert!(
        !surface
            .bot_work
            .as_ref()
            .unwrap()
            .layout
            .as_ref()
            .unwrap()
            .lines
            .is_empty(),
        "the real read still has details"
    );
}

#[test]
fn pending_approval_stays_prominent_outside_the_closed_disclosure() {
    let (mut surface, _, _) = bot_surface();
    let transcript = surface.transcript.as_mut().unwrap();
    transcript.messages[1].state = MessageState::Pending;
    transcript.tool_activities[0].status = ToolActivityStatus::ApprovalRequired;
    transcript.tool_activities[0].approval = koru_ai_core::ToolApproval::Pending;
    surface.rebuild_messages(420.0);
    assert!(!surface.work_layouts[0].open);
    assert!(surface.work_layouts[0].lines.is_empty());
    assert_eq!(surface.tool_activity_layouts.len(), 1);
    assert!(surface.tool_activity_layouts[0].expanded_height > STATUS_H);
    assert!(surface.tool_activity_layouts[0].top >= surface.work_layouts[0].top + STATUS_H);
}

#[test]
fn native_streamed_prose_and_actual_tools_use_the_same_bot_disclosure() {
    let mut opened = crate::managed::tests::opened();
    let mut state = crate::managed::State::new(&mut opened, Instant::now());
    let reply = MessageId::from_uuid(state.snapshot.turns[0].reply_message.unwrap());
    let mut call = crate::managed::tests::event(&state, 3, None);
    let wire::AgentEvent::TurnStarted { turn_id } = call.observation.event else {
        panic!()
    };
    call.observation.event = wire::AgentEvent::ToolStarted {
        turn_id,
        tool_call_id: uuid::Uuid::new_v4().to_string().parse().unwrap(),
        label: "Read fixture".into(),
        input: Some("fixture.rs".into()),
    };
    let events = vec![
        crate::managed::tests::event(&state, 1, None),
        crate::managed::tests::event(&state, 2, Some("I’ll check the recorded result.")),
        call,
    ];
    state.refresh(
        crate::managed::Refreshed {
            navigation: Default::default(),
            snapshot: state.snapshot.clone(),
            messages: vec![],
            after: 0,
            feed: crate::managed::Feed {
                boot: uuid::Uuid::new_v4(),
                cursor: 3,
                gap: false,
                entries: events,
            },
            questions: vec![],
            attention: Default::default(),
        },
        Instant::now(),
    );
    let mut surface = ChatSurface::new();
    surface.transcript = Some(state.transcript());
    surface.managed = Some(state);
    surface.rebuild_messages(420.0);
    let message = surface
        .message_layouts
        .iter()
        .find(|message| message.id == Some(reply))
        .unwrap();
    assert_eq!(message.display, "I’ll check the recorded result.");
    let work = &surface.work_layouts[0];
    assert!(work.bot && work.live && !work.open);
    assert!(work.top >= message.top + surface.shown_message_height(message));
    surface.toggle_work(WorkHit::Header(reply));
    surface.rebuild_messages(420.0);
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
            .any(|line| matches!(line.hit, Some(WorkHit::Call(_))))
    );
}

#[test]
fn empty_pending_bot_has_only_one_measured_status_and_no_blank_answer_band() {
    let (mut surface, reply, _) = bot_surface();
    let transcript = surface.transcript.as_mut().unwrap();
    transcript.messages[1].state = MessageState::Pending;
    transcript.messages[1].markdown.clear();
    transcript.blocks.clear();
    transcript.tool_activities.clear();
    surface.answering = true;
    surface.pending_assistant_message_id = Some(reply);
    surface.rebuild_messages(420.0);
    let message = surface
        .message_layouts
        .iter()
        .find(|m| m.id == Some(reply))
        .unwrap();
    assert_eq!(surface.shown_message_height(message), 0.0);
    assert_eq!(surface.work_layouts.len(), 1);
    assert_eq!(surface.work_layouts[0].top, message.top);
    assert_eq!(surface.work_layouts[0].height, STATUS_H);
    assert_eq!(
        surface.work_layouts[0].header_text,
        koru_i18n::t(surface.lang, koru_i18n::Key::AiThinking)
    );
}

#[test]
fn status_parent_contains_its_avatar_and_label_without_a_disclosure_at_narrow_widths() {
    let (mut surface, _, _) = bot_surface();
    surface.transcript.as_mut().unwrap().messages[1].state = MessageState::Pending;
    for width in [180.0, 420.0] {
        surface.rebuild_messages(width);
        let work = &surface.work_layouts[0];
        let tree = status_tree(work, surface.working_bot.as_ref(), width - PAD * 2.0, None);
        let at = Rect::new(0.0, 0.0, width - PAD * 2.0, STATUS_H);
        let frame = questions::elements::layout(&tree, at, FrameCx::root());
        let parent = frame.node(status_key(work)).unwrap();
        for name in ["Working bot avatar", "Bot status label"] {
            let child = frame.named(name).next().unwrap();
            assert!(child.parent.is_some());
            assert!(
                child.rect.x >= parent.rect.x
                    && child.rect.x + child.rect.w <= parent.rect.x + parent.rect.w + 0.5
            );
            assert!(
                child.rect.y >= parent.rect.y
                    && child.rect.y + child.rect.h <= parent.rect.y + parent.rect.h + 0.5
            );
        }
        assert_eq!(frame.hit_at(parent.rect.x + 1.0, parent.rect.y + 1.0), None);
    }
}

fn two_responders() -> (ChatSurface, MessageId, MessageId, uuid::Uuid) {
    let mut opened = crate::managed::tests::opened();
    let host = opened.snapshot.conversation.label.bot_id.as_uuid();
    opened.snapshot.conversation.label.bot_name = "CFO".into();
    let host_reply = MessageId::from_uuid(opened.snapshot.turns[0].reply_message.unwrap());
    let scout = uuid::Uuid::new_v4();
    let scout_reply = MessageId::new();
    let mut other = opened.snapshot.turns[0].clone();
    other.id = uuid::Uuid::new_v4();
    other.bot = scout;
    other.reply_message = Some(scout_reply.as_uuid());
    opened.snapshot.turns.insert(0, other);
    opened.snapshot.participants = vec![
        wire::Participant {
            bot: host,
            name: "CFO".into(),
        },
        wire::Participant {
            bot: scout,
            name: "Scout".into(),
        },
    ];
    let mut state = crate::managed::State::new(&mut opened, Instant::now());
    state.refresh(
        crate::managed::Refreshed {
            navigation: Default::default(),
            snapshot: state.snapshot.clone(),
            messages: vec![],
            after: 0,
            feed: crate::managed::Feed {
                boot: uuid::Uuid::new_v4(),
                cursor: 0,
                gap: false,
                entries: vec![],
            },
            questions: vec![],
            attention: Default::default(),
        },
        Instant::now(),
    );
    let mut surface = ChatSurface::new();
    surface.working_bot = Some(WorkingBot {
        face: 3,
        name: "CFO".into(),
        pose: koru_draw::face::FacePose::at(640, 3),
    });
    surface.bot_faces.insert(scout, 7);
    surface.transcript = Some(state.transcript());
    surface.managed = Some(state);
    surface.rebuild_messages(420.0);
    (surface, host_reply, scout_reply, scout)
}

#[test]
fn native_working_faces_follow_each_exact_reply_and_share_only_the_animation_pose() {
    let (surface, host_reply, scout_reply, _) = two_responders();
    let pose = surface.working_bot.as_ref().unwrap().pose;
    assert_eq!(surface.work_layouts.len(), 2);
    for work in &surface.work_layouts {
        assert!(work.live);
        let bot = surface.working_bot_for(work.message).unwrap();
        assert_eq!(bot.pose, pose);
        if work.message == host_reply {
            assert_eq!((bot.face, bot.name.as_str()), (3, "CFO"));
        } else {
            assert_eq!(work.message, scout_reply);
            assert_eq!((bot.face, bot.name.as_str()), (7, "Scout"));
        }
    }
    assert!(surface.working_bot_for(MessageId::new()).is_none());
}

#[test]
fn missing_other_bot_face_never_borrows_the_host_identity() {
    let (mut surface, _, scout_reply, scout) = two_responders();
    surface.bot_faces.remove(&scout);
    let bot = surface.working_bot_for(scout_reply).unwrap();
    assert_eq!((bot.face, bot.name.as_str()), (0, "Scout"));
    surface
        .managed
        .as_mut()
        .unwrap()
        .snapshot
        .participants
        .retain(|participant| participant.bot != scout);
    assert!(surface.working_bot_for(scout_reply).is_none());

    surface.managed = None;
    let legacy = surface.working_bot_for(scout_reply).unwrap();
    assert_eq!((legacy.face, legacy.name.as_str()), (3, "CFO"));
}
