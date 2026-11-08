// SPDX-License-Identifier: MPL-2.0

//! **An ACP form elicitation as an agent's questions** (protocol v21,
//! koru-ai D108).
//!
//! Koru advertises `clientCapabilities.elicitation.form` at `initialize`
//! only for a session whose creator draws questions. Claude's wrapper
//! (0.76.0) then offers its model the `AskUserQuestion` tool — without it,
//! the tool is disallowed outright — and sends each call as an
//! `elicitation/create` request in form mode: a property per question,
//! `question_<n>`, a single-select `string` with a titled `oneOf` (or a
//! multi-select `array` with a titled `anyOf`), the question's header as its
//! `title` and its text as its `description` (one question's text is the
//! request's `message` instead), and beside each a free-text
//! `question_<n>_custom` marked by the shared
//! `_meta._askUserQuestionCustomAnswer` — the *Other* box. The answer is
//! `{"action":"accept","content":{…}}` keyed the same way; a skip is
//! `{"action":"decline"}` (Claude tells its model the person skipped); a
//! set closed by its turn is `{"action":"cancel"}`.
//!
//! The request is generic — any agent, and any MCP server through one, may
//! send it — so the mapping is too: a `string` field with options, a
//! `string` field without (a question that takes text), an `array` field
//! with options (answered with the one option chosen). A field that is none
//! of those is left out when optional; a required one declines the form.
//! The properties are read in their natural key order (`question_2` before
//! `question_10`): the adapter's JSON keeps keys sorted, not as sent.

use super::*;
use koru_session_protocol::{
    AgentQuestion, QuestionAnswer, QuestionAnswerValue, QuestionOption, QuestionRequestId,
    QuestionsOutcome,
};

pub(super) const CREATE: &str = "elicitation/create";
pub(super) const CANCEL_REQUEST: &str = "$/cancel_request";

/// The `_meta` key that marks a field as the custom answer of another.
const CUSTOM_ANSWER_META: &str = "_askUserQuestionCustomAnswer";

/// One question's fields in the form.
#[derive(Clone)]
pub(super) struct FormField {
    /// The field the choice or the text goes in.
    key: String,
    /// The field is a multi-select: its value is a list.
    array: bool,
    /// Each option's label and the value the form takes for it.
    values: Vec<(String, String)>,
    /// The companion field the person's own words go in.
    custom: Option<String>,
}

/// One form the agent asked and nobody has answered yet.
#[derive(Clone)]
pub(super) struct PendingElicitation {
    pub(super) request_id: Value,
    pub(super) turn_id: AgentTurnId,
    pub(super) questions: Vec<AgentQuestion>,
    fields: Vec<FormField>,
}

/// `a < b` with every run of digits compared as a number.
fn natural_key(key: &str) -> Vec<(u8, u64, String)> {
    let mut parts = Vec::new();
    let mut digits = String::new();
    let mut letters = String::new();
    for ch in key.chars() {
        if ch.is_ascii_digit() {
            if !letters.is_empty() {
                parts.push((1, 0, std::mem::take(&mut letters)));
            }
            digits.push(ch);
        } else {
            if !digits.is_empty() {
                parts.push((
                    0,
                    digits.parse().unwrap_or(u64::MAX),
                    std::mem::take(&mut digits),
                ));
            }
            letters.push(ch);
        }
    }
    if !digits.is_empty() {
        parts.push((0, digits.parse().unwrap_or(u64::MAX), digits));
    }
    if !letters.is_empty() {
        parts.push((1, 0, letters));
    }
    parts
}

/// A field's options: a titled `oneOf`/`anyOf` (the label is the title,
/// the value the `const`), or a bare `enum` (both the string).
fn options_of(schema: &Value) -> Option<Vec<(QuestionOption, String)>> {
    if let Some(titled) = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Value::as_array)
    {
        return Some(
            titled
                .iter()
                .filter_map(|option| {
                    let value = option.get("const")?.as_str()?.to_owned();
                    let label = option
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or(&value)
                        .to_owned();
                    Some((
                        QuestionOption {
                            label,
                            description: option
                                .get("description")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        },
                        value,
                    ))
                })
                .collect(),
        );
    }
    schema.get("enum").and_then(Value::as_array).map(|values| {
        values
            .iter()
            .filter_map(Value::as_str)
            .map(|value| {
                (
                    QuestionOption {
                        label: value.to_owned(),
                        description: String::new(),
                    },
                    value.to_owned(),
                )
            })
            .collect()
    })
}

/// **The form as questions**, with the fields they map to; `None` when the
/// form asks something Koru cannot draw (a required field of another kind)
/// or nothing at all.
pub(super) fn read_form(params: &Value) -> Option<(Vec<AgentQuestion>, Vec<FormField>)> {
    let properties = params.pointer("/requestedSchema/properties")?.as_object()?;
    let required: Vec<&str> = params
        .pointer("/requestedSchema/required")
        .and_then(Value::as_array)
        .map(|required| required.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let message = params
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // The custom-answer companions, by the field each belongs to.
    let custom_of = |key: &str, schema: &Value| -> Option<String> {
        let marked = schema
            .pointer(&format!("/_meta/{CUSTOM_ANSWER_META}/questionId"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        marked.or_else(|| {
            key.strip_suffix("_custom")
                .filter(|base| properties.contains_key(*base))
                .map(str::to_owned)
        })
    };
    let mut companions = BTreeMap::new();
    for (key, schema) in properties {
        if let Some(base) = custom_of(key, schema)
            && schema.get("type").and_then(Value::as_str) == Some("string")
        {
            companions.insert(base, key.clone());
        }
    }
    let mut keys: Vec<&String> = properties
        .keys()
        .filter(|key| !companions.values().any(|custom| custom == *key))
        .collect();
    keys.sort_by_key(|key| natural_key(key));
    let single = keys.len() == 1;
    let mut questions = Vec::new();
    let mut fields = Vec::new();
    for key in keys {
        let schema = &properties[key];
        let kind = schema.get("type").and_then(Value::as_str);
        let (array, options) = match kind {
            Some("string") => (false, options_of(schema).unwrap_or_default()),
            Some("array") => match schema.get("items").and_then(options_of) {
                Some(options) if !options.is_empty() => (true, options),
                _ if required.contains(&key.as_str()) => return None,
                _ => continue,
            },
            _ if required.contains(&key.as_str()) => return None,
            _ => continue,
        };
        let title = schema
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let description = schema.get("description").and_then(Value::as_str);
        // The question's words: its own description, else — the one field
        // of a form — the form's message, else its title, else its key.
        let (header, question) = match description {
            Some(text) if !text.trim().is_empty() => (title, text),
            _ if single && !message.trim().is_empty() => (title, message),
            _ if !title.trim().is_empty() => ("", title),
            _ => ("", key.as_str()),
        };
        let custom = companions.get(key).cloned();
        let (labels, values): (Vec<QuestionOption>, Vec<String>) = options.into_iter().unzip();
        questions.push(AgentQuestion {
            id: key.clone(),
            header: header.to_owned(),
            question: question.to_owned(),
            options: labels.clone(),
            allow_other: custom.is_some(),
            secret: false,
        });
        fields.push(FormField {
            key: key.clone(),
            array,
            values: labels
                .into_iter()
                .map(|option| option.label)
                .zip(values)
                .collect(),
            custom,
        });
    }
    let questions = koru_session_protocol::bound_questions(questions)?;
    // The bound may drop a question; its field goes with it.
    fields.retain(|field| questions.iter().any(|question| question.id == field.key));
    Some((questions, fields))
}

/// The `elicitation/create` result for `answers`: `accept` with each
/// answer in its field — a chosen option's value (in a list for a
/// multi-select), the person's own words in the companion field, or in the
/// field itself where it takes text. `None` is a skip: `decline`.
pub(super) fn accept_content(fields: &[FormField], answers: Option<&[QuestionAnswer]>) -> Value {
    let Some(answers) = answers else {
        return json!({"action":"decline"});
    };
    let mut content = serde_json::Map::new();
    for answer in answers {
        let Some(field) = fields.iter().find(|field| field.key == answer.question_id) else {
            continue;
        };
        match &answer.value {
            QuestionAnswerValue::Option { label } => {
                let Some((_, value)) = field.values.iter().find(|(known, _)| known == label) else {
                    continue;
                };
                content.insert(
                    field.key.clone(),
                    if field.array {
                        json!([value])
                    } else {
                        json!(value)
                    },
                );
            }
            QuestionAnswerValue::Text { text } => {
                let key = match (&field.custom, field.values.is_empty()) {
                    (Some(custom), _) => custom.clone(),
                    (None, true) => field.key.clone(),
                    (None, false) => continue,
                };
                content.insert(key, json!(text));
            }
            QuestionAnswerValue::Withheld => {}
        }
    }
    json!({"action":"accept","content":content})
}

impl AcpControl {
    /// `elicitation/create`: a question set on the running turn, when the
    /// session draws questions and the form is one Koru can draw.
    pub(super) fn elicitation(
        &mut self,
        request_id: Value,
        params: &Value,
    ) -> Result<(), ProviderFailure> {
        decode::<acp::CreateElicitationRequest>(params)?;
        let decline = json!({"action":"decline"});
        if params["mode"].as_str() != Some("form") {
            self.mark_unknown();
            return self.reply_elicitation(request_id, decline);
        }
        let Some(turn_id) = self.active else {
            self.mark_unknown();
            return self.reply_elicitation(request_id, decline);
        };
        if params["sessionId"].as_str() != self.native_session.as_deref() {
            self.log.foreign_session();
            return self.reply_elicitation(request_id, decline);
        }
        if self.turns[&turn_id].cancelling.is_some() {
            return self.reply_elicitation(request_id, json!({"action":"cancel"}));
        }
        // A duplicated provider request never opens a second set.
        if self
            .elicitations
            .values()
            .any(|pending| pending.request_id == request_id)
        {
            return Ok(());
        }
        let Some((questions, fields)) = read_form(params) else {
            tracing::info!(target: "koru::acp", event = "acp.questions.unrepresentable");
            return self.reply_elicitation(request_id, decline);
        };
        // Reserve the set's eventual resolution, as an approval does.
        if self.events.len() + self.approvals.len() + self.elicitations.len() + 2
            > self.config.event_capacity
        {
            return Err(failure(
                ErrorCode::Backpressure,
                ProviderStage::Approval,
                ProviderDiagnostic::Backpressure,
            ));
        }
        let id = QuestionRequestId::mint();
        tracing::info!(target: "koru::acp", event = "acp.questions.asked", count = questions.len());
        self.elicitations.insert(
            id,
            PendingElicitation {
                request_id,
                turn_id,
                questions: questions.clone(),
                fields,
            },
        );
        self.queue(AgentEvent::QuestionsRequired {
            turn_id,
            request_id: id,
            questions,
            blocking: true,
            delivery: koru_session_protocol::QuestionsDelivery::InTurn,
        })
    }

    /// Answer or skip a pending form, and close its set.
    pub(super) fn answer_elicitation(
        &mut self,
        id: QuestionRequestId,
        answers: Option<Vec<QuestionAnswer>>,
    ) -> Result<(), ProviderFailure> {
        self.ensure_usable(ProviderStage::Approval)?;
        let pending = self.elicitations.get(&id).cloned().ok_or_else(|| {
            failure(
                ErrorCode::ApprovalStale,
                ProviderStage::Approval,
                ProviderDiagnostic::InvalidState,
            )
        })?;
        let answers = answers
            .map(|answers| koru_session_protocol::validate_answers(&pending.questions, answers))
            .transpose()
            .map_err(|_| {
                failure(
                    ErrorCode::InvalidRequest,
                    ProviderStage::Approval,
                    ProviderDiagnostic::InvalidInput,
                )
            })?;
        self.reply_elicitation(
            pending.request_id.clone(),
            accept_content(&pending.fields, answers.as_deref()),
        )?;
        self.elicitations.remove(&id);
        tracing::info!(target: "koru::acp", event = "acp.questions.answered", skipped = answers.is_none());
        self.queue(AgentEvent::QuestionsResolved {
            turn_id: pending.turn_id,
            request_id: id,
            outcome: match answers {
                Some(answers) => QuestionsOutcome::Answered {
                    answers: koru_session_protocol::withhold_secrets(&pending.questions, &answers),
                },
                None => QuestionsOutcome::Skipped,
            },
        })
    }

    /// `$/cancel_request` for a form still open: the agent took it back.
    /// It expires, and the request is answered as cancelled.
    pub(super) fn cancel_request(&mut self, params: &Value) -> Result<(), ProviderFailure> {
        let Some(request_id) = params.get("requestId") else {
            self.mark_unknown();
            return Ok(());
        };
        let Some(id) = self
            .elicitations
            .iter()
            .find_map(|(id, pending)| (pending.request_id == *request_id).then_some(*id))
        else {
            self.mark_unknown();
            return Ok(());
        };
        let pending = self.elicitations.remove(&id).unwrap();
        let sent = self
            .transport()?
            .send(&json!({"jsonrpc":"2.0","id":pending.request_id,
            "error":{"code":-32800,"message":"Request cancelled"}}));
        if let Err(error) = &sent {
            self.fail_transport(error.clone());
        }
        sent?;
        self.queue(AgentEvent::QuestionsResolved {
            turn_id: pending.turn_id,
            request_id: id,
            outcome: QuestionsOutcome::Expired,
        })
    }

    /// The turn ended or is being stopped: each form it asked is answered
    /// `cancel` and its set expires.
    pub(super) fn expire_elicitations(
        &mut self,
        turn_id: AgentTurnId,
    ) -> Result<(), ProviderFailure> {
        let ids: Vec<_> = self
            .elicitations
            .iter()
            .filter_map(|(id, pending)| (pending.turn_id == turn_id).then_some(*id))
            .collect();
        for id in ids {
            let pending = self.elicitations.remove(&id).unwrap();
            self.reply_elicitation(pending.request_id, json!({"action":"cancel"}))?;
            self.queue(AgentEvent::QuestionsResolved {
                turn_id,
                request_id: id,
                outcome: QuestionsOutcome::Expired,
            })?;
        }
        Ok(())
    }

    fn reply_elicitation(
        &mut self,
        request_id: Value,
        result: Value,
    ) -> Result<(), ProviderFailure> {
        let sent = self
            .transport()?
            .send(&json!({"jsonrpc":"2.0","id":request_id,"result":result}));
        if let Err(error) = &sent {
            self.fail_transport(error.clone());
        }
        sent
    }
}
