// SPDX-License-Identifier: MPL-2.0

//! The builder's public surface is enough for a dependent: this drives the
//! resume driver from the prepared decision to the acknowledged successor
//! using only what `koru_delegate_runtime::test_support` and the crate root
//! export (no private field, no crate-internal item). Scripted runtime
//! composition, not provider qualification. No disk, Store or provider.

use crate::DecisionResumeStep as Step;
use crate::test_support::{DecisionChain, SessionCall, decision_chain};
use koru_delegate_core::{AgentTurnId, SessionId};

#[test]
fn the_public_decision_chain_drives_the_resume_driver_to_one_acknowledged_successor() {
    let DecisionChain {
        mut orchestrator,
        sessions,
        prepared,
        ..
    } = decision_chain();
    let (owner, run) = (prepared.owner, prepared.run);

    // The state the builder promises: the decision owns the Run's
    // publication here and is the one open wait the driver is offered.
    assert!(orchestrator.run_publication_pending(run));
    assert_eq!(
        orchestrator.bot_decision_resume_runs(None).unwrap(),
        vec![(owner, run)]
    );
    assert!(
        prepared
            .successor_keys(&mut orchestrator)
            .unwrap()
            .is_none()
    );
    {
        let script = sessions.lock().unwrap();
        assert_eq!(script.creations_under(prepared.origin_creation), 1);
        assert_eq!(script.created().len(), 1);
    }

    let successor = SessionId::mint();
    let turn = AgentTurnId::mint();
    prepared.script_resume(&mut sessions.lock().unwrap(), successor, turn);
    let mut bridge = prepared.enrollment_bridge();
    let mut advance = |orchestrator: &mut crate::Orchestrator| {
        orchestrator
            .bot_advance_decision_resume(owner, run, &mut bridge)
            .unwrap()
    };

    for expected in [
        Step::EnrollmentRequested,
        Step::Enrolled,
        Step::OriginStopped,
        Step::Waiting("bot_answer_pending"),
    ] {
        assert_eq!(advance(&mut orchestrator), expected);
    }
    // The person allows the edit once, through the public answer route.
    prepared.allow_once(&mut orchestrator).unwrap();
    for expected in [
        Step::SuccessorReserved,
        Step::SuccessorBound,
        Step::SuccessorInstalled,
        Step::InputAcknowledged,
        Step::Waiting("bot_consume_owned"),
    ] {
        assert_eq!(advance(&mut orchestrator), expected);
    }
    // A later turn neither creates nor sends again.
    assert_eq!(
        advance(&mut orchestrator),
        Step::Waiting("bot_consume_owned")
    );
    drop(advance);
    assert_eq!(bridge.requests(), 1, "one enrollment request");

    let keys = prepared.successor_keys(&mut orchestrator).unwrap().unwrap();
    assert_eq!(keys.session, Some(successor));
    assert_eq!(
        keys.generation.get(),
        prepared.fence.generation.get() + 1,
        "the successor is the next generation"
    );
    {
        let script = sessions.lock().unwrap();
        // Exactly one creation under the reserved key, one under the origin's.
        assert_eq!(script.creations_under(keys.creation), 1);
        assert_eq!(script.creations_under(prepared.origin_creation), 1);
        assert_eq!(
            script.created(),
            &[
                (prepared.origin_creation, prepared.fence.session),
                (keys.creation, successor)
            ][..]
        );
        // The origin was terminated and retired by its own creation key.
        assert!(
            script
                .calls()
                .contains(&SessionCall::Terminate(prepared.fence.session))
        );
        assert!(
            script
                .calls()
                .contains(&SessionCall::Retire(prepared.origin_creation))
        );
        // The controller was taken, and one continuation input was sent
        // under the retained key and acknowledged as the scripted turn.
        assert!(script.attached(successor));
        assert_eq!(script.inputs_with_key(keys.input), 1);
        assert_eq!(script.inputs_to(successor), vec![keys.input]);
        assert_eq!(script.acknowledged_turn(keys.input), Some(turn));
        assert!(
            !script
                .calls()
                .iter()
                .any(|call| matches!(call, SessionCall::Refused(_))),
            "nothing unscripted was asked: {:?}",
            script.calls()
        );
    }
    // The ledger recorded that acknowledgment for exactly this successor.
    let acknowledged = prepared
        .acknowledged_continuation(&mut orchestrator)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            acknowledged.decision,
            acknowledged.attempt,
            acknowledged.generation,
            acknowledged.session,
            acknowledged.turn
        ),
        (
            prepared.decision,
            keys.attempt,
            keys.generation,
            successor,
            turn
        )
    );
}
