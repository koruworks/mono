// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_ai_core::{AgentAnswer, AgentAnswerValue};
use koru_session_protocol as s;

#[test]
fn native_answers_refuse_normalization_and_overflow_instead_of_losing_input() {
    let mut question = s::AgentQuestion {
        id: "q".into(),
        header: String::new(),
        question: "Text".into(),
        options: vec![],
        allow_other: true,
        secret: false,
    };
    let answer = |text: &str| {
        vec![AgentAnswer {
            question_id: "q".into(),
            value: AgentAnswerValue::Text { text: text.into() },
        }]
    };
    assert!(validate_bot_answers(&[question.clone()], answer("  meaningful spaces  ")).is_err());
    assert!(validate_bot_answers(&[question.clone()], answer(" ")).is_err());
    assert!(
        validate_bot_answers(
            &[question.clone()],
            answer(&"x".repeat(s::QUESTION_TEXT_CHARS + 1))
        )
        .is_err()
    );
    let exact = "Unicode é answer";
    assert_eq!(
        validate_bot_answers(&[question.clone()], answer(exact)).unwrap(),
        vec![s::QuestionAnswer {
            question_id: "q".into(),
            value: s::QuestionAnswerValue::Text { text: exact.into() }
        }]
    );
    question.secret = true;
    let secret = "  exact secret spaces  ";
    assert_eq!(
        validate_bot_answers(&[question.clone()], answer(secret)).unwrap(),
        vec![s::QuestionAnswer {
            question_id: "q".into(),
            value: s::QuestionAnswerValue::Text {
                text: secret.into()
            }
        }]
    );
    assert!(
        validate_bot_answers(&[question], answer(&"s".repeat(s::QUESTION_TEXT_CHARS + 1))).is_err()
    );
}
