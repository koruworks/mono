// SPDX-License-Identifier: MPL-2.0

//! **A local agent's questions, in the chat's own terms** (koru-ai D108).
//!
//! The session host says an agent stopped to ask; the chat draws the set as
//! one card in the reply, a block a question — its header, its words, its
//! options in the agent's order (the first is the one the agent
//! recommends), an *Other* box, a masked box for a secret — and sends back
//! an answer per question, or skips the set.
//!
//! **The set is an agent activity row, filed as a permission request**
//! (`agent.approval`), so it reads as one in every build and needs no new
//! column: it waits as `approval_required` (the chat *needs you*), an answer
//! completes it as allowed, a skip denies it, and a turn that ends first
//! cancels it, like any request left open. The set's own words are kept on
//! the row so it reads the same after a reload: the questions in
//! `agent_input` and the answers in `agent_output`, each a small JSON record
//! ([`QuestionsRecord`], [`AnswersRecord`]) bounded to the column's
//! [`AGENT_CONTENT_CHARS`](crate::AGENT_CONTENT_CHARS), and a plain line of
//! them in `agent_summary`, which is what an older build shows. A secret's
//! answer is never kept: the host has already withheld it.

use serde::{Deserialize, Serialize};

use crate::AGENT_CONTENT_CHARS;

/// One option a question offers: its label, sent back when chosen, and the
/// line under it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AgentQuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// One question an agent asked. `id` is the agent's own key for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AgentQuestion {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub header: String,
    pub question: String,
    /// In the agent's order: the first is the one it recommends.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<AgentQuestionOption>,
    #[serde(default)]
    pub allow_other: bool,
    #[serde(default)]
    pub secret: bool,
}

impl AgentQuestion {
    /// Whether it takes typed words: *Other*, a secret, or no options.
    pub fn takes_text(&self) -> bool {
        self.allow_other || self.secret || self.options.is_empty()
    }
}

/// What one question was answered with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentAnswerValue {
    /// One of its options, by label.
    Option { label: String },
    /// The person's own words.
    Text { text: String },
    /// A secret was given; it is not repeated.
    Withheld,
}

/// One question's answer, by the question's id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AgentAnswer {
    pub question_id: String,
    pub value: AgentAnswerValue,
}

/// The marker a question row's `agent_input` starts its record with.
const QUESTIONS_MARK: &str = "koru_questions";
/// The marker of the answers' record in `agent_output`.
const ANSWERS_MARK: &str = "koru_answers";

/// **The questions as the row keeps them**: each one's id, header and
/// words — what the summary after a reload shows. Options are live only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionsRecord {
    pub questions: Vec<RecordedQuestion>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedQuestion {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub header: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub secret: bool,
}

/// **The answers as the row keeps them**: a secret's is withheld.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswersRecord {
    pub answers: Vec<AgentAnswer>,
}

/// The longest any one text is cut to before the record is tried; halved
/// until the record fits its column.
const FIRST_CUT: usize = 600;

fn cut(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(limit.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// `value` under `mark`, as JSON, with every text cut by `shorten` — at
/// [`FIRST_CUT`] characters, then half that, until it fits
/// [`AGENT_CONTENT_CHARS`]. `None` if nothing fits.
fn fitted<T: Serialize>(mark: &str, shorten: impl Fn(usize) -> T) -> Option<String> {
    let mut limit = FIRST_CUT;
    loop {
        let mut record = serde_json::Map::new();
        record.insert(mark.to_owned(), serde_json::to_value(shorten(limit)).ok()?);
        let json = serde_json::to_string(&record).ok()?;
        if json.chars().count() <= AGENT_CONTENT_CHARS {
            return Some(json);
        }
        if limit <= 8 {
            return None;
        }
        limit /= 2;
    }
}

impl QuestionsRecord {
    pub fn of(questions: &[AgentQuestion]) -> Self {
        Self {
            questions: questions
                .iter()
                .map(|question| RecordedQuestion {
                    id: question.id.clone(),
                    header: question.header.clone(),
                    question: question.question.clone(),
                    secret: question.secret,
                })
                .collect(),
        }
    }

    /// The record as the row's `agent_input`, fitted to the column.
    pub fn encode(&self) -> Option<String> {
        fitted(QUESTIONS_MARK, |limit| Self {
            questions: self
                .questions
                .iter()
                .map(|question| RecordedQuestion {
                    id: question.id.clone(),
                    header: cut(&question.header, limit),
                    question: cut(&question.question, limit),
                    secret: question.secret,
                })
                .collect(),
        })
    }

    /// A row's `agent_input` read back: `None` for any row that is not a
    /// question set's.
    pub fn decode(input: &str) -> Option<Self> {
        decode_marked(input, QUESTIONS_MARK)
    }

    /// The set as one plain line — what an older build shows as the
    /// request's summary: each question's header, else its words.
    pub fn summary(&self) -> String {
        self.questions
            .iter()
            .map(|question| {
                if question.header.is_empty() {
                    question.question.as_str()
                } else {
                    question.header.as_str()
                }
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

impl AnswersRecord {
    /// The record as the row's `agent_output`, fitted to the column.
    pub fn encode(&self) -> Option<String> {
        fitted(ANSWERS_MARK, |limit| Self {
            answers: self
                .answers
                .iter()
                .map(|answer| AgentAnswer {
                    question_id: answer.question_id.clone(),
                    value: match &answer.value {
                        AgentAnswerValue::Option { label } => AgentAnswerValue::Option {
                            label: cut(label, limit),
                        },
                        AgentAnswerValue::Text { text } => AgentAnswerValue::Text {
                            text: cut(text, limit),
                        },
                        AgentAnswerValue::Withheld => AgentAnswerValue::Withheld,
                    },
                })
                .collect(),
        })
    }

    pub fn decode(output: &str) -> Option<Self> {
        decode_marked(output, ANSWERS_MARK)
    }
}

/// The record under `mark` in `text`, which [`fitted`] wrote as
/// `{"<mark>":…}`. Anything that does not start so — every other row's
/// input or output — is refused before it is parsed: a transcript is
/// rebuilt on every streamed delta and asks this of every row.
fn decode_marked<T: serde::de::DeserializeOwned>(text: &str, mark: &str) -> Option<T> {
    let rest = text.strip_prefix("{\"")?.strip_prefix(mark)?;
    if !rest.starts_with("\":") {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    serde_json::from_value(value.get(mark)?.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(id: &str, text: &str) -> AgentQuestion {
        AgentQuestion {
            id: id.into(),
            header: format!("Header {id}"),
            question: text.into(),
            options: vec![AgentQuestionOption {
                label: "Yes".into(),
                description: "Recommended".into(),
            }],
            allow_other: true,
            secret: false,
        }
    }

    /// **A set's record reads back, fits its column, and is nothing else's**:
    /// a long set is cut until it fits; an ordinary tool's input is not a
    /// set; the plain line names each question by its header.
    #[test]
    fn a_question_record_round_trips_and_fits_its_column() {
        let set = vec![question("a", "Which one?"), question("b", "And then?")];
        let record = QuestionsRecord::of(&set);
        let encoded = record.encode().unwrap();
        assert_eq!(QuestionsRecord::decode(&encoded), Some(record.clone()));
        assert_eq!(record.summary(), "Header a · Header b");
        assert_eq!(QuestionsRecord::decode("cargo test"), None);
        assert_eq!(QuestionsRecord::decode("{\"command\":\"ls\"}"), None);

        let long: Vec<AgentQuestion> = (0..16)
            .map(|index| question(&format!("q{index}"), &"words ".repeat(400)))
            .collect();
        let encoded = QuestionsRecord::of(&long).encode().unwrap();
        assert!(encoded.chars().count() <= AGENT_CONTENT_CHARS);
        let back = QuestionsRecord::decode(&encoded).unwrap();
        assert_eq!(back.questions.len(), 16);
        assert!(back.questions[0].question.ends_with('…'));

        let answers = AnswersRecord {
            answers: vec![
                AgentAnswer {
                    question_id: "a".into(),
                    value: AgentAnswerValue::Option {
                        label: "Yes".into(),
                    },
                },
                AgentAnswer {
                    question_id: "b".into(),
                    value: AgentAnswerValue::Withheld,
                },
            ],
        };
        let encoded = answers.encode().unwrap();
        assert_eq!(AnswersRecord::decode(&encoded), Some(answers));
        assert_eq!(
            AnswersRecord::decode(&QuestionsRecord::of(&set).encode().unwrap()),
            None
        );
    }
}
