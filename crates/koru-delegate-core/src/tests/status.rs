// SPDX-License-Identifier: MPL-2.0

//! Every (state, event) pair, and exactly the legal ones succeed.
//!
//! The table below is a second spelling of §7.5, §7.4, §7.6 and D36, written
//! as literal rows rather than as rules, so it cannot share a bug with the
//! function it checks. Every state in `every_state()` is crossed with every
//! event in `every_event()`; a pair not in the table must be refused, and a
//! pair in the table must produce exactly the state the row says.

use super::*;

use Attention::*;
use CancelReason::{Run, TakeoverBoundary};
use ControlState::{HandoverPending, Human, Orchestrator};
use FailureStage::{ProviderStart, Turn};
use PriorAttempt::{Ended, Unknown};
use RunEvent as E;
use RunStatus as S;
use TurnOriginator::{Human as ByHuman, Orchestrator as ByOrchestrator};

fn running(attention: &[Attention], control: ControlState) -> RunStatus {
    S::Running {
        attention: attention.iter().copied().collect(),
        control,
    }
}

fn hp(objective_satisfied: bool) -> ControlState {
    HandoverPending {
        objective_satisfied,
    }
}

fn human(objective_satisfied: bool) -> ControlState {
    Human {
        objective_satisfied,
    }
}

/// Every event variant, with every payload that changes the answer. The
/// `match` at the bottom is exhaustive on purpose: a new variant fails this
/// test's build until it is enumerated here.
fn every_event() -> Vec<RunEvent> {
    let events = vec![
        E::Start,
        E::ProviderStarted,
        E::StartFailed {
            stage: ProviderStart,
        },
        E::Interrupt {
            prior_attempt: Ended,
        },
        E::Interrupt {
            prior_attempt: Unknown,
        },
        E::Resume,
        E::Cancel,
        E::TurnStarted {
            originator: ByOrchestrator,
        },
        E::TurnStarted {
            originator: ByHuman,
        },
        E::TurnCompleted {
            originator: ByOrchestrator,
            artifacts_registered: 0,
        },
        E::TurnCompleted {
            originator: ByOrchestrator,
            artifacts_registered: 1,
        },
        E::TurnCompleted {
            originator: ByHuman,
            artifacts_registered: 0,
        },
        E::TurnCompleted {
            originator: ByHuman,
            artifacts_registered: 1,
        },
        E::TurnFailed {
            originator: ByOrchestrator,
            stage: Turn,
        },
        E::TurnFailed {
            originator: ByHuman,
            stage: Turn,
        },
        E::TurnCancelled {
            originator: ByOrchestrator,
            reason: Run,
        },
        E::TurnCancelled {
            originator: ByOrchestrator,
            reason: TakeoverBoundary,
        },
        E::TurnCancelled {
            originator: ByHuman,
            reason: Run,
        },
        E::TurnCancelled {
            originator: ByHuman,
            reason: TakeoverBoundary,
        },
        E::BotResponseDelivered,
        E::ApprovalRequired,
        E::ApprovalResolved,
        E::WaitingForInput,
        E::ChildSessionDetected,
        E::ChildSessionTerminated,
        E::HandoverRequested,
        E::ControlTaken,
        E::ControlReturned,
    ];
    for event in &events {
        match event {
            E::Start
            | E::ProviderStarted
            | E::StartFailed { .. }
            | E::Interrupt { .. }
            | E::Resume
            | E::Cancel
            | E::TurnStarted { .. }
            | E::TurnCompleted { .. }
            | E::TurnFailed { .. }
            | E::TurnCancelled { .. }
            | E::BotResponseDelivered
            | E::ApprovalRequired
            | E::ApprovalResolved
            | E::WaitingForInput
            | E::ChildSessionDetected
            | E::ChildSessionTerminated
            | E::HandoverRequested
            | E::ControlTaken
            | E::ControlReturned => {}
        }
    }
    events
}

/// Every state the table covers. The `match` is exhaustive for the same
/// reason as above.
fn every_state() -> Vec<RunStatus> {
    let states = vec![
        S::Created,
        S::Starting,
        S::Interrupted {
            prior_attempt: Ended,
        },
        S::Interrupted {
            prior_attempt: Unknown,
        },
        S::Completed,
        S::Failed { stage: Turn },
        S::Cancelled,
        S::AwaitingReview,
        S::WaitingForParticipant,
        running(&[], Orchestrator),
        running(&[ApprovalRequired], Orchestrator),
        running(&[WaitingForInput], Orchestrator),
        running(&[ChildSessionDetected], Orchestrator),
        running(&[FinishedNeedingAttention], Orchestrator),
        running(&[], hp(false)),
        running(&[], hp(true)),
        running(&[], human(false)),
        running(&[], human(true)),
        running(&[ApprovalRequired], human(true)),
    ];
    for state in &states {
        match state {
            S::Created
            | S::Starting
            | S::Running { .. }
            | S::Interrupted { .. }
            | S::Completed
            | S::Failed { .. }
            | S::Cancelled
            | S::AwaitingReview
            | S::WaitingForParticipant => {}
        }
    }
    states
}

type Row = (RunEvent, RunStatus);

/// The rows every `Running` state shares: a restart interrupts it, a person
/// cancels it, and the three provider asks add their marker.
fn shared_running_rows(attention: &[Attention], control: ControlState) -> Vec<Row> {
    let with = |marker: Attention| {
        let mut markers = attention.to_vec();
        markers.push(marker);
        running(&markers, control)
    };
    vec![
        (
            E::Interrupt {
                prior_attempt: Ended,
            },
            S::Interrupted {
                prior_attempt: Ended,
            },
        ),
        (
            E::Interrupt {
                prior_attempt: Unknown,
            },
            S::Interrupted {
                prior_attempt: Unknown,
            },
        ),
        (E::Cancel, S::Cancelled),
        (E::ApprovalRequired, with(ApprovalRequired)),
        (E::WaitingForInput, with(WaitingForInput)),
        (E::ChildSessionDetected, with(ChildSessionDetected)),
    ]
}

/// An original orchestrator turn can end under borrowed control once its
/// approvals close. `cleared` drops `WaitingForInput` from the attention.
fn orchestrator_turn_end_rows(cleared: &[Attention], control: ControlState) -> Vec<Row> {
    let finished = {
        let mut markers = cleared.to_vec();
        markers.push(FinishedNeedingAttention);
        running(&markers, control)
    };
    let delivered = match control {
        Orchestrator => S::Completed,
        HandoverPending { .. } => running(cleared, hp(true)),
        Human { .. } => running(cleared, human(true)),
    };
    let mut rows = vec![
        (
            E::TurnCompleted {
                originator: ByOrchestrator,
                artifacts_registered: 0,
            },
            finished,
        ),
        (
            E::TurnCompleted {
                originator: ByOrchestrator,
                artifacts_registered: 1,
            },
            delivered,
        ),
        (
            E::TurnFailed {
                originator: ByOrchestrator,
                stage: Turn,
            },
            S::Failed { stage: Turn },
        ),
        (
            E::TurnCancelled {
                originator: ByOrchestrator,
                reason: Run,
            },
            S::Cancelled,
        ),
    ];
    if control == Orchestrator {
        rows.push((E::BotResponseDelivered, S::Completed));
    }
    rows
}

/// The rows for a `Running` state under human control with no approval
/// open: the person's own turns are recorded and never terminalize, except
/// a deliberate run cancellation.
fn human_turn_rows(cleared: &[Attention], control: ControlState) -> Vec<Row> {
    vec![
        (
            E::TurnStarted {
                originator: ByHuman,
            },
            running(cleared, control),
        ),
        (
            E::TurnCompleted {
                originator: ByHuman,
                artifacts_registered: 0,
            },
            running(cleared, control),
        ),
        (
            E::TurnCompleted {
                originator: ByHuman,
                artifacts_registered: 1,
            },
            running(cleared, control),
        ),
        (
            E::TurnFailed {
                originator: ByHuman,
                stage: Turn,
            },
            running(cleared, control),
        ),
        (
            E::TurnCancelled {
                originator: ByHuman,
                reason: Run,
            },
            S::Cancelled,
        ),
    ]
}

/// The whole table: for each state, every legal event and where it goes.
fn table() -> Vec<(RunStatus, Vec<Row>)> {
    let mut table = vec![
        (
            S::Created,
            vec![
                (E::Start, S::Starting),
                (
                    E::StartFailed {
                        stage: ProviderStart,
                    },
                    S::Failed {
                        stage: ProviderStart,
                    },
                ),
                (E::Cancel, S::Cancelled),
            ],
        ),
        (
            S::Starting,
            vec![
                (E::ProviderStarted, running(&[], Orchestrator)),
                (
                    E::StartFailed {
                        stage: ProviderStart,
                    },
                    S::Failed {
                        stage: ProviderStart,
                    },
                ),
                (
                    E::Interrupt {
                        prior_attempt: Ended,
                    },
                    S::Interrupted {
                        prior_attempt: Ended,
                    },
                ),
                (
                    E::Interrupt {
                        prior_attempt: Unknown,
                    },
                    S::Interrupted {
                        prior_attempt: Unknown,
                    },
                ),
                (E::Cancel, S::Cancelled),
            ],
        ),
        (
            S::Interrupted {
                prior_attempt: Ended,
            },
            vec![(E::Resume, S::Starting), (E::Cancel, S::Cancelled)],
        ),
        (
            S::Interrupted {
                prior_attempt: Unknown,
            },
            vec![(E::Resume, S::Starting), (E::Cancel, S::Cancelled)],
        ),
        (S::Completed, vec![]),
        (S::Failed { stage: Turn }, vec![]),
        (S::Cancelled, vec![]),
        (S::AwaitingReview, vec![]),
        (S::WaitingForParticipant, vec![]),
    ];

    // Running, orchestrator in control, nothing pending.
    let mut rows = shared_running_rows(&[], Orchestrator);
    rows.push((
        E::TurnStarted {
            originator: ByOrchestrator,
        },
        running(&[], Orchestrator),
    ));
    rows.extend(orchestrator_turn_end_rows(&[], Orchestrator));
    rows.push((E::HandoverRequested, running(&[], hp(false))));
    rows.push((E::ControlTaken, running(&[], human(false))));
    table.push((running(&[], Orchestrator), rows));

    // Running with an approval open: no turn may start or end, but the run
    // can still be cancelled and control can still move.
    let mut rows = shared_running_rows(&[ApprovalRequired], Orchestrator);
    rows.push((E::ApprovalResolved, running(&[], Orchestrator)));
    rows.push((
        E::TurnCancelled {
            originator: ByOrchestrator,
            reason: Run,
        },
        S::Cancelled,
    ));
    rows.push((
        E::HandoverRequested,
        running(&[ApprovalRequired], hp(false)),
    ));
    rows.push((E::ControlTaken, running(&[ApprovalRequired], human(false))));
    table.push((running(&[ApprovalRequired], Orchestrator), rows));

    // Running, waiting for input: any turn boundary clears the wait.
    let mut rows = shared_running_rows(&[WaitingForInput], Orchestrator);
    rows.push((
        E::TurnStarted {
            originator: ByOrchestrator,
        },
        running(&[], Orchestrator),
    ));
    rows.extend(orchestrator_turn_end_rows(&[], Orchestrator));
    rows.push((E::HandoverRequested, running(&[WaitingForInput], hp(false))));
    rows.push((E::ControlTaken, running(&[WaitingForInput], human(false))));
    table.push((running(&[WaitingForInput], Orchestrator), rows));

    // Running with a child session detected: the flag persists across turns
    // and does not fence completion (§7.5 lists what does).
    let mut rows = shared_running_rows(&[ChildSessionDetected], Orchestrator);
    rows.push((E::ChildSessionTerminated, running(&[], Orchestrator)));
    rows.push((
        E::TurnStarted {
            originator: ByOrchestrator,
        },
        running(&[ChildSessionDetected], Orchestrator),
    ));
    rows.extend(orchestrator_turn_end_rows(
        &[ChildSessionDetected],
        Orchestrator,
    ));
    rows.push((
        E::HandoverRequested,
        running(&[ChildSessionDetected], hp(false)),
    ));
    rows.push((
        E::ControlTaken,
        running(&[ChildSessionDetected], human(false)),
    ));
    table.push((running(&[ChildSessionDetected], Orchestrator), rows));

    // Running, finished needing attention (D29): a new turn clears it; a
    // person can cancel or take control.
    let mut rows = shared_running_rows(&[FinishedNeedingAttention], Orchestrator);
    rows.push((
        E::TurnStarted {
            originator: ByOrchestrator,
        },
        running(&[], Orchestrator),
    ));
    rows.extend(orchestrator_turn_end_rows(
        &[FinishedNeedingAttention],
        Orchestrator,
    ));
    rows.push((
        E::HandoverRequested,
        running(&[FinishedNeedingAttention], hp(false)),
    ));
    rows.push((
        E::ControlTaken,
        running(&[FinishedNeedingAttention], human(false)),
    ));
    table.push((running(&[FinishedNeedingAttention], Orchestrator), rows));

    // Handover pending: the orchestrator submits nothing more; the turn in
    // flight may end any way, a completion is fenced, a takeover-boundary
    // cancel ends the step and not the job (D36).
    let mut rows = shared_running_rows(&[], hp(false));
    rows.extend(orchestrator_turn_end_rows(&[], hp(false)));
    rows.push((
        E::TurnStarted {
            originator: ByOrchestrator,
        },
        running(&[], hp(false)),
    ));
    rows.push((
        E::TurnCancelled {
            originator: ByOrchestrator,
            reason: TakeoverBoundary,
        },
        running(&[], hp(false)),
    ));
    rows.push((E::ControlTaken, running(&[], human(false))));
    table.push((running(&[], hp(false)), rows));

    // Handover pending with the objective already satisfied: the fence
    // carries into human control.
    let mut rows = shared_running_rows(&[], hp(true));
    rows.extend(orchestrator_turn_end_rows(&[], hp(true)));
    rows.push((
        E::TurnStarted {
            originator: ByOrchestrator,
        },
        running(&[], hp(true)),
    ));
    rows.push((
        E::TurnCancelled {
            originator: ByOrchestrator,
            reason: TakeoverBoundary,
        },
        running(&[], hp(true)),
    ));
    rows.push((E::ControlTaken, running(&[], human(true))));
    table.push((running(&[], hp(true)), rows));

    // Human control can temporarily borrow an original turn's approval
    // lease. Its completion is fenced until return; it stays the submitter's.
    let mut rows = shared_running_rows(&[], human(false));
    rows.extend(human_turn_rows(&[], human(false)));
    rows.extend(orchestrator_turn_end_rows(&[], human(false)));
    rows.push((
        E::TurnStarted {
            originator: ByOrchestrator,
        },
        running(&[], human(false)),
    ));
    rows.push((
        E::TurnCancelled {
            originator: ByOrchestrator,
            reason: TakeoverBoundary,
        },
        running(&[], human(false)),
    ));
    rows.push((E::ControlReturned, running(&[], Orchestrator)));
    table.push((running(&[], human(false)), rows));

    // Human control after a fenced completion: return completes the run.
    let mut rows = shared_running_rows(&[], human(true));
    rows.extend(human_turn_rows(&[], human(true)));
    rows.extend(orchestrator_turn_end_rows(&[], human(true)));
    rows.push((
        E::TurnStarted {
            originator: ByOrchestrator,
        },
        running(&[], human(true)),
    ));
    rows.push((
        E::TurnCancelled {
            originator: ByOrchestrator,
            reason: TakeoverBoundary,
        },
        running(&[], human(true)),
    ));
    rows.push((E::ControlReturned, S::Completed));
    table.push((running(&[], human(true)), rows));

    // Human control, fenced completion, approval open: the approval travels
    // with control, blocks the person's turns, and blocks the completion.
    let mut rows = shared_running_rows(&[ApprovalRequired], human(true));
    rows.push((E::ApprovalResolved, running(&[], human(true))));
    rows.push((
        E::TurnCancelled {
            originator: ByOrchestrator,
            reason: Run,
        },
        S::Cancelled,
    ));
    rows.push((
        E::TurnCancelled {
            originator: ByOrchestrator,
            reason: TakeoverBoundary,
        },
        running(&[ApprovalRequired], human(true)),
    ));
    rows.push((
        E::TurnCancelled {
            originator: ByHuman,
            reason: Run,
        },
        S::Cancelled,
    ));
    rows.push((
        E::ControlReturned,
        running(&[ApprovalRequired], Orchestrator),
    ));
    table.push((running(&[ApprovalRequired], human(true)), rows));

    table
}

#[test]
fn every_state_and_event_pair_matches_the_table() {
    let table = table();
    let states = every_state();
    let events = every_event();
    assert_eq!(
        table.iter().map(|(state, _)| state).collect::<Vec<_>>(),
        states.iter().collect::<Vec<_>>(),
        "the table covers every enumerated state, in order"
    );
    let mut checked = 0;
    for (state, rows) in &table {
        for event in &events {
            let expected = rows
                .iter()
                .filter(|(row_event, _)| row_event == event)
                .map(|(_, to)| to.clone())
                .collect::<Vec<_>>();
            assert!(
                expected.len() <= 1,
                "the table lists {event:?} twice for {state:?}"
            );
            let actual = transition(state, event);
            match expected.first() {
                Some(to) => assert_eq!(
                    actual.as_ref(),
                    Ok(to),
                    "{state:?} + {event:?} should reach {to:?}"
                ),
                None => assert_eq!(
                    actual,
                    Err(IllegalTransition {
                        from: state.clone(),
                        event: *event,
                    }),
                    "{state:?} + {event:?} must be refused"
                ),
            }
            checked += 1;
        }
    }
    assert_eq!(checked, states.len() * events.len());
}

#[test]
fn terminal_states_accept_nothing() {
    for state in [S::Completed, S::Failed { stage: Turn }, S::Cancelled] {
        assert!(state.is_terminal());
        for event in every_event() {
            assert!(transition(&state, &event).is_err(), "{state:?} + {event:?}");
        }
    }
}

#[test]
fn the_two_m5_states_are_minted_and_unreachable() {
    for state in every_state() {
        for event in every_event() {
            if let Ok(to) = transition(&state, &event) {
                assert!(
                    !matches!(to, S::AwaitingReview | S::WaitingForParticipant),
                    "{state:?} + {event:?} reached {to:?}"
                );
            }
        }
    }
    for state in [S::AwaitingReview, S::WaitingForParticipant] {
        assert!(
            !state.is_terminal(),
            "M5 decides what they are; M1 does not"
        );
        for event in every_event() {
            assert!(transition(&state, &event).is_err(), "{state:?} + {event:?}");
        }
    }
}

#[test]
fn only_a_run_cancellation_terminalizes() {
    assert!(Run.terminalizes());
    assert!(!TakeoverBoundary.terminalizes());
    for state in every_state() {
        for originator in [ByOrchestrator, ByHuman] {
            let event = E::TurnCancelled {
                originator,
                reason: TakeoverBoundary,
            };
            if let Ok(to) = transition(&state, &event) {
                assert!(!to.is_terminal(), "{state:?} + {event:?} reached {to:?}");
            }
        }
    }
}

#[test]
fn a_human_turn_never_completes_or_fails_the_run() {
    for state in every_state() {
        for event in [
            E::TurnCompleted {
                originator: ByHuman,
                artifacts_registered: 3,
            },
            E::TurnFailed {
                originator: ByHuman,
                stage: Turn,
            },
        ] {
            if let Ok(to) = transition(&state, &event) {
                assert!(
                    matches!(to, S::Running { .. }),
                    "{state:?} + {event:?} reached {to:?}"
                );
            }
        }
    }
}

#[test]
fn a_completion_racing_a_pending_takeover_is_fenced_and_lands_on_return() {
    let pending = transition(&S::running(), &E::HandoverRequested).unwrap();
    let fenced = transition(
        &pending,
        &E::TurnCompleted {
            originator: ByOrchestrator,
            artifacts_registered: 2,
        },
    )
    .unwrap();
    assert_eq!(fenced, running(&[], hp(true)));
    let taken = transition(&fenced, &E::ControlTaken).unwrap();
    assert_eq!(taken, running(&[], human(true)));
    let typed = transition(
        &taken,
        &E::TurnStarted {
            originator: ByHuman,
        },
    )
    .unwrap();
    assert_eq!(typed, taken);
    assert_eq!(
        transition(&typed, &E::ControlReturned).unwrap(),
        S::Completed
    );
}

#[test]
fn an_empty_result_is_finished_needing_attention_not_delivered() {
    let after = transition(
        &S::running(),
        &E::TurnCompleted {
            originator: ByOrchestrator,
            artifacts_registered: 0,
        },
    )
    .unwrap();
    assert_eq!(after, running(&[FinishedNeedingAttention], Orchestrator));
    assert!(!after.is_terminal());
    assert!(
        after
            .attention()
            .unwrap()
            .contains(FinishedNeedingAttention)
    );
}

#[test]
fn an_interruption_is_never_a_failure_and_resume_is_a_fresh_start() {
    let interrupted = transition(
        &running(&[ApprovalRequired], human(true)),
        &E::Interrupt {
            prior_attempt: Unknown,
        },
    )
    .unwrap();
    assert_eq!(
        interrupted,
        S::Interrupted {
            prior_attempt: Unknown
        }
    );
    assert_eq!(transition(&interrupted, &E::Resume).unwrap(), S::Starting);
    assert!(
        transition(
            &S::Created,
            &E::Interrupt {
                prior_attempt: Ended
            }
        )
        .is_err(),
        "an unsatisfied intent is completed forward, never interrupted"
    );
}

#[test]
fn illegal_transition_names_the_pair_without_text() {
    let refused = transition(&S::Completed, &E::Cancel).unwrap_err();
    assert_eq!(
        refused.to_string(),
        "illegal transition: completed cannot accept cancel"
    );
    assert_eq!(refused.from.name(), "completed");
    assert_eq!(refused.event.name(), "cancel");
}
