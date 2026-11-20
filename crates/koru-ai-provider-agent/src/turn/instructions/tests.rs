// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_session_protocol::{SequencedAgentEvent, SessionId, StreamSequence};

fn observation(events: Vec<AgentEvent>) -> AgentEventObservation {
    AgentEventObservation {
        events: events
            .into_iter()
            .enumerate()
            .map(|(index, event)| SequencedAgentEvent {
                session_id: SessionId::mint(),
                sequence: StreamSequence::new(index as u64 + 2).unwrap(),
                event,
            })
            .collect(),
        cursor: StreamSequence::new(8).unwrap(),
        earlier_truncated: false,
        commands: None,
        session_cursor: None,
    }
}

fn custom(text: &str) -> Resolved {
    let mut source = session::bundled();
    source.text = text.to_owned().into();
    source
}

#[test]
fn reattached_session_after_refused_first_send_still_gets_instructions() {
    let request = super::super::instructions_tests::request("MODE");
    // The host's preflight refusal leaves no accepted turn. Reattachment is
    // not itself a reason to omit the first prompt (EnsuredSession::fresh=false).
    let unused = observation(vec![AgentEvent::CommandsUpdated { commands: vec![] }]);
    let initial = needs_initial_prompt(None, &unused);
    let (first, _) = prepare(&request, initial, || custom("COMMON"));
    assert!(first.starts_with("COMMON\n\nMODE\n\n"));
    let (retry, _) = prepare(&request, needs_initial_prompt(None, &unused), || {
        custom("COMMON")
    });
    assert_eq!(retry, first);
    // Once accepted, both an uncertain in-flight reply and a completed turn
    // prove the session already has its instructions. No file is read again.
    let turn = AgentTurnId::mint();
    for (active, events) in [
        (Some(turn), vec![]),
        (None, vec![AgentEvent::TurnCompleted { turn_id: turn }]),
    ] {
        let used = observation(events);
        let (later, source) = prepare(&request, needs_initial_prompt(active, &used), || {
            panic!("no reread")
        });
        assert_eq!(later, "PERSON-TASK");
        assert!(source.is_none());
    }
}

#[test]
fn ambiguous_or_evicted_turn_history_never_repeats_initial_instructions() {
    let turn = AgentTurnId::mint();
    for event in [
        AgentEvent::TurnStarted { turn_id: turn },
        AgentEvent::TurnCancelled {
            turn_id: turn,
            reason: Default::default(),
        },
        AgentEvent::ProviderState {
            provider: "claude".parse().unwrap(),
            state: ManagedState::Running,
        },
    ] {
        assert!(!needs_initial_prompt(None, &observation(vec![event])));
    }
    let mut evicted = observation(vec![]);
    evicted.earlier_truncated = true;
    assert!(!needs_initial_prompt(None, &evicted));
}

#[test]
fn fallback_notice_belongs_only_to_the_prompt_that_uses_it_and_debug_stays_exact() {
    let mut request = super::super::instructions_tests::request("MODE");
    let (text, snapshot) = prepare(&request, true, || {
        let mut source = session::bundled();
        source.notice = Some(koru_delegate_preamble::Notice::Unreadable);
        source
    });
    assert!(text.starts_with(session::BUNDLED));
    assert!(snapshot.unwrap().notice.is_some());
    assert!(text.contains("A missing connection means"));
    request.prompt_template_version = "debug-override-v1".into();
    let (debug, snapshot) = prepare(&request, true, || {
        panic!("replacement does not read common")
    });
    assert_eq!(debug, "MODE\n\nPERSON-TASK");
    assert!(snapshot.is_none());
}
