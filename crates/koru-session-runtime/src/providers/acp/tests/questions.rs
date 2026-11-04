// SPDX-License-Identifier: MPL-2.0

//! **An ACP form elicitation as an agent's questions** (koru-ai D108):
//! Claude's `AskUserQuestion` as 0.76.0 sends it (`elicitation/create` in
//! form mode, `askUserQuestionsToCreateRequest`), and the answers back as
//! its `CreateElicitationResponse`.

use super::*;
use koru_session_protocol::{
    AgentQuestion, QuestionAnswer, QuestionAnswerValue, QuestionRequestId, QuestionsOutcome,
};

const ELICITATION: &str = include_str!("fixtures/claude-0.76.0/ask-user-question.elicitation.json");

fn elicitation() -> Value {
    serde_json::from_str(ELICITATION).unwrap()
}

/// A started session in a running turn, drawing questions or not.
fn running(questions: bool) -> (Harness, AgentTurnId) {
    let mut h = harness(FakeAcpConfig::default(), None, None);
    h.control.config.questions = questions;
    start(&mut h.control);
    let turn = AgentTurnId::mint();
    h.control.submit(input(turn)).unwrap();
    drain(&mut h.control);
    (h, turn)
}

fn ask(h: &mut Harness, frame: Value) -> Vec<AgentEvent> {
    h.injected.lock().unwrap().push_back(frame);
    drain(&mut h.control)
}

fn required(events: &[AgentEvent]) -> (QuestionRequestId, Vec<AgentQuestion>) {
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::QuestionsRequired {
                request_id,
                questions,
                blocking: true,
                ..
            } => Some((*request_id, questions.clone())),
            _ => None,
        })
        .expect("a question set")
}

fn replies(h: &Harness, id: u64) -> Vec<Value> {
    h.state
        .lock()
        .unwrap()
        .sent()
        .iter()
        .filter(|frame| frame["id"] == id && frame.get("method").is_none())
        .cloned()
        .collect()
}

fn initialize_capabilities(h: &Harness) -> Value {
    h.state
        .lock()
        .unwrap()
        .sent()
        .iter()
        .find(|frame| frame["method"] == "initialize")
        .unwrap()["params"]["clientCapabilities"]
        .clone()
}

fn resolved(events: &[AgentEvent], id: QuestionRequestId) -> QuestionsOutcome {
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::QuestionsResolved {
                request_id,
                outcome,
                ..
            } if *request_id == id => Some(outcome.clone()),
            _ => None,
        })
        .expect("the set closed")
}

/// **Claude's form becomes one set in its order, and the answers go back
/// keyed as it reads them**: a chosen option's value in its field — in a
/// list for a multi-select — and the person's own words in the companion
/// *Other* field. Form elicitation is advertised only for a session that
/// draws questions.
#[test]
fn claudes_ask_user_question_form_becomes_a_set_and_its_answers_an_accept() {
    let (mut h, turn) = running(true);
    assert_eq!(
        initialize_capabilities(&h),
        json!({"elicitation":{"form":{}}})
    );
    let events = ask(&mut h, elicitation());
    let (id, questions) = required(&events);
    let ids: Vec<&str> = questions
        .iter()
        .map(|question| question.id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["question_0", "question_1"],
        "companions fold into Other"
    );
    assert_eq!(questions[0].header, "Framework");
    assert_eq!(
        questions[0].question,
        "Which web framework should the service use?"
    );
    let labels: Vec<&str> = questions[0]
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(labels, ["Axum (Recommended)", "Actix Web"]);
    assert_eq!(
        questions[0].options[0].description,
        "Tokio-native, and already a dependency."
    );
    assert!(questions.iter().all(|question| question.allow_other));
    assert!(replies(&h, 7).is_empty(), "nothing answered yet");

    h.control
        .answer_questions(
            id,
            Some(vec![
                QuestionAnswer {
                    question_id: "question_0".into(),
                    value: QuestionAnswerValue::Option {
                        label: "Axum (Recommended)".into(),
                    },
                },
                QuestionAnswer {
                    question_id: "question_1".into(),
                    value: QuestionAnswerValue::Option {
                        label: "macOS".into(),
                    },
                },
            ]),
        )
        .unwrap();
    assert_eq!(
        replies(&h, 7),
        [
            json!({"jsonrpc":"2.0","id":7,"result":{"action":"accept","content":{
                "question_0":"Axum (Recommended)","question_1":["macOS"]
            }}})
        ]
    );
    let events = drain(&mut h.control);
    assert!(matches!(
        resolved(&events, id),
        QuestionsOutcome::Answered { answers } if answers.len() == 2
    ));
    assert!(events.iter().all(|event| match event {
        AgentEvent::QuestionsResolved { turn_id, .. } => *turn_id == turn,
        _ => true,
    }));

    // Other: the person's words go in the companion field.
    let mut again = elicitation();
    again["id"] = json!(8);
    let (id, _) = required(&ask(&mut h, again));
    h.control
        .answer_questions(
            id,
            Some(vec![QuestionAnswer {
                question_id: "question_0".into(),
                value: QuestionAnswerValue::Text {
                    text: "Poem".into(),
                },
            }]),
        )
        .unwrap();
    assert_eq!(
        replies(&h, 8)[0]["result"],
        json!({"action":"accept","content":{"question_0_custom":"Poem"}})
    );
}

/// **A skip is `decline`** — Claude tells its model the person skipped —
/// and a turn stopped with a form open answers it `cancel` and closes the
/// set; so does the agent taking it back with `$/cancel_request`.
#[test]
fn a_skip_declines_and_a_stop_or_a_take_back_expires_the_set() {
    let (mut h, _) = running(true);
    let (id, _) = required(&ask(&mut h, elicitation()));
    h.control.answer_questions(id, None).unwrap();
    assert_eq!(replies(&h, 7)[0]["result"], json!({"action":"decline"}));
    assert_eq!(
        resolved(&drain(&mut h.control), id),
        QuestionsOutcome::Skipped
    );

    let (mut h, turn) = running(true);
    let (id, _) = required(&ask(&mut h, elicitation()));
    h.control.cancel(turn).unwrap();
    assert_eq!(replies(&h, 7)[0]["result"], json!({"action":"cancel"}));
    assert_eq!(
        resolved(&drain(&mut h.control), id),
        QuestionsOutcome::Expired
    );

    let (mut h, _) = running(true);
    let (id, _) = required(&ask(&mut h, elicitation()));
    let events = ask(
        &mut h,
        json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":7}}),
    );
    assert_eq!(resolved(&events, id), QuestionsOutcome::Expired);
    assert_eq!(replies(&h, 7)[0]["error"]["code"], -32800);
    assert_eq!(
        h.control.answer_questions(id, None).unwrap_err().code,
        ErrorCode::ApprovalStale
    );
}

/// **A session whose creator cannot draw questions advertises nothing**, so
/// Claude 0.76.0 does not offer its model the tool at all; a form sent
/// anyway is answered *Method not found*, as every Koru before v21 did.
#[test]
fn without_questions_nothing_is_advertised_and_a_form_is_not_found() {
    let (mut h, _) = running(false);
    assert_eq!(initialize_capabilities(&h), json!({}));
    let events = ask(&mut h, elicitation());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::QuestionsRequired { .. }))
    );
    assert_eq!(replies(&h, 7)[0]["error"]["code"], -32601);
}

/// **Any agent's form maps the same way**: fields in their natural order
/// (`question_10` after `question_2`), a string without options a question
/// that takes text, an optional field of another kind left out — and a
/// required one declines the whole form.
#[test]
fn a_generic_form_maps_in_natural_order_or_declines() {
    let form = |properties: Value, required: Value| {
        json!({"mode":"form","sessionId":SESSION,"message":"Set up the repository",
            "requestedSchema":{"type":"object","properties":properties,"required":required}})
    };
    let (questions, _) = elicitation::read_form(&form(
        json!({
            "question_10":{"type":"string","enum":["Late"]},
            "question_2":{"type":"string","title":"Name","description":"The repository's name"},
            "private":{"type":"boolean","title":"Private"},
        }),
        json!(["question_2"]),
    ))
    .unwrap();
    let ids: Vec<&str> = questions
        .iter()
        .map(|question| question.id.as_str())
        .collect();
    assert_eq!(ids, ["question_2", "question_10"]);
    assert!(questions[0].options.is_empty() && questions[0].takes_text());
    assert_eq!(questions[0].header, "Name");
    assert_eq!(questions[1].options[0].label, "Late");

    assert!(
        elicitation::read_form(&form(
            json!({"private":{"type":"boolean","title":"Private"}}),
            json!(["private"]),
        ))
        .is_none()
    );
    // One field: the form's message is its question.
    let (single, _) = elicitation::read_form(&form(
        json!({"answer":{"type":"string","title":"Answer"}}),
        json!([]),
    ))
    .unwrap();
    assert_eq!(single[0].question, "Set up the repository");
}
