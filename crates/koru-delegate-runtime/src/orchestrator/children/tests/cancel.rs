// SPDX-License-Identifier: MPL-2.0

//! Cohorts `m6_child_timeout`, `m6_child_cancel` and `m6_child_late`
//! (contract 6.2, 6.4 and 6.3): a request's clock runs out, the parent is
//! stopped in each wait phase, and a child that ends after its request was
//! settled changes nothing. Only the Runs the wait names are touched.

use super::clock::count;
use super::*;
use crate::test_support::bot_submission;
use koru_delegate_core::BotSubmission;

/// Another Run of the parent's own Bot, queued behind the parent, that no
/// wait names.
fn unrelated(chain: &mut ChildChain) -> BotSubmission {
    let mut submission = bot_submission(0x40);
    submission.request.run = RunId::mint();
    submission.provenance.submission =
        koru_delegate_core::BotRequestId::from_uuid(uuid::Uuid::new_v4());
    submission.input.idempotency_key = koru_delegate_core::IdempotencyKey::mint();
    let device = device(chain);
    chain
        .orchestrator
        .bot_submit(submission.clone(), device)
        .expect("an unrelated queued Run");
    submission
}

fn stop_parent(chain: &mut ChildChain) {
    let key = BotRunKey {
        owner: chain.parent.owner,
        bot: chain.parent.bot,
        run: chain.parent.run,
    };
    let device = device(chain);
    // A person's Stop names the Run's current fence, as the admission view
    // reports it: the origin's, or none while a reserved successor has no
    // session yet. Stop answers "pending" while the cleanup is proven.
    let record = chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .bot_execution(key.run)
        .unwrap()
        .unwrap();
    let fence = chain
        .orchestrator
        .bot_admission_view(&record)
        .unwrap()
        .fence;
    let _ = chain.orchestrator.bot_stop(&key, device, fence.as_ref());
}

fn assert_children_stopped(chain: &mut ChildChain, requested: &ChildRequested) {
    for receipt in &requested.receipts {
        assert!(
            matches!(
                child_state(chain, receipt.child_run),
                BotAdmissionState::Settled | BotAdmissionState::Refused
            ),
            "child {:?} still runs",
            receipt.ordinal
        );
    }
}

#[test]
fn m6_child_timeout_settles_each_request_stops_its_child_and_resumes_the_parent() {
    let (mut chain, requested, mut bridge) = held(&[1, 2]);
    let other = unrelated(&mut chain);
    assert_eq!(count(&mut chain, BOT_CHILD_WAIT), 2);
    let after = requests(&mut chain);
    assert!(
        after
            .iter()
            .all(|receipt| receipt.state == ChildRequestState::Settled
                && receipt.settled_by == Some(ChildSettledBy::TimedOut))
    );
    assert_children_stopped(&mut chain, &requested);
    assert!(!chain.orchestrator.child_stops_owed());
    // The parent does not wait for those stops: it resumes.
    assert_eq!(advance(&mut chain, &mut bridge), Step::SuccessorReserved);
    assert_eq!(
        child_state(&mut chain, other.request.run),
        BotAdmissionState::Queued,
        "an unrelated Run is untouched"
    );
}

#[test]
fn m6_child_cancel_stop_while_hold_requested() {
    let mut chain = child_chain();
    let origin = chain.parent.origin_creation;
    chain
        .sessions
        .lock()
        .unwrap()
        .script_retirement(origin, koru_session_protocol::CreationLookup::NoEvidence);
    let (_, requested) = asked(&mut chain, &[1, 2]);
    let run = chain.parent.run;
    publish_owed(&mut chain.orchestrator, run).unwrap();
    let other = unrelated(&mut chain);
    stop_parent(&mut chain);
    let after = requests(&mut chain);
    assert!(
        after
            .iter()
            .all(|receipt| receipt.settled_by == Some(ChildSettledBy::ParentStopped))
    );
    assert_children_stopped(&mut chain, &requested);
    assert_eq!(
        execution(&mut chain).map(|execution| execution.wait.phase),
        Some(BotWaitPhase::CleanupPending)
    );
    assert_eq!(
        child_state(&mut chain, other.request.run),
        BotAdmissionState::Queued
    );
    // No successor is reserved for a stopped parent.
    let mut bridge = chain.parent.enrollment_bridge();
    assert!(matches!(
        advance(&mut chain, &mut bridge),
        Step::Waiting(_) | Step::Idle
    ));
    assert_eq!(chain.sessions.lock().unwrap().created().len(), 1);
}

#[test]
fn m6_child_cancel_stop_while_retired() {
    let (mut chain, requested, mut bridge) = held(&[1]);
    let other = unrelated(&mut chain);
    stop_parent(&mut chain);
    let after = requests(&mut chain);
    assert_eq!(after[0].settled_by, Some(ChildSettledBy::ParentStopped));
    assert_children_stopped(&mut chain, &requested);
    assert!(matches!(
        advance(&mut chain, &mut bridge),
        Step::Waiting(_) | Step::Idle
    ));
    assert_eq!(
        chain.sessions.lock().unwrap().created().len(),
        1,
        "no successor"
    );
    assert_eq!(
        child_state(&mut chain, other.request.run),
        BotAdmissionState::Queued
    );
}

#[test]
fn m6_child_cancel_stop_while_resume_reserved() {
    let (mut chain, _, mut bridge) = held(&[1]);
    let receipt = requests(&mut chain)[0].clone();
    stop_child(&mut chain, &receipt);
    assert_eq!(advance(&mut chain, &mut bridge), Step::SuccessorReserved);
    stop_parent(&mut chain);
    let after = execution(&mut chain);
    assert!(after.is_none_or(|execution| execution.wait.phase != BotWaitPhase::ResumeReserved));
    // The request a person settled stays as it was.
    assert_eq!(
        requests(&mut chain)[0].settled_by,
        Some(ChildSettledBy::ChildEnded)
    );
    let _ = advance(&mut chain, &mut bridge);
    assert_eq!(
        chain.sessions.lock().unwrap().created().len(),
        1,
        "the reserved successor is never created"
    );
}

/// A person stops only the child (decision 15): the parent resumes with
/// that request settled by the child's own end, and continues.
#[test]
fn m6_child_cancel_a_person_stops_only_the_child() {
    let (mut chain, _, mut bridge) = held(&[1, 2]);
    let first = requests(&mut chain)[0].clone();
    stop_child(&mut chain, &first);
    let after = requests(&mut chain);
    assert_eq!(after[0].settled_by, Some(ChildSettledBy::ChildEnded));
    assert_eq!(after[1].state, ChildRequestState::Open);
    assert_eq!(parent_state(&mut chain), BotAdmissionState::Executing);
    assert_eq!(
        advance(&mut chain, &mut bridge),
        Step::Waiting("bot_child_requests_open")
    );
}

#[test]
fn m6_child_late_settlement_after_timeout_changes_nothing() {
    let (mut chain, requested, _bridge) = held(&[1]);
    assert_eq!(count(&mut chain, BOT_CHILD_WAIT), 1);
    let timed_out = requests(&mut chain)[0].clone();
    assert_eq!(timed_out.settled_by, Some(ChildSettledBy::TimedOut));
    // The child's own end arrives later: its request is already settled.
    let _ = try_stop_child(&mut chain, &requested.receipts[0]);
    let after = requests(&mut chain)[0].clone();
    assert_eq!(after.settled_by, Some(ChildSettledBy::TimedOut));
    assert_eq!(after.revision, timed_out.revision);
}

#[test]
fn m6_child_late_settlement_after_parent_stop_changes_nothing() {
    let (mut chain, requested, _bridge) = held(&[1]);
    stop_parent(&mut chain);
    let stopped = requests(&mut chain)[0].clone();
    assert_eq!(stopped.settled_by, Some(ChildSettledBy::ParentStopped));
    let _ = try_stop_child(&mut chain, &requested.receipts[0]);
    count(&mut chain, BOT_CHILD_WAIT);
    let after = requests(&mut chain)[0].clone();
    assert_eq!(after.settled_by, Some(ChildSettledBy::ParentStopped));
    assert_eq!(after.revision, stopped.revision);
}
