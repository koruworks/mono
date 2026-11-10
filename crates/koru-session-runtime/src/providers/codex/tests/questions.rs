// SPDX-License-Identifier: MPL-2.0

//! **Codex's questions** (koru-ai D108): `item/tool/requestUserInput` in the
//! shape 0.166.0's generated schema gives it, to a question set, and the
//! person's answers back as its `ToolRequestUserInputResponse`.

use koru_session_protocol::{
    AgentQuestion, ManagedState, QuestionAnswer, QuestionAnswerValue, QuestionRequestId,
    QuestionsDelivery, QuestionsOutcome, QuestionsReply,
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

const ASYNC_ITEM: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/codex-app-server/0.166.0/request-user-input-async.server.json"
));
const ASYNC_REPLY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/codex-app-server/0.166.0/request-user-input-async.client.json"
));

fn async_item() -> Value {
    serde_json::from_str(ASYNC_ITEM).unwrap()
}

fn text_input(text: &str) -> AgentInput {
    AgentInput {
        model: None,
        turn_id: AgentTurnId::mint(),
        text: text.into(),
        attachments: Vec::new(),
        references: Vec::new(),
        mode: None,
        fast: None,
        reasoning: None,
        skills: Vec::new(),
    }
}

/// A started control whose first turn (`native-turn-1`) was submitted;
/// `frames` follow its `turn/start` result, and the session draws
/// questions when `questions` says so.
fn running(
    frames: Vec<Value>,
    questions: bool,
) -> (CodexControl, Arc<Mutex<ReplayState>>, AgentTurnId) {
    let mut inbound = initialize_frames(false);
    inbound.push(json!({ "id": 4, "result": { "turn": { "id": "native-turn-1" } } }));
    inbound.extend(frames);
    let (mut control, state) = replay_control(inbound, Arc::new(NoopAttachmentPathResolver));
    control.config.questions = questions;
    control
        .start(AgentStart {
            session_id: SessionId::mint(),
            parent_session_id: None,
            resume: None,
        })
        .unwrap();
    let input = text_input("ask me");
    let turn_id = input.turn_id;
    control.submit(input).unwrap();
    (control, state, turn_id)
}

fn async_answers() -> Vec<QuestionAnswer> {
    vec![
        answer(
            "[\"request_user_input_async\",\"call_fixture_async\",0]",
            QuestionAnswerValue::Option {
                label: "SQLite (Recommended)".into(),
            },
        ),
        answer(
            "[\"request_user_input_async\",\"call_fixture_async\",1]",
            QuestionAnswerValue::Text {
                text: " .cache ".into(),
            },
        ),
    ]
}

fn sent_with(state: &Arc<Mutex<ReplayState>>, method: &str) -> Vec<Value> {
    state
        .lock()
        .unwrap()
        .sent
        .iter()
        .filter(|message| message.get("method").and_then(Value::as_str) == Some(method))
        .cloned()
        .collect()
}

/// The events up to and including the first `last` matches: a frame queued
/// after it — the result of a call the test makes next — is left unread.
fn drain_through(
    control: &mut CodexControl,
    last: impl Fn(&AgentEvent) -> bool,
) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = control.next_event(Duration::ZERO).unwrap() {
        let done = last(&event);
        events.push(event);
        if done {
            break;
        }
    }
    events
}

fn next_message_set(events: &[AgentEvent]) -> (QuestionRequestId, Vec<AgentQuestion>) {
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::QuestionsRequired {
                request_id,
                questions,
                blocking: false,
                delivery: QuestionsDelivery::NextMessage,
                ..
            } => Some((*request_id, questions.clone())),
            _ => None,
        })
        .expect("a set answered by the next message")
}

/// **`request_user_input_async`'s item becomes a set answered by the next
/// message** (D108): the 0.166.0 item shape, each question under Codex's own
/// `questionItemId`, its options in Codex's order, the person's words always
/// taken and nothing secret; its plain text is not also the reply, and the
/// set outlives the turn. The answers then go as a new turn, in Codex's
/// reply envelope exactly as its clients write it, and close the set on the
/// turn that asked.
#[test]
fn an_async_item_becomes_a_set_answered_by_the_next_message() {
    let (mut control, state, asking) = running(
        vec![
            async_item(),
            turn_completed("native-turn-1", "completed"),
            json!({ "id": 5, "result": { "turn": { "id": "native-turn-2" } } }),
        ],
        true,
    );
    let events = drain_through(&mut control, |event| {
        matches!(event, AgentEvent::TurnCompleted { .. })
    });
    let (id, questions) = next_message_set(&events);
    let ids: Vec<&str> = questions.iter().map(|q| q.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "[\"request_user_input_async\",\"call_fixture_async\",0]",
            "[\"request_user_input_async\",\"call_fixture_async\",1]"
        ]
    );
    assert_eq!(
        questions[0].question,
        "Which database should the cache use?"
    );
    let labels: Vec<&str> = questions[0]
        .options
        .iter()
        .map(|o| o.label.as_str())
        .collect();
    assert_eq!(labels, ["SQLite (Recommended)", "Redis"]);
    assert!(
        questions[0]
            .options
            .iter()
            .all(|o| o.description.is_empty())
    );
    assert!(
        questions
            .iter()
            .all(|q| q.allow_other && !q.secret && q.header.is_empty())
    );
    assert!(questions[1].options.is_empty());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::MessageDelta { .. })),
        "the card replaces the item's text"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::QuestionsResolved { .. })),
        "the set outlives its turn"
    );
    assert!(
        events.iter().any(
            |event| matches!(event, AgentEvent::TurnCompleted { turn_id } if *turn_id == asking)
        )
    );

    let next = text_input("");
    let next_turn = next.turn_id;
    control
        .submit_answers(
            next,
            QuestionsReply {
                request_id: id,
                answers: async_answers(),
            },
        )
        .unwrap();
    let started = sent_with(&state, "turn/start");
    assert_eq!(started.len(), 2);
    let expected: Value = serde_json::from_str(ASYNC_REPLY).unwrap();
    assert_eq!(started[1]["params"]["input"], json!([expected]));
    let events = drain(&mut control);
    assert!(events.iter().any(
        |event| matches!(event, AgentEvent::TurnStarted { turn_id } if *turn_id == next_turn)
    ));
    let outcome = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::QuestionsResolved {
                turn_id,
                request_id,
                outcome,
            } if *request_id == id => Some((*turn_id, outcome.clone())),
            _ => None,
        })
        .expect("closed");
    assert_eq!(outcome.0, asking, "closed on the turn that asked");
    assert!(matches!(outcome.1, QuestionsOutcome::Answered { answers } if answers.len() == 2));
    // Answered once.
    assert_eq!(
        control
            .submit_answers(
                text_input(""),
                QuestionsReply {
                    request_id: id,
                    answers: async_answers(),
                },
            )
            .unwrap_err()
            .code,
        ErrorCode::ApprovalStale
    );
}

/// **The envelope is Codex's own**: `[answer, question, questionItemId]`
/// with no spaces, one entry a question answered in the set's order, and a
/// question's words cut at 512 bytes on a character boundary, on one line —
/// the bound `AnsweredQuestion::new` applies.
#[test]
fn the_reply_envelope_is_codexs_and_bounds_the_question() {
    let long = "é\n".repeat(1_000);
    let questions = vec![AgentQuestion {
        id: "[\"request_user_input_async\",\"item\",0]".into(),
        header: String::new(),
        question: long.clone(),
        options: Vec::new(),
        allow_other: true,
        secret: false,
    }];
    let text = super::super::questions::reply_text(
        &questions,
        &[answer(
            "[\"request_user_input_async\",\"item\",0]",
            QuestionAnswerValue::Text {
                text: "A \"quoted\" answer\nwith a second line".into(),
            },
        )],
    );
    let body = text
        .strip_prefix("<send_user_message_question_reply>\n")
        .and_then(|rest| rest.strip_suffix("\n</send_user_message_question_reply>"))
        .unwrap();
    assert!(body.starts_with("[{\"answer\":"));
    let parsed: Value = serde_json::from_str(body).unwrap();
    let mut end = 512;
    while !long.is_char_boundary(end) {
        end -= 1;
    }
    assert_eq!(
        parsed,
        json!([{
            "answer": "A \"quoted\" answer\nwith a second line",
            "question": long[..end].replace('\n', " "),
            "questionItemId": "[\"request_user_input_async\",\"item\",0]",
        }])
    );
}

/// **While the turn that asked still runs, the answers steer into it**
/// (`turn/steer`, the envelope as its input); once it ended they are
/// refused here — they go as the next message — and nothing is sent. A
/// skip sends nothing at all.
#[test]
fn a_running_turn_takes_the_answers_by_steer_and_a_skip_sends_nothing() {
    let (mut control, state, asking) = running(
        vec![
            async_item(),
            json!({ "id": 5, "result": { "turnId": "native-turn-1" } }),
        ],
        true,
    );
    let (id, _) = next_message_set(&drain_through(&mut control, |event| {
        matches!(event, AgentEvent::QuestionsRequired { .. })
    }));
    control.answer_questions(id, Some(async_answers())).unwrap();
    let steered = sent_with(&state, "turn/steer");
    assert_eq!(steered.len(), 1);
    assert_eq!(steered[0]["params"]["expectedTurnId"], "native-turn-1");
    let expected: Value = serde_json::from_str(ASYNC_REPLY).unwrap();
    assert_eq!(steered[0]["params"]["input"], json!([expected]));
    assert!(drain(&mut control).iter().any(|event| matches!(
        event,
        AgentEvent::QuestionsResolved { turn_id, request_id, outcome: QuestionsOutcome::Answered { .. } }
            if *turn_id == asking && *request_id == id
    )));

    let (mut control, state, _) = running(
        vec![async_item(), turn_completed("native-turn-1", "completed")],
        true,
    );
    let (id, _) = next_message_set(&drain(&mut control));
    let before = state.lock().unwrap().sent.len();
    assert_eq!(
        control
            .answer_questions(id, Some(async_answers()))
            .unwrap_err()
            .code,
        ErrorCode::InvalidTransition,
        "an ended turn takes no steer: the answers are the next message"
    );
    control.answer_questions(id, None).unwrap();
    assert_eq!(
        state.lock().unwrap().sent.len(),
        before,
        "a skip costs nothing"
    );
    assert!(drain(&mut control).iter().any(|event| matches!(
        event,
        AgentEvent::QuestionsResolved { request_id, outcome: QuestionsOutcome::Skipped, .. }
            if *request_id == id
    )));
}

/// **Another message passes the set over**: it expires when the message
/// starts its turn, and its answers are then stale.
#[test]
fn another_message_passes_an_async_set_over() {
    let (mut control, _, _) = running(
        vec![
            async_item(),
            turn_completed("native-turn-1", "completed"),
            json!({ "id": 5, "result": { "turn": { "id": "native-turn-2" } } }),
        ],
        true,
    );
    let (id, _) = next_message_set(&drain_through(&mut control, |event| {
        matches!(event, AgentEvent::TurnCompleted { .. })
    }));
    control.submit(text_input("something else")).unwrap();
    assert!(drain(&mut control).iter().any(|event| matches!(
        event,
        AgentEvent::QuestionsResolved { request_id, outcome: QuestionsOutcome::Expired, .. }
            if *request_id == id
    )));
    assert_eq!(
        control
            .submit_answers(
                text_input(""),
                QuestionsReply {
                    request_id: id,
                    answers: async_answers(),
                },
            )
            .unwrap_err()
            .code,
        ErrorCode::ApprovalStale
    );
}

/// **A session that asks nobody reads the item as the reply's words**, and
/// so does a message that streamed nothing: a completed `agentMessage`'s
/// text is said once, never lost and never twice.
#[test]
fn a_message_that_streamed_nothing_is_said_whole_and_once() {
    let (mut control, _, asking) = running(vec![async_item()], false);
    let events = drain(&mut control);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::QuestionsRequired { .. }))
    );
    let said: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::MessageDelta { turn_id, text } if *turn_id == asking => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        said,
        [async_item()["params"]["item"]["text"].as_str().unwrap()]
    );

    let message = |id: &str, text: &str| {
        json!({
            "method": "item/completed",
            "params": {
                "threadId": "fixture-thread",
                "turnId": "native-turn-1",
                "completedAtMs": 0,
                "item": { "type": "agentMessage", "id": id, "text": text, "phase": "final_answer" }
            }
        })
    };
    let delta = json!({
        "method": "item/agentMessage/delta",
        "params": {
            "threadId": "fixture-thread",
            "turnId": "native-turn-1",
            "itemId": "msg_streamed",
            "delta": "Streamed."
        }
    });
    let (mut control, _, _) = running(
        vec![
            delta,
            message("msg_streamed", "Streamed."),
            message("msg_silent", "Said once."),
            message("msg_empty", ""),
        ],
        true,
    );
    let said: Vec<String> = drain(&mut control)
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::MessageDelta { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(said, ["Streamed.", "Said once."]);
}
