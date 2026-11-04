// SPDX-License-Identifier: MPL-2.0

//! **Codex's questions to the person** (protocol v21, koru-ai D108).
//!
//! Codex asks through the app-server's `item/tool/requestUserInput` server
//! request (`ToolRequestUserInputParams`, marked experimental in its schema
//! and sent without the experimental opt-in, 0.166.0): `questions[]`, each
//! `{id, header, question, options?: [{label, description}], isOther,
//! isSecret}`, and `isBlocking`. The answer is a
//! `ToolRequestUserInputResponse`: `{"answers": {"<id>": {"answers":
//! ["<label or text>"]}}}`. A skipped set is the same shape with no answers
//! — what the app-server itself hands the model when its client cannot
//! answer.
//!
//! A session whose creator does not draw questions answers the request
//! `-32601 unsupported request`, exactly as every Koru before v21 did.

use koru_session_protocol::{
    AgentEvent, AgentQuestion, ErrorCode, QuestionAnswer, QuestionAnswerValue, QuestionOption,
    QuestionRequestId, QuestionsOutcome,
};
use serde_json::{Map, Value, json};

use super::{
    CodexControl, ProviderDiagnostic, ProviderFailure, ProviderStage, RpcId, failure, provider_kind,
};

/// One set Codex asked and nobody has answered yet.
#[derive(Clone)]
pub(super) struct PendingQuestions {
    pub(super) request_id: RpcId,
    pub(super) turn_id: koru_session_protocol::AgentTurnId,
    pub(super) questions: Vec<AgentQuestion>,
    pub(super) blocking: bool,
}

pub(super) const METHOD: &str = "item/tool/requestUserInput";

/// The questions of a `ToolRequestUserInputParams`, in Codex's order and
/// with each one's options in its order. A question that is not an object,
/// or has no string id or text, is skipped; the bound drops the rest.
fn read_questions(params: &Value) -> Vec<AgentQuestion> {
    let Some(questions) = params.get("questions").and_then(Value::as_array) else {
        return Vec::new();
    };
    questions
        .iter()
        .filter_map(|question| {
            let id = question.get("id")?.as_str()?.to_owned();
            let text = question.get("question")?.as_str()?.to_owned();
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(|option| {
                            Some(QuestionOption {
                                label: option.get("label")?.as_str()?.to_owned(),
                                description: option
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(AgentQuestion {
                id,
                header: question
                    .get("header")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                question: text,
                options,
                allow_other: question.get("isOther").and_then(Value::as_bool) == Some(true),
                secret: question.get("isSecret").and_then(Value::as_bool) == Some(true),
            })
        })
        .collect()
}

/// The `ToolRequestUserInputResponse` for `answers`: each answered question
/// by its id, its one answer — the option's label or the person's words —
/// in a list. `None`, a skip, is no answers at all.
pub(super) fn response(answers: Option<&[QuestionAnswer]>) -> Value {
    let mut map = Map::new();
    for answer in answers.unwrap_or_default() {
        let said = match &answer.value {
            QuestionAnswerValue::Option { label } => label,
            QuestionAnswerValue::Text { text } => text,
            QuestionAnswerValue::Withheld => continue,
        };
        map.insert(answer.question_id.clone(), json!({ "answers": [said] }));
    }
    json!({ "answers": map })
}

impl CodexControl {
    /// `item/tool/requestUserInput`: a question set for a session that
    /// draws them, `-32601` for any other.
    pub(super) fn handle_questions(
        &mut self,
        request_id: RpcId,
        params: &Value,
    ) -> Result<(), ProviderFailure> {
        if !self.config.questions {
            self.mark_unknown();
            return self.reply_error(request_id, -32601, "unsupported request");
        }
        let Some(turn_id) = self.koru_turn(params.get("turnId")) else {
            self.mark_unknown();
            return self.reply_error(request_id, -32602, "unknown turn");
        };
        let Some(questions) = koru_session_protocol::bound_questions(read_questions(params)) else {
            // Nothing a person could answer: the agent hears no answers.
            return self.reply_result(request_id, response(None));
        };
        // Only a set Codex says it does not wait on leaves the turn running.
        let blocking = params.get("isBlocking").and_then(Value::as_bool) != Some(false);
        let id = QuestionRequestId::mint();
        self.questions.insert(
            id,
            PendingQuestions {
                request_id,
                turn_id,
                questions: questions.clone(),
                blocking,
            },
        );
        tracing::info!(
            target: "koru::codex",
            event = "codex.questions.asked",
            count = questions.len(),
            blocking
        );
        self.queue(AgentEvent::QuestionsRequired {
            turn_id,
            request_id: id,
            questions,
            blocking,
        })?;
        if blocking {
            self.queue(AgentEvent::ProviderState {
                provider: provider_kind(),
                state: koru_session_protocol::ManagedState::WaitingForInput,
            })?;
        }
        Ok(())
    }

    /// Answer or skip a pending set, and close it.
    pub(super) fn answer_pending_questions(
        &mut self,
        id: QuestionRequestId,
        answers: Option<Vec<QuestionAnswer>>,
    ) -> Result<(), ProviderFailure> {
        let pending = self.questions.get(&id).cloned().ok_or_else(|| {
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
        self.reply_result(pending.request_id.clone(), response(answers.as_deref()))?;
        self.questions.remove(&id);
        let outcome = match &answers {
            Some(answers) => QuestionsOutcome::Answered {
                answers: koru_session_protocol::withhold_secrets(&pending.questions, answers),
            },
            None => QuestionsOutcome::Skipped,
        };
        tracing::info!(
            target: "koru::codex",
            event = "codex.questions.answered",
            skipped = answers.is_none()
        );
        self.close_questions(pending, id, outcome)
    }

    /// Say a set closed, and give the turn back once nothing waits on the
    /// person.
    pub(super) fn close_questions(
        &mut self,
        pending: PendingQuestions,
        id: QuestionRequestId,
        outcome: QuestionsOutcome,
    ) -> Result<(), ProviderFailure> {
        self.queue(AgentEvent::QuestionsResolved {
            turn_id: pending.turn_id,
            request_id: id,
            outcome,
        })?;
        let waiting = self.questions.values().any(|other| other.blocking);
        if pending.blocking && !waiting && self.approvals.is_empty() {
            self.queue(AgentEvent::ProviderState {
                provider: provider_kind(),
                state: koru_session_protocol::ManagedState::Running,
            })?;
        }
        Ok(())
    }

    /// The set Codex resolved itself (`serverRequest/resolved`), if it was
    /// one: it expires. `true` when it was.
    pub(super) fn expire_resolved_questions(
        &mut self,
        request_id: &RpcId,
    ) -> Result<bool, ProviderFailure> {
        let Some(id) = self
            .questions
            .iter()
            .find_map(|(id, pending)| (pending.request_id == *request_id).then_some(*id))
        else {
            return Ok(false);
        };
        let pending = self
            .questions
            .remove(&id)
            .expect("the set was found immediately before removal");
        self.close_questions(pending, id, QuestionsOutcome::Expired)?;
        Ok(true)
    }

    /// The turn ended: every set it asked expires.
    pub(super) fn expire_turn_questions(
        &mut self,
        turn_id: koru_session_protocol::AgentTurnId,
    ) -> Result<(), ProviderFailure> {
        let ids: Vec<QuestionRequestId> = self
            .questions
            .iter()
            .filter_map(|(id, pending)| (pending.turn_id == turn_id).then_some(*id))
            .collect();
        for id in ids {
            self.questions.remove(&id);
            self.queue(AgentEvent::QuestionsResolved {
                turn_id,
                request_id: id,
                outcome: QuestionsOutcome::Expired,
            })?;
        }
        Ok(())
    }

    pub(super) fn reply_result(
        &mut self,
        request_id: RpcId,
        result: Value,
    ) -> Result<(), ProviderFailure> {
        self.transport()?.send(&json!({
            "id": request_id.value(),
            "result": result,
        }))
    }
}
