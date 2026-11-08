// SPDX-License-Identifier: MPL-2.0

use crate::*;

fn question(id: &str, options: &[&str]) -> AgentQuestion {
    AgentQuestion {
        id: id.into(),
        header: "Header".into(),
        question: format!("Question {id}?"),
        options: options
            .iter()
            .map(|label| QuestionOption {
                label: (*label).into(),
                description: String::new(),
            })
            .collect(),
        allow_other: false,
        secret: false,
    }
}

fn option(label: &str) -> QuestionAnswerValue {
    QuestionAnswerValue::Option {
        label: label.into(),
    }
}

fn text(text: &str) -> QuestionAnswerValue {
    QuestionAnswerValue::Text { text: text.into() }
}

fn answer(id: &str, value: QuestionAnswerValue) -> QuestionAnswer {
    QuestionAnswer {
        question_id: id.into(),
        value,
    }
}

/// **The events are v21, and say their tags** (koru-ai D108): a v20 peer is
/// never sent either, and the set's shape round-trips with its options in
/// the agent's order.
#[test]
fn questions_ride_protocol_twenty_one_in_the_agents_order() {
    let required = AgentEvent::QuestionsRequired {
        turn_id: super::turn_id(),
        request_id: QuestionRequestId::mint(),
        questions: vec![question("pick", &["Recommended", "Second", "Third"])],
        blocking: true,
        delivery: QuestionsDelivery::InTurn,
    };
    assert_eq!(required.minimum_protocol_version(), 21);
    let json = serde_json::to_value(&required).unwrap();
    assert_eq!(json["kind"], "questions_required");
    // A set answered in its turn writes no delivery: the v21 shape as it was.
    assert!(json.get("delivery").is_none());
    assert_eq!(json["questions"][0]["options"][0]["label"], "Recommended");
    assert_eq!(json["questions"][0]["options"][2]["label"], "Third");
    assert!(
        json["questions"][0]["options"][0]
            .get("description")
            .is_none()
    );
    let back: AgentEvent = serde_json::from_value(json).unwrap();
    assert_eq!(back, required);

    let resolved = AgentEvent::QuestionsResolved {
        turn_id: super::turn_id(),
        request_id: QuestionRequestId::mint(),
        outcome: QuestionsOutcome::Skipped,
    };
    assert_eq!(resolved.minimum_protocol_version(), 21);
    let json = serde_json::to_value(&resolved).unwrap();
    assert_eq!(json["kind"], "questions_resolved");
    assert_eq!(json["outcome"]["kind"], "skipped");
}

/// A set written without `blocking` is one the agent waits on.
#[test]
fn a_set_blocks_unless_it_says_otherwise() {
    let json = serde_json::json!({
        "kind": "questions_required",
        "turn_id": "33333333-3333-4333-8333-333333333333",
        "request_id": "66666666-6666-4666-8666-666666666666",
        "questions": [{"id": "q", "question": "Why?"}],
    });
    let AgentEvent::QuestionsRequired {
        blocking,
        questions,
        ..
    } = serde_json::from_value(json).unwrap()
    else {
        panic!("a questions event");
    };
    assert!(blocking);
    assert!(questions[0].takes_text(), "no options takes text");
}

/// **A set the agent moved on from is answered by the next message**
/// (koru-ai D108): it says so as `delivery: next_message`, a set that says
/// nothing is answered in its turn, and the answers sent as input carry the
/// set's id and the answers — never their words in a diagnostic.
#[test]
fn a_set_answered_by_the_next_message_says_so() {
    let required = AgentEvent::QuestionsRequired {
        turn_id: super::turn_id(),
        request_id: QuestionRequestId::mint(),
        questions: vec![question("pick", &["One"])],
        blocking: false,
        delivery: QuestionsDelivery::NextMessage,
    };
    let json = serde_json::to_value(&required).unwrap();
    assert_eq!(json["delivery"], "next_message");
    assert_eq!(
        serde_json::from_value::<AgentEvent>(json).unwrap(),
        required
    );

    let json = serde_json::json!({
        "kind": "questions_required",
        "turn_id": "33333333-3333-4333-8333-333333333333",
        "request_id": "66666666-6666-4666-8666-666666666666",
        "questions": [{"id": "q", "question": "Why?"}],
    });
    let AgentEvent::QuestionsRequired { delivery, .. } = serde_json::from_value(json).unwrap()
    else {
        panic!("a questions event");
    };
    assert_eq!(delivery, QuestionsDelivery::InTurn);

    let reply = QuestionsReply {
        request_id: QuestionRequestId::mint(),
        answers: vec![answer("pick", text("private words"))],
    };
    let json = serde_json::to_value(&reply).unwrap();
    assert_eq!(json["answers"][0]["question_id"], "pick");
    assert_eq!(
        serde_json::from_value::<QuestionsReply>(json).unwrap(),
        reply
    );
    assert!(!format!("{reply:?}").contains("private"));
}

/// **A set is bounded and a blank one is nothing**: a question with no id,
/// a repeated id or no words is dropped, a blank option goes, a long text
/// is cut, and the count is capped.
#[test]
fn a_set_is_bounded_and_blank_is_nothing() {
    assert_eq!(bound_questions(Vec::new()), None);
    let mut blank = question("q", &[]);
    blank.question = "   ".into();
    assert_eq!(bound_questions(vec![blank]), None);

    let mut long = question("long", &["", "  Keep  ", "Keep", "Other"]);
    long.question = "é".repeat(QUESTION_TEXT_CHARS + 10);
    let bounded = bound_questions(vec![
        question("", &["A"]),
        long,
        question("long", &["Dup"]),
        question("bad\u{7}", &["A"]),
    ])
    .unwrap();
    assert_eq!(bounded.len(), 1);
    assert_eq!(bounded[0].question.chars().count(), QUESTION_TEXT_CHARS);
    assert!(bounded[0].question.ends_with('…'));
    let labels: Vec<&str> = bounded[0]
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect();
    assert_eq!(labels, ["Keep", "Other"], "blank and repeated labels go");

    let many = (0..MAX_QUESTIONS + 4)
        .map(|index| question(&format!("q{index}"), &["A"]))
        .collect();
    assert_eq!(bound_questions(many).unwrap().len(), MAX_QUESTIONS);
}

/// **An answer fits its question or is refused**: an offered option, text
/// only where the question takes it, each question once, never a withheld
/// value; blank text is no answer.
#[test]
fn answers_fit_their_questions_or_are_refused() {
    let mut other = question("other", &["A", "B"]);
    other.allow_other = true;
    let mut secret = question("secret", &[]);
    secret.secret = true;
    let questions = vec![question("plain", &["A", "B"]), other, secret];

    let valid = validate_answers(
        &questions,
        vec![
            answer("plain", option("B")),
            answer("other", text("  my own  ")),
            answer("secret", text(" hunter2 ")),
        ],
    )
    .unwrap();
    assert_eq!(valid[1].value, text("my own"));
    assert_eq!(
        valid[2].value,
        text(" hunter2 "),
        "a secret is kept as typed"
    );

    for (answers, error) in [
        (
            vec![answer("nobody", option("A"))],
            QuestionAnswerError::UnknownQuestion,
        ),
        (
            vec![answer("plain", option("A")), answer("plain", option("B"))],
            QuestionAnswerError::UnknownQuestion,
        ),
        (
            vec![answer("plain", option("C"))],
            QuestionAnswerError::Unrepresentable,
        ),
        (
            vec![answer("plain", text("free"))],
            QuestionAnswerError::Unrepresentable,
        ),
        (
            vec![answer("other", QuestionAnswerValue::Withheld)],
            QuestionAnswerError::Unrepresentable,
        ),
    ] {
        assert_eq!(validate_answers(&questions, answers), Err(error));
    }
    assert_eq!(
        validate_answers(&questions, vec![answer("other", text("   "))]).unwrap(),
        Vec::new()
    );
}

/// **A secret is never repeated**: the event that closes the set withholds
/// it, and no diagnostic prints a question's or an answer's words.
#[test]
fn a_secret_is_withheld_and_words_never_reach_a_diagnostic() {
    let mut secret = question("token", &[]);
    secret.secret = true;
    let questions = vec![question("plain", &["Yes"]), secret];
    let answers = vec![
        answer("plain", option("Yes")),
        answer("token", text("sk-123")),
    ];
    let said = withhold_secrets(&questions, &answers);
    assert_eq!(said[0].value, option("Yes"));
    assert_eq!(said[1].value, QuestionAnswerValue::Withheld);
    let printed = format!("{questions:?} {answers:?}");
    assert!(!printed.contains("sk-123"), "{printed}");
    assert!(!printed.contains("Question plain?"), "{printed}");
    assert!(!printed.contains("Header"), "{printed}");
    assert!(!printed.contains("Yes"), "{printed}");
}
