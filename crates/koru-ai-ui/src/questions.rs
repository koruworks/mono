// SPDX-License-Identifier: MPL-2.0

//! **A local agent's questions, as a card in the reply** (koru-ai D108).
//!
//! An agent that stops to ask — Codex's `request_user_input`, Claude's
//! `AskUserQuestion` — reaches the chat as one row, filed as a permission
//! request, with its questions live. The chat draws it as a card in front of
//! its reply (where a permission request's card stands): a block a
//! question, its header as a small label, its words, its options as buttons
//! **in the agent's order** — the agent's first is the one it recommends —
//! each with its line under it, an *Other…* box where the agent takes the
//! person's own words, and a masked box for a secret. One choice a question:
//! an option, or the box's words. **Submit** is on once every question has
//! an answer and sends them all; **Skip** declines the set.
//!
//! While a set the agent waits on is open, the chat **needs you** (D83): its
//! row waits as a permission request does, so the amber dot is the one a
//! pending approval lights. The composer stays usable: a message sent then
//! is queued (D56) — the reply is still running — and goes once the reply
//! ends, after the set is answered or skipped. Steer still reaches the
//! running turn.
//!
//! **The keyboard**: the card takes it when a set arrives while the composer
//! is empty and has it, and on any press in the card. Tab and Shift+Tab walk
//! its options, boxes and actions; Enter or Space chooses an option or
//! presses an action — Enter in a box moves on, and submits from the last;
//! Escape gives the keyboard back to the composer, and so does a letter
//! typed while an option or an action has it, which is typed there.
//!
//! **After answering** the card is a summary, *You answered*, a line a
//! question — the answer's words, a secret as *(hidden)*. The row keeps the
//! questions and the answers (`agent_input`, `agent_output`), so the summary
//! reads the same after a reload; a skipped set, or one its turn closed,
//! says so in one line.
//!
//! **A set answered by the next message** — one the agent asked in its reply
//! and moved on from, Codex's `request_user_input_async` — is the same card,
//! and stays answerable after its reply ends. *Submit* then sends the
//! answers as the person's next message: a reply of its own, which follows
//! with no card of the person's — the answers are read here, as *You
//! answered*. While its reply still runs, *Submit* answers into it as any
//! set does. *Skip* sends the agent nothing. Another message passes it
//! over, and so does the end of the agent's session: it then reads *Passed
//! over*, and can no longer be answered.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use koru_ai_core::{
    AgentAnswer, AgentAnswerValue, AgentQuestion, AgentToolKind, AnswersRecord, QuestionsRecord,
    ToolActivity, ToolActivityStatus, ToolApproval, ToolInvocationId,
};
use koru_draw::el::{FrameCx, Interaction, Key as ElKey};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_i18n::{Key, Lang, t};
use koru_ui::Typed;
use koru_ui::button::{ActionButton, ActionStyle};
use koru_ui::text_input::{InputStyle, TextInput};
use vello::Scene;
use vello::kurbo::Affine;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

use crate::{ChatSurface, Focus, reader::Command};

mod elements;
mod tokens;

/// **A question's answer box**, shared with the leaf that draws it: an
/// element leaf owns what it paints, and the box is the card's state.
#[derive(Clone)]
pub(crate) struct AnswerInput(Rc<RefCell<TextInput>>);

impl AnswerInput {
    fn new(engine: &mut TextEngine, secret: bool) -> Self {
        let input = TextInput::new(engine, koru_draw::theme::role::row());
        Self(Rc::new(RefCell::new(if secret {
            input.secret()
        } else {
            input
        })))
    }

    fn value(&self) -> String {
        self.0.borrow().value()
    }

    /// What is typed, untouched: a secret's answer.
    fn raw(&self) -> String {
        self.0.borrow().raw_text().to_owned()
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        style: InputStyle,
        placeholder: &str,
        focused: bool,
    ) {
        self.0
            .borrow_mut()
            .draw(scene, root, engine, at, style, Some(placeholder), focused);
    }
}

/// What a question is answered with on the card.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pick {
    Option(usize),
    /// The words in its box.
    Words,
}

/// One part of the card the keyboard can be on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Control {
    Option(usize, usize),
    Field(usize),
    Skip,
    Submit,
}

/// One waiting set's card: what is chosen and typed, and where the keyboard
/// is. Kept by the row's id while the set waits.
pub(crate) struct Card {
    picks: Vec<Option<Pick>>,
    inputs: Vec<Option<AnswerInput>>,
    pub(crate) cursor: Control,
    /// Sent and not yet closed: the actions are inert meanwhile.
    pub(crate) sent: bool,
}

impl Card {
    fn new(engine: &mut TextEngine, questions: &[AgentQuestion]) -> Self {
        Self {
            picks: vec![None; questions.len()],
            inputs: questions
                .iter()
                .map(|question| {
                    question
                        .takes_text()
                        .then(|| AnswerInput::new(engine, question.secret))
                })
                .collect(),
            cursor: controls(questions)
                .first()
                .copied()
                .unwrap_or(Control::Skip),
            sent: false,
        }
    }

    /// **Every question's answer, or `None` while one has none**: its chosen
    /// option, or the words in its box — chosen by typing them, or the only
    /// way a question with no options is answered. A secret is kept as
    /// typed; other words are trimmed.
    pub(crate) fn answers(&self, questions: &[AgentQuestion]) -> Option<Vec<AgentAnswer>> {
        questions
            .iter()
            .enumerate()
            .map(|(index, question)| {
                let words = || {
                    let input = self.inputs.get(index)?.as_ref()?;
                    // A secret is sent exactly as typed; the box's value is
                    // trimmed.
                    let typed = if question.secret {
                        input.raw()
                    } else {
                        input.value().trim().to_owned()
                    };
                    (!typed.is_empty()).then_some(AgentAnswerValue::Text { text: typed })
                };
                let value = match self.picks.get(index).copied().flatten() {
                    Some(Pick::Option(option)) => AgentAnswerValue::Option {
                        label: question.options.get(option)?.label.clone(),
                    },
                    Some(Pick::Words) => words()?,
                    None if question.options.is_empty() => words()?,
                    None => return None,
                };
                Some(AgentAnswer {
                    question_id: question.id.clone(),
                    value,
                })
            })
            .collect()
    }
}

/// The card's controls in Tab order: each question's options, then its
/// box; then *Skip* and *Submit*.
pub(crate) fn controls(questions: &[AgentQuestion]) -> Vec<Control> {
    let mut controls = Vec::new();
    for (index, question) in questions.iter().enumerate() {
        controls.extend((0..question.options.len()).map(|option| Control::Option(index, option)));
        if question.takes_text() {
            controls.push(Control::Field(index));
        }
    }
    controls.extend([Control::Skip, Control::Submit]);
    controls
}

/// A control's element key: the row's id, the question and the option —
/// never a label or a place in a list the agent could reorder.
fn control_key(tool: ToolInvocationId, control: Control) -> ElKey {
    let id = |part: &str| crate::model_picker::stable_id(&format!("{tool}/{part}"));
    match control {
        Control::Option(question, option) => {
            ElKey::model(id(&format!("{question}/{option}")), "question-option")
        }
        Control::Field(question) => ElKey::model(id(&question.to_string()), "question-field"),
        Control::Skip => ElKey::model(id("skip"), "questions-skip"),
        Control::Submit => ElKey::model(id("submit"), "questions-submit"),
    }
}

/// **Whether a row is a set of an agent's questions**: it has them live,
/// or keeps their record (a reload, or after it closed).
pub(crate) fn is_question_row(activity: &ToolActivity) -> bool {
    activity.tool_name == AgentToolKind::Approval.as_token()
        && (waiting(activity).is_some()
            || activity
                .agent_input
                .as_deref()
                .and_then(QuestionsRecord::decode)
                .is_some())
}

/// **Whether a row is a set answered by the next message** (D108), as its
/// record says.
pub(crate) fn is_next_message(activity: &ToolActivity) -> bool {
    activity
        .agent_input
        .as_deref()
        .and_then(QuestionsRecord::decode)
        .is_some_and(|record| record.next_message)
}

/// The questions of a set still waiting for the person; `None` once it was
/// answered, skipped or closed, and on every other row.
pub(crate) fn waiting(activity: &ToolActivity) -> Option<&[AgentQuestion]> {
    let questions = activity.live.as_ref()?.questions.as_slice();
    (!questions.is_empty()
        && activity.approval == ToolApproval::Pending
        && !activity.status.is_terminal())
    .then_some(questions)
}

/// **What a closed set says**: *You answered* and a line a question — its
/// header or its words, and the answer — or one line for a set skipped or
/// closed unanswered.
pub(crate) fn summary(activity: &ToolActivity, lang: Lang) -> elements::SummarySpec {
    let questions = activity
        .agent_input
        .as_deref()
        .and_then(QuestionsRecord::decode)
        .unwrap_or_default();
    let answers = activity
        .agent_output
        .as_deref()
        .and_then(AnswersRecord::decode);
    match (activity.status, answers) {
        (ToolActivityStatus::Complete, Some(answers)) => elements::SummarySpec {
            heading: t(lang, Key::AiQuestionsAnswered).to_owned(),
            lines: questions
                .questions
                .iter()
                .map(|question| {
                    let asked = if question.header.is_empty() {
                        question.question.as_str()
                    } else {
                        question.header.as_str()
                    };
                    let said = answers
                        .answers
                        .iter()
                        .find(|answer| answer.question_id == question.id)
                        .map(|answer| match &answer.value {
                            AgentAnswerValue::Option { label } => label.clone(),
                            AgentAnswerValue::Text { text } => text.clone(),
                            AgentAnswerValue::Withheld => {
                                t(lang, Key::AiQuestionsWithheld).to_owned()
                            }
                        })
                        .unwrap_or_else(|| t(lang, Key::AiQuestionsUnanswered).to_owned());
                    format!("{asked} — {said}")
                })
                .collect(),
        },
        (ToolActivityStatus::Denied, _) => elements::SummarySpec {
            heading: t(lang, Key::AiQuestionsSkipped).to_owned(),
            lines: Vec::new(),
        },
        // A set answered by the next message that closed unanswered was
        // passed over: another message, or its agent's session ended.
        _ if questions.next_message => elements::SummarySpec {
            heading: t(lang, Key::AiQuestionsPassedOver).to_owned(),
            lines: Vec::new(),
        },
        _ => elements::SummarySpec {
            heading: t(lang, Key::AiQuestionsClosed).to_owned(),
            lines: Vec::new(),
        },
    }
}

/// The card for a waiting set, as the tree draws it: `focused` is the
/// control the keyboard is on, when the card has it.
fn spec(
    engine: &mut TextEngine,
    lang: Lang,
    tool: ToolInvocationId,
    questions: &[AgentQuestion],
    card: &Card,
    focused: Option<Control>,
) -> elements::CardSpec {
    let blocks = questions
        .iter()
        .enumerate()
        .map(|(index, question)| elements::BlockSpec {
            header: question.header.clone(),
            question: question.question.clone(),
            options: question
                .options
                .iter()
                .enumerate()
                .map(|(option, offered)| elements::OptionSpec {
                    key: control_key(tool, Control::Option(index, option)),
                    label: offered.label.clone(),
                    line: offered.description.clone(),
                    chosen: card.picks[index] == Some(Pick::Option(option)),
                })
                .collect(),
            field: card.inputs[index]
                .as_ref()
                .map(|input| elements::FieldSpec {
                    key: control_key(tool, Control::Field(index)),
                    input: input.clone(),
                    placeholder: t(
                        lang,
                        if question.secret {
                            Key::AiQuestionsSecret
                        } else if question.options.is_empty() {
                            Key::AiQuestionsAnswer
                        } else {
                            Key::AiQuestionsOther
                        },
                    )
                    .to_owned(),
                    focused: focused == Some(Control::Field(index)),
                }),
        })
        .collect();
    let button = |engine: &mut TextEngine, control: Control, key: Key, primary: bool| {
        let label = t(lang, key).to_owned();
        let style = if primary {
            ActionStyle::Primary
        } else {
            ActionStyle::Secondary
        };
        elements::ButtonSpec {
            key: control_key(tool, control),
            width: ActionButton::new(&label, style, None).width(engine),
            label,
            primary,
            disabled: card.sent
                || (control == Control::Submit && card.answers(questions).is_none()),
            focused: focused == Some(control),
        }
    };
    elements::CardSpec {
        heading: t(lang, Key::AiQuestionsTitle).to_owned(),
        blocks,
        skip: button(engine, Control::Skip, Key::AiQuestionsSkip, false),
        submit: button(engine, Control::Submit, Key::AiQuestionsSubmit, true),
    }
}

/// **The height a question row's card takes** at `width`, the card made
/// for a set that has just arrived: a waiting set's card, or a closed set's
/// summary.
pub(crate) fn card_height(
    engine: &mut TextEngine,
    cards: &mut HashMap<ToolInvocationId, Card>,
    lang: Lang,
    activity: &ToolActivity,
    width: f32,
) -> f32 {
    match waiting(activity) {
        Some(questions) => {
            let card = cards
                .entry(activity.id)
                .or_insert_with(|| Card::new(engine, questions));
            // A set is fixed once asked; a card that does not fit it is made
            // again rather than read out of step.
            if card.picks.len() != questions.len() {
                *card = Card::new(engine, questions);
            }
            let spec = spec(engine, lang, activity.id, questions, card, None);
            elements::height(&elements::tree(&spec), width)
        }
        None => elements::height(&elements::summary_tree(&summary(activity, lang)), width),
    }
}

/// **Draw a question row's card** in `row`: a waiting set's, lit under the
/// pointer and ringed where the keyboard is, or a closed set's summary.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_card(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    cards: &HashMap<ToolInvocationId, Card>,
    lang: Lang,
    pointer: (f32, f32),
    focused: Option<Control>,
    row: Rect,
    activity: &ToolActivity,
) {
    let tree = match (waiting(activity), cards.get(&activity.id)) {
        (Some(questions), Some(card)) => {
            elements::tree(&spec(engine, lang, activity.id, questions, card, focused))
        }
        _ => elements::summary_tree(&summary(activity, lang)),
    };
    let hot = elements::layout(&tree, row, FrameCx::root()).hit_at(pointer.0, pointer.1);
    let frame = elements::layout(
        &tree,
        row,
        FrameCx::root().with_interaction(Interaction {
            hot,
            pressed: None,
            focused: focused.map(|control| control_key(activity.id, control)),
        }),
    );
    frame.paint(&tree, canvas, root, engine);
}

impl ChatSurface {
    /// The waiting question rows on screen, with their rectangles.
    fn question_rows(&self) -> Vec<(Rect, ToolActivity)> {
        let area = self.conversation_hit_rect;
        self.tool_activity_layouts
            .iter()
            .filter(|tool| waiting(&tool.activity).is_some())
            .map(|tool| {
                (
                    Rect::new(
                        area.x,
                        area.y - self.conversation_scroll + tool.top,
                        area.w,
                        tool.expanded_height,
                    ),
                    tool.activity.clone(),
                )
            })
            .collect()
    }

    /// The control of a waiting card at `x`, `y`, from its frame — the
    /// rectangles paint uses — with the card's row and its rectangle.
    fn question_control_at(
        &mut self,
        x: f32,
        y: f32,
    ) -> Option<(ToolActivity, Rect, Option<Control>)> {
        let area = self.conversation_hit_rect;
        if !area.contains(x, y) {
            return None;
        }
        let (row, activity) = self
            .question_rows()
            .into_iter()
            .find(|(row, _)| row.contains(x, y))?;
        let questions = waiting(&activity)?;
        let card = self.question_cards.get(&activity.id)?;
        let spec = spec(
            &mut self.engine,
            self.lang,
            activity.id,
            questions,
            card,
            None,
        );
        let frame = elements::layout(&elements::tree(&spec), row, FrameCx::root());
        let key = frame.hit_at(x, y);
        let control = key.and_then(|key| {
            controls(questions)
                .into_iter()
                .find(|control| control_key(activity.id, *control) == key)
        });
        let field = control.and_then(|control| match control {
            Control::Field(_) => frame
                .node(control_key(activity.id, control))
                .map(|node| node.rect),
            _ => None,
        });
        Some((activity, field.unwrap_or(row), control))
    }

    /// **A press on a waiting card**: an option is chosen, a box takes the
    /// caret, an action acts; anywhere on the card the keyboard moves to it.
    /// `None` when the press is not on one.
    pub(crate) fn question_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        let (activity, rect, control) = self.question_control_at(x, y)?;
        let tool = activity.id;
        self.focus = Focus::Questions(tool);
        self.composer.set_focused(false);
        let Some(control) = control else {
            return Some(true);
        };
        if let Some(card) = self.question_cards.get_mut(&tool) {
            card.cursor = control;
        }
        Some(match control {
            Control::Field(question) => {
                if let Some(input) = self
                    .question_cards
                    .get(&tool)
                    .and_then(|card| card.inputs[question].clone())
                {
                    input.0.borrow_mut().click(&mut self.engine, rect, x, y);
                }
                true
            }
            control => self.press_question_control(&activity, control),
        })
    }

    /// Choose an option or press an action.
    fn press_question_control(&mut self, activity: &ToolActivity, control: Control) -> bool {
        let tool = activity.id;
        let Some(questions) = waiting(activity) else {
            return false;
        };
        let Some(card) = self.question_cards.get_mut(&tool) else {
            return false;
        };
        match control {
            Control::Option(question, option) => {
                if !card.sent {
                    card.picks[question] = Some(Pick::Option(option));
                }
                true
            }
            Control::Field(_) => true,
            Control::Skip => {
                if card.sent {
                    return true;
                }
                card.sent = true;
                tracing::info!(target: "koru_ai::ui", "agent questions skipped");
                self.give_keyboard_back();
                self.send_command(Command::DenyTool(tool))
            }
            Control::Submit => {
                let Some(answers) = card.answers(questions).filter(|_| !card.sent) else {
                    return true;
                };
                card.sent = true;
                tracing::info!(target: "koru_ai::ui", count = answers.len(), "agent questions answered");
                self.give_keyboard_back();
                // A set answered by the next message whose reply ended:
                // the answers are a message of their own (D108).
                if is_next_message(activity) && !self.asking_reply_running(activity) {
                    return self.send_answers(tool, answers);
                }
                self.send_command(Command::AnswerQuestions(tool, answers))
            }
        }
    }

    /// Whether the reply that asked `activity`'s set is the one running.
    fn asking_reply_running(&self, activity: &ToolActivity) -> bool {
        self.reply_streaming() && self.attempt == Some(activity.attempt_id)
    }

    /// **Send a set's answers as the next message** (D108): a reply of its
    /// own, followed as a sent message's is, with no card of the person's —
    /// the set's card turns to *You answered* when the agent takes them.
    /// Refused, the card can be answered again.
    pub(crate) fn send_answers(
        &mut self,
        tool: ToolInvocationId,
        answers: Vec<AgentAnswer>,
    ) -> bool {
        let Some(chat_id) = self.shown_chat_id() else {
            return false;
        };
        self.answering = true;
        self.pulse = Some(koru_ui::PulsingDots::new(std::time::Instant::now()));
        self.live_usage = None;
        self.live_sources.clear();
        self.open_searches.clear();
        self.live_reasoning.clear();
        self.pending_assistant_message_id = None;
        self.draft.clear();
        self.messages_dirty = true;
        let request = koru_ai_wire::SendTurnRequest {
            operation_id: uuid::Uuid::new_v4(),
            chat_id,
            surface: crate::SURFACE,
            markdown: String::new(),
            explicit_refs: Vec::new(),
            reasoning_override: None,
            web_search: false,
            system_prompt_override: None,
            replace_pinned_documents: false,
            attachment_ids: Vec::new(),
            answers: Some(koru_ai_core::NextMessageAnswers {
                invocation: tool,
                answers,
            }),
        };
        if !self.queue_command(Command::Send(request)) {
            self.answering = false;
            self.pulse = None;
            if let Some(card) = self.question_cards.get_mut(&tool) {
                card.sent = false;
            }
        }
        true
    }

    fn give_keyboard_back(&mut self) {
        self.focus = Focus::Composer;
        self.composer.set_focused(true);
    }

    /// **A set arrived** while the composer is empty and has the keyboard:
    /// the card takes it, on the agent's first option — the one it
    /// recommends. Typing anywhere else is never interrupted.
    pub(crate) fn offer_question_keyboard(&mut self, activity: &ToolActivity) {
        if waiting(activity).is_none()
            || self.focus != Focus::Composer
            || !self.composer.text().trim().is_empty()
            || self.attempt.is_none()
        {
            return;
        }
        self.focus = Focus::Questions(activity.id);
        self.composer.set_focused(false);
    }

    /// The control the keyboard is on in `tool`'s card, when the card has
    /// it.
    pub(crate) fn question_focus(&self, tool: ToolInvocationId) -> Option<Control> {
        match self.focus {
            Focus::Questions(focused) if focused == tool => {
                self.question_cards.get(&tool).map(|card| card.cursor)
            }
            _ => None,
        }
    }

    /// **A key while a card has the keyboard.** `None` passes it on: the
    /// card closed, or a letter typed on an option or an action, which goes
    /// to the composer.
    pub(crate) fn questions_key(
        &mut self,
        tool: ToolInvocationId,
        key: &WinitKey,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        let activity = self.transcript.as_ref().and_then(|transcript| {
            transcript
                .tool_activities
                .iter()
                .find(|activity| activity.id == tool)
                .cloned()
        });
        let Some(activity) = activity.filter(|activity| waiting(activity).is_some()) else {
            self.focus = Focus::Composer;
            return None;
        };
        let questions = waiting(&activity)?.to_vec();
        let order = controls(&questions);
        let cursor = self.question_cards.get(&tool)?.cursor;
        let step = |forward: bool| {
            let at = order
                .iter()
                .position(|control| *control == cursor)
                .unwrap_or(0);
            let next = if forward {
                (at + 1) % order.len()
            } else {
                (at + order.len() - 1) % order.len()
            };
            order[next]
        };
        match key {
            WinitKey::Named(NamedKey::Escape) => {
                self.give_keyboard_back();
                return Some(true);
            }
            WinitKey::Named(NamedKey::Tab) if !koru_keys::is_chord(mods) => {
                let next = step(!mods.shift_key());
                self.question_cards.get_mut(&tool)?.cursor = next;
                return Some(true);
            }
            _ => {}
        }
        if let Control::Field(question) = cursor {
            if matches!(key, WinitKey::Named(NamedKey::Enter)) {
                // On from the box: to the next control, or — the set
                // complete — Submit.
                let card = self.question_cards.get(&tool)?;
                if card.answers(&questions).is_some() && !card.sent {
                    return Some(self.press_question_control(&activity, Control::Submit));
                }
                self.question_cards.get_mut(&tool)?.cursor = step(true);
                return Some(true);
            }
            let card = self.question_cards.get_mut(&tool)?;
            let input = card.inputs[question].clone()?;
            let typed = input.0.borrow_mut().key(&mut self.engine, key, text, mods);
            // Words typed into a question with options are its answer:
            // *Other*. Emptied, it has none until an option is chosen.
            if !questions[question].options.is_empty() {
                card.picks[question] = if input.value().trim().is_empty() {
                    None
                } else {
                    Some(Pick::Words)
                };
            }
            return Some(typed == Typed::Handled);
        }
        let activates = match key {
            WinitKey::Named(NamedKey::Enter | NamedKey::Space) => true,
            WinitKey::Character(value) => value.as_str() == " ",
            _ => false,
        };
        if activates && mods.is_empty() {
            return Some(self.press_question_control(&activity, cursor));
        }
        // Up and Down walk a question's options as Tab does.
        if mods.is_empty()
            && let Control::Option(..) = cursor
            && let WinitKey::Named(direction @ (NamedKey::ArrowUp | NamedKey::ArrowDown)) = key
        {
            let next = step(*direction == NamedKey::ArrowDown);
            self.question_cards.get_mut(&tool)?.cursor = next;
            return Some(true);
        }
        // A letter on an option or an action is typing: the composer's.
        if text.is_some_and(|text| !text.trim().is_empty()) && !koru_keys::is_chord(mods) {
            self.give_keyboard_back();
            return None;
        }
        Some(false)
    }

    /// Cards whose sets are no longer waiting are dropped; a set sent and
    /// refused can be answered again.
    pub(crate) fn settle_question_cards(&mut self, refused: bool) {
        let waiting_ids: Vec<ToolInvocationId> = self
            .transcript
            .as_ref()
            .map(|transcript| {
                transcript
                    .tool_activities
                    .iter()
                    .filter(|activity| waiting(activity).is_some())
                    .map(|activity| activity.id)
                    .collect()
            })
            .unwrap_or_default();
        self.question_cards.retain(|id, _| waiting_ids.contains(id));
        if refused {
            for card in self.question_cards.values_mut() {
                card.sent = false;
            }
        }
        // A set answered by the next message still waiting once nothing is
        // on its way — its answers were refused, or their reply ended
        // without the agent taking them — can be answered again (D108).
        if !self.reply_streaming() && !self.message_on_its_way() && !self.answering {
            let next_message: Vec<ToolInvocationId> = self
                .transcript
                .as_ref()
                .map(|transcript| {
                    transcript
                        .tool_activities
                        .iter()
                        .filter(|activity| waiting(activity).is_some() && is_next_message(activity))
                        .map(|activity| activity.id)
                        .collect()
                })
                .unwrap_or_default();
            for id in next_message {
                if let Some(card) = self.question_cards.get_mut(&id) {
                    card.sent = false;
                }
            }
        }
        if let Focus::Questions(tool) = self.focus
            && !self.question_cards.contains_key(&tool)
            && !waiting_ids.contains(&tool)
        {
            self.focus = Focus::Composer;
        }
    }
}

#[cfg(test)]
mod tests;
