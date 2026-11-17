// SPDX-License-Identifier: MPL-2.0

//! Cohort `m6_child_routine_parent` (contract 6.7, decision 17): a Routine
//! Run asks, its child is an ordinary version 1 submission, and its
//! successor is generation 2 with the Routine provenance kept as admitted.
//! None of the decision path's Routine refusals is reached.

use super::*;

#[test]
fn m6_child_routine_parent_asks_and_resumes_with_its_routine_provenance() {
    let mut chain = routine_child_chain();
    let routine = chain
        .parent
        .submission
        .provenance
        .routine
        .expect("a Routine Run");
    let (mut chain, requested, mut bridge) = {
        let successor = SessionId::mint();
        let turn = AgentTurnId::mint();
        chain
            .parent
            .script_resume(&mut chain.sessions.lock().unwrap(), successor, turn);
        let request = chain.parent.ask(&mut chain.orchestrator, &[1]);
        // The binding names the parent as a Routine submission (version 2);
        // each child is an ordinary interactive submission (version 1).
        assert_eq!(
            request.requests[0].binding.parent_submission,
            koru_delegate_core::ROUTINE_BOT_SUBMISSION_VERSION
        );
        assert_eq!(request.requests[0].binding.parent_routine, Some(routine));
        assert!(request.requests[0].submission.provenance.routine.is_none());
        let requested = match send(&mut chain, &request) {
            ChildRequestOutcome::Created(requested) => requested,
            other => panic!("a Routine Run may ask: {other:?}"),
        };
        let run = chain.parent.run;
        publish_owed(&mut chain.orchestrator, run).unwrap();
        let mut bridge = chain.parent.enrollment_bridge();
        assert_eq!(advance(&mut chain, &mut bridge), Step::Enrolled);
        assert_eq!(advance(&mut chain, &mut bridge), Step::OriginStopped);
        (chain, requested, bridge)
    };
    let child = requested.receipts[0].child_run;
    let admitted_child = chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .bot_admission(child)
        .unwrap()
        .unwrap();
    assert_eq!(
        u32::from(admitted_child.submission_version),
        koru_delegate_core::INTERACTIVE_BOT_SUBMISSION_VERSION
    );
    stop_child(&mut chain, &requested.receipts[0]);
    resume(&mut chain, &mut bridge);
    let run = chain.parent.run;
    let admitted = chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .bot_admission(run)
        .unwrap()
        .unwrap();
    assert_eq!(admitted.submission, chain.parent.submission);
    assert_eq!(admitted.submission.provenance.routine, Some(routine));
    assert_eq!(admitted.state, BotAdmissionState::Executing);
    let row = chain
        .orchestrator
        .ports()
        .store
        .current_attempt(run)
        .unwrap()
        .unwrap();
    assert_eq!(
        row.generation.get(),
        chain.parent.fence.generation.get() + 1
    );
}

/// A Routine whose access was edited while its Run waited cannot be
/// resumed under the access it asked with: the reservation's Routine check
/// fails and the wait is cleaned up (contract section 9 row 22), and no
/// successor is ever created.
#[test]
fn m6_child_routine_parent_whose_access_was_edited_is_cleaned_up() {
    use super::clock::count;
    let (mut chain, requested, mut bridge) = held_in(routine_child_chain(), &[1]);
    stop_child(&mut chain, &requested.receipts[0]);
    let mut grant = chain
        .parent
        .proof
        .routine
        .clone()
        .expect("the Routine grant");
    grant.grant_revision = 2;
    grant.request = koru_delegate_core::BotRequestId::from_uuid(uuid::Uuid::new_v4()).into();
    grant.effects_allowed = false;
    let device = device(&chain);
    chain
        .orchestrator
        .synchronize_routine_policy(grant, device)
        .expect("the edited Routine access is synchronized");
    let (owner, run) = (chain.parent.owner, chain.parent.run);
    let mut cleaned = None;
    for _ in 0..4 {
        match chain
            .orchestrator
            .bot_advance_child_resume(owner, run, &mut bridge)
        {
            Ok(Step::CleanupRequested(code)) => {
                cleaned = Some(code);
                break;
            }
            Ok(step) => assert!(
                !matches!(step, Step::SuccessorReserved),
                "reserved under edited access"
            ),
            Err(_) => {}
        }
        count(&mut chain, CHILD_STALL_LIMIT_MS);
    }
    assert!(cleaned.is_some(), "the wait was cleaned up");
    assert_eq!(
        chain.sessions.lock().unwrap().created().len(),
        1,
        "no successor"
    );
}
