// SPDX-License-Identifier: MPL-2.0

//! **Codex's questions to the person** (protocol v21, koru-ai D108).
//!
//! Codex asks in two ways, and both reach the person as the same card.
//!
//! **A request it waits on.** The app-server's `item/tool/requestUserInput`
//! server request (`ToolRequestUserInputParams`, marked experimental in its
//! schema and sent without the experimental opt-in, 0.166.0): `questions[]`,
//! each `{id, header, question, options?: [{label, description}], isOther,
//! isSecret}`, and `isBlocking`. The answer is a
//! `ToolRequestUserInputResponse`: `{"answers": {"<id>": {"answers":
//! ["<label or text>"]}}}`. A skipped set is the same shape with no answers
//! — what the app-server itself hands the model when its client cannot
//! answer.
//!
//! **Questions it asks in its reply and moves on from.** The model's
//! `request_user_input_async` tool (codex-rs `core/src/tools/handlers/
//! request_user_input_async.rs`, 0.166.0) answers its model
//! `{"accepted":true}` at once and adds an `agentMessage` thread item with
//! `delivery: "async"` and `questions: [{title, options?: [string]}]`; its
//! `text` is the same set as plain lines, and no delta streams it. The
//! answers are expected later, as the person's next message, in the reply
//! envelope Codex's own clients write (`context-fragments/src/
//! answered_question.rs`, built by the TUI in `bottom_pane/async_questions/
//! state.rs`):
//!
//! ```text
//! <send_user_message_question_reply>
//! [{"answer":"…","question":"…","questionItemId":"[\"request_user_input_async\",\"<item id>\",<index>]"}]
//! </send_user_message_question_reply>
//! ```
//!
//! Each question's id here is Codex's own `questionItemId` — the JSON text
//! of `["request_user_input_async", <item id>, <index>]` — so an answer
//! names its question exactly as Codex's clients do. Every question takes
//! the person's words beside its options, and none is a secret. The set
//! outlives its turn (`QuestionsDelivery::NextMessage`): the answers go
//! with `turn/steer` while the turn that asked still runs, and as a new
//! `turn/start` after; a skip sends nothing; another message passes it over.
//!
//! A session whose creator does not draw questions answers the request
//! `-32601 unsupported request`, exactly as every Koru before v21 did, and
//! reads an async set's `text` as the reply's words.

use koru_session_protocol::{
    AgentEvent, AgentQuestion, AgentTurnId, ErrorCode, QuestionAnswer, QuestionAnswerValue,
    QuestionOption, QuestionRequestId, QuestionsDelivery, QuestionsOutcome, QuestionsReply,
};
use serde_json::{Map, Value, json};

use super::{
    AgentInput, CodexControl, ProviderDiagnostic, ProviderFailure, ProviderStage, RpcId, failure,
    provider_kind, valid_native_id,
};

/// How Codex asked a set, which says how it is answered.
#[derive(Clone)]
pub(super) enum Asked {
    /// `item/tool/requestUserInput`: answered on the request, which Codex
    /// waits on unless it said `isBlocking: false`.
    Request { request_id: RpcId, blocking: bool },
    /// `request_user_input_async`'s `agentMessage` item: answered by the
    /// person's next message, in Codex's reply envelope.
    Message,
}

/// One set Codex asked and nobody has answered yet.
#[derive(Clone)]
pub(super) struct PendingQuestions {
    pub(super) asked: Asked,
    pub(super) turn_id: AgentTurnId,
    pub(super) questions: Vec<AgentQuestion>,
}

impl PendingQuestions {
    fn blocking(&self) -> bool {
        matches!(self.asked, Asked::Request { blocking: true, .. })
    }
}

pub(super) const METHOD: &str = "item/tool/requestUserInput";

/// The tool name Codex's clients put first in an async question's id.
const ASYNC_TOOL: &str = "request_user_input_async";
/// The reply envelope's markers (`AnsweredQuestion::type_markers`).
const REPLY_OPEN: &str = "<send_user_message_question_reply>";
const REPLY_CLOSE: &str = "</send_user_message_question_reply>";
/// The bytes of a question's words the envelope repeats, as Codex bounds
/// them (`AnsweredQuestion::new`).
const REPLY_QUESTION_BYTES: usize = 512;

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

/// The questions of an async `agentMessage` item (`AsyncUserInputQuestion`:
/// `{title, options?: [string]}`), each under Codex's own id for it — the
/// JSON text of `["request_user_input_async", <item id>, <index>]` — with
/// its options as labels, the person's words always taken, and no secret.
fn read_async_questions(item_id: &str, item: &Value) -> Vec<AgentQuestion> {
    let Some(questions) = item.get("questions").and_then(Value::as_array) else {
        return Vec::new();
    };
    questions
        .iter()
        .enumerate()
        .filter_map(|(index, question)| {
            let title = question.get("title")?.as_str()?.to_owned();
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(Value::as_str)
                        .map(|label| QuestionOption {
                            label: label.to_owned(),
                            description: String::new(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(AgentQuestion {
                id: json!([ASYNC_TOOL, item_id, index]).to_string(),
                header: String::new(),
                question: title,
                options,
                allow_other: true,
                secret: false,
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

/// **An async set's answers as Codex's reply envelope** — the person's next
/// message. One entry a question answered, in the set's order, written as
/// Codex's `AnsweredQuestion` writes one: `answer`, `question` (its words cut
/// at 512 bytes, on one line) and `questionItemId`, the question's id. Codex
/// writes its JSON with its keys in that order and no spaces, and so does
/// this.
pub(super) fn reply_text(questions: &[AgentQuestion], answers: &[QuestionAnswer]) -> String {
    let replies: Vec<Value> = questions
        .iter()
        .filter_map(|question| {
            let answer = answers
                .iter()
                .find(|answer| answer.question_id == question.id)?;
            let said = match &answer.value {
                QuestionAnswerValue::Option { label } => label,
                QuestionAnswerValue::Text { text } => text,
                QuestionAnswerValue::Withheld => return None,
            };
            let mut entry = Map::new();
            entry.insert("answer".into(), json!(said));
            entry.insert("question".into(), json!(reply_question(&question.question)));
            entry.insert("questionItemId".into(), json!(question.id));
            Some(Value::Object(entry))
        })
        .collect();
    format!("{REPLY_OPEN}\n{}\n{REPLY_CLOSE}", Value::Array(replies))
}

/// A question's words as the envelope repeats them: at most
/// [`REPLY_QUESTION_BYTES`], cut at a character boundary, on one line.
fn reply_question(question: &str) -> String {
    let mut end = question.len().min(REPLY_QUESTION_BYTES);
    while !question.is_char_boundary(end) {
        end -= 1;
    }
    question[..end].replace(['\n', '\r'], " ")
}

fn stale_set() -> ProviderFailure {
    failure(
        ErrorCode::ApprovalStale,
        ProviderStage::Approval,
        ProviderDiagnostic::InvalidState,
    )
}

fn unrepresentable() -> ProviderFailure {
    failure(
        ErrorCode::InvalidRequest,
        ProviderStage::Approval,
        ProviderDiagnostic::InvalidInput,
    )
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
                asked: Asked::Request {
                    request_id,
                    blocking,
                },
                turn_id,
                questions: questions.clone(),
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
            delivery: QuestionsDelivery::InTurn,
        })?;
        if blocking {
            self.queue(AgentEvent::ProviderState {
                provider: provider_kind(),
                state: koru_session_protocol::ManagedState::WaitingForInput,
            })?;
        }
        Ok(())
    }

    /// **A completed `agentMessage` that is an async question set**: a set
    /// answered by the next message, for a session that draws questions.
    /// `false` when it is not one — an ordinary message, a set with nothing
    /// a person could answer, or a session that asks nobody — and its text
    /// is then the reply's.
    pub(super) fn handle_async_questions(
        &mut self,
        turn_id: AgentTurnId,
        item: &Value,
    ) -> Result<bool, ProviderFailure> {
        if !self.config.questions || item.get("delivery").and_then(Value::as_str) != Some("async") {
            return Ok(false);
        }
        let Some(item_id) = item
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| valid_native_id(value))
        else {
            return Ok(false);
        };
        let Some(questions) =
            koru_session_protocol::bound_questions(read_async_questions(item_id, item))
        else {
            return Ok(false);
        };
        let id = QuestionRequestId::mint();
        self.questions.insert(
            id,
            PendingQuestions {
                asked: Asked::Message,
                turn_id,
                questions: questions.clone(),
            },
        );
        tracing::info!(
            target: "koru::codex",
            event = "codex.questions.asked",
            count = questions.len(),
            delivery = "next_message"
        );
        self.queue(AgentEvent::QuestionsRequired {
            turn_id,
            request_id: id,
            questions,
            blocking: false,
            delivery: QuestionsDelivery::NextMessage,
        })?;
        Ok(true)
    }

    /// Answer or skip a pending set, and close it. A set asked in a reply
    /// takes its answers into the turn that asked while it still runs
    /// (`turn/steer`); once that turn ended they go as the next message
    /// ([`Self::send_answers`]) and this refuses them. Its skip sends
    /// nothing.
    pub(super) fn answer_pending_questions(
        &mut self,
        id: QuestionRequestId,
        answers: Option<Vec<QuestionAnswer>>,
    ) -> Result<(), ProviderFailure> {
        let pending = self.questions.get(&id).cloned().ok_or_else(stale_set)?;
        let answers = answers
            .map(|answers| koru_session_protocol::validate_answers(&pending.questions, answers))
            .transpose()
            .map_err(|_| unrepresentable())?;
        match &pending.asked {
            Asked::Request { request_id, .. } => {
                self.reply_result(request_id.clone(), response(answers.as_deref()))?;
            }
            Asked::Message => {
                if let Some(answers) = &answers {
                    if answers.is_empty() {
                        return Err(unrepresentable());
                    }
                    let running = self
                        .turns
                        .get(&pending.turn_id)
                        .is_some_and(|turn| !turn.terminal && turn.native_id.is_some());
                    if !running {
                        return Err(failure(
                            ErrorCode::InvalidTransition,
                            ProviderStage::Submit,
                            ProviderDiagnostic::InvalidState,
                        ));
                    }
                    self.steer_turn(AgentInput {
                        model: None,
                        turn_id: pending.turn_id,
                        text: reply_text(&pending.questions, answers),
                        attachments: Vec::new(),
                        references: Vec::new(),
                        mode: None,
                        fast: None,
                        reasoning: None,
                        skills: Vec::new(),
                    })?;
                }
            }
        }
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

    /// **An async set's answers as the person's next message**: Codex's
    /// reply envelope, as a new turn (`turn/start`) or into the running one
    /// (`turn/steer`). The input brings the turn's id and settings and no
    /// words of its own; the set must be one asked in a reply and still
    /// pending, and the answers must fit it.
    pub(super) fn send_answers(
        &mut self,
        mut input: AgentInput,
        reply: QuestionsReply,
        steer: bool,
    ) -> Result<(), ProviderFailure> {
        if !input.text.is_empty() || !input.attachments.is_empty() {
            return Err(failure(
                ErrorCode::InvalidRequest,
                ProviderStage::Submit,
                ProviderDiagnostic::InvalidInput,
            ));
        }
        let pending = self
            .questions
            .get(&reply.request_id)
            .filter(|pending| matches!(pending.asked, Asked::Message))
            .cloned()
            .ok_or_else(stale_set)?;
        let answers = koru_session_protocol::validate_answers(&pending.questions, reply.answers)
            .map_err(|_| unrepresentable())?;
        if answers.is_empty() {
            return Err(unrepresentable());
        }
        input.text = reply_text(&pending.questions, &answers);
        if steer {
            self.steer_turn(input)?;
        } else {
            self.start_turn(input)?;
        }
        self.questions.remove(&reply.request_id);
        tracing::info!(
            target: "koru::codex",
            event = "codex.questions.answered",
            skipped = false,
            steered = steer
        );
        self.close_questions(
            pending,
            reply.request_id,
            QuestionsOutcome::Answered { answers },
        )
    }

    /// **Another message passes over every set asked in a reply**: the
    /// person moved on, and the sets expire.
    pub(super) fn pass_over_message_questions(&mut self) -> Result<(), ProviderFailure> {
        let ids: Vec<QuestionRequestId> = self
            .questions
            .iter()
            .filter_map(|(id, pending)| matches!(pending.asked, Asked::Message).then_some(*id))
            .collect();
        for id in ids {
            let pending = self
                .questions
                .remove(&id)
                .expect("the set was found immediately before removal");
            self.close_questions(pending, id, QuestionsOutcome::Expired)?;
        }
        Ok(())
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
        let waiting = self.questions.values().any(PendingQuestions::blocking);
        if pending.blocking() && !waiting && self.approvals.is_empty() {
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
        let Some(id) = self.questions.iter().find_map(|(id, pending)| {
            matches!(&pending.asked, Asked::Request { request_id: asked, .. } if asked == request_id)
                .then_some(*id)
        }) else {
            return Ok(false);
        };
        let pending = self
            .questions
            .remove(&id)
            .expect("the set was found immediately before removal");
        self.close_questions(pending, id, QuestionsOutcome::Expired)?;
        Ok(true)
    }

    /// The turn ended: every set it asked on a request expires. A set asked
    /// in its reply outlives it — it is answered by the next message.
    pub(super) fn expire_turn_questions(
        &mut self,
        turn_id: AgentTurnId,
    ) -> Result<(), ProviderFailure> {
        let ids: Vec<QuestionRequestId> = self
            .questions
            .iter()
            .filter_map(|(id, pending)| {
                (pending.turn_id == turn_id && matches!(pending.asked, Asked::Request { .. }))
                    .then_some(*id)
            })
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
