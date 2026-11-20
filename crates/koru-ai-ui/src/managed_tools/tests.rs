// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::managed::tests::{event, message, opened};
use crate::managed::{Feed, Refreshed, State};
use std::time::Instant;

fn started(state: &State, cursor: u64, call: Uuid) -> wire::Event {
    let mut entry = event(state, cursor, None);
    entry.observation.event = wire::AgentEvent::ToolStarted {
        turn_id: state
            .primary_turn()
            .unwrap()
            .id
            .to_string()
            .parse()
            .unwrap(),
        tool_call_id: call.to_string().parse().unwrap(),
        label: "Read source".into(),
        input: Some("{\"file\":\"fixture.rs\"}".into()),
    };
    entry
}
fn completed(start: &wire::Event, cursor: u64, output: &str) -> wire::Event {
    let mut entry = start.clone();
    let wire::AgentEvent::ToolStarted {
        turn_id,
        tool_call_id,
        ..
    } = start.observation.event
    else {
        panic!()
    };
    entry.cursor = cursor;
    entry.observation.event = wire::AgentEvent::ToolCompleted {
        turn_id,
        tool_call_id,
        succeeded: true,
        tool: None,
        failure: None,
        detail: None,
        output: Some(output.into()),
    };
    entry
}
fn feed(boot: Uuid, entries: Vec<wire::Event>) -> Feed {
    Feed {
        boot,
        cursor: entries.last().map_or(0, |event| event.cursor),
        gap: false,
        entries,
    }
}

#[test]
fn only_exact_current_incarnation_is_displayed_and_repeated_feed_is_inert() {
    let state = State::new(&mut opened(), Instant::now());
    let boot = Uuid::new_v4();
    let mut tools = Observed::default();
    let start = started(&state, 1, Uuid::new_v4());
    let mut wrong = start.clone();
    wrong.observation.turn.generation += 1;
    assert!(!tools.apply(
        &feed(boot, vec![wrong]),
        &state.snapshot,
        &state.snapshot,
        &HashSet::new()
    ));
    let mut start = start;
    start.cursor = 2;
    assert!(tools.apply(
        &feed(boot, vec![start.clone()]),
        &state.snapshot,
        &state.snapshot,
        &HashSet::new()
    ));
    assert_eq!(tools.rows().len(), 1);
    assert_eq!(
        tools.rows()[0].attempt_id.as_uuid(),
        start.observation.turn.id
    );
    assert_eq!(tools.rows()[0].approval, ToolApproval::NotRequired);
    assert!(tools.rows()[0].live.is_none());
    assert!(!tools.apply(
        &feed(boot, vec![start.clone()]),
        &state.snapshot,
        &state.snapshot,
        &HashSet::new()
    ));
    let mut wrong = completed(&start, 3, "wrong thread");
    wrong.observation.thread = Uuid::new_v4();
    tools.apply(
        &feed(boot, vec![wrong]),
        &state.snapshot,
        &state.snapshot,
        &HashSet::new(),
    );
    assert!(tools.rows()[0].agent_output.is_none());
}

#[test]
fn completion_with_publication_and_removed_turn_keeps_known_actual_output() {
    let mut state = State::new(&mut opened(), Instant::now());
    let boot = Uuid::new_v4();
    let start = started(&state, 1, Uuid::new_v4());
    let reply = state.snapshot.turns[0].reply_message.unwrap();
    state.tools.apply(
        &feed(boot, vec![start.clone()]),
        &state.snapshot,
        &state.snapshot,
        &HashSet::new(),
    );
    let mut snapshot = state.snapshot.clone();
    snapshot.turns.clear();
    assert!(state.refresh(
        Refreshed {
            navigation: Default::default(),
            snapshot,
            messages: vec![message(1, reply)],
            after: 1,
            feed: feed(boot, vec![completed(&start, 2, "{\"answer\":42}")]),
            questions: vec![],
            attention: Default::default(),
        },
        Instant::now()
    ));
    let transcript = state.transcript();
    assert_eq!(transcript.tool_activities.len(), 1);
    assert_eq!(
        transcript.tool_activities[0].assistant_message_id.as_uuid(),
        reply
    );
    assert_eq!(
        transcript.tool_activities[0].agent_output.as_deref(),
        Some("{\"answer\":42}")
    );
    assert_eq!(
        transcript.tool_activities[0].status,
        ToolActivityStatus::Complete
    );
    assert!(
        State::new(&mut opened(), Instant::now())
            .transcript()
            .tool_activities
            .is_empty()
    );
}

#[test]
fn reset_drops_observations_and_never_invents_missing_call_starts() {
    let state = State::new(&mut opened(), Instant::now());
    let boot = Uuid::new_v4();
    let start = started(&state, 1, Uuid::new_v4());
    for changed_boot in [false, true] {
        let mut tools = Observed::default();
        tools.apply(
            &feed(boot, vec![start.clone()]),
            &state.snapshot,
            &state.snapshot,
            &HashSet::new(),
        );
        let mut reset = feed(
            if changed_boot { Uuid::new_v4() } else { boot },
            vec![completed(&start, 2, "suffix")],
        );
        reset.gap = !changed_boot;
        tools.apply(&reset, &state.snapshot, &state.snapshot, &HashSet::new());
        assert!(tools.rows().is_empty());
    }
}

#[test]
fn observed_tool_memory_is_bounded_on_utf8_boundaries() {
    let state = State::new(&mut opened(), Instant::now());
    let boot = Uuid::new_v4();
    let mut tools = Observed::default();
    let content = "🦀".repeat(MAX_CONTENT);
    for index in 0..(MAX_CALLS * 2) {
        let mut start = started(&state, index as u64 * 2 + 1, Uuid::new_v4());
        if let wire::AgentEvent::ToolStarted { input, .. } = &mut start.observation.event {
            *input = Some(content.clone());
        }
        let end = completed(&start, index as u64 * 2 + 2, &content);
        tools.apply(
            &feed(boot, vec![start, end]),
            &state.snapshot,
            &state.snapshot,
            &HashSet::new(),
        );
    }
    assert!(tools.calls.len() <= MAX_CALLS && tools.bytes() <= MAX_BYTES);
    assert!(
        tools.calls.iter().all(
            |call| call.row.agent_input.as_ref().unwrap().len() <= MAX_CONTENT
                && call.row.agent_output.as_ref().unwrap().len() <= MAX_CONTENT
        )
    );
}
