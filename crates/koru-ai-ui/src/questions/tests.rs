// SPDX-License-Identifier: MPL-2.0

use std::time::Duration;

use koru_ai_core::{
    AgentActivityLive, AgentQuestionOption, AiProviderId, Chat, ChatId, InferenceAttemptId,
    MessageId, ModelSelection, ReasoningChoice, Timestamp,
};
use koru_ai_wire::ChatTranscript;
use koru_draw::el::Name;

use super::*;
use crate::Update;
use crate::reader::Reader;

fn question(id: &str, options: &[&str]) -> AgentQuestion {
    AgentQuestion {
        id: id.into(),
        header: format!("Header {id}"),
        question: format!("Question {id}?"),
        options: options
            .iter()
            .map(|label| AgentQuestionOption {
                label: (*label).into(),
                description: format!("Why {label}"),
            })
            .collect(),
        allow_other: false,
        secret: false,
    }
}

/// Three questions: options and *Other*, options only, and a secret.
fn set() -> Vec<AgentQuestion> {
    let mut other = question("framework", &["Axum (Recommended)", "Actix", "Rocket"]);
    other.allow_other = true;
    let mut secret = question("token", &[]);
    secret.secret = true;
    vec![other, question("port", &["8080", "3000"]), secret]
}

fn transcript() -> ChatTranscript {
    let now = Timestamp::from_millis(1);
    let chat_id = ChatId::new();
    ChatTranscript {
        sources: Vec::new(),
        chat: Chat {
            id: chat_id,
            title: "Agent chat".into(),
            private: false,
            current_model: ModelSelection {
                provider: AiProviderId::parse("agent-codex").unwrap(),
                requested_model: "default".into(),
                default_reasoning: ReasoningChoice::Default,
            },
            branched_from: None,
            agent_working_folder: None,
            agent_mode: None,
            agent_fast: None,
            pinned_at: None,
            profile_id: None,
            created_at: now,
            updated_at: now,
        },
        tool_policy: koru_ai_core::ChatToolPolicy {
            chat_id,
            mode: koru_ai_core::ToolMode::Manual,
            workspace_ids: Vec::new(),
        },
        tool_activities: Vec::new(),
        notices: Vec::new(),
        messages: Vec::new(),
        blocks: Vec::new(),
        pinned_documents: Vec::new(),
        feedback: Vec::new(),
        usage: Vec::new(),
        reasoning_control: None,
        attachments: Vec::new(),
    }
}

/// The row a waiting set is: a permission request, with its questions live.
fn row(chat: ChatId, questions: Vec<AgentQuestion>, blocking: bool) -> ToolActivity {
    ToolActivity {
        id: ToolInvocationId::new(),
        attempt_id: InferenceAttemptId::new(),
        chat_id: chat,
        assistant_message_id: MessageId::new(),
        tool_name: AgentToolKind::Approval.as_token().into(),
        read_only: false,
        status: if blocking {
            ToolActivityStatus::ApprovalRequired
        } else {
            ToolActivityStatus::Running
        },
        approval: ToolApproval::Pending,
        proposed_at: Timestamp::from_millis(2),
        started_at: (!blocking).then(|| Timestamp::from_millis(2)),
        completed_at: None,
        result_bytes: None,
        error_code: None,
        target_workspace_id: None,
        target_document_id: None,
        error_message: None,
        agent_label: None,
        agent_summary: Some(QuestionsRecord::of(&questions).summary()),
        agent_input: QuestionsRecord::of(&questions).encode(),
        agent_output: None,
        text_offset: None,
        display_target: None,
        live: Some(AgentActivityLive {
            choices: Vec::new(),
            questions,
        }),
    }
}

fn surface_with(
    activity: &ToolActivity,
    transcript: ChatTranscript,
) -> (
    ChatSurface,
    std::sync::mpsc::Receiver<Command>,
    std::sync::mpsc::SyncSender<Update>,
) {
    let mut surface = ChatSurface::new();
    surface.lang = Lang::En;
    surface.set_viewport(Rect::new(0.0, 0.0, 900.0, 900.0));
    let (reader, commands, updates) = Reader::test_pair();
    surface.reader = Some(reader);
    surface.transcript = Some(transcript);
    surface.validated_ready = true;
    surface.upsert_tool_activity(activity.clone());
    let area = surface.conversation_rect();
    surface.rebuild_messages(area.w);
    surface.position_conversation_items();
    (surface, commands, updates)
}

/// The card's frame where the surface draws it.
fn card_frame(surface: &mut ChatSurface) -> (Rect, koru_draw::el::Frame, ToolActivity) {
    let (rect, activity) = surface.question_rows().into_iter().next().expect("a card");
    let questions = waiting(&activity).unwrap().to_vec();
    let card = surface.question_cards.get(&activity.id).unwrap();
    let spec = spec(
        &mut surface.engine,
        Lang::En,
        activity.id,
        &questions,
        card,
        None,
    );
    let frame = elements::layout(&elements::tree(&spec), rect, FrameCx::root());
    (rect, frame, activity)
}

fn centre(rect: Rect) -> (f32, f32) {
    (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5)
}

/// **The card draws each question in the agent's order, its options in the
/// agent's order — the recommended one first — with their lines, an
/// *Other* box where the agent takes words, and a masked one for a
/// secret.**
#[test]
fn the_card_draws_questions_and_options_in_the_agents_order() {
    let transcript = transcript();
    let activity = row(transcript.chat.id, set(), true);
    let (mut surface, _, _) = surface_with(&activity, transcript);
    assert_eq!(surface.tool_activity_layouts.len(), 1, "one card");
    assert!(surface.work_layouts.is_empty(), "never a step");
    let (_, frame, _) = card_frame(&mut surface);
    let labels: Vec<String> = frame
        .named(elements::OPTION)
        .filter_map(|node| match &node.label {
            Some(Name::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels,
        ["Axum (Recommended)", "Actix", "Rocket", "8080", "3000"]
    );
    assert_eq!(
        frame.named(elements::OPTION_LINE).count(),
        5,
        "each its line"
    );
    assert_eq!(frame.named(elements::BLOCK).count(), 3);
    assert_eq!(frame.named(elements::HEADER).count(), 3);
    // An Other box for the first, a masked one for the secret; none for
    // the question that takes only its options.
    let fields: Vec<String> = frame
        .named(elements::FIELD)
        .filter_map(|node| match &node.label {
            Some(Name::Text(text)) => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(fields, ["Other…", "Hidden as you type"]);
    let card = surface.question_cards.get(&activity.id).unwrap();
    assert!(card.inputs[2].as_ref().unwrap().0.borrow().masked());
    // The order the keyboard walks it.
    assert_eq!(
        controls(&set()),
        [
            Control::Option(0, 0),
            Control::Option(0, 1),
            Control::Option(0, 2),
            Control::Field(0),
            Control::Option(1, 0),
            Control::Option(1, 1),
            Control::Field(2),
            Control::Skip,
            Control::Submit,
        ]
    );
}

/// **Submit is on only once every question has an answer**, and sends them
/// all: a chosen option by its label, *Other*'s words, the secret as typed.
#[test]
fn submit_waits_for_every_answer_and_sends_them_all() {
    let transcript = transcript();
    let activity = row(transcript.chat.id, set(), true);
    let (mut surface, commands, _) = surface_with(&activity, transcript);
    let submit_disabled = |surface: &mut ChatSurface| {
        let (_, frame, activity) = card_frame(surface);
        frame
            .node(control_key(activity.id, Control::Submit))
            .unwrap()
            .disabled
    };
    assert!(submit_disabled(&mut surface));
    let (_, frame, _) = card_frame(&mut surface);
    let option = |frame: &koru_draw::el::Frame, control| {
        frame.node(control_key(activity.id, control)).unwrap().rect
    };
    let (x, y) = centre(option(&frame, Control::Option(0, 2)));
    assert_eq!(surface.question_mouse_down(x, y), Some(true));
    assert_eq!(surface.focus, Focus::Questions(activity.id));
    let (x, y) = centre(option(&frame, Control::Option(1, 0)));
    surface.question_mouse_down(x, y);
    assert!(submit_disabled(&mut surface), "the secret is unanswered");
    // Type the secret: Tab to its box first.
    surface.question_cards.get_mut(&activity.id).unwrap().cursor = Control::Field(2);
    for letter in ["s", "k", " "] {
        surface.key(
            &WinitKey::Character(letter.into()),
            Some(letter),
            ModifiersState::empty(),
        );
    }
    assert!(!submit_disabled(&mut surface));
    // Words typed into the first question's box are its answer: Other.
    surface.question_cards.get_mut(&activity.id).unwrap().cursor = Control::Field(0);
    surface.key(
        &WinitKey::Character("P".into()),
        Some("P"),
        ModifiersState::empty(),
    );
    // Enter in a box of a complete set submits it.
    surface.key(
        &WinitKey::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    );
    let Command::AnswerQuestions(tool, answers) =
        commands.recv_timeout(Duration::from_millis(50)).unwrap()
    else {
        panic!("answers sent");
    };
    assert_eq!(tool, activity.id);
    assert_eq!(
        answers
            .iter()
            .map(|answer| answer.value.clone())
            .collect::<Vec<_>>(),
        [
            AgentAnswerValue::Text { text: "P".into() },
            AgentAnswerValue::Option {
                label: "8080".into()
            },
            AgentAnswerValue::Text { text: "sk ".into() },
        ]
    );
    assert_eq!(surface.focus, Focus::Composer, "the keyboard goes back");
    assert!(submit_disabled(&mut surface), "sent: inert until it closes");
}

/// **Skip declines the set**; the keyboard walks the card with Tab and
/// presses with Enter, and Escape gives it back.
#[test]
fn skip_declines_and_the_keyboard_walks_the_card() {
    let transcript = transcript();
    let activity = row(
        transcript.chat.id,
        vec![question("q", &["Yes", "No"])],
        true,
    );
    let (mut surface, commands, _) = surface_with(&activity, transcript);
    // The set arrived while the composer was empty: the card has the
    // keyboard, on the agent's first option.
    surface.attempt = Some(activity.attempt_id);
    surface.focus = Focus::Composer;
    surface.offer_question_keyboard(&activity);
    assert_eq!(surface.focus, Focus::Questions(activity.id));
    assert_eq!(
        surface.question_focus(activity.id),
        Some(Control::Option(0, 0))
    );
    let tab = |surface: &mut ChatSurface, shift: bool| {
        surface.key(
            &WinitKey::Named(NamedKey::Tab),
            None,
            if shift {
                ModifiersState::SHIFT
            } else {
                ModifiersState::empty()
            },
        )
    };
    tab(&mut surface, false);
    assert_eq!(
        surface.question_focus(activity.id),
        Some(Control::Option(0, 1))
    );
    surface.key(
        &WinitKey::Named(NamedKey::Space),
        Some(" "),
        ModifiersState::empty(),
    );
    assert_eq!(
        surface.question_cards[&activity.id].picks[0],
        Some(Pick::Option(1))
    );
    tab(&mut surface, false);
    assert_eq!(surface.question_focus(activity.id), Some(Control::Skip));
    tab(&mut surface, true);
    tab(&mut surface, false);
    surface.key(
        &WinitKey::Named(NamedKey::Enter),
        None,
        ModifiersState::empty(),
    );
    assert!(matches!(
        commands.recv_timeout(Duration::from_millis(50)).unwrap(),
        Command::DenyTool(id) if id == activity.id
    ));
    assert_eq!(surface.focus, Focus::Composer);

    surface.focus = Focus::Questions(activity.id);
    surface.key(
        &WinitKey::Named(NamedKey::Escape),
        None,
        ModifiersState::empty(),
    );
    assert_eq!(surface.focus, Focus::Composer);
}

/// **While a set the agent waits on is open, the chat needs you** (D83's
/// amber dot); a set it does not wait on shows its card without stopping
/// the reply.
#[test]
fn a_waiting_set_needs_you_and_one_not_waited_on_does_not() {
    let transcript = transcript();
    let blocking = row(transcript.chat.id, set(), true);
    let (surface, _, _) = surface_with(&blocking, transcript.clone());
    assert!(surface.needs_you());
    assert_eq!(
        surface.chat_status(false),
        koru_chats_ui::ChatStatus::NeedsYou
    );
    let open = row(transcript.chat.id, set(), false);
    let (surface, _, _) = surface_with(&open, transcript);
    assert!(!surface.needs_you());
    assert_eq!(surface.tool_activity_layouts.len(), 1, "its card shows");
}

/// **After answering, the card is a summary**, read from the row alone —
/// what a reload shows: a line a question, a secret as *(hidden)*; a skipped
/// set, or one its turn closed, says so.
#[test]
fn an_answered_set_reads_as_a_summary_from_its_row() {
    let transcript = transcript();
    let mut answered = row(transcript.chat.id, set(), true);
    answered.live = None;
    answered.status = ToolActivityStatus::Complete;
    answered.approval = ToolApproval::Approved;
    answered.started_at = Some(Timestamp::from_millis(3));
    answered.completed_at = Some(Timestamp::from_millis(4));
    answered.agent_output = AnswersRecord {
        answers: vec![
            AgentAnswer {
                question_id: "framework".into(),
                value: AgentAnswerValue::Option {
                    label: "Axum (Recommended)".into(),
                },
            },
            AgentAnswer {
                question_id: "token".into(),
                value: AgentAnswerValue::Withheld,
            },
        ],
    }
    .encode();
    assert!(is_question_row(&answered) && waiting(&answered).is_none());
    let summary = summary(&answered, Lang::En);
    assert_eq!(summary.heading, "You answered");
    assert_eq!(
        summary.lines,
        [
            "Header framework — Axum (Recommended)",
            "Header port — (no answer)",
            "Header token — (hidden)",
        ]
    );
    let (surface, _, _) = surface_with(&answered, transcript);
    assert!(!surface.needs_you());
    assert_eq!(surface.tool_activity_layouts.len(), 1, "a summary card");
    assert!(surface.question_cards.is_empty(), "nothing to answer");

    let mut skipped = row(ChatId::new(), set(), true);
    skipped.live = None;
    skipped.status = ToolActivityStatus::Denied;
    assert_eq!(
        super::summary(&skipped, Lang::En).heading,
        "You skipped the agent’s questions"
    );
    skipped.status = ToolActivityStatus::Cancelled;
    assert_eq!(
        super::summary(&skipped, Lang::En).heading,
        "The agent’s questions closed unanswered"
    );
}

/// **A refused answer can be sent again**: a failure from the service makes
/// the card's actions live once more.
#[test]
fn a_refused_answer_can_be_sent_again() {
    let transcript = transcript();
    let activity = row(transcript.chat.id, vec![question("q", &["Yes"])], true);
    let (mut surface, _, _) = surface_with(&activity, transcript);
    surface.question_cards.get_mut(&activity.id).unwrap().sent = true;
    surface.apply_update(Update::Failure("Not now.".into()));
    assert!(!surface.question_cards[&activity.id].sent);
}

/// A set answered by the next message (D108): its row runs, and its record
/// says so.
fn next_message_row(chat: ChatId) -> ToolActivity {
    let mut questions = vec![question(
        "[\"request_user_input_async\",\"call\",0]",
        &["SQLite", "Redis"],
    )];
    questions[0].allow_other = true;
    questions[0].header = String::new();
    let mut activity = row(chat, questions.clone(), false);
    activity.agent_input = QuestionsRecord::of(&questions)
        .answered_by_next_message(true)
        .encode();
    activity
}

/// **A set answered by the next message is answered by a message of its
/// own once its reply ended** (D108): *Submit* sends the answers as the
/// next message — no text, no card of the person's — and while the reply
/// that asked still runs, they go into it as any set's do.
#[test]
fn a_next_message_set_is_answered_by_a_message_of_its_own_after_its_reply() {
    let transcript = transcript();
    let chat = transcript.chat.id;
    let activity = next_message_row(chat);
    assert!(is_next_message(&activity));
    let (mut surface, commands, _) = surface_with(&activity, transcript.clone());
    assert!(!surface.needs_you(), "it does not stop anything");
    let (_, frame, _) = card_frame(&mut surface);
    let (x, y) = centre(
        frame
            .node(control_key(activity.id, Control::Option(0, 0)))
            .unwrap()
            .rect,
    );
    surface.question_mouse_down(x, y);
    surface.press_question_control(&activity, Control::Submit);
    let Command::Send(request) = commands.recv_timeout(Duration::from_millis(50)).unwrap() else {
        panic!("sent as a message");
    };
    assert_eq!(request.chat_id, chat);
    assert!(request.markdown.is_empty() && request.attachment_ids.is_empty());
    let next = request.answers.expect("the answers");
    assert_eq!(next.invocation, activity.id);
    assert_eq!(
        next.answers[0].value,
        AgentAnswerValue::Option {
            label: "SQLite".into()
        }
    );
    assert!(
        surface.answering && surface.pending_user.is_none(),
        "no card of the person's"
    );

    // While the reply that asked runs, the answers go into it.
    let (mut surface, commands, _) = surface_with(&activity, transcript);
    surface.attempt = Some(activity.attempt_id);
    let (_, frame, _) = card_frame(&mut surface);
    let (x, y) = centre(
        frame
            .node(control_key(activity.id, Control::Option(0, 1)))
            .unwrap()
            .rect,
    );
    surface.question_mouse_down(x, y);
    surface.press_question_control(&activity, Control::Submit);
    assert!(matches!(
        commands.recv_timeout(Duration::from_millis(50)).unwrap(),
        Command::AnswerQuestions(tool, _) if tool == activity.id
    ));
}

/// **A set answered by the next message that closed unanswered was passed
/// over**, and says so; its answers refused, the card can be answered again
/// once nothing is on its way.
#[test]
fn a_passed_over_set_says_so_and_refused_answers_can_be_sent_again() {
    let transcript = transcript();
    let activity = next_message_row(transcript.chat.id);
    let mut passed = activity.clone();
    passed.live = None;
    passed.status = ToolActivityStatus::Cancelled;
    passed.approval = ToolApproval::Denied;
    assert_eq!(
        summary(&passed, Lang::En).heading,
        "Passed over — these questions can no longer be answered"
    );

    let (mut surface, _, _) = surface_with(&activity, transcript);
    surface.question_cards.get_mut(&activity.id).unwrap().sent = true;
    surface.answering = true;
    surface.settle_question_cards(false);
    assert!(
        surface.question_cards[&activity.id].sent,
        "still on its way"
    );
    surface.answering = false;
    surface.settle_question_cards(false);
    assert!(!surface.question_cards[&activity.id].sent);
}
