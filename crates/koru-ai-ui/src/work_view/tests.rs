// SPDX-License-Identifier: MPL-2.0

use koru_ai_core::{
    AiProviderId, Chat, ChatId, ChatRole, InferenceAttemptId, Message, MessageState,
    ModelSelection, ReasoningChoice, Timestamp, ToolActivity, ToolActivityStatus, ToolApproval,
};
use koru_ai_wire::{
    AI_STREAM_VERSION, ChatTranscript, PublicAiEvent, SendTurnAccepted, StreamEvent,
};
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::*;
use crate::reader::Update;
use crate::{ChatSurface, Focus, SurfaceMode};

const COMMENTARY: &str = "I'll look at the file.";
const ANSWER: &str = "Done: it works.";

fn transcript() -> ChatTranscript {
    let now = Timestamp::from_millis(1);
    let chat_id = ChatId::new();
    ChatTranscript {
        sources: Vec::new(),
        chat: Chat {
            id: chat_id,
            title: "Work".into(),
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
            created_at: now,
            updated_at: now,
        },
        tool_policy: koru_ai_core::ChatToolPolicy {
            chat_id,
            mode: koru_ai_core::ToolMode::Manual,
            workspace_ids: Vec::new(),
        },
        tool_activities: Vec::new(),
        messages: Vec::new(),
        blocks: Vec::new(),
        pinned_documents: Vec::new(),
        feedback: Vec::new(),
        usage: Vec::new(),
        reasoning_control: None,
        attachments: Vec::new(),
    }
}

fn message(chat: ChatId, role: ChatRole, state: MessageState, markdown: &str) -> Message {
    Message {
        id: MessageId::new(),
        chat_id: chat,
        ordinal: 0,
        role,
        state,
        markdown: markdown.into(),
        origin_message_id: None,
        provider: None,
        requested_model: None,
        resolved_model: None,
        error_class: None,
        created_at: Timestamp::from_millis(1_000),
        completed_at: (state != MessageState::Pending).then(|| Timestamp::from_millis(95_000)),
    }
}

fn row(
    chat: ChatId,
    reply: MessageId,
    tool_name: &str,
    label: Option<&str>,
    status: ToolActivityStatus,
    offset: Option<u32>,
) -> ToolActivity {
    ToolActivity {
        id: ToolInvocationId::new(),
        attempt_id: InferenceAttemptId::new(),
        chat_id: chat,
        assistant_message_id: reply,
        tool_name: tool_name.into(),
        read_only: false,
        status,
        approval: ToolApproval::NotRequired,
        proposed_at: Timestamp::from_millis(2_000),
        started_at: Some(Timestamp::from_millis(2_000)),
        completed_at: status.is_terminal().then(|| Timestamp::from_millis(3_000)),
        result_bytes: None,
        error_code: None,
        target_workspace_id: None,
        target_document_id: None,
        error_message: None,
        agent_label: label.map(str::to_owned),
        agent_summary: None,
        agent_input: None,
        agent_output: None,
        text_offset: offset,
        display_target: None,
        live: None,
    }
}

/// A finished reply that read two files after saying what it would do,
/// with the commentary and the answer run together as an agent stores them.
fn finished(surface: &mut ChatSurface) -> (MessageId, [ToolInvocationId; 2]) {
    let mut transcript = transcript();
    let chat = transcript.chat.id;
    let question = message(
        chat,
        ChatRole::User,
        MessageState::Complete,
        "Does it work?",
    );
    let markdown = format!("{COMMENTARY}{ANSWER}");
    let reply = message(chat, ChatRole::Assistant, MessageState::Complete, &markdown);
    let offset = Some(COMMENTARY.len() as u32);
    let reads = [
        row(
            chat,
            reply.id,
            "agent.read",
            Some("Read a.rs"),
            ToolActivityStatus::Complete,
            offset,
        ),
        row(
            chat,
            reply.id,
            "agent.other",
            Some("Read b.rs"),
            ToolActivityStatus::Complete,
            offset,
        ),
    ];
    let ids = [reads[0].id, reads[1].id];
    transcript.blocks = koru_ai_core::finalize_markdown(reply.id, &markdown);
    transcript.tool_activities.extend(reads);
    let id = reply.id;
    transcript.messages = vec![question, reply];
    surface.transcript = Some(transcript);
    (id, ids)
}

fn rebuild(surface: &mut ChatSurface) {
    let width = surface.conversation_rect().w;
    surface.rebuild_messages(width);
}

fn layout_of(surface: &ChatSurface, id: MessageId) -> &crate::MessageLayout {
    surface
        .message_layouts
        .iter()
        .find(|message| message.id == Some(id))
        .unwrap()
}

#[test]
fn a_finished_reply_folds_its_work_and_keeps_its_answer() {
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    let (reply, _) = finished(&mut surface);
    rebuild(&mut surface);

    assert_eq!(surface.work_layouts.len(), 1);
    let work = &surface.work_layouts[0];
    assert!(!work.live);
    assert!(!work.open, "folded by default");
    assert!(work.lines.is_empty());
    let answer = layout_of(&surface, reply);
    assert!(work.top < answer.top);
    // The answer is what followed the last call; the commentary is folded.
    assert_eq!(
        answer
            .blocks
            .iter()
            .map(|block| block.markdown.as_str())
            .collect::<Vec<_>>(),
        [ANSWER]
    );
    assert_eq!(
        tool_groups::header(false, 94_000, Lang::En),
        "Worked for 1m 34s"
    );
}

#[test]
fn the_header_opens_the_commentary_and_the_steps_by_click_and_by_key() {
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    let (reply, [first, _]) = finished(&mut surface);
    rebuild(&mut surface);
    let area = surface.conversation_rect();
    let top = surface.work_layouts[0].top;

    assert!(surface.mouse_down(area.x + 20.0, area.y + top + 8.0));
    assert!(surface.opened_work.contains(&reply));
    assert_eq!(surface.focus, Focus::Work(reply));
    rebuild(&mut surface);
    let work = &surface.work_layouts[0];
    assert!(work.open);
    // What the agent said, in the text color, then one line for both reads.
    assert_eq!(work.lines.len(), 2);
    assert!(work.lines[0].said);
    assert_eq!(work.lines[1].hit, Some(WorkHit::Group(reply, 0)));

    // The group opens to a line per call, and a call to its detail.
    surface.toggle_work(WorkHit::Group(reply, 0));
    rebuild(&mut surface);
    assert_eq!(surface.work_layouts[0].lines.len(), 4);
    assert_eq!(
        surface.work_layouts[0].lines[2].hit,
        Some(WorkHit::Call(first))
    );
    surface.toggle_work(WorkHit::Call(first));
    rebuild(&mut surface);
    assert_eq!(surface.work_layouts[0].lines.len(), 5);

    // Enter on the focused header folds it again.
    assert!(surface.key(&Key::Named(NamedKey::Enter), None, ModifiersState::empty()));
    assert!(!surface.opened_work.contains(&reply));
    rebuild(&mut surface);
    assert!(surface.work_layouts[0].lines.is_empty());
}

#[test]
fn a_running_reply_shows_what_it_said_and_the_call_it_is_making() {
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    let transcript = transcript();
    let chat = transcript.chat.id;
    surface.transcript = Some(transcript);
    let reply = MessageId::new();
    surface.pending_user = Some("Does it work?".into());
    surface.pending_assistant_message_id = Some(reply);
    surface.live_turn_started_ms = Some(Timestamp::now().millis());
    surface.draft = "Let me check.".into();
    // The row arrives without a stored offset: the surface notes the draft.
    surface.upsert_tool_activity(row(
        chat,
        reply,
        "agent.tool",
        Some("Read big.log"),
        ToolActivityStatus::Running,
        None,
    ));
    surface.draft.push_str("Still reading.");
    rebuild(&mut surface);

    assert_eq!(surface.work_layouts.len(), 1);
    let work = &surface.work_layouts[0];
    assert!(work.live && work.open, "a running reply is always shown");
    assert_eq!(work.lines.len(), 2);
    assert!(work.lines[0].said);
    assert_eq!(work.lines[1].hit, Some(WorkHit::Group(reply, 0)));
    assert_eq!(layout_of(&surface, reply).display, "Still reading.");
    // The header ticks on the surface's own maintenance deadline.
    assert!(surface.work_tick_deadline().is_some());
    let group = tool_groups::group_steps(
        &[&surface.transcript.as_ref().unwrap().tool_activities[0]],
        |_| None,
    );
    assert_eq!(tool_groups::group_line(&group[0], Lang::En), "Reading…");
}

fn event(event: PublicAiEvent) -> Update {
    Update::Event(StreamEvent {
        version: AI_STREAM_VERSION,
        sequence: 1,
        event,
    })
}

const SAID: &str = "I'll search for today's top US news.";
const BETWEEN: &str = "Three wires have it. Reading them.";
const NEWS: &str = "# Top US News\n\n**1. Election eve**";

/// A reply streamed as the service sends it: words, a search, more words,
/// a read, then the answer — every call over before the answer starts.
/// With `side_failure`, a command alongside the turn fails halfway through,
/// and the stream runs on: the failure is not the reply's end.
fn stream_news(surface: &mut ChatSurface, side_failure: bool) -> MessageId {
    let transcript = transcript();
    let chat = transcript.chat.id;
    surface.transcript = Some(transcript);
    surface.pending_user = Some("Today's top US news?".into());
    let reply = MessageId::new();
    let attempt_id = InferenceAttemptId::new();
    surface.apply_update(Update::Accepted(SendTurnAccepted {
        attempt_id,
        user_message_id: MessageId::new(),
        assistant_message_id: reply,
        replayed: false,
    }));
    let say = |surface: &mut ChatSurface, text: &str| {
        surface.apply_update(event(PublicAiEvent::TextDelta {
            attempt_id,
            text: text.into(),
            at: Timestamp::from_millis(2_000),
        }));
    };
    let call = |surface: &mut ChatSurface, tool: &str, label: &str| {
        let running = row(
            chat,
            reply,
            tool,
            Some(label),
            ToolActivityStatus::Running,
            None,
        );
        let mut done = running.clone();
        done.status = ToolActivityStatus::Complete;
        done.completed_at = Some(Timestamp::from_millis(3_000));
        for activity in [running, done] {
            surface.apply_update(event(PublicAiEvent::ToolActivity {
                attempt_id,
                activity,
            }));
        }
    };
    say(surface, SAID);
    call(surface, "agent.search", "Search the web");
    if side_failure {
        surface.apply_update(Update::Failure("Could not rename the chat.".into()));
    }
    say(surface, BETWEEN);
    call(surface, "agent.read", "Read the wire");
    say(surface, NEWS);
    rebuild(surface);
    reply
}

#[test]
fn a_running_reply_with_its_calls_done_is_still_working_above_its_answer() {
    for side_failure in [false, true] {
        let mut surface = ChatSurface::new(SurfaceMode::Standalone);
        let reply = stream_news(&mut surface, side_failure);

        assert_eq!(
            surface.work_layouts.len(),
            1,
            "side_failure: {side_failure}"
        );
        let work = &surface.work_layouts[0];
        assert_eq!(work.message, reply);
        // Every call has ended, the reply has not: still *Working for …*.
        assert!(work.live, "side_failure: {side_failure}");
        assert!(tool_groups::header(work.live, 27_000, Lang::En).starts_with("Working for"));
        let answer = layout_of(&surface, reply);
        assert!(
            work.top + work.height <= answer.top,
            "side_failure: {side_failure}"
        );
        // What it said before and between its calls is the work's; what
        // followed the last call is the answer.
        let said = work.lines.iter().filter(|line| line.said).count();
        assert_eq!(said, 2, "side_failure: {side_failure}");
        assert_eq!(answer.display, NEWS, "side_failure: {side_failure}");
    }
}

#[test]
fn a_pending_permission_request_is_never_folded() {
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    let (reply, _) = finished(&mut surface);
    let transcript = surface.transcript.as_mut().unwrap();
    let chat = transcript.chat.id;
    let mut ask = row(
        chat,
        reply,
        "agent.approval",
        None,
        ToolActivityStatus::ApprovalRequired,
        Some(0),
    );
    ask.approval = ToolApproval::Pending;
    ask.completed_at = None;
    ask.started_at = None;
    ask.agent_summary = Some("Run `cargo test`?".into());
    transcript.tool_activities.push(ask);
    rebuild(&mut surface);

    assert!(!surface.work_layouts[0].open);
    assert_eq!(surface.tool_activity_layouts.len(), 1, "the card stays");
    let card = &surface.tool_activity_layouts[0];
    assert!(surface.work_layouts[0].top < card.top);
    assert!(card.top < layout_of(&surface, reply).top);
}

#[test]
fn a_reply_without_tools_reads_as_before() {
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    let mut transcript = transcript();
    let chat = transcript.chat.id;
    let reply = message(chat, ChatRole::Assistant, MessageState::Complete, ANSWER);
    let id = reply.id;
    transcript.blocks = koru_ai_core::finalize_markdown(id, ANSWER);
    transcript.messages = vec![reply];
    surface.transcript = Some(transcript);
    rebuild(&mut surface);

    assert!(surface.work_layouts.is_empty());
    assert!(surface.tool_activity_layouts.is_empty());
    let answer = layout_of(&surface, id);
    assert_eq!(answer.display, ANSWER);
    assert_eq!(answer.blocks.len(), 1);
    assert!(surface.work_tick_deadline().is_none());
}

#[test]
fn a_reply_from_before_offsets_folds_only_its_steps() {
    let mut surface = ChatSurface::new(SurfaceMode::Standalone);
    let mut transcript = transcript();
    let chat = transcript.chat.id;
    let markdown = format!("{COMMENTARY}{ANSWER}");
    let reply = message(chat, ChatRole::Assistant, MessageState::Complete, &markdown);
    let id = reply.id;
    transcript.tool_activities.push(row(
        chat,
        id,
        "agent.command",
        None,
        ToolActivityStatus::Complete,
        None,
    ));
    transcript.messages = vec![reply];
    surface.transcript = Some(transcript);
    rebuild(&mut surface);

    assert_eq!(surface.work_layouts.len(), 1);
    assert_eq!(layout_of(&surface, id).display, markdown);
}
