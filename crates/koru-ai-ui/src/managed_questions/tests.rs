// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn waiting_notice_names_the_occupants_and_the_global_setting_without_an_attempt() {
    let mut opened = crate::managed::tests::opened();
    let mut notice = crate::managed::tests::message(1, Uuid::new_v4());
    notice.kind = "setup_note".into();
    notice.content.body.clear();
    notice.content.references = vec![
        serde_json::json!({"notice":"waiting_for_capacity","running_bots":[{"name":"First bot"},{"name":"Second bot"}],"limit":2}),
    ];
    let words = notice_words(koru_i18n::Lang::En, &notice.content.references[0]).unwrap();
    assert!(words.contains("First bot, Second bot"));
    assert!(words.contains("Settings > Bots > Bots that can work at once"));
    opened.messages.push(notice);
    let mut surface = ChatSurface::new_owned_at(None);
    surface.managed_opened(opened);
    surface.rebuild_managed_notices(400.0);
    assert_eq!(surface.notice_layouts.len(), 1);
    assert!(surface.attempt.is_none());
    assert!(surface.transcript.as_ref().unwrap().messages.is_empty());
}
use crate::reader::Reader;
use std::sync::mpsc::{Receiver, SyncSender};

fn question(secret: bool) -> wire::Prompt {
    serde_json::from_value(serde_json::json!({"kind":"questions", "questions":[{
        "id":"first", "header":"Choice", "question":"What should happen?", "secret":secret,
        "options":[], "allow_other":true
    }], "blocking":true, "delivery":"in_turn"}))
    .unwrap()
}

fn surface(
    prompt: wire::Prompt,
    available: bool,
) -> (ChatSurface, Receiver<Command>, SyncSender<Update>, Uuid) {
    let mut opened = managed::tests::opened();
    let id = Uuid::new_v4();
    opened.snapshot.turns[0].state = "needs_you".into();
    opened.questions = vec![wire::Question {
        id,
        turn: opened.snapshot.turns[0].id,
        prompt,
        answer: None,
        state: "open".into(),
        delivery_state: "none".into(),
        provider_available: available,
    }];
    let mut surface = ChatSurface::new_owned_at(None);
    surface.bind_owned(OwnedConversation {
        chat: opened.snapshot.conversation.chat.id,
        destination: WorkspaceId::new(),
        provider: opened
            .snapshot
            .conversation
            .chat
            .current_model
            .provider
            .clone(),
        offers: Default::default(),
        refusals: &[],
    });
    let (reader, commands, updates) = Reader::test_pair();
    surface.reader = Some(reader);
    surface.managed_opened(opened);
    surface.rebuild_messages(600.0);
    while commands.try_recv().is_ok() {}
    (surface, commands, updates, id)
}

#[test]
fn secret_form_sends_exact_answer_then_forgets_it_on_acceptance() {
    let (mut surface, commands, _updates, id) = surface(question(true), true);
    assert_eq!(surface.focus, Focus::ManagedQuestion(id));
    assert_eq!(
        surface.send_mode(),
        queue::SendMode::Stop,
        "waiting for a question remains stoppable"
    );
    surface.managed_question_key(
        id,
        &Key::Character("s".into()),
        Some("synthetic secret"),
        ModifiersState::empty(),
    );
    surface.managed_question_key(
        id,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    );
    let Command::Engine { thread, command } = commands.try_recv().unwrap() else {
        panic!("expected answer")
    };
    assert!(!format!("{command:?}").contains("synthetic secret"));
    let wire::Command::Answer {
        id: request,
        question,
        answer: wire::Answer::Questions {
            answers: Some(answers),
        },
    } = command
    else {
        panic!("expected question answer")
    };
    assert_eq!(question, id);
    assert!(
        matches!(&answers[0].value, wire::QuestionAnswerValue::Text { text } if text == "synthetic secret")
    );
    surface.managed_reply(
        thread,
        wire::Response::Answered {
            request,
            question: id,
            continued: false,
            replayed: false,
        },
    );
    let card = &surface.managed_cards[&id];
    assert!(card.form.is_none() && card.pending.is_none());
    assert!(!card.body(surface.lang).contains("synthetic secret"));
    assert!(
        card.body(surface.lang)
            .contains(t(surface.lang, Words::AiQuestionsWithheld))
    );
    // A pre-acknowledgement snapshot still says open. It cannot reopen the
    // accepted form or restore the secret input.
    surface.rebuild_messages(600.0);
    assert!(!surface.managed_cards[&id].open);
    assert!(surface.managed_cards[&id].form.is_none());
}

#[test]
fn secret_provider_loss_offers_reask_but_plain_answers_can_continue() {
    let (mut surface, commands, _updates, id) = surface(question(true), false);
    assert!(surface.managed_cards[&id].form.is_none());
    surface.activate_managed_question(id, questions::Control::Option(0, 0));
    assert!(
        matches!(commands.try_recv().unwrap(), Command::Engine { command: wire::Command::Reask { question, .. }, .. } if question == id)
    );
    let (plain, _, _updates, plain_id) = self::surface(question(false), false);
    assert!(plain.managed_cards[&plain_id].form.is_some());
}

#[test]
fn approval_uses_the_exact_offered_choice_and_has_no_ordinary_attempt() {
    let choice = Uuid::new_v4();
    let prompt: wire::Prompt = serde_json::from_value(
        serde_json::json!({"kind":"approval", "summary":"command_execution",
        "choices":[{"id":choice,"intent":"reject_remembered","label":""}]}),
    )
    .unwrap();
    let (mut surface, commands, _updates, id) = surface(prompt, true);
    surface.activate_managed_question(id, questions::Control::Option(0, 0));
    let Command::Engine {
        command:
            wire::Command::Answer {
                answer:
                    wire::Answer::Approval {
                        decision,
                        choice: Some(sent),
                    },
                ..
            },
        ..
    } = commands.try_recv().unwrap()
    else {
        panic!("expected approval choice")
    };
    assert_eq!(sent.as_uuid(), choice);
    assert_eq!(decision, wire::ApprovalDecision::Reject);
    assert!(surface.attempt.is_none());
}

#[test]
fn uncertain_answer_retries_same_receipt_and_tab_reaches_composer() {
    let (mut surface, commands, _updates, id) = surface(question(false), true);
    let card = surface.managed_cards.get_mut(&id).unwrap();
    card.form.as_mut().unwrap().1.cursor = questions::Control::Submit;
    assert_eq!(
        surface.managed_question_key(
            id,
            &Key::Named(NamedKey::Tab),
            None,
            ModifiersState::empty()
        ),
        Some(true)
    );
    assert_eq!(surface.focus, Focus::Composer);
    assert!(surface.step_managed_question(None, false));
    assert_eq!(surface.focus, Focus::ManagedQuestion(id));
    surface.activate_managed_question(id, questions::Control::Skip);
    let Command::Engine { thread, command } = commands.try_recv().unwrap() else {
        panic!("expected skipped form")
    };
    let original = serde_json::to_value(&command).unwrap();
    surface.managed_failed(
        thread,
        command,
        koru_ai_wire::Failure {
            code: "gateway_unavailable".into(),
            message: "offline".into(),
            retryable: true,
            details: Default::default(),
        },
    );
    surface.maintain_managed_requests(Instant::now() + Duration::from_secs(3));
    let Command::Engine { command, .. } = commands.try_recv().unwrap() else {
        panic!("expected retry")
    };
    assert_eq!(serde_json::to_value(command).unwrap(), original);
}

#[test]
fn saved_questions_use_real_history_without_empty_user_answer_cards() {
    let (mut surface, _, _updates, id) = surface(question(true), true);
    let state = surface.managed.as_mut().unwrap();
    let prompt = state.questions[0].prompt.clone();
    state.questions.clear();
    let mut asked = managed::tests::message(1, Uuid::new_v4());
    asked.kind = "setup_note".into();
    asked.content.body.clear();
    asked.content.references =
        vec![serde_json::json!({"notice":"question","question":id,"prompt":prompt})];
    state.ingest(asked);
    let mut answered = managed::tests::message(2, Uuid::new_v4());
    answered.sender = "person:fixture".into();
    answered.content.body.clear();
    answered.content.references = vec![
        serde_json::json!({"question":id,"answer":{"kind":"questions","answers":[{"question_id":"first","value":{"kind":"withheld"}}]}}),
    ];
    state.ingest(answered);
    assert!(state.transcript().messages.is_empty());
    state.requests_dirty = true;
    surface.rebuild_messages(600.0);
    let card = &surface.managed_cards[&id];
    assert!(!card.open && card.form.is_none());
    assert_eq!(
        card.heading(surface.lang),
        t(surface.lang, Words::AiQuestionsAnswered)
    );
    assert!(
        card.body(surface.lang)
            .contains(t(surface.lang, Words::AiQuestionsWithheld))
    );
}
