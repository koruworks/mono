// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn single_bot_main_omits_identity_rows_but_side_and_shared_threads_keep_them() {
    let (mut view, _, _) = crate::managed_surface::tests::surface();
    let state = view.managed.as_mut().unwrap();
    let bot = state.snapshot.conversation.label.bot_id.as_uuid();
    state.snapshot.participants = vec![wire::Participant { bot, name: "Scout".into() }];
    let mut message = crate::managed::tests::message(1, Uuid::new_v4());
    message.sender = bot.to_string();
    let id = message.id;
    state.ingest(message);
    view.rebuild_managed_navigation(420.0);
    assert!(!view.navigation().rows.iter().any(|r| r.id == id));
    view.managed.as_mut().unwrap().snapshot.conversation.label.purpose = koru_ai_core::bots::BotChatPurpose::Additional;
    view.rebuild_managed_navigation(420.0);
    assert!(view.navigation().rows.iter().any(|r| r.id == id && !r.targets.is_empty()));
    let state = view.managed.as_mut().unwrap();
    state.snapshot.conversation.label.purpose = koru_ai_core::bots::BotChatPurpose::Main;
    state.snapshot.participants.push(wire::Participant { bot: Uuid::new_v4(), name: "Guest".into() });
    view.rebuild_managed_navigation(420.0);
    assert!(view.navigation().rows.iter().any(|r| r.id == id && !r.targets.is_empty()));
}
use crate::managed_surface::tests::surface;

#[test]
fn compact_threads_reserve_their_painted_height_and_the_scroll_tail() {
    let (mut view, _, _) = surface();
    // A running turn must not add a Stop card or reserve space above the links.
    view.managed.as_mut().unwrap().navigation = State::new(Page {
        items: ["Scout", "CFO"]
            .into_iter()
            .map(|name| wire::Card {
                bot_name: name.into(),
                ..side()
            })
            .collect(),
        ..Default::default()
    });
    for width in [180.0, 420.0, 1000.0] {
        view.rebuild_managed_navigation(width);
        let end = view.place_managed_navigation(None, 80.0);
        let rows = &view.managed.as_ref().unwrap().navigation.rows;
        assert_eq!(rows.len(), 2);
        for row in rows {
            let viewport = Rect::new(24.0, 60.0, width, 240.0);
            let at = row.rect(viewport, 0.0);
            let scrolled = row.rect(viewport, 48.0);
            assert_eq!(scrolled.y, at.y - 48.0);
            assert!(scrolled.contains(at.x + 1.0, at.y - 48.0 + 1.0));
            let tree = row.compact_tree(&mut view.engine, &view.bot_faces, width);
            let frame = questions::elements::layout(
                &tree,
                Rect::new(0.0, row.top, width, row.height),
                koru_draw::el::FrameCx::root(),
            );
            let identity = frame.rect_of("Bot identity").unwrap();
            assert!(row.height >= identity.h && identity.h > 0.0);
            assert!(identity.y >= row.top);
            assert!(identity.y + identity.h <= row.top + row.height + 0.01);
        }
        assert!(rows[1].top >= rows[0].top + rows[0].height + CARD_GAP);
        assert_eq!(view.managed_navigation_extent(), end);
        assert!(view.conversation_content_extent() >= end);
    }
}

#[test]
fn side_threads_have_no_join_stop_or_composer_and_reject_sends() {
    let (mut view, commands, _) = surface();
    let thread = view.managed.as_ref().unwrap().thread();
    let state = view.managed.as_mut().unwrap();
    state.snapshot.joined = false;
    state.snapshot.origin = Some(wire::Origin {
        thread: ChatId::new(),
        bot: Uuid::new_v4(),
        name: "CFO".into(),
        title: "Main".into(),
        main: true,
    });
    view.composer.set_text("a draft that must remain unsent");
    assert!(view.observing_bot_thread());
    view.rebuild_managed_navigation(420.0);
    assert!(
        !view
            .navigation()
            .rows
            .iter()
            .any(|row| matches!(row.action, Some(Action::Join)))
    );
    assert_eq!(view.composer_rect().h, 0.0);
    assert!(!view.send_composer());
    assert!(!view.queue_command(Command::Engine {
        thread,
        command: wire::Command::Join {
            id: Uuid::new_v4(),
            thread
        }
    }));
    assert!(commands.try_recv().is_err());
    assert_eq!(view.composer.text(), "a draft that must remain unsent");
}

#[test]
fn native_thread_links_open_the_thread_and_do_not_turn_a_main_chat_read_only() {
    let (mut view, commands, _) = surface();
    let bot = koru_ai_core::bots::BotId::mint();
    let chat = ChatId::new();
    let delegated = MessageId::new();
    view.managed.as_mut().unwrap().navigation = State::new(Page {
        items: vec![wire::Card {
            bot: Some(bot),
            bot_name: "Scout".into(),
            thread: Some(chat),
            ..side()
        }],
        ..Default::default()
    });
    view.rebuild_managed_navigation(420.0);
    assert!(!view.observing_bot_thread());
    assert!(
        !view.offers_retry(delegated),
        "the exchange replaces retrying the ask"
    );
    assert!(view.navigation().rows[0].compact());
    assert!(view.focus_managed_navigation(false));
    assert_eq!(
        view.managed_navigation_key(&Key::Named(NamedKey::Enter), ModifiersState::empty()),
        Some(true)
    );
    assert_eq!(
        view.take_open_bot_conversation(),
        Some((bot, "Scout".into(), chat))
    );
    assert!(commands.try_recv().is_err());
    view.managed.as_mut().unwrap().snapshot.origin = Some(wire::Origin {
        thread: ChatId::new(),
        bot: bot.as_uuid(),
        name: "Scout".into(),
        title: "Main".into(),
        main: true,
    });
    assert!(view.observing_bot_thread());
    assert!(!view.composer_key(
        &Key::Character("x".into()),
        Some("x"),
        ModifiersState::empty()
    ));
}

#[test]
fn native_exchange_stays_with_its_sender_when_the_next_message_has_no_id() {
    let (mut view, _, _) = surface();
    view.managed.as_mut().unwrap().snapshot.turns.clear();
    let sender = MessageId::new();
    let mut ask = view.user_card("I will ask Scout.", Vec::new(), 420.0);
    ask.id = Some(sender);
    ask.role = ChatRole::Assistant;
    let pending = view.user_card("Another question", Vec::new(), 420.0);
    assert!(pending.id.is_none(), "an optimistic send has no durable ID");
    view.message_layouts = vec![ask, pending];
    let mut card = side();
    card.dispatches.push(wire::Dispatch {
        group: Uuid::new_v4(),
        origin_reply: Some(sender.as_uuid()),
        message: Uuid::new_v4(),
        recipient: card.bot.unwrap(),
        name: "Scout".into(),
        state: "owed".into(),
        reply: None,
        preview: None,
    });
    view.managed.as_mut().unwrap().navigation = State::new(Page {
        items: vec![card.clone()],
        ..Default::default()
    });
    view.rebuild_managed_navigation(420.0);
    assert_eq!(view.navigation().rows[0].body, "Messaged");
    card.dispatches[0].state = "answered".into();
    card.dispatches[0].reply = Some(Uuid::new_v4());
    view.managed.as_mut().unwrap().navigation = State::new(Page {
        items: vec![card],
        ..Default::default()
    });
    view.rebuild_managed_navigation(420.0);
    view.position_conversation_items();
    let receipt = &view.navigation().rows[0];
    assert!(receipt.body.contains("1 of 1 replied"));
    assert!(receipt.top > view.message_layouts[0].top);
    assert!(receipt.top + receipt.height <= view.message_layouts[1].top);

    // A refreshed transcript may not have loaded the sender yet. Its old
    // receipt must not appear to describe the new turn at the tail.
    view.message_layouts.remove(0);
    view.rebuild_managed_navigation(420.0);
    assert!(view.navigation().rows.is_empty());
}

fn side() -> wire::Card {
    wire::Card {
        id: Uuid::new_v4(),
        sequence: 2,
        message: Uuid::new_v4(),
        kind: "thread".into(),
        title: "Synthetic side".into(),
        thread: Some(ChatId::new()),
        bot: Some(koru_ai_core::bots::BotId::mint()),
        bot_name: "Collaborator".into(),
        workspace: None,
        document: None,
        messages: 3,
        outputs: 1,
        state: "active".into(),
        dispatches: Vec::new(),
        routine: None,
    }
}

#[test]
fn exchanges_group_by_causal_turn_and_count_only_recorded_replies() {
    let (mut view, commands, _) = surface();
    let sender = MessageId::new();
    let mut source = view.user_card("Asking both bots", Vec::new(), 420.0);
    source.id = Some(sender);
    view.message_layouts = vec![source];
    let group = Uuid::new_v4();
    let mut first = side();
    first.dispatches.push(wire::Dispatch {
        group,
        origin_reply: Some(sender.as_uuid()),
        message: Uuid::new_v4(),
        recipient: koru_ai_core::bots::BotId::mint(),
        name: "CFO".into(),
        state: "answered".into(),
        reply: Some(Uuid::new_v4()),
        preview: None,
    });
    let mut second = side();
    second.messages = 99; // Unrelated history must not count as replies.
    second.dispatches.push(wire::Dispatch {
        group,
        origin_reply: Some(sender.as_uuid()),
        message: Uuid::new_v4(),
        recipient: koru_ai_core::bots::BotId::mint(),
        name: "Scout".into(),
        state: "owed".into(),
        reply: None,
        preview: None,
    });
    view.managed.as_mut().unwrap().navigation = State::new(Page {
        items: vec![first.clone(), second.clone()],
        ..Default::default()
    });
    view.rebuild_managed_navigation(420.0);
    let index = view
        .navigation()
        .rows
        .iter()
        .position(|r| r.id == group)
        .unwrap();
    let row = &view.navigation().rows[index];
    assert_eq!(row.targets.len(), 2);
    assert!(row.body.contains("1 of 2 replied"));
    assert!(row.height > 0.0);
    view.navigation_mut().cursor = index;
    view.navigation_mut().target_cursor = 1;
    assert!(view.activate_managed_navigation(index));
    assert_eq!(
        view.take_open_bot_conversation(),
        Some((
            second.bot.unwrap(),
            second.bot_name.clone(),
            second.thread.unwrap()
        ))
    );
    assert!(commands.try_recv().is_err(), "observing sends no command");

    first.dispatches[0].group = Uuid::new_v4();
    let rows = exchange::rows(&[first, second], &view.message_layouts, koru_i18n::Lang::En);
    assert_eq!(rows.len(), 2, "different turns never merge");
}

#[test]
fn returned_words_are_grouped_bounded_markdown_below_the_origin_work() {
    use koru_ai_core::bots::BotReplyPreview;
    let (mut view, _, _) = surface();
    let sender = MessageId::new();
    let answer = MessageId::new();
    let mut ask = view.user_card("", Vec::new(), 420.0);
    ask.id = Some(sender);
    ask.role = ChatRole::Assistant;
    ask.state = MessageState::Complete;
    ask.markdown = "I am asking Scout now.".into(); // folded into work
    view.message_layouts = vec![ask];
    view.work_layouts = vec![work_view::WorkLayout {
        message: sender,
        top: 0.0,
        live: false,
        bot: false,
        started_ms: 0,
        header: std::rc::Rc::new(view.engine.simple_layout("Worked for 1m", role::ui())),
        header_text: "Worked for 1m".into(),
        open: false,
        lines: vec![],
        height: work_view::HEADER_H,
    }];
    let mut card = side();
    card.dispatches.push(wire::Dispatch {
        group: Uuid::new_v4(),
        origin_reply: Some(sender.as_uuid()),
        message: Uuid::new_v4(),
        recipient: card.bot.unwrap(),
        name: "Scout".into(),
        state: "answered".into(),
        reply: Some(answer.as_uuid()),
        preview: BotReplyPreview::new(
            answer,
            "**Actual answer**\n\nRead [source](https://example.com).",
        ),
    });
    view.managed.as_mut().unwrap().navigation = State::new(Page {
        items: vec![card.clone()],
        ..Default::default()
    });
    for width in [180.0, 420.0] {
        view.rebuild_managed_navigation(width);
        view.position_conversation_items();
        assert_eq!(message_height(&view.message_layouts[0]), 0.0);
        let row = &view.managed.as_ref().unwrap().navigation.rows[0];
        assert!(
            row.top - work_view::HEADER_H <= 8.0,
            "no phantom answer/action slot"
        );
        assert_eq!(row.replies.len(), 1);
        assert_eq!(row.replies[0].preview.message, answer);
        assert_eq!(row.replies[0].blocks[0].text, "Actual answer");
        assert_eq!(row.replies[0].blocks[1].links[0].url, "https://example.com");
        let tree = row.compact_tree(&mut view.engine, &view.bot_faces, width);
        let frame = questions::elements::layout(
            &tree,
            Rect::new(0.0, row.top, width, row.height),
            koru_draw::el::FrameCx::root(),
        );
        let reply = frame
            .nodes
            .iter()
            .position(|node| node.name == "Bot reply")
            .unwrap();
        assert!(
            frame
                .nodes
                .iter()
                .filter(|node| node.name == "Reply paragraph")
                .all(|node| node.parent == Some(reply)
                    && node.rect.x >= 0.0
                    && node.rect.x + node.rect.w <= width + 0.01)
        );
        assert!(
            frame.rect_of("Bot reply").unwrap().y > frame.rect_of("Centered targets").unwrap().y
        );
        let parent = view.conversation_turn_rect(0, Rect::new(0.0, 0.0, width, 800.0));
        assert_eq!(parent.y, 0.0);
        assert!(parent.h >= row.top + row.height);
    }
    // Exercise the real painter: work and exchange have the same measured
    // owner, and reply paragraphs remain nested under their reply column.
    view.viewport = Rect::new(0.0, 0.0, 500.0, 800.0);
    let area = view.conversation_rect();
    view.rebuild_managed_navigation(area.w);
    view.position_conversation_items();
    view.message_width = area.w;
    view.messages_dirty = false;
    let inspection =
        koru_draw::inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, 500.0, 800.0));
    view.draw_messages(&mut Scene::new(), Affine::IDENTITY);
    let snapshot = inspection.finish();
    let turn = snapshot
        .records
        .iter()
        .position(|node| node.name == "Conversation turn")
        .unwrap();
    for name in ["Turn work", "Bot message thread"] {
        let record = snapshot
            .records
            .iter()
            .find(|node| node.name == name)
            .unwrap();
        assert_eq!(record.parent, Some(turn));
        assert!(record.rect().y0 >= snapshot.records[turn].rect().y0);
        assert!(record.rect().y1 <= snapshot.records[turn].rect().y1 + 0.01);
    }
    // Legitimate usage survives, without empty body padding or action reserve.
    view.message_layouts[0].usage = Some(view.engine.simple_layout("1k tokens", role::hint()));
    assert_eq!(message_content_height(&view.message_layouts[0]), 0.0);
    assert_eq!(view.message_layouts[0].padding(), 0.0);
    assert_eq!(
        message_height(&view.message_layouts[0]),
        role::hint().metrics.line_height + USAGE_GAP
    );
    // Before completion, a payload cannot masquerade as a returned reply.
    card.dispatches[0].state = "owed".into();
    card.dispatches[0].reply = None;
    view.managed.as_mut().unwrap().navigation = State::new(Page {
        items: vec![card],
        ..Default::default()
    });
    view.rebuild_managed_navigation(420.0);
    assert!(view.navigation().rows[0].replies.is_empty());
    assert_eq!(view.navigation().rows[0].body, "Messaged");
}

#[test]
fn native_preview_must_match_the_recorded_reply_and_is_not_invented() {
    let (mut view, _, _) = surface();
    let sender = MessageId::new();
    let mut source = view.user_card("Asking Scout", Vec::new(), 420.0);
    source.id = Some(sender);
    view.message_layouts = vec![source];
    let mut card = side();
    let answer = Uuid::new_v4();
    card.dispatches.push(wire::Dispatch {
        group: Uuid::new_v4(),
        origin_reply: Some(sender.as_uuid()),
        message: Uuid::new_v4(),
        recipient: koru_ai_core::bots::BotId::mint(),
        name: "Scout".into(),
        state: "answered".into(),
        reply: Some(answer),
        preview: koru_ai_core::bots::BotReplyPreview::new(MessageId::new(), "Wrong answer"),
    });
    let rows = exchange::rows(&[card.clone()], &view.message_layouts, view.lang);
    assert!(rows[0].replies.is_empty());
    card.dispatches[0].preview.as_mut().unwrap().message = MessageId::from_uuid(answer);
    view.managed.as_mut().unwrap().navigation = State::new(Page {
        items: vec![card],
        ..Default::default()
    });
    view.rebuild_managed_navigation(420.0);
    let row = view
        .navigation()
        .rows
        .iter()
        .find(|row| !row.replies.is_empty())
        .unwrap();
    assert_eq!(row.replies.len(), 1);
    assert_eq!(row.replies[0].preview.message.as_uuid(), answer);
    let next = view.user_card("A later optimistic send", Vec::new(), 420.0);
    assert!(next.id.is_none());
    view.message_layouts.push(next);
    view.position_conversation_items();
    let row = view
        .navigation()
        .rows
        .iter()
        .find(|row| !row.replies.is_empty())
        .unwrap();
    assert_eq!(row.after, Some(sender));
    assert!(row.top + row.height <= view.message_layouts[1].top);
    view.message_layouts.remove(0);
    view.rebuild_managed_navigation(420.0);
    assert!(
        !view
            .navigation()
            .rows
            .iter()
            .any(|row| !row.replies.is_empty()),
        "an unloaded native origin never moves its reply below a different turn"
    );
}

#[test]
fn host_navigation_opens_the_target_starter_without_joining_or_losing_draft() {
    let (mut surface, commands, _) = surface();
    let card = side();
    surface.composer.set_text("unfinished draft");
    surface.managed.as_mut().unwrap().navigation = State::new(Page {
        items: vec![card.clone()],
        ..Default::default()
    });
    surface.rebuild_managed_navigation(420.0);
    let index = surface
        .managed
        .as_ref()
        .unwrap()
        .navigation
        .rows
        .iter()
        .position(|r| r.id == card.id)
        .unwrap();
    assert!(surface.activate_managed_navigation(index));
    assert_eq!(
        surface.take_open_bot_conversation(),
        Some((card.bot.unwrap(), card.bot_name, card.thread.unwrap()))
    );
    assert!(surface.take_open_bot_conversation().is_none());
    assert_eq!(surface.composer.text(), "unfinished draft");
    assert!(commands.try_recv().is_err(), "opening never sends or joins");
}

#[test]
fn join_is_explicit_and_running_turns_add_no_stop_cards() {
    let (mut surface, commands, _) = surface();
    let state = surface.managed.as_mut().unwrap();
    state.snapshot.joined = false;
    let mut second = state.snapshot.turns[0].clone();
    second.id = Uuid::new_v4();
    second.bot = Uuid::new_v4();
    second.session = Some(Uuid::new_v4());
    second.worker = Some(Uuid::new_v4());
    second.reply_message = Some(Uuid::new_v4());
    state.snapshot.turns.push(second.clone());
    surface.rebuild_managed_navigation(400.0);
    let join = surface
        .managed
        .as_ref()
        .unwrap()
        .navigation
        .rows
        .iter()
        .position(|r| matches!(r.action, Some(Action::Join)))
        .unwrap();
    surface.activate_managed_navigation(join);
    let Command::Engine {
        thread,
        command: wire::Command::Join { id, .. },
    } = commands.try_recv().unwrap()
    else {
        panic!("Join command");
    };
    assert!(
        !surface.managed.as_ref().unwrap().snapshot.joined,
        "wait for read-back"
    );
    surface.managed_reply(
        thread,
        wire::Response::Changed {
            request: id,
            replayed: false,
        },
    );
    assert_eq!(surface.navigation().rows.len(), 1, "only Join is shown");
    assert!(surface.activate_send_button());
    let Command::Engine {
        command: wire::Command::Stop { turn, .. },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!("Stop command");
    };
    let primary = surface.managed.as_ref().unwrap().primary_turn().unwrap();
    assert_eq!(turn.id, primary.id);
    assert_eq!(turn.worker, primary.worker);
    assert_eq!(turn.session, primary.session);
    assert_ne!(turn.id, second.id, "the composer stops its own Bot");
}

#[test]
fn outputs_open_native_document_and_partial_pages_keep_the_complete_view() {
    let (mut surface, commands, _) = surface();
    let mut card = side();
    let document = DocumentId::new();
    card.kind = "output".into();
    card.thread = None;
    card.bot = None;
    card.document = Some(document);
    let state = &mut surface.managed.as_mut().unwrap().navigation;
    state.ingest(Page {
        items: vec![card.clone()],
        ..Default::default()
    });
    assert!(!state.ingest(Page {
        items: vec![],
        next: Some(Uuid::new_v4()),
        after: None
    }));
    assert_eq!(state.items.len(), 1);
    surface.rebuild_managed_navigation(420.0);
    let index = surface
        .managed
        .as_ref()
        .unwrap()
        .navigation
        .rows
        .iter()
        .position(|r| r.id == card.id)
        .unwrap();
    surface.activate_managed_navigation(index);
    assert!(
        matches!(surface.take_open_document_request(),Some(koru_link::KoruLink::Document{document:id,..}) if id==document.as_uuid())
    );
    assert!(commands.try_recv().is_err());
}

#[test]
fn disappearing_join_returns_focus_to_composer_and_reposition_is_stable() {
    let (mut surface, _, _) = surface();
    surface.managed.as_mut().unwrap().snapshot.joined = false;
    surface.rebuild_managed_navigation(360.0);
    let end = surface.position_conversation_items();
    let again = surface.position_conversation_items();
    assert_eq!(end, again);
    assert!(surface.focus_managed_navigation(true));
    surface.managed.as_mut().unwrap().snapshot.joined = true;
    surface.rebuild_managed_navigation(360.0);
    assert!(surface.focus == Focus::Composer);
}

#[test]
fn bot_branch_is_refused_before_either_carrier_or_navigation_is_created() {
    let (mut surface, commands, _) = surface();
    let thread = surface.managed.as_ref().unwrap().thread();
    let reply = MessageId::new();
    let request = BranchChatRequest {
        branch_chat_id: ChatId::new(),
        through_message_id: reply,
        title: "branch".into(),
    };
    assert!(!surface.perform_response_action(ResponseAction::Branch(reply)));
    assert!(!surface.queue_command(Command::Branch(thread, request.clone())));
    assert!(
        surface
            .managed_command(Command::Branch(thread, request))
            .is_none()
    );
    assert!(!surface.queue_command(Command::Engine {
        thread,
        command: wire::Command::Branch {
            id: uuid::Uuid::new_v4(),
            thread,
            branch: ChatId::new(),
            through: reply.as_uuid(),
            title: "branch".into(),
        },
    }));
    assert!(commands.try_recv().is_err());
    assert!(
        surface
            .managed
            .as_ref()
            .unwrap()
            .navigation
            .pending
            .is_none()
    );
}

#[test]
fn unsent_branch_does_not_leave_navigation_disabled() {
    let (mut surface, _, _) = surface();
    let thread = surface.managed.as_ref().unwrap().thread();
    surface.reader = None;
    assert!(!surface.queue_command(Command::Branch(
        thread,
        BranchChatRequest {
            branch_chat_id: ChatId::new(),
            through_message_id: MessageId::new(),
            title: "Synthetic".into()
        }
    )));
    assert!(
        surface
            .managed
            .as_ref()
            .unwrap()
            .navigation
            .pending
            .is_none()
    );
}

#[test]
fn eight_languages_have_nonempty_navigation_and_limit_words() {
    assert_eq!(
        t(koru_i18n::Lang::En, Words::BotsEngineJoin),
        "Join conversation"
    );
    assert_eq!(
        t(koru_i18n::Lang::En, Words::BotsEnginePageNumber),
        "Page {}"
    );
    assert_eq!(
        t(koru_i18n::Lang::En, Words::BotsEngineBranchTitle),
        "{} — branch"
    );
    for lang in koru_i18n::Lang::ALL {
        for word in [
            Words::BotsEngineReadAlong,
            Words::BotsEngineJoin,
            Words::BotsEngineOrigin,
            Words::BotsEngineThreadSummary,
            Words::BotsEngineChainLimit,
        ] {
            assert!(!t(lang, word).trim().is_empty());
        }
        let (mut surface, _, _) = surface();
        surface.lang = lang;
        surface.managed.as_mut().unwrap().snapshot.joined = false;
        surface.managed.as_mut().unwrap().navigation = State::new(Page {
            items: vec![side()],
            ..Default::default()
        });
        surface.rebuild_managed_navigation(320.0);
        assert!(
            surface
                .managed
                .as_ref()
                .unwrap()
                .navigation
                .rows
                .iter()
                .all(|r| r.height.is_finite() && r.height > 0.0)
        );
    }
}

#[test]
fn origin_navigation_uses_the_origin_starter_not_the_asking_participant() {
    let (mut surface, _, _) = surface();
    let origin = ChatId::new();
    let bot = Uuid::new_v4();
    let state = surface.managed.as_mut().unwrap();
    state.snapshot.origin = Some(wire::Origin {
        thread: origin,
        bot,
        name: "Origin owner".into(),
        title: "Origin conversation".into(),
        main: false,
    });
    surface.rebuild_managed_navigation(400.0);
    let index = surface
        .managed
        .as_ref()
        .unwrap()
        .navigation
        .rows
        .iter()
        .position(|r| r.id == origin.as_uuid())
        .unwrap();
    assert!(
        surface.managed.as_ref().unwrap().navigation.rows[index]
            .body
            .contains("Origin conversation")
    );
    surface.activate_managed_navigation(index);
    let request = surface.take_open_bot_conversation().unwrap();
    assert_eq!(request.0.as_uuid(), bot);
    assert_eq!(request.2, origin);
}
