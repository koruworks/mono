// SPDX-License-Identifier: MPL-2.0

use crate::*;

fn failure() -> SessionFailure {
    SessionFailure {
        code: ErrorCode::SpawnFailed,
        message: "child did not start".into(),
    }
}

#[test]
fn process_transition_table_is_exhaustive() {
    let states = [
        ProcessState::Starting,
        ProcessState::Running,
        ProcessState::Exited { code: Some(0) },
        ProcessState::Failed { error: failure() },
    ];
    let expected = [
        [true, true, true, true],
        [false, true, true, true],
        [false, false, true, false],
        [false, false, false, true],
    ];
    for (from_index, from) in states.iter().enumerate() {
        for (to_index, to) in states.iter().enumerate() {
            assert_eq!(
                from.can_transition_to(to),
                expected[from_index][to_index],
                "{from:?} -> {to:?}"
            );
        }
    }
}

#[test]
fn controller_presence_never_changes_process_state() {
    let mut summary = sample_summary();
    let process = summary.process.clone();
    summary = summary.with_controller(ControllerState::Detached);
    assert_eq!(summary.process, process);
    summary = summary.with_controller(ControllerState::Controlled);
    assert_eq!(summary.process, process);
}

#[test]
fn controller_presence_transition_table_is_explicit() {
    let states = [ControllerState::Detached, ControllerState::Controlled];
    for from in states {
        for to in states {
            assert!(from.can_transition_to(to), "{from:?} -> {to:?}");
        }
    }
}

#[test]
fn managed_turn_transition_table_is_exhaustive() {
    use ManagedState::{
        Cancelled, Completed, Failed, Idle, Running, WaitingForApproval, WaitingForInput,
    };
    let states = [
        Idle,
        Running,
        WaitingForInput,
        WaitingForApproval,
        Completed,
        Cancelled,
        Failed,
    ];
    let expected = [
        [true, true, false, false, false, true, true],
        [false, true, true, true, true, true, true],
        [false, true, true, false, true, true, true],
        [false, true, false, true, true, true, true],
        [false, false, false, false, true, false, false],
        [false, false, false, false, false, true, false],
        [false, false, false, false, false, false, true],
    ];
    for (from_index, from) in states.iter().enumerate() {
        for (to_index, to) in states.iter().enumerate() {
            assert_eq!(
                from.can_transition_to(*to),
                expected[from_index][to_index],
                "{from:?} -> {to:?}"
            );
        }
    }
}

#[test]
fn raw_and_managed_state_are_orthogonal_and_validated() {
    let mut summary = sample_summary();
    assert_eq!(summary.validate(), Ok(()));
    summary.managed = Some(ManagedState::Running);
    assert_eq!(summary.validate(), Err(ErrorCode::InvalidRequest));
}

pub(super) fn sample_summary() -> SessionSummary {
    SessionSummary {
        id: super::session_id(),
        parent_session_id: None,
        profile: SessionProfileKey::BuiltIn("powershell".parse().unwrap()),
        display_name: "PowerShell".into(),
        working_directory: None,
        mode: SessionMode::RawPty,
        process: ProcessState::Running,
        controller: ControllerState::Controlled,
        managed: None,
        active_turn_id: None,
        attention: AttentionState::default(),
        pending_attachment_count: 0,
        capabilities: CapabilitySet::default(),
        project_id: None,
        handover: HandoverState::Idle,
        model: None,
        origin: Default::default(),
        created_at_ms: 42,
    }
}

#[test]
fn positive_counters_reject_zero() {
    assert_eq!(StreamSequence::new(0), Err(ErrorCode::InvalidRequest));
    assert!(serde_json::from_str::<ViewportGeneration>("0").is_err());
}

#[test]
fn managed_agent_initial_cursor_precedes_the_first_emitted_event() {
    let baseline = StreamSequence::new(AGENT_EVENT_INITIAL_CURSOR).unwrap();
    assert_eq!(baseline.next().unwrap().get(), AGENT_EVENT_FIRST_SEQUENCE);
    assert_eq!(
        serde_json::to_value(baseline).unwrap(),
        serde_json::json!(1)
    );
}
