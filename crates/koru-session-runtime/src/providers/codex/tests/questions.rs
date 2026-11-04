// SPDX-License-Identifier: MPL-2.0

//! **Codex's questions** (koru-ai D108): `item/tool/requestUserInput` in the
//! shape 0.166.0's generated schema gives it, to a question set, and the
//! person's answers back as its `ToolRequestUserInputResponse`.

use koru_session_protocol::{
    AgentQuestion, ManagedState, QuestionAnswer, QuestionAnswerValue, QuestionRequestId,
    QuestionsOutcome,
};

use super::*;

const REQUEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/codex-app-server/0.166.0/request-user-input.server.json"
));
const RESPONSE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/codex-app-server/0.166.0/request-user-input.client.json"
));

fn request() -> Value {
    serde_json::from_str(REQUEST).unwrap()
}

/// A control in a running turn whose transcript holds `frames`; the session
/// draws questions when `questions` says so.
fn asked(
    frames: Vec<Value>,
    questions: bool,
) -> (CodexControl, Arc<Mutex<ReplayState>>, AgentTurnId) {
    let turn_id = AgentTurnId::mint();
    let native_turn = "native-tool-turn";
    let (mut control, state) = replay_control(frames, Arc::new(NoopAttachmentPathResolver));
    control.config.questions = questions;
    control.thread_id = Some("fixture-thread".into());
    control.turns.insert(
        turn_id,
        NativeTurn {
            native_id: Some(native_turn.into()),
            fingerprint: blake3::hash(b"fixture"),
            terminal: false,
        },
    );
    control.native_turns.insert(native_turn.into(), turn_id);
    (control, state, turn_id)
}

fn drain(control: &mut CodexControl) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = control.next_event(Duration::ZERO).unwrap() {
        events.push(event);
    }
    events
}

fn required(events: &[AgentEvent]) -> (QuestionRequestId, Vec<AgentQuestion>, bool) {
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::QuestionsRequired {
                request_id,
                questions,
                blocking,
                ..
            } => Some((*request_id, questions.clone(), *blocking)),
            _ => None,
        })
        .expect("a question set")
}

fn answer(id: &str, value: QuestionAnswerValue) -> QuestionAnswer {
    QuestionAnswer {
        question_id: id.into(),
        value,
    }
}

/// **The fixture becomes one set, in Codex's order, and the answers go back
/// in Codex's exact shape**: the options in the agent's order (the
/// recommended one first), *Other* and the secret carried, the turn waiting
/// on the person until the answer goes, and the secret withheld from the
/// event that closes the set.
#[test]
fn a_request_user_input_becomes_a_set_and_its_answers_codexs_response() {
    let (mut control, state, turn_id) = asked(vec![request()], true);
    let events = drain(&mut control);
    let (id, questions, blocking) = required(&events);
    assert!(blocking);
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ProviderState {
            state: ManagedState::WaitingForInput,
            ..
        }
    )));
    let ids: Vec<&str> = questions
        .iter()
        .map(|question| question.id.as_str())
        .collect();
    assert_eq!(ids, ["framework", "port", "token"]);
    let labels: Vec<&str> = questions[0]
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(labels, ["Axum (Recommended)", "Actix Web", "Rocket"]);
    assert_eq!(
        questions[0].options[0].description,
        "Tokio-native, and already a dependency."
    );
    assert_eq!(questions[0].header, "Framework");
    assert!(questions[0].allow_other && !questions[0].secret);
    assert!(questions[1].options.is_empty() && questions[1].takes_text());
    assert!(questions[2].secret);
    assert!(
        state.lock().unwrap().sent.is_empty(),
        "nothing answered yet"
    );

    control
        .answer_questions(
            id,
            Some(vec![
                answer(
                    "framework",
                    QuestionAnswerValue::Option {
                        label: "Axum (Recommended)".into(),
                    },
                ),
                answer(
                    "port",
                    QuestionAnswerValue::Text {
                        text: " 8080 ".into(),
                    },
                ),
                answer(
                    "token",
                    QuestionAnswerValue::Text {
                        text: "sk-fixture-SECRET".into(),
                    },
                ),
            ]),
        )
        .unwrap();
    let expected: Value = serde_json::from_str(RESPONSE).unwrap();
    assert_eq!(state.lock().unwrap().sent, vec![expected]);
    let events = drain(&mut control);
    let resolved = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::QuestionsResolved {
                turn_id: resolved_turn,
                request_id,
                outcome,
            } if *request_id == id => Some((*resolved_turn, outcome.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(resolved.0, turn_id);
    let QuestionsOutcome::Answered { answers } = resolved.1 else {
        panic!("answered");
    };
    assert_eq!(answers[2].value, QuestionAnswerValue::Withheld);
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("sk-fixture"),
        "the secret is never repeated"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ProviderState {
            state: ManagedState::Running,
            ..
        }
    )));
    // Answered once: a second answer is stale.
    assert_eq!(
        control.answer_questions(id, None).unwrap_err().code,
        ErrorCode::ApprovalStale
    );
}

/// **A skip is the response with no answers** — what the app-server itself
/// gives the model when its client cannot answer — and a bad answer is
/// refused before anything is sent.
#[test]
fn a_skip_is_no_answers_and_a_bad_answer_is_refused_unsent() {
    let (mut control, state, _) = asked(vec![request()], true);
    let (id, _, _) = required(&drain(&mut control));
    let refused = control
        .answer_questions(
            id,
            Some(vec![answer(
                "framework",
                QuestionAnswerValue::Option {
                    label: "Flask".into(),
                },
            )]),
        )
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidRequest);
    assert!(state.lock().unwrap().sent.is_empty());

    control.answer_questions(id, None).unwrap();
    assert_eq!(
        state.lock().unwrap().sent,
        vec![json!({ "id": 41, "result": { "answers": {} } })]
    );
    assert!(drain(&mut control).iter().any(|event| matches!(
        event,
        AgentEvent::QuestionsResolved {
            outcome: QuestionsOutcome::Skipped,
            ..
        }
    )));
}

/// **A session its creator cannot draw questions for answers as Koru always
/// has**: `-32601 unsupported request`, and no set is asked — the v20 peer's
/// behaviour, with no hang.
#[test]
fn without_questions_the_request_is_unsupported_as_before() {
    let (mut control, state, _) = asked(vec![request()], false);
    let events = drain(&mut control);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::QuestionsRequired { .. }))
    );
    assert_eq!(
        state.lock().unwrap().sent,
        vec![json!({
            "id": 41,
            "error": { "code": -32601, "message": "unsupported request" }
        })]
    );
    assert_eq!(control.diagnostics().unknown_events, 1);
}

/// **A set Codex does not wait on leaves the turn running**, and expires
/// when Codex resolves it itself; a waiting set expires with its turn.
#[test]
fn a_set_codex_does_not_wait_on_runs_on_and_expires_with_its_resolution() {
    let mut open = request();
    open["params"]["isBlocking"] = json!(false);
    let resolved = json!({
        "method": "serverRequest/resolved",
        "params": { "threadId": "fixture-thread", "requestId": 41 }
    });
    let (mut control, state, _) = asked(vec![open, resolved], true);
    let events = drain(&mut control);
    let (id, _, blocking) = required(&events);
    assert!(!blocking);
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::ProviderState {
                state: ManagedState::WaitingForInput,
                ..
            }
        )),
        "the turn was never held"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::QuestionsResolved { request_id, outcome: QuestionsOutcome::Expired, .. }
            if *request_id == id
    )));
    assert!(state.lock().unwrap().sent.is_empty(), "Codex closed it");

    let (mut control, _, turn_id) = asked(
        vec![request(), turn_completed("native-tool-turn", "completed")],
        true,
    );
    let events = drain(&mut control);
    let (id, _, _) = required(&events);
    let expired = events
        .iter()
        .position(|event| {
            matches!(
                event,
                AgentEvent::QuestionsResolved { request_id, outcome: QuestionsOutcome::Expired, .. }
                    if *request_id == id
            )
        })
        .expect("expired with its turn");
    let completed = events
        .iter()
        .position(|event| matches!(event, AgentEvent::TurnCompleted { turn_id: done } if *done == turn_id))
        .unwrap();
    assert!(expired < completed);
}
