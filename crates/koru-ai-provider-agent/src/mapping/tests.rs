// SPDX-License-Identifier: MPL-2.0
use super::*;

fn usage(turn_id: AgentTurnId, total: u64) -> AgentEvent {
    AgentEvent::Usage {
        turn_id,
        usage: AgentUsage {
            input_tokens: Some(total),
            output_tokens: Some(5),
            cached_input_tokens: Some(2),
            cached_input: Default::default(),
            model: None,
        },
    }
}

#[test]
fn ordinary_totals_remain_deferred_and_bot_totals_replace_per_source_frame() {
    let attempt = InferenceAttemptId::new();
    let turn = AgentTurnId::mint();
    let at = Timestamp::from_millis(1);
    let mut ordinary = AgentEventMapper::new(attempt, turn, None, UsageDelivery::AtTerminal);
    let mut bot = AgentEventMapper::new(attempt, turn, None, UsageDelivery::EachEvent);
    for total in [10, 20] {
        assert!(
            ordinary
                .apply(usage(turn, total), at, false)
                .events
                .is_empty()
        );
        let mapped = bot.apply(usage(turn, total), at, false);
        assert!(
            matches!(&mapped.events[..],[AiEvent::Usage {usage,..}] if usage.total_tokens==Some(total+5) && usage.source==UsageSource::ProviderReported)
        );
    }
    let done = AgentEvent::TurnCompleted { turn_id: turn };
    assert!(
        matches!(&ordinary.apply(done.clone(),at,false).events[..],[AiEvent::Usage {usage,..},AiEvent::Completed {..}] if usage.total_tokens==Some(25))
    );
    assert!(matches!(
        &bot.apply(done, at, false).events[..],
        [AiEvent::Completed { .. }]
    ));
}

#[test]
fn source_identity_can_be_restored_and_staged_mapping_discarded() {
    let attempt = InferenceAttemptId::new();
    let turn = AgentTurnId::mint();
    let at = Timestamp::from_millis(1);
    let committed = AgentEventMapper::new(attempt, turn, None, UsageDelivery::EachEvent);
    let source = ToolCallId::mint();
    let id = ToolInvocationId::new();
    let start = AgentEvent::ToolStarted {
        turn_id: turn,
        tool_call_id: source,
        label: "tool".into(),
        input: None,
    };
    let mut staged = committed.clone();
    staged.seed(AgentSource::Tool(source), id);
    let mapped = staged.apply(start.clone(), at, false);
    assert_eq!(mapped.sources, vec![(AgentSource::Tool(source), id)]);
    assert_eq!(committed.pending_sources(), 0);
    let mut retried = committed.clone();
    retried.seed(AgentSource::Tool(source), id);
    assert_eq!(retried.apply(start, at, false).sources, mapped.sources);
    let mut reconnected = AgentEventMapper::new(attempt, turn, None, UsageDelivery::EachEvent);
    reconnected.seed(AgentSource::Tool(source), id);
    let done = AgentEvent::ToolCompleted {
        turn_id: turn,
        tool_call_id: source,
        succeeded: true,
        tool: Some(ToolKind::CommandExecution),
        failure: None,
        detail: None,
        output: None,
    };
    let mapped = reconnected.apply(done, at, false);
    assert!(
        matches!(&mapped.events[..],[AiEvent::AgentActivity {activity:AgentActivityEvent::ToolCompleted {invocation,..},..}] if *invocation==id)
    );
    assert_eq!(mapped.sources, vec![(AgentSource::Tool(source), id)]);
}

#[test]
fn unknown_tool_completion_is_one_source_with_both_creation_and_completion() {
    let turn = AgentTurnId::mint();
    let source = ToolCallId::mint();
    let mut mapper = AgentEventMapper::new(
        InferenceAttemptId::new(),
        turn,
        None,
        UsageDelivery::EachEvent,
    );
    let mapped = mapper.apply(
        AgentEvent::ToolCompleted {
            turn_id: turn,
            tool_call_id: source,
            succeeded: true,
            tool: None,
            failure: None,
            detail: None,
            output: None,
        },
        Timestamp::from_millis(1),
        false,
    );
    assert_eq!(mapped.events.len(), 2);
    assert_eq!(mapped.sources.len(), 1);
    assert_eq!(
        invocation_of(&mapped.events[0]),
        invocation_of(&mapped.events[1])
    );
    assert_eq!(agent_tool_kind(None), AgentToolKind::Other);
    assert_eq!(
        choice_intent(ApprovalIntent::AllowRemembered),
        ChoiceIntent::AllowAlways
    );
}
