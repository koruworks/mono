// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::worker::bots::{
    admission::make_intent,
    tests::{created, fixture, turn as pending},
};
use koru_delegate_core as d;
use koru_session_protocol as s;

fn setup() -> (Repository, InferenceAttemptId, LiveChat) {
    let (mut repo, authority, create) = fixture();
    let bot = created(&mut repo, authority, create);
    let mut turn = pending(bot.main_chat, Uuid::new_v4(), 1);
    turn.attempt.provider = bot.config.model.provider.clone();
    turn.attempt.requested_model = bot.config.model.requested_model.clone();
    turn.assistant_message.provider = Some(turn.attempt.provider.clone());
    turn.assistant_message.requested_model = Some(turn.attempt.requested_model.clone());
    let profile = repo
        .chat_with_access(
            bot.main_chat,
            &ChatReadAccess::authorized(vec![bot.main_chat]),
        )
        .unwrap()
        .unwrap()
        .profile_id
        .unwrap();
    let pins = vec![d::RunContextItem::Document {
        workspace: bot.home_workspace,
        document: bot.config.role.document,
        version: d::DocumentVersionId::new(),
    }];
    let mut intent = make_intent(
        &bot,
        BotRequestId::mint(),
        bot.main_chat,
        bot.home_workspace,
        &BotTask {
            text: turn.user_message.markdown.clone(),
            documents: Vec::new(),
        },
        None,
        &pins,
        profile,
    )
    .unwrap();
    intent.submission.provenance.ai_attempt_id = Some(turn.attempt.id.as_uuid());
    intent.submission.provenance.assistant_message_id = Some(turn.assistant_message.id.as_uuid());
    repo.prepare_bot_chat(
        &BotChatPreparation::Turn(turn.clone()),
        &intent,
        &"a".repeat(64),
        1,
        turn.attempt.queued_at,
    )
    .unwrap();
    let fence = BotExecutionFence {
        owner: intent.submission.provenance.owner,
        bot: bot.id,
        run: intent.submission.request.run,
        attempt: d::RunAttemptId::mint(),
        generation: d::AttemptGeneration::from_raw(1).unwrap(),
        session: s::SessionId::mint(),
        turn: Some(s::AgentTurnId::mint()),
    };
    repo.bind_bot_execution(turn.attempt.id, &fence, Timestamp::now())
        .unwrap();
    (
        repo,
        turn.attempt.id,
        LiveChat::fixture(intent.submission, fence),
    )
}
fn observation(chat: &LiveChat, first: u64, events: Vec<AgentEvent>) -> BotObservation {
    let fence = chat.fence.clone().unwrap();
    let last = first + events.len().saturating_sub(1) as u64;
    BotObservation {
        fence: fence.clone(),
        observation: s::AgentEventObservation {
            events: events
                .into_iter()
                .enumerate()
                .map(|(i, event)| s::SequencedAgentEvent {
                    session_id: fence.session,
                    sequence: s::StreamSequence::new(first + i as u64).unwrap(),
                    event,
                })
                .collect(),
            cursor: s::StreamSequence::new(last).unwrap(),
            earlier_truncated: false,
            commands: None,
            session_cursor: None,
        },
    }
}

#[test]
fn real_initial_cursor_is_not_an_ack_and_first_event_is_processed_once() {
    let (mut repo, id, chat) = setup();
    let turn_id = chat.fence.as_ref().unwrap().turn.unwrap();
    let mut empty = observation(&chat, s::AGENT_EVENT_FIRST_SEQUENCE, vec![]);
    empty.observation.cursor = s::StreamSequence::new(s::AGENT_EVENT_INITIAL_CURSOR).unwrap();
    assert!(
        stage_observation(&repo, id, &chat, empty.clone())
            .unwrap()
            .is_none()
    );
    assert_eq!(repo.bot_replay_state(id).unwrap().last_sequence, None);
    assert_eq!(repo.bot_reply_bytes(id).unwrap(), 0);
    empty.observation.cursor = s::StreamSequence::new(s::AGENT_EVENT_FIRST_SEQUENCE).unwrap();
    empty.observation.earlier_truncated = true;
    assert!(stage_observation(&repo, id, &chat, empty).is_err());
    for (first, why) in [
        (
            s::AGENT_EVENT_INITIAL_CURSOR,
            "baseline is not an emitted event",
        ),
        (
            s::AGENT_EVENT_FIRST_SEQUENCE + 1,
            "first emitted event is missing",
        ),
    ] {
        assert!(
            stage_observation(
                &repo,
                id,
                &chat,
                observation(&chat, first, vec![AgentEvent::TurnStarted { turn_id }])
            )
            .is_err(),
            "{why}"
        );
    }
    let mut repeated = observation(
        &chat,
        s::AGENT_EVENT_FIRST_SEQUENCE,
        vec![
            AgentEvent::TurnStarted { turn_id },
            AgentEvent::MessageDelta {
                turn_id,
                text: "repeated".into(),
            },
        ],
    );
    repeated.observation.events[1].sequence = repeated.observation.events[0].sequence;
    assert!(
        stage_observation(&repo, id, &chat, repeated).is_err(),
        "a repeated sequence inside one observation is not contiguous"
    );
    let first = observation(
        &chat,
        s::AGENT_EVENT_FIRST_SEQUENCE,
        vec![AgentEvent::TurnStarted { turn_id }],
    );
    let staged = stage_observation(&repo, id, &chat, first.clone())
        .unwrap()
        .unwrap();
    repo.apply_bot_replay(&staged.batch, Timestamp::now())
        .unwrap();
    assert_eq!(
        repo.bot_replay_state(id).unwrap().last_sequence,
        Some(s::AGENT_EVENT_FIRST_SEQUENCE)
    );
    assert!(
        stage_observation(&repo, id, &chat, first)
            .unwrap()
            .is_none()
    );
    for truncated in [false, true] {
        let mut gap = observation(
            &chat,
            s::AGENT_EVENT_FIRST_SEQUENCE + 2,
            vec![AgentEvent::MessageDelta {
                turn_id,
                text: "missing predecessor".into(),
            }],
        );
        gap.observation.earlier_truncated = truncated;
        assert!(stage_observation(&repo, id, &chat, gap).is_err());
    }
    let next = observation(
        &chat,
        s::AGENT_EVENT_FIRST_SEQUENCE + 1,
        vec![AgentEvent::MessageDelta {
            turn_id,
            text: "next".into(),
        }],
    );
    let staged = stage_observation(&repo, id, &chat, next).unwrap().unwrap();
    repo.apply_bot_replay(&staged.batch, Timestamp::now())
        .unwrap();
    assert_eq!(repo.bot_reply_bytes(id).unwrap(), 4);
    assert_eq!(
        repo.bot_replay_state(id).unwrap().last_sequence,
        Some(s::AGENT_EVENT_FIRST_SEQUENCE + 1)
    );
}

#[test]
fn replay_rollback_leaves_mapper_sources_and_live_choices_uninstalled() {
    let (mut repo, id, mut chat) = setup();
    let turn_id = chat.fence.as_ref().unwrap().turn.unwrap();
    let call = s::ToolCallId::mint();
    let native = observation(
        &chat,
        s::AGENT_EVENT_FIRST_SEQUENCE,
        vec![
            AgentEvent::MessageDelta {
                turn_id,
                text: "é".into(),
            },
            AgentEvent::ToolStarted {
                turn_id,
                tool_call_id: call,
                label: "Read file".into(),
                input: Some("fixture".into()),
            },
        ],
    );
    let staged = stage_observation(&repo, id, &chat, native.clone())
        .unwrap()
        .unwrap();
    assert_eq!(staged.mapper.pending_sources(), 1);
    repo.refuse_bot_replay_commit_for_fixture(true).unwrap();
    assert!(
        repo.apply_bot_replay(&staged.batch, Timestamp::now())
            .is_err()
    );
    assert_eq!(chat.mapper.as_ref().unwrap().pending_sources(), 0);
    assert!(chat.live.is_empty());
    assert_eq!(repo.bot_replay_state(id).unwrap().last_sequence, None);
    assert_eq!(repo.bot_reply_bytes(id).unwrap(), 0);
    assert!(
        repo.bot_tool_source(id, &source(AgentSource::Tool(call)))
            .unwrap()
            .is_none()
    );
    repo.refuse_bot_replay_commit_for_fixture(false).unwrap();
    let staged = stage_observation(&repo, id, &chat, native.clone())
        .unwrap()
        .unwrap();
    repo.apply_bot_replay(&staged.batch, Timestamp::now())
        .unwrap();
    chat.mapper = Some(staged.mapper);
    assert_eq!(repo.bot_reply_bytes(id).unwrap(), 2);
    let tool = repo
        .bot_tool_source(id, &source(AgentSource::Tool(call)))
        .unwrap()
        .unwrap();
    assert_eq!(
        repo.tool_activity(tool).unwrap().unwrap().text_offset,
        Some(2)
    );
    assert!(
        stage_observation(&repo, id, &chat, native)
            .unwrap()
            .is_none()
    );
    // A completion after reconnection uses the same durable source binding.
    chat.mapper = Some(AgentEventMapper::new(
        id,
        turn_id,
        None,
        UsageDelivery::EachEvent,
    ));
    let end = observation(
        &chat,
        s::AGENT_EVENT_FIRST_SEQUENCE + 2,
        vec![AgentEvent::ToolCompleted {
            turn_id,
            tool_call_id: call,
            succeeded: true,
            tool: None,
            failure: None,
            detail: None,
            output: None,
        }],
    );
    let staged = stage_observation(&repo, id, &chat, end).unwrap().unwrap();
    repo.apply_bot_replay(&staged.batch, Timestamp::now())
        .unwrap();
    assert_eq!(
        repo.bot_tool_source(id, &source(AgentSource::Tool(call)))
            .unwrap(),
        Some(tool)
    );
    assert_eq!(
        repo.tool_activity(tool).unwrap().unwrap().status,
        ToolActivityStatus::Complete
    );
}

#[test]
fn wrong_turn_gap_and_native_child_cannot_be_empty_checkpoints() {
    let (repo, id, chat) = setup();
    let turn_id = chat.fence.as_ref().unwrap().turn.unwrap();
    for event in [
        AgentEvent::MessageDelta {
            turn_id: s::AgentTurnId::mint(),
            text: "foreign".into(),
        },
        AgentEvent::ChildSessionCreated {
            turn_id,
            child_session_id: s::SessionId::mint(),
        },
    ] {
        assert!(
            stage_observation(
                &repo,
                id,
                &chat,
                observation(&chat, s::AGENT_EVENT_FIRST_SEQUENCE, vec![event])
            )
            .is_err()
        );
    }
    let mut gap = observation(
        &chat,
        s::AGENT_EVENT_FIRST_SEQUENCE + 1,
        vec![AgentEvent::TurnStarted { turn_id }],
    );
    assert!(stage_observation(&repo, id, &chat, gap.clone()).is_err());
    gap.observation.earlier_truncated = true;
    assert!(stage_observation(&repo, id, &chat, gap).is_err());
    assert_eq!(repo.bot_replay_state(id).unwrap().last_sequence, None);
}

#[test]
fn committed_cursor_recovers_lost_ack_and_terminal_delivery_without_double_usage() {
    let (mut repo, id, chat) = setup();
    let turn_id = chat.fence.as_ref().unwrap().turn.unwrap();
    let usage = |total| AgentEvent::Usage {
        turn_id,
        usage: s::AgentUsage {
            input_tokens: Some(total),
            output_tokens: Some(5),
            cached_input_tokens: Some(2),
            cached_input: Default::default(),
            model: None,
        },
    };
    let native = observation(
        &chat,
        s::AGENT_EVENT_FIRST_SEQUENCE,
        vec![
            AgentEvent::MessageDelta {
                turn_id,
                text: "durable".into(),
            },
            usage(10),
            usage(20),
            AgentEvent::TurnCompleted { turn_id },
        ],
    );
    let staged = stage_observation(&repo, id, &chat, native.clone())
        .unwrap()
        .unwrap();
    repo.fail_after_bot_replay_commit_for_fixture();
    assert!(commit_staged(&mut repo, &staged.batch).unwrap());
    assert_eq!(
        repo.bot_replay_state(id).unwrap().last_sequence,
        Some(s::AGENT_EVENT_FIRST_SEQUENCE + 3)
    );
    let attempt = repo.attempt(id).unwrap().unwrap();
    assert_eq!(attempt.status, AttemptStatus::Complete);
    assert_eq!(attempt.usage.total_tokens, Some(25));
    assert_eq!(
        repo.message(attempt.assistant_message_id.unwrap())
            .unwrap()
            .unwrap()
            .markdown,
        "durable"
    );
    let receipt = repo.bot_delivery_receipt(id).unwrap().unwrap();
    // Replayed terminal frames are deduplicated; the durable outbox remains
    // enough to resume Delivery even with no new native event.
    assert!(
        stage_observation(&repo, id, &chat, native)
            .unwrap()
            .is_none()
    );
    assert!(commit_staged(&mut repo, &staged.batch).unwrap());
    assert_eq!(repo.bot_delivery_receipt(id).unwrap(), Some(receipt));
    assert_eq!(
        repo.attempt(id).unwrap().unwrap().usage.total_tokens,
        Some(25)
    );
}
