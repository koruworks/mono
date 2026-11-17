// SPDX-License-Identifier: MPL-2.0

//! Cohorts `m6_child_restart` (rows 8 to 13 of contract section 9: the owner
//! is rebuilt over the same in-memory ledger, documents and session host)
//! and `m6_child_stranded` (each row of the strand table ends as stated).

use super::clock::{count, tick};
use super::*;
use crate::Options;
use crate::test_support::admission_barrier;

/// A new process over the same ledger, documents and session host: a new
/// orchestrator (new delegate boot, empty working memory) and its restart.
fn rebuild(chain: &mut ChildChain) {
    let placeholder = crate::test_support::child_chain().orchestrator;
    let old = std::mem::replace(&mut chain.orchestrator, placeholder);
    let ports = old.into_ports();
    let mut orchestrator = Orchestrator::new(
        ports,
        Options {
            reask_attempts: 3,
            ..Options::default()
        },
    );
    orchestrator.restart().expect("the restart completes");
    // A new delegate run holds no team value until the AI worker's `Hello`
    // shows it the run; the worker then sends it (M8).
    crate::test_support::tell_team_never_paused(&mut orchestrator, chain.parent.owner);
    chain.orchestrator = orchestrator;
    chain.parent.boot = chain.orchestrator.delegate_boot();
}

/// Row 8: restarted while `hold_requested`. The Run is kept with its wait,
/// not settled, and the driver continues from the row.
#[test]
fn m6_child_restart_row8_hold_requested_continues_from_the_row() {
    let mut chain = child_chain();
    chain.parent.script_resume(
        &mut chain.sessions.lock().unwrap(),
        SessionId::mint(),
        AgentTurnId::mint(),
    );
    let (_, requested) = asked(&mut chain, &[1]);
    let run = chain.parent.run;
    publish_owed(&mut chain.orchestrator, run).unwrap();
    rebuild(&mut chain);
    assert_eq!(parent_state(&mut chain), BotAdmissionState::Executing);
    let still = execution(&mut chain).expect("the wait survives the restart");
    assert_eq!(still.wait.phase, BotWaitPhase::HoldRequested);
    let mut bridge = chain.parent.enrollment_bridge();
    assert_eq!(advance(&mut chain, &mut bridge), Step::Enrolled);
    assert_eq!(advance(&mut chain, &mut bridge), Step::OriginStopped);
    // Clocks restart under the new boot.
    tick(&mut chain);
    assert_eq!(requests(&mut chain)[0].clock, ChildClock::Running);
    assert_eq!(
        requests(&mut chain)[0].child_request,
        requested.receipts[0].child_request
    );
}

/// Row 9: restarted while `retired`. The wait stays, its clocks restart
/// from their folded values, and nothing is fenced by the old boot.
#[test]
fn m6_child_restart_row9_retired_keeps_the_wait_and_its_folded_clock() {
    let (mut chain, _, mut bridge) = held(&[1]);
    count(&mut chain, 120_000);
    let device = device(&chain);
    chain.orchestrator.bot_fold_child_clocks(device).unwrap();
    let folded = requests(&mut chain)[0].elapsed_ms;
    rebuild(&mut chain);
    // Ten minutes passed while nothing ran: not counted.
    chain.clock.fetch_add(600_000, Ordering::SeqCst);
    chain.orchestrator.set_child_clock_monotonic(Some(0));
    tick(&mut chain);
    let adopted = requests(&mut chain)[0].elapsed_ms;
    assert!(adopted <= folded + 1_000, "{folded} -> {adopted}");
    assert_eq!(
        execution(&mut chain).unwrap().wait.phase,
        BotWaitPhase::Retired
    );
    // The request still times out under the new boot.
    let left = BOT_CHILD_WAIT - u64::from(adopted);
    assert_eq!(count(&mut chain, left), 1);
    assert_eq!(advance(&mut chain, &mut bridge), Step::SuccessorReserved);
}

/// Rows 10 and 11: a queued child stays queued across the restart; its
/// request is still open and settles when the child is stopped.
#[test]
fn m6_child_restart_rows10_11_a_queued_child_stays_queued() {
    let (mut chain, requested, _bridge) = held(&[1]);
    rebuild(&mut chain);
    let child = requested.receipts[0].child_run;
    assert_eq!(child_state(&mut chain, child), BotAdmissionState::Queued);
    assert_eq!(requests(&mut chain)[0].state, ChildRequestState::Open);
    stop_child(&mut chain, &requested.receipts[0]);
    assert_eq!(
        requests(&mut chain)[0].settled_by,
        Some(ChildSettledBy::ChildEnded)
    );
}

/// Row 12: restarted between the last request settling and the resume.
/// The selector finds the wait from the index and the successor launches.
#[test]
fn m6_child_restart_row12_between_last_settlement_and_resume() {
    let (mut chain, requested, _) = held(&[1]);
    stop_child(&mut chain, &requested.receipts[0]);
    rebuild(&mut chain);
    let device = device(&chain);
    let offered = chain
        .orchestrator
        .bot_child_resume_runs(device, None)
        .unwrap();
    assert_eq!(offered.len(), 1);
    assert_eq!(offered[0].run, chain.parent.run);
    let mut bridge = chain.parent.enrollment_bridge();
    resume(&mut chain, &mut bridge);
    assert!(execution(&mut chain).is_none());
    let created = chain.sessions.lock().unwrap().created().len();
    assert_eq!(created, 2, "the origin and exactly one successor");
}

/// Row 13, `session_bound`: the successor was created and bound by the
/// earlier process. The new one installs it and resends the input under
/// the same key. No second successor.
#[test]
fn m6_child_restart_row13_session_bound_is_recovered_by_its_keys() {
    let (mut chain, requested, mut bridge) = held(&[1]);
    stop_child(&mut chain, &requested.receipts[0]);
    assert_eq!(advance(&mut chain, &mut bridge), Step::SuccessorReserved);
    assert_eq!(advance(&mut chain, &mut bridge), Step::SuccessorBound);
    rebuild(&mut chain);
    assert_eq!(advance(&mut chain, &mut bridge), Step::SuccessorInstalled);
    assert_eq!(advance(&mut chain, &mut bridge), Step::InputAcknowledged);
    let resume = execution(&mut chain).unwrap().resume.unwrap();
    let (_, _, _, creation) = resume.consumer_identity().unwrap();
    let script = chain.sessions.lock().unwrap();
    assert_eq!(script.creations_under(creation), 1, "no second creation");
    assert_eq!(script.inputs_with_key(resume.input_key), 1);
}

/// Row 13, `prepared`: the earlier process reserved the successor but its
/// creation's fate is unknown. The new process only looks it up by its key,
/// never creates it, and after two minutes of counted time of nothing but
/// "unknown" the wait is cleaned up (strand table, `resume_reserved`).
#[test]
fn m6_child_restart_row13_prepared_is_looked_up_never_created_again() {
    let (mut chain, requested, mut bridge) = held(&[1]);
    stop_child(&mut chain, &requested.receipts[0]);
    assert_eq!(advance(&mut chain, &mut bridge), Step::SuccessorReserved);
    let resume = execution(&mut chain).unwrap().resume.unwrap();
    let (_, _, _, creation) = resume.consumer_identity().unwrap();
    rebuild(&mut chain);
    chain
        .sessions
        .lock()
        .unwrap()
        .script_lookup(creation, koru_session_protocol::CreationLookup::NoEvidence);
    assert_eq!(
        advance(&mut chain, &mut bridge),
        Step::Waiting("bot_successor_creation_unknown")
    );
    count(&mut chain, CHILD_STALL_LIMIT_MS + 10_000);
    assert_eq!(
        advance(&mut chain, &mut bridge),
        Step::CleanupRequested("bot_child_stalled")
    );
    assert_eq!(chain.sessions.lock().unwrap().creations_under(creation), 0);
}

// ---------------------------------------------------------- m6_child_stranded

/// `hold_requested`, the AI worker never answers the enroll call: each
/// request's clock settles it, then the wait is cleaned up instead of
/// resumed.
#[test]
fn m6_child_stranded_enrollment_never_answers() {
    let mut chain = child_chain();
    asked(&mut chain, &[1]);
    let run = chain.parent.run;
    publish_owed(&mut chain.orchestrator, run).unwrap();
    let mut bridge = chain.parent.enrollment_bridge();
    bridge.refuse(true);
    let owner = chain.parent.owner;
    assert!(
        chain
            .orchestrator
            .bot_advance_child_resume(owner, run, &mut bridge)
            .is_err()
    );
    count(&mut chain, BOT_CHILD_WAIT);
    assert_eq!(
        requests(&mut chain)[0].settled_by,
        Some(ChildSettledBy::TimedOut)
    );
    assert_eq!(
        chain
            .orchestrator
            .bot_advance_child_resume(owner, run, &mut bridge)
            .unwrap(),
        Step::CleanupRequested("bot_origin_stop_unproven")
    );
}

/// `retired`, nothing open, a definitive refusal (the host can no longer
/// delegate): cleaned up at once.
#[test]
fn m6_child_stranded_definitive_refusal_is_cleaned_up_at_once() {
    let (mut chain, requested, mut bridge) = held(&[1]);
    stop_child(&mut chain, &requested.receipts[0]);
    chain.sessions.lock().unwrap().set_delegation(false);
    assert_eq!(
        advance(&mut chain, &mut bridge),
        Step::CleanupRequested("bot_successor_unsupported")
    );
    assert_eq!(
        execution(&mut chain).map(|execution| execution.wait.phase),
        Some(BotWaitPhase::CleanupPending)
    );
}

/// `retired`, nothing open, the parent Bot paused: the one state with no
/// timer. It waits however long, and resumes once unpaused.
#[test]
fn m6_child_stranded_paused_parent_waits_without_a_timer() {
    let (mut chain, requested, mut bridge) = held(&[1]);
    stop_child(&mut chain, &requested.receipts[0]);
    let device = device(&chain);
    let mut pause = admission_barrier(&chain.parent.submission);
    pause.epoch = 2;
    pause.paused = true;
    pause.request = koru_delegate_core::BotRequestId::from_uuid(uuid::Uuid::new_v4());
    chain.orchestrator.bot_barrier(&pause, device).unwrap();
    assert_eq!(
        advance(&mut chain, &mut bridge),
        Step::Waiting("bot_paused")
    );
    count(&mut chain, 3 * BOT_CHILD_WAIT);
    assert_eq!(
        advance(&mut chain, &mut bridge),
        Step::Waiting("bot_paused")
    );
    assert_eq!(
        execution(&mut chain).unwrap().wait.phase,
        BotWaitPhase::Retired
    );
}

/// `resume_reserved`, the successor's input is never acknowledged (its
/// creation never answers): cleaned up after two minutes of counted time.
#[test]
fn m6_child_stranded_unacknowledged_successor_is_cleaned_up() {
    let mut chain = child_chain();
    // Only the origin's retirement is scripted: no successor creation and
    // no continuation turn.
    let origin = chain.parent.origin_creation;
    chain
        .sessions
        .lock()
        .unwrap()
        .script_retirement(origin, koru_session_protocol::CreationLookup::NoEvidence);
    let (_, requested) = asked(&mut chain, &[1]);
    let run = chain.parent.run;
    publish_owed(&mut chain.orchestrator, run).unwrap();
    let mut bridge = chain.parent.enrollment_bridge();
    assert_eq!(advance(&mut chain, &mut bridge), Step::Enrolled);
    assert_eq!(advance(&mut chain, &mut bridge), Step::OriginStopped);
    stop_child(&mut chain, &requested.receipts[0]);
    assert_eq!(advance(&mut chain, &mut bridge), Step::SuccessorReserved);
    // The creation is asked for once; its answer is unknown.
    assert!(matches!(advance(&mut chain, &mut bridge), Step::Waiting(_)));
    count(&mut chain, CHILD_STALL_LIMIT_MS + 10_000);
    let mut last = advance(&mut chain, &mut bridge);
    if matches!(last, Step::Waiting(_)) {
        last = advance(&mut chain, &mut bridge);
    }
    assert_eq!(last, Step::CleanupRequested("bot_child_stalled"));
}
