// SPDX-License-Identifier: MPL-2.0
//! Engine-owned requests rendered by the shared question form and action card.
use super::*;
use koru_ai_wire::bot_threads as wire;
use koru_i18n::{Key as Words, t};
use questions::input::{Actions, Effect};
use uuid::Uuid;

pub(crate) struct RequestCard {
    id: Uuid,
    prompt: wire::Prompt,
    answer: Option<wire::Answer>,
    open: bool,
    available: bool,
    before: Option<MessageId>,
    sequence: u64,
    form: Option<(Vec<koru_ai_core::AgentQuestion>, questions::Card)>,
    cursor: usize,
    pending: Option<wire::Command>,
    retry: Option<Instant>,
    settled: bool,
    pub top: f32,
    pub height: f32,
}

impl RequestCard {
    fn labels(&self, lang: koru_i18n::Lang) -> Vec<(String, bool)> {
        if !self.open {
            return vec![];
        }
        if !self.available {
            return vec![(t(lang, Words::BotsEngineAskAgain).into(), true)];
        }
        let wire::Prompt::Approval { choices, .. } = &self.prompt else {
            return vec![];
        };
        if choices.is_empty() {
            return vec![
                (t(lang, Words::AiAgentAllow).into(), true),
                (t(lang, Words::AiAgentReject).into(), false),
            ];
        }
        choices
            .iter()
            .map(|choice| {
                let key = match choice.intent {
                    wire::ApprovalIntent::Allow => Words::AiAgentAllowOnce,
                    wire::ApprovalIntent::AllowRemembered => Words::AiAgentAllowAlways,
                    wire::ApprovalIntent::Reject => Words::AiAgentReject,
                    wire::ApprovalIntent::RejectRemembered => Words::AiAgentRejectAlways,
                };
                (
                    if choice.label.trim().is_empty() {
                        t(lang, key).into()
                    } else {
                        choice.label.clone()
                    },
                    choice.intent.grants(),
                )
            })
            .collect()
    }
    fn heading(&self, lang: koru_i18n::Lang) -> &'static str {
        if let Some(wire::Answer::Approval { decision, .. }) = &self.answer {
            return t(
                lang,
                match decision {
                    wire::ApprovalDecision::Approve => Words::AiAgentOutcomeAllowed,
                    wire::ApprovalDecision::Reject => Words::AiAgentOutcomeRejected,
                },
            );
        }
        t(
            lang,
            if self.answer.is_some() {
                Words::AiQuestionsAnswered
            } else if matches!(self.prompt, wire::Prompt::Approval { .. }) {
                Words::AiAgentAskedPermission
            } else {
                Words::AiQuestionsTitle
            },
        )
    }
    fn body(&self, lang: koru_i18n::Lang) -> String {
        if self.open && !self.available {
            return t(lang, Words::BotsEngineQuestionLost).into();
        }
        match &self.prompt {
            wire::Prompt::Approval { summary, .. } => agents::summary_words(lang, summary),
            wire::Prompt::Questions { questions, .. } => questions
                .iter()
                .map(|question| {
                    let answer = match &self.answer {
                        Some(wire::Answer::Questions {
                            answers: Some(answers),
                        }) => answers
                            .iter()
                            .find(|a| a.question_id == question.id)
                            .map(|answer| {
                                if question.secret {
                                    return t(lang, Words::AiQuestionsWithheld).to_owned();
                                }
                                match &answer.value {
                                    wire::QuestionAnswerValue::Option { label } => label.clone(),
                                    wire::QuestionAnswerValue::Text { text } => text.clone(),
                                    wire::QuestionAnswerValue::Withheld => {
                                        t(lang, Words::AiQuestionsWithheld).into()
                                    }
                                }
                            }),
                        Some(wire::Answer::Questions { answers: None }) => {
                            Some(t(lang, Words::AiQuestionsSkipped).into())
                        }
                        _ => None,
                    };
                    answer.map_or_else(
                        || question.question.clone(),
                        |answer| format!("{}\n{}", question.question, answer),
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n"),
        }
    }
}

impl ChatSurface {
    pub(crate) fn rebuild_managed_notices(&mut self, width: f32) {
        let Some(state) = &self.managed else {
            return;
        };
        for message in state
            .messages
            .values()
            .filter(|message| message.kind == "setup_note")
        {
            if !message.content.references.iter().any(|r| {
                r.get("notice").and_then(|value| value.as_str())
                    == Some("secret_answer_not_delivered")
            }) {
                continue;
            }
            let anchor = state
                .messages
                .values()
                .find(|next| {
                    next.sequence > message.sequence
                        && next.kind == "words"
                        && !next.content.body.is_empty()
                })
                .map_or(message.id, |next| next.id);
            self.notice_layouts.push(NoticeLayout {
                message: MessageId::from_uuid(anchor),
                top: f32::NAN,
                layout: self.engine.simple_layout_wrapped(
                    t(self.lang, Words::BotsEngineSecretLost),
                    role::meta(),
                    width,
                ),
            });
        }
    }

    pub(crate) fn rebuild_managed_cards(&mut self, width: f32) {
        let Some(state) = &mut self.managed else {
            self.managed_cards.clear();
            return;
        };
        if !state.requests_dirty && (self.message_width - width).abs() <= 0.5 {
            return;
        }
        state.requests_dirty = false;
        let mut rows = Vec::new();
        for question in &state.questions {
            let before = state
                .snapshot
                .turns
                .iter()
                .find(|turn| turn.id == question.turn)
                .and_then(|turn| turn.reply_message)
                .map(MessageId::from_uuid);
            rows.push((
                question.id,
                question.prompt.clone(),
                question.answer.clone(),
                question.state == "open",
                question.provider_available || !matches!(&question.prompt, wire::Prompt::Questions { questions, .. } if questions.iter().any(|question| question.secret)),
                before,
                u64::MAX,
            ));
        }
        // Stored question/answer records are presentation data. Never invent
        // an ordinary ToolActivity or inference attempt to draw them.
        for message in state.messages.values().filter(|m| m.kind == "setup_note") {
            let Some(reference) = message.content.references.first() else {
                continue;
            };
            let Some(id) = reference
                .get("question")
                .and_then(|value| value.as_str())
                .and_then(|id| Uuid::parse_str(id).ok())
            else {
                continue;
            };
            let Some(prompt) = reference
                .get("prompt")
                .and_then(|p| serde_json::from_value::<wire::Prompt>(p.clone()).ok())
            else {
                continue;
            };
            if let Some(row) = rows.iter_mut().find(|row| row.0 == id) {
                row.6 = message.sequence;
                continue;
            }
            let answer = state
                .messages
                .values()
                .flat_map(|m| &m.content.references)
                .find_map(|r| {
                    (r.get("question").and_then(|v| v.as_str()) == Some(id.to_string().as_str()))
                        .then(|| {
                            r.get("answer").and_then(|a| {
                                serde_json::from_value::<wire::Answer>(a.clone()).ok()
                            })
                        })
                        .flatten()
                });
            let before = state
                .messages
                .values()
                .find(|m| {
                    m.sequence > message.sequence && m.kind == "words" && !m.content.body.is_empty()
                })
                .map(|m| MessageId::from_uuid(m.id));
            rows.push((id, prompt, answer, false, false, before, message.sequence));
        }
        self.managed_cards
            .retain(|id, _| rows.iter().any(|row| row.0 == *id));
        let mut first_new = None;
        for (id, prompt, answer, open, available, before, sequence) in rows {
            let card = self.managed_cards.entry(id).or_insert_with(|| {
                if open && available && first_new.is_none() {
                    first_new = Some(id);
                }
                RequestCard {
                    id,
                    prompt: prompt.clone(),
                    answer: None,
                    open,
                    available,
                    before,
                    sequence,
                    form: None,
                    cursor: 0,
                    pending: None,
                    retry: None,
                    settled: false,
                    top: 0.0,
                    height: 0.0,
                }
            });
            card.prompt = prompt;
            card.answer = answer.or_else(|| card.settled.then(|| card.answer.clone()).flatten());
            card.open = open && !card.settled;
            card.available = available;
            card.before = before;
            card.sequence = sequence;
            if let wire::Prompt::Questions { questions, .. } = &card.prompt {
                if card.open && available {
                    let data = questions
                        .iter()
                        .map(|q| koru_ai_core::AgentQuestion {
                            id: q.id.clone(),
                            header: q.header.clone(),
                            question: q.question.clone(),
                            options: q
                                .options
                                .iter()
                                .map(|o| koru_ai_core::AgentQuestionOption {
                                    label: o.label.clone(),
                                    description: o.description.clone(),
                                })
                                .collect(),
                            allow_other: q.allow_other,
                            secret: q.secret,
                        })
                        .collect::<Vec<_>>();
                    if card.form.as_ref().is_none_or(|(prior, _)| prior != &data) {
                        let mut form = questions::Card::new(&mut self.engine, &data);
                        form.sent = card.pending.is_some();
                        card.form = Some((data, form));
                    }
                } else {
                    card.form = None;
                }
            }
            card.height = if let Some((data, form)) = &card.form {
                questions::input::height(&mut self.engine, self.lang, id, data, form, width)
            } else {
                Actions {
                    id,
                    heading: card.heading(self.lang),
                    body: &card.body(self.lang),
                    labels: &card.labels(self.lang),
                    focused: None,
                    disabled: card.pending.is_some(),
                }
                .height(&mut self.engine, width)
            };
        }
        if self.focus == Focus::Composer
            && self.composer.is_blank()
            && let Some(id) = first_new
        {
            self.focus = Focus::ManagedQuestion(id);
            self.composer.set_focused(false);
        }
        if let Focus::ManagedQuestion(id) = self.focus
            && !self.managed_cards.get(&id).is_some_and(|card| card.open)
        {
            self.focus = Focus::Composer;
            self.composer.set_focused(true);
        }
    }

    pub(crate) fn place_managed_cards(&mut self, message: Option<MessageId>, mut top: f32) -> f32 {
        let mut ids = self
            .managed_cards
            .values()
            .filter(|card| card.top.is_nan() && (message.is_none() || card.before == message))
            .map(|card| (card.sequence, card.id))
            .collect::<Vec<_>>();
        ids.sort_unstable();
        for (_, id) in ids {
            let card = self.managed_cards.get_mut(&id).expect("collected above");
            card.top = top;
            top += card.height + CARD_GAP;
        }
        top
    }

    pub(crate) fn draw_managed_cards(&mut self, canvas: &mut Scene, root: Affine) {
        let area = self.conversation_rect();
        for card in self.managed_cards.values() {
            let row = Rect::new(
                area.x,
                area.y - self.conversation_scroll + card.top,
                area.w,
                card.height,
            );
            if row.y + row.h < area.y || row.y > area.y + area.h {
                continue;
            }
            let focused = self.focus == Focus::ManagedQuestion(card.id);
            if let Some((data, form)) = &card.form {
                questions::input::draw(
                    canvas,
                    root,
                    &mut self.engine,
                    self.lang,
                    card.id,
                    data,
                    form,
                    focused,
                    self.pointer,
                    row,
                );
            } else {
                Actions {
                    id: card.id,
                    heading: card.heading(self.lang),
                    body: &card.body(self.lang),
                    labels: &card.labels(self.lang),
                    focused: focused.then_some(card.cursor),
                    disabled: card.pending.is_some(),
                }
                .draw(canvas, root, &mut self.engine, row, self.pointer);
            }
        }
    }

    pub(crate) fn managed_question_press(&mut self, x: f32, y: f32) -> Option<bool> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        let card = self.managed_cards.values_mut().find(|card| {
            card.open
                && Rect::new(
                    area.x,
                    area.y - self.conversation_scroll + card.top,
                    area.w,
                    card.height,
                )
                .contains(x, y)
        })?;
        let id = card.id;
        let row = Rect::new(
            area.x,
            area.y - self.conversation_scroll + card.top,
            area.w,
            card.height,
        );
        self.focus = Focus::ManagedQuestion(id);
        self.composer.set_focused(false);
        let control = if let Some((data, form)) = &mut card.form {
            questions::input::press(&mut self.engine, self.lang, id, data, form, row, x, y)
        } else {
            Actions {
                id,
                heading: card.heading(self.lang),
                body: &card.body(self.lang),
                labels: &card.labels(self.lang),
                focused: Some(card.cursor),
                disabled: card.pending.is_some(),
            }
            .hit(&mut self.engine, row, x, y)
            .map(|index| questions::Control::Option(0, index))
        };
        Some(control.is_none_or(|control| self.activate_managed_question(id, control)))
    }

    pub(crate) fn managed_question_key(
        &mut self,
        id: Uuid,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        if matches!(key, Key::Named(NamedKey::Tab)) && !koru_keys::is_chord(mods) {
            let card = self.managed_cards.get(&id)?;
            let edge = if let Some((data, form)) = &card.form {
                let order = questions::controls(data);
                Some(&form.cursor)
                    == if mods.shift_key() {
                        order.first()
                    } else {
                        order.last()
                    }
            } else {
                card.cursor
                    == if mods.shift_key() {
                        0
                    } else {
                        card.labels(self.lang).len().saturating_sub(1)
                    }
            };
            if edge {
                return Some(self.step_managed_question(Some(id), !mods.shift_key()));
            }
        }
        let card = self.managed_cards.get_mut(&id)?;
        let effect = if let Some((data, form)) = &mut card.form {
            form.key(&mut self.engine, data, key, text, mods)
        } else {
            let count = card.labels(self.lang).len();
            match key {
                Key::Named(NamedKey::Escape) => Effect::Leave,
                Key::Named(NamedKey::Tab) if count > 0 && !koru_keys::is_chord(mods) => {
                    card.cursor = if mods.shift_key() {
                        (card.cursor + count - 1) % count
                    } else {
                        (card.cursor + 1) % count
                    };
                    Effect::Handled(true)
                }
                Key::Named(NamedKey::Enter | NamedKey::Space) if count > 0 && mods.is_empty() => {
                    Effect::Activate(questions::Control::Option(0, card.cursor))
                }
                _ if text.is_some_and(|text| !text.trim().is_empty())
                    && !koru_keys::is_chord(mods) =>
                {
                    Effect::Leave
                }
                _ => Effect::Handled(false),
            }
        };
        match effect {
            Effect::Handled(changed) => Some(changed),
            Effect::Activate(control) => Some(self.activate_managed_question(id, control)),
            Effect::Leave => {
                self.focus = Focus::Composer;
                self.composer.set_focused(true);
                matches!(key, Key::Named(NamedKey::Escape)).then_some(true)
            }
        }
    }

    fn activate_managed_question(&mut self, id: Uuid, control: questions::Control) -> bool {
        let Some(state) = &self.managed else {
            return false;
        };
        let thread = state.thread();
        let Some(card) = self.managed_cards.get_mut(&id) else {
            return false;
        };
        if card.pending.is_some() || !card.open {
            return true;
        }
        let request = Uuid::new_v4();
        let command = if !card.available {
            wire::Command::Reask {
                id: request,
                thread,
                question: id,
            }
        } else if let Some((data, form)) = &mut card.form {
            match control {
                questions::Control::Skip => wire::Command::Answer {
                    id: request,
                    question: id,
                    answer: wire::Answer::Questions { answers: None },
                },
                questions::Control::Submit => {
                    let Some(answers) = form.answers(data) else {
                        return true;
                    };
                    let answers = answers
                        .into_iter()
                        .map(|a| wire::QuestionAnswer {
                            question_id: a.question_id,
                            value: match a.value {
                                koru_ai_core::AgentAnswerValue::Option { label } => {
                                    wire::QuestionAnswerValue::Option { label }
                                }
                                koru_ai_core::AgentAnswerValue::Text { text } => {
                                    wire::QuestionAnswerValue::Text { text }
                                }
                                koru_ai_core::AgentAnswerValue::Withheld => {
                                    wire::QuestionAnswerValue::Withheld
                                }
                            },
                        })
                        .collect();
                    wire::Command::Answer {
                        id: request,
                        question: id,
                        answer: wire::Answer::Questions {
                            answers: Some(answers),
                        },
                    }
                }
                _ => {
                    form.choose(control);
                    return true;
                }
            }
        } else {
            let questions::Control::Option(_, index) = control else {
                return false;
            };
            let wire::Prompt::Approval { choices, .. } = &card.prompt else {
                return false;
            };
            let (decision, choice) = if choices.is_empty() {
                match index {
                    0 => (wire::ApprovalDecision::Approve, None),
                    1 => (wire::ApprovalDecision::Reject, None),
                    _ => return false,
                }
            } else {
                let Some(choice) = choices.get(index) else {
                    return false;
                };
                (choice.intent.decision(), Some(choice.id))
            };
            wire::Command::Answer {
                id: request,
                question: id,
                answer: wire::Answer::Approval { decision, choice },
            }
        };
        if !self.queue_command(Command::Engine {
            thread,
            command: command.clone(),
        }) {
            return true;
        }
        let card = self
            .managed_cards
            .get_mut(&id)
            .expect("command enqueue retains card");
        if let Some((_, form)) = &mut card.form {
            form.sent = true;
        }
        card.pending = Some(command);
        card.retry = None;
        self.focus = Focus::Composer;
        self.composer.set_focused(true);
        true
    }

    pub(crate) fn settle_managed_request(&mut self, thread: ChatId, reply: &wire::Response) {
        if self
            .managed
            .as_ref()
            .is_none_or(|state| state.thread() != thread)
        {
            return;
        }
        let request = match reply {
            wire::Response::Answered { request, .. } | wire::Response::Changed { request, .. } => {
                *request
            }
            _ => return,
        };
        for card in self.managed_cards.values_mut() {
            if card
                .pending
                .as_ref()
                .is_some_and(|command| request_id(command) == Some(request))
            {
                if let Some(wire::Command::Answer { mut answer, .. }) = card.pending.take() {
                    if let wire::Answer::Questions {
                        answers: Some(answers),
                    } = &mut answer
                        && let wire::Prompt::Questions { questions, .. } = &card.prompt
                    {
                        for answer in answers {
                            if questions
                                .iter()
                                .any(|q| q.id == answer.question_id && q.secret)
                            {
                                answer.value = wire::QuestionAnswerValue::Withheld;
                            }
                        }
                    }
                    card.answer = Some(answer);
                }
                card.retry = None;
                card.open = false;
                card.settled = true;
                // Drop the typed secret immediately after acceptance. The next
                // history read contains only the engine's withheld projection.
                card.form = None;
            }
        }
        if let Some(state) = &mut self.managed {
            state.requests_dirty = true;
        }
    }

    pub(crate) fn failed_managed_request(
        &mut self,
        thread: ChatId,
        command: &wire::Command,
        failure: &koru_ai_wire::Failure,
    ) -> bool {
        if self
            .managed
            .as_ref()
            .is_none_or(|state| state.thread() != thread)
        {
            return false;
        }
        let Some(request) = request_id(command) else {
            return false;
        };
        let Some(card) = self.managed_cards.values_mut().find(|card| {
            card.pending
                .as_ref()
                .is_some_and(|c| request_id(c) == Some(request))
        }) else {
            return false;
        };
        if failure.retryable {
            card.retry = Some(Instant::now() + Duration::from_secs(2));
        } else {
            card.pending = None;
            card.retry = None;
            if let Some((_, form)) = &mut card.form {
                form.sent = false;
            }
        }
        self.waiting = false;
        self.status = Some(self.failure_words(failure));
        true
    }

    pub(crate) fn maintain_managed_requests(&mut self, now: Instant) {
        let (Some(reader), Some(state)) = (&self.reader, &self.managed) else {
            return;
        };
        for card in self
            .managed_cards
            .values_mut()
            .filter(|card| card.retry.is_some_and(|at| now >= at))
        {
            if let Some(command) = card.pending.clone() {
                card.retry = if reader
                    .send(Command::Engine {
                        thread: state.thread(),
                        command,
                    })
                    .is_ok()
                {
                    None
                } else {
                    Some(now + Duration::from_secs(2))
                };
            }
        }
    }

    pub(crate) fn suspend_managed_requests(&mut self) {
        for card in self.managed_cards.values_mut() {
            if card.pending.is_some() {
                card.retry = Some(Instant::now());
            }
        }
    }

    pub(crate) fn step_managed_question(&mut self, from: Option<Uuid>, forward: bool) -> bool {
        let mut ids = self
            .managed_cards
            .values()
            .filter(|card| card.open)
            .map(|card| (card.sequence, card.id))
            .collect::<Vec<_>>();
        ids.sort_unstable();
        if ids.is_empty() {
            return false;
        }
        let next = match from.and_then(|id| ids.iter().position(|(_, key)| *key == id)) {
            Some(index) if forward => ids.get(index + 1),
            Some(index) => index.checked_sub(1).and_then(|index| ids.get(index)),
            None if forward => ids.first(),
            None => ids.last(),
        }
        .map(|(_, id)| *id);
        if let Some(id) = next {
            let card = self.managed_cards.get_mut(&id).expect("collected above");
            if let Some((data, form)) = &mut card.form {
                let order = questions::controls(data);
                form.cursor = *if forward { order.first() } else { order.last() }
                    .expect("question actions exist");
            } else {
                card.cursor = if forward {
                    0
                } else {
                    card.labels(self.lang).len().saturating_sub(1)
                };
            }
            self.focus = Focus::ManagedQuestion(id);
            self.composer.set_focused(false);
            self.conversation_scroll = card.top.max(0.0);
            self.follow_tail = false;
        } else if !(forward && self.focus_managed_attention(false)) {
            self.focus = Focus::Composer;
            self.composer.set_focused(true);
        }
        true
    }
}

fn request_id(command: &wire::Command) -> Option<Uuid> {
    match command {
        wire::Command::Answer { id, .. } | wire::Command::Reask { id, .. } => Some(*id),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
