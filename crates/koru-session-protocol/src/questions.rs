// SPDX-License-Identifier: MPL-2.0

//! **An agent's questions to the person** (protocol v21, koru-ai D108).
//!
//! An agent may stop and ask: Codex through its `request_user_input` tool,
//! Claude through `AskUserQuestion` as an ACP form elicitation. Both reach
//! Koru as one shape — a set of questions, each with its options in the
//! agent's order (the first is the one it recommends), an *Other* box where
//! the agent takes free text, and a masked box for a secret — and go back
//! as one shape: an answer per question, or the whole set skipped.
//!
//! The words are the agent's and the person's: they are drawn, never put in
//! a diagnostic (`Debug` names only the shape), and a secret answer is never
//! repeated — the event that says the set was answered carries
//! [`QuestionAnswerValue::Withheld`] in its place.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most questions one set keeps; any after it are dropped.
pub const MAX_QUESTIONS: usize = 16;
/// The most options one question keeps; any after it are dropped.
pub const MAX_QUESTION_OPTIONS: usize = 16;
/// The most characters of a question's text, or a free answer.
pub const QUESTION_TEXT_CHARS: usize = 2_000;
/// The most characters of a header, an option's label or its line.
pub const QUESTION_LABEL_CHARS: usize = 500;
/// The most bytes of a question's id, which is the agent's and is sent back
/// exactly as it came.
pub const QUESTION_ID_BYTES: usize = 256;

/// One option a question offers. `label` is what is sent back when it is
/// chosen; `description` is the line under it, empty when the agent gave
/// none.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// One question. `id` is the agent's own key for it, returned with the
/// answer; `header` is the short label above it, empty when the agent gave
/// none. With no options the question takes text only.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentQuestion {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub header: String,
    pub question: String,
    /// In the agent's order: the first is the one it recommends.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<QuestionOption>,
    /// The agent takes an answer of the person's own words beside its
    /// options (*Other*).
    #[serde(default)]
    pub allow_other: bool,
    /// The answer is a secret: typed into a masked box and never repeated.
    #[serde(default)]
    pub secret: bool,
}

impl AgentQuestion {
    /// Whether the question takes typed text: it allows *Other*, is a
    /// secret, or offers no options at all.
    pub fn takes_text(&self) -> bool {
        self.allow_other || self.secret || self.options.is_empty()
    }
}

/// What a question was answered with.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuestionAnswerValue {
    /// One of its options, by label.
    Option { label: String },
    /// The person's own words: *Other*, a secret, or a question with no
    /// options.
    Text { text: String },
    /// A secret was given and is not repeated. Only ever in
    /// [`QuestionsOutcome::Answered`]; an answer sent with it is refused.
    Withheld,
}

/// One question's answer, by the question's id.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QuestionAnswer {
    pub question_id: String,
    pub value: QuestionAnswerValue,
}

/// How a set of questions ended.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuestionsOutcome {
    /// The person answered; a secret answer is [`QuestionAnswerValue::Withheld`].
    Answered { answers: Vec<QuestionAnswer> },
    /// The person skipped the set; the agent was told so.
    Skipped,
    /// It closed unanswered: the turn ended or was stopped, the agent took
    /// it back, or the session went away.
    Expired,
}

/// Why answers were refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestionAnswerError {
    /// An answer names no question of the set, or one twice.
    UnknownQuestion,
    /// An option that question does not offer, text it does not take, or a
    /// withheld value.
    Unrepresentable,
}

/// `questions` bounded for [`crate::AgentEvent::QuestionsRequired`]: at most
/// [`MAX_QUESTIONS`], each with a usable id and a question, texts trimmed and
/// cut at a character boundary, options without a label dropped. `None`
/// when nothing is left to ask.
pub fn bound_questions(questions: Vec<AgentQuestion>) -> Option<Vec<AgentQuestion>> {
    let mut seen = std::collections::BTreeSet::new();
    let bounded: Vec<AgentQuestion> = questions
        .into_iter()
        .filter(|question| valid_question_id(&question.id))
        .filter(|question| seen.insert(question.id.clone()))
        .filter_map(|question| {
            let text = cut(&question.question, QUESTION_TEXT_CHARS)?;
            let mut labels = std::collections::BTreeSet::new();
            let options = question
                .options
                .into_iter()
                .filter_map(|option| {
                    let label = cut(&option.label, QUESTION_LABEL_CHARS)?;
                    labels.insert(label.clone()).then(|| QuestionOption {
                        label,
                        description: cut(&option.description, QUESTION_LABEL_CHARS)
                            .unwrap_or_default(),
                    })
                })
                .take(MAX_QUESTION_OPTIONS)
                .collect();
            Some(AgentQuestion {
                id: question.id,
                header: cut(&question.header, QUESTION_LABEL_CHARS).unwrap_or_default(),
                question: text,
                options,
                allow_other: question.allow_other,
                secret: question.secret,
            })
        })
        .take(MAX_QUESTIONS)
        .collect();
    (!bounded.is_empty()).then_some(bounded)
}

/// Check `answers` against the set they answer: each names one of its
/// questions once, an option is one that question offers, and text goes
/// only where the question takes it. A question left unanswered is allowed;
/// the agent hears it was not answered. Free text is trimmed and bounded.
pub fn validate_answers(
    questions: &[AgentQuestion],
    answers: Vec<QuestionAnswer>,
) -> Result<Vec<QuestionAnswer>, QuestionAnswerError> {
    let mut answered = std::collections::BTreeSet::new();
    let mut valid = Vec::with_capacity(answers.len());
    for answer in answers {
        let question = questions
            .iter()
            .find(|question| question.id == answer.question_id)
            .ok_or(QuestionAnswerError::UnknownQuestion)?;
        if !answered.insert(question.id.clone()) {
            return Err(QuestionAnswerError::UnknownQuestion);
        }
        let value = match answer.value {
            QuestionAnswerValue::Option { label } => {
                if !question.options.iter().any(|option| option.label == label) {
                    return Err(QuestionAnswerError::Unrepresentable);
                }
                QuestionAnswerValue::Option { label }
            }
            QuestionAnswerValue::Text { text } => {
                if !question.takes_text() {
                    return Err(QuestionAnswerError::Unrepresentable);
                }
                // A secret is kept as typed; other words are trimmed.
                let text = if question.secret {
                    text.chars().take(QUESTION_TEXT_CHARS).collect()
                } else {
                    cut(&text, QUESTION_TEXT_CHARS).unwrap_or_default()
                };
                if text.is_empty() {
                    continue;
                }
                QuestionAnswerValue::Text { text }
            }
            QuestionAnswerValue::Withheld => return Err(QuestionAnswerError::Unrepresentable),
        };
        valid.push(QuestionAnswer {
            question_id: answer.question_id,
            value,
        });
    }
    Ok(valid)
}

/// `answers` as the event that closes the set says them: a secret
/// question's answer is [`QuestionAnswerValue::Withheld`].
pub fn withhold_secrets(
    questions: &[AgentQuestion],
    answers: &[QuestionAnswer],
) -> Vec<QuestionAnswer> {
    answers
        .iter()
        .map(|answer| {
            let secret = questions
                .iter()
                .any(|question| question.id == answer.question_id && question.secret);
            QuestionAnswer {
                question_id: answer.question_id.clone(),
                value: if secret {
                    QuestionAnswerValue::Withheld
                } else {
                    answer.value.clone()
                },
            }
        })
        .collect()
}

/// An agent's question id is usable: non-empty, bounded, and free of
/// control characters. It is sent back byte for byte.
pub fn valid_question_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= QUESTION_ID_BYTES && !id.chars().any(char::is_control)
}

/// `text` trimmed and cut to `limit` characters, with an ellipsis when cut;
/// `None` when nothing is left.
fn cut(text: &str, limit: usize) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= limit {
        return Some(text.to_owned());
    }
    let mut bounded: String = text.chars().take(limit - 1).collect();
    bounded.push('…');
    Some(bounded)
}

impl std::fmt::Debug for QuestionOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuestionOption").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for AgentQuestion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentQuestion")
            .field("options", &self.options.len())
            .field("allow_other", &self.allow_other)
            .field("secret", &self.secret)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for QuestionAnswerValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Option { .. } => "Option",
            Self::Text { .. } => "Text",
            Self::Withheld => "Withheld",
        })
    }
}

impl std::fmt::Debug for QuestionAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuestionAnswer")
            .field("value", &self.value)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for QuestionsOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Answered { answers } => f
                .debug_struct("Answered")
                .field("answers", &answers.len())
                .finish(),
            Self::Skipped => f.write_str("Skipped"),
            Self::Expired => f.write_str("Expired"),
        }
    }
}
