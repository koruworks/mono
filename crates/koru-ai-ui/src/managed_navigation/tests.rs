// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::managed_surface::tests::surface;

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
    }
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
fn join_is_explicit_and_every_bot_stop_uses_its_own_turn() {
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
    let stop = surface
        .managed
        .as_ref()
        .unwrap()
        .navigation
        .rows
        .iter()
        .position(|r| r.id == second.id)
        .unwrap();
    surface.activate_managed_navigation(stop);
    let Command::Engine {
        command: wire::Command::Stop { turn, .. },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!("Stop command");
    };
    assert_eq!(turn.id, second.id);
    assert_eq!(Some(turn.worker), second.worker);
    assert_eq!(Some(turn.session), second.session);
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
fn disappearing_stop_returns_focus_to_composer_and_reposition_is_stable() {
    let (mut surface, _, _) = surface();
    surface.rebuild_managed_navigation(360.0);
    let end = surface.position_conversation_items();
    let again = surface.position_conversation_items();
    assert_eq!(end, again);
    assert!(surface.focus_managed_navigation(true));
    surface.managed.as_mut().unwrap().snapshot.turns.clear();
    surface.rebuild_managed_navigation(360.0);
    assert!(surface.focus == Focus::Composer);
}

#[test]
fn branch_uses_real_response_identity_and_uncertain_retry_keeps_its_command() {
    let (mut surface, commands, _) = surface();
    let thread = surface.managed.as_ref().unwrap().thread();
    let through = MessageId::new();
    let branch = ChatId::new();
    assert!(surface.queue_command(Command::Branch(
        thread,
        BranchChatRequest {
            branch_chat_id: branch,
            through_message_id: through,
            title: "branch".into(),
        },
    )));
    let Command::Engine { command, .. } = commands.try_recv().unwrap() else {
        panic!("engine branch");
    };
    let wire::Command::Branch {
        id,
        through: actual,
        ..
    } = &command
    else {
        panic!()
    };
    assert_eq!(*actual, through.as_uuid());
    let id = *id;
    surface.managed_failed(
        thread,
        command.clone(),
        koru_ai_wire::Failure {
            code: "engine_unavailable".into(),
            message: "synthetic".into(),
            retryable: true,
            details: Default::default(),
        },
    );
    surface.maintain_managed(Instant::now() + Duration::from_secs(3));
    let retried = commands.try_iter().find_map(|c| match c {
        Command::Engine {
            command: wire::Command::Branch { id, branch, .. },
            ..
        } => Some((id, branch)),
        _ => None,
    });
    assert_eq!(retried, Some((id, branch)));
    surface.managed_reply(
        thread,
        wire::Response::Branched {
            request: id,
            thread: branch,
            replayed: true,
        },
    );
    assert_eq!(surface.take_open_bot_conversation().unwrap().2, branch);
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
