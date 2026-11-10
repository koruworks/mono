// SPDX-License-Identifier: MPL-2.0

//! Shared session-event mapping without IPC, persistence or live-control effects.
//! A durable consumer stages a clone, commits its output, then installs it.
use koru_ai_core::{
    AgentActivityEvent, AgentToolFailure, AgentToolKind, AiError, AiErrorClass, AiEvent, AiUsage,
    ChoiceIntent, InferenceAttemptId, OfferedChoice, Timestamp, ToolInvocationId, UsageSource,
    bound_agent_content, bound_live_text,
};
use koru_session_protocol::{
    AgentEvent, AgentTurnId, AgentUsage, ApprovalChoiceId, ApprovalDecision, ApprovalId,
    ApprovalIntent, QuestionRequestId, ToolCallId, ToolFailure, ToolKind,
};
use std::collections::HashMap;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentSource {
    Tool(ToolCallId),
    Approval(ApprovalId),
    Questions(QuestionRequestId),
}
impl AgentSource {
    pub fn of(event: &AgentEvent) -> Option<Self> {
        Some(match event {
            AgentEvent::ToolStarted { tool_call_id, .. }
            | AgentEvent::ToolCompleted { tool_call_id, .. } => Self::Tool(*tool_call_id),
            AgentEvent::ApprovalRequired { approval_id, .. }
            | AgentEvent::ApprovalResolved { approval_id, .. }
            | AgentEvent::ApprovalExpired { approval_id, .. } => Self::Approval(*approval_id),
            AgentEvent::QuestionsRequired { request_id, .. }
            | AgentEvent::QuestionsResolved { request_id, .. } => Self::Questions(*request_id),
            _ => return None,
        })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UsageDelivery {
    AtTerminal,
    EachEvent,
}

#[derive(Clone)]
pub enum AgentLiveAction {
    OfferApproval {
        invocation: ToolInvocationId,
        approval: ApprovalId,
        choices: Vec<(ApprovalChoiceId, ApprovalDecision)>,
    },
    SettleApproval {
        approval: ApprovalId,
    },
    OfferQuestions {
        invocation: ToolInvocationId,
        request: QuestionRequestId,
        questions: Vec<koru_session_protocol::AgentQuestion>,
        next_message: bool,
    },
    SettleQuestions {
        request: QuestionRequestId,
    },
    ForgetNextMessage {
        invocation: ToolInvocationId,
    },
}
#[derive(Default)]
pub struct MappedAgentEvent {
    pub events: Vec<AiEvent>,
    pub sources: Vec<(AgentSource, ToolInvocationId)>,
    pub live: Vec<AgentLiveAction>,
}
#[derive(Clone)]
pub struct AgentEventMapper {
    attempt: InferenceAttemptId,
    turn: AgentTurnId,
    tools: HashMap<ToolCallId, ToolInvocationId>,
    approvals: HashMap<ApprovalId, ToolInvocationId>,
    question_rows: HashMap<QuestionRequestId, ToolInvocationId>,
    usage: Option<AiUsage>,
    usage_delivery: UsageDelivery,
    started_in: Option<String>,
}
impl AgentEventMapper {
    pub fn new(
        attempt: InferenceAttemptId,
        turn: AgentTurnId,
        started_in: Option<String>,
        usage_delivery: UsageDelivery,
    ) -> Self {
        Self {
            attempt,
            turn,
            started_in,
            usage_delivery,
            tools: HashMap::new(),
            approvals: HashMap::new(),
            question_rows: HashMap::new(),
            usage: None,
        }
    }
    pub fn seed(&mut self, source: AgentSource, invocation: ToolInvocationId) {
        match source {
            AgentSource::Tool(id) => {
                self.tools.insert(id, invocation);
            }
            AgentSource::Approval(id) => {
                self.approvals.insert(id, invocation);
            }
            AgentSource::Questions(id) => {
                self.question_rows.insert(id, invocation);
            }
        }
    }
    pub fn pending_sources(&self) -> usize {
        self.tools.len() + self.approvals.len() + self.question_rows.len()
    }
    pub fn queue_terminal(&mut self, events: &mut Vec<AiEvent>, terminal: AiEvent) {
        if let Some(usage) = self.usage.take() {
            events.push(AiEvent::Usage {
                attempt_id: self.attempt,
                usage,
            });
        }
        events.push(terminal);
    }
    pub fn apply(
        &mut self,
        event: AgentEvent,
        at: Timestamp,
        cancelling: bool,
    ) -> MappedAgentEvent {
        let source = AgentSource::of(&event);
        let mut mapped = MappedAgentEvent::default();
        let attempt_id = self.attempt;
        match event {
            AgentEvent::TurnStarted { turn_id } if turn_id == self.turn => {}
            AgentEvent::ContextUsage { used, size } if size > 0 => {
                mapped.events.push(AiEvent::ContextUsage {
                    attempt_id,
                    used,
                    size,
                });
            }
            AgentEvent::MessageDelta { turn_id, text } if turn_id == self.turn => {
                mapped.events.push(AiEvent::TextDelta {
                    attempt_id,
                    text,
                    at,
                });
            }
            AgentEvent::ToolStarted {
                turn_id,
                tool_call_id,
                label,
                input,
            } if turn_id == self.turn => {
                let invocation = *self
                    .tools
                    .entry(tool_call_id)
                    .or_insert_with(ToolInvocationId::new);
                mapped.events.push(AiEvent::AgentActivity {
                    attempt_id,
                    activity: AgentActivityEvent::ToolStarted {
                        invocation,
                        label: bound_live_text(&label),
                        input: input.as_deref().and_then(bound_agent_content),
                    },
                    at,
                });
            }
            AgentEvent::ToolCompleted {
                turn_id,
                tool_call_id,
                succeeded,
                tool,
                failure,
                detail,
                output,
            } if turn_id == self.turn => {
                let invocation = match self.tools.remove(&tool_call_id) {
                    Some(invocation) => invocation,
                    None => {
                        // A completion for a start this turn never saw:
                        // give it a row anyway, so the outcome is kept.
                        let invocation = ToolInvocationId::new();
                        mapped.events.push(AiEvent::AgentActivity {
                            attempt_id,
                            activity: AgentActivityEvent::ToolStarted {
                                invocation,
                                label: String::new(),
                                input: None,
                            },
                            at,
                        });
                        invocation
                    }
                };
                mapped.events.push(AiEvent::AgentActivity {
                    attempt_id,
                    activity: AgentActivityEvent::ToolCompleted {
                        invocation,
                        tool: agent_tool_kind(tool),
                        succeeded,
                        failure: failure.map(|failure| match failure {
                            ToolFailure::Denied => AgentToolFailure::Denied,
                            ToolFailure::Cancelled => AgentToolFailure::Cancelled,
                            ToolFailure::TimedOut => AgentToolFailure::TimedOut,
                            ToolFailure::Errored => AgentToolFailure::Errored,
                            ToolFailure::Unclassified => AgentToolFailure::Unclassified,
                        }),
                        detail: detail.as_deref().map(bound_live_text),
                        output: output.as_deref().and_then(bound_agent_content),
                    },
                    at,
                });
            }
            AgentEvent::ApprovalRequired {
                turn_id,
                approval_id,
                summary,
                choices,
            } if turn_id == self.turn => {
                let invocation = *self
                    .approvals
                    .entry(approval_id)
                    .or_insert_with(ToolInvocationId::new);
                mapped.live.push(AgentLiveAction::OfferApproval {
                    invocation,
                    approval: approval_id,
                    choices: choices
                        .iter()
                        .map(|choice| (choice.id, choice.intent.decision()))
                        .collect(),
                });
                mapped.events.push(AiEvent::AgentActivity {
                    attempt_id,
                    activity: AgentActivityEvent::ApprovalRequired {
                        invocation,
                        summary: bound_live_text(&summary),
                        choices: choices
                            .into_iter()
                            .map(|choice| OfferedChoice {
                                id: choice.id.to_string(),
                                intent: choice_intent(choice.intent),
                                label: bound_live_text(&choice.label),
                            })
                            .collect(),
                    },
                    at,
                });
            }
            AgentEvent::ApprovalResolved {
                turn_id,
                approval_id,
                decision,
                ..
            } if turn_id == self.turn => {
                mapped.live.push(AgentLiveAction::SettleApproval {
                    approval: approval_id,
                });
                if let Some(invocation) = self.approvals.remove(&approval_id) {
                    mapped.events.push(AiEvent::AgentActivity {
                        attempt_id,
                        activity: AgentActivityEvent::ApprovalResolved {
                            invocation,
                            approved: decision == ApprovalDecision::Approve,
                        },
                        at,
                    });
                }
            }
            AgentEvent::ApprovalExpired {
                turn_id,
                approval_id,
            } if turn_id == self.turn => {
                mapped.live.push(AgentLiveAction::SettleApproval {
                    approval: approval_id,
                });
                if let Some(invocation) = self.approvals.remove(&approval_id) {
                    mapped.events.push(AiEvent::AgentActivity {
                        attempt_id,
                        activity: AgentActivityEvent::ApprovalExpired { invocation },
                        at,
                    });
                }
            }
            AgentEvent::Usage { turn_id, usage } if turn_id == self.turn => {
                // The adapter reports the turn's total so far, replacing the
                // last; one usage event reaches the chat, at the end.
                if self.usage_delivery == UsageDelivery::EachEvent {
                    mapped.events.push(AiEvent::Usage {
                        attempt_id,
                        usage: ai_usage(&usage),
                    });
                } else {
                    self.usage = Some(ai_usage(&usage));
                }
            }
            AgentEvent::TurnCompleted { turn_id } if turn_id == self.turn => {
                self.queue_terminal(
                    &mut mapped.events,
                    AiEvent::Completed {
                        attempt_id,
                        stop_reason: None,
                        at,
                    },
                );
            }
            AgentEvent::TurnCancelled { turn_id, .. } if turn_id == self.turn => {
                self.queue_terminal(&mut mapped.events, AiEvent::Cancelled { attempt_id, at });
            }
            AgentEvent::TurnFailed { turn_id, .. } if turn_id == self.turn => {
                let terminal = if cancelling {
                    AiEvent::Cancelled { attempt_id, at }
                } else {
                    AiEvent::Failed {
                        attempt_id,
                        error: AiError::new(
                            AiErrorClass::Provider,
                            "The agent could not finish this turn.",
                            true,
                        ),
                        at,
                    }
                };
                self.queue_terminal(&mut mapped.events, terminal);
            }
            AgentEvent::ModeChanged { turn_id, mode } if turn_id == self.turn => {
                mapped.events.push(AiEvent::AgentActivity {
                    attempt_id,
                    activity: AgentActivityEvent::ModeChanged {
                        started_in: self.started_in.clone(),
                        mode: mode.as_str().to_owned(),
                    },
                    at,
                });
            }
            // A status line, not the reply (D69): kept beside it.
            AgentEvent::Notice { turn_id, text } if turn_id == self.turn => {
                mapped.events.push(AiEvent::AgentActivity {
                    attempt_id,
                    activity: AgentActivityEvent::Notice { text },
                    at,
                });
            }
            // The agent stopped to ask (D108): a row of its own, filed as a
            // permission request, whose questions are live until answered.
            AgentEvent::QuestionsRequired {
                turn_id,
                request_id,
                questions,
                blocking,
                delivery,
            } if turn_id == self.turn => {
                let invocation = *self
                    .question_rows
                    .entry(request_id)
                    .or_insert_with(ToolInvocationId::new);
                let next_message =
                    delivery == koru_session_protocol::QuestionsDelivery::NextMessage;
                self.question_rows.insert(request_id, invocation);
                mapped.live.push(AgentLiveAction::OfferQuestions {
                    invocation,
                    request: request_id,
                    questions: questions.clone(),
                    next_message,
                });
                mapped.events.push(AiEvent::AgentActivity {
                    attempt_id,
                    activity: AgentActivityEvent::QuestionsAsked {
                        invocation,
                        questions: questions.into_iter().map(chat_question).collect(),
                        blocking,
                        next_message,
                    },
                    at,
                });
            }
            // A set this turn asked, or one whose answers this turn carries.
            AgentEvent::QuestionsResolved {
                turn_id,
                request_id,
                outcome,
            } if turn_id == self.turn || self.question_rows.contains_key(&request_id) => {
                mapped.live.push(AgentLiveAction::SettleQuestions {
                    request: request_id,
                });
                if let Some(invocation) = self.question_rows.remove(&request_id) {
                    mapped
                        .live
                        .push(AgentLiveAction::ForgetNextMessage { invocation });
                    use koru_session_protocol::QuestionsOutcome;
                    let activity = match outcome {
                        QuestionsOutcome::Answered { answers } => {
                            AgentActivityEvent::QuestionsAnswered {
                                invocation,
                                answers: Some(answers.into_iter().map(chat_answer).collect()),
                            }
                        }
                        QuestionsOutcome::Skipped => AgentActivityEvent::QuestionsAnswered {
                            invocation,
                            answers: None,
                        },
                        QuestionsOutcome::Expired => {
                            AgentActivityEvent::ApprovalExpired { invocation }
                        }
                    };
                    mapped.events.push(AiEvent::AgentActivity {
                        attempt_id,
                        activity,
                        at,
                    });
                }
            }
            // Another turn's events, provider state, attachments, children:
            // counted, never surfaced.
            _ => {}
        }
        if let Some(source) = source {
            for event in &mapped.events {
                if let Some(invocation) = invocation_of(event)
                    && !mapped.sources.iter().any(|(_, id)| *id == invocation)
                {
                    mapped.sources.push((source, invocation));
                }
            }
        }
        mapped
    }
}

fn invocation_of(event: &AiEvent) -> Option<ToolInvocationId> {
    match event {
        AiEvent::AgentActivity { activity, .. } => match activity {
            AgentActivityEvent::ToolStarted { invocation, .. }
            | AgentActivityEvent::ToolCompleted { invocation, .. }
            | AgentActivityEvent::ApprovalRequired { invocation, .. }
            | AgentActivityEvent::ApprovalResolved { invocation, .. }
            | AgentActivityEvent::ApprovalExpired { invocation }
            | AgentActivityEvent::QuestionsAsked { invocation, .. }
            | AgentActivityEvent::QuestionsAnswered { invocation, .. } => Some(*invocation),
            _ => None,
        },
        _ => None,
    }
}

/// A question as the chat keeps it: the agent's words, its options in its
/// order (D108).
fn chat_question(question: koru_session_protocol::AgentQuestion) -> koru_ai_core::AgentQuestion {
    koru_ai_core::AgentQuestion {
        id: question.id,
        header: question.header,
        question: question.question,
        options: question
            .options
            .into_iter()
            .map(|option| koru_ai_core::AgentQuestionOption {
                label: option.label,
                description: option.description,
            })
            .collect(),
        allow_other: question.allow_other,
        secret: question.secret,
    }
}

/// An answer as the chat keeps it; a secret's arrives withheld.
fn chat_answer(answer: koru_session_protocol::QuestionAnswer) -> koru_ai_core::AgentAnswer {
    use koru_ai_core::AgentAnswerValue;
    use koru_session_protocol::QuestionAnswerValue;
    koru_ai_core::AgentAnswer {
        question_id: answer.question_id,
        value: match answer.value {
            QuestionAnswerValue::Option { label } => AgentAnswerValue::Option { label },
            QuestionAnswerValue::Text { text } => AgentAnswerValue::Text { text },
            QuestionAnswerValue::Withheld => AgentAnswerValue::Withheld,
        },
    }
}

fn agent_tool_kind(kind: Option<ToolKind>) -> AgentToolKind {
    match kind {
        Some(ToolKind::CommandExecution) => AgentToolKind::Command,
        Some(ToolKind::FileChange) => AgentToolKind::FileChange,
        Some(ToolKind::WebSearch) => AgentToolKind::Search,
        Some(ToolKind::McpTool) => AgentToolKind::Mcp,
        Some(_) | None => AgentToolKind::Other,
    }
}

fn choice_intent(intent: ApprovalIntent) -> ChoiceIntent {
    match intent {
        ApprovalIntent::Allow => ChoiceIntent::AllowOnce,
        ApprovalIntent::AllowRemembered => ChoiceIntent::AllowAlways,
        ApprovalIntent::Reject => ChoiceIntent::RejectOnce,
        ApprovalIntent::RejectRemembered => ChoiceIntent::RejectAlways,
    }
}

fn ai_usage(usage: &AgentUsage) -> AiUsage {
    let reported = usage.input_tokens.is_some() || usage.output_tokens.is_some();
    AiUsage {
        prompt_tokens: usage.input_tokens,
        cached_prompt_tokens: usage.cached_input_tokens,
        completion_tokens: usage.output_tokens,
        reasoning_tokens: None,
        total_tokens: match (usage.input_tokens, usage.output_tokens) {
            (Some(input), Some(output)) => Some(input.saturating_add(output)),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        },
        source: if reported {
            UsageSource::ProviderReported
        } else {
            UsageSource::Unavailable
        },
    }
}
