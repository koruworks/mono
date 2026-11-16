// SPDX-License-Identifier: MPL-2.0

//! A store that does not compose the private-operation ledger refuses every
//! private command with `bot_private_unavailable`, before anything is read
//! or written. This harness's recording store is such a store.
use super::*;
use koru_delegate_core::{
    PrivateDispatcherId, PrivateEffectKey, PrivateEffectTransition, PrivateOperationProposal,
};

fn code(error: RunError) -> String {
    match error {
        RunError::Port(error) => error.code,
        other => format!("{other:?}"),
    }
}

#[test]
fn m5_private_actor_every_command_is_unavailable_without_the_private_ledger() {
    let (mut h, _, fence, _, _) = native_results::active();
    let device = fence.owner.device;
    let proposal: PrivateOperationProposal = crate::test_support::private_proposal();
    let dispatcher = PrivateDispatcherId::mint();
    let boot = h.orchestrator.delegate_boot();
    let proof = koru_delegate_core::FreshBotPolicyProof {
        global: crate::test_support::bot_policy(&crate::test_support::bot_submission(1), 1),
        routine: None,
    };
    let key = PrivateEffectKey {
        run: fence.run,
        operation_id: proposal.operation_id,
        attempt: fence.attempt,
        generation: fence.generation,
        turn: fence.turn.unwrap(),
    };
    let unavailable = "bot_private_unavailable";
    assert_eq!(
        code(
            h.orchestrator
                .bot_announce_private_dispatcher(fence.owner, dispatcher, device)
                .unwrap_err()
        ),
        unavailable
    );
    assert_eq!(
        code(
            h.orchestrator
                .bot_prepare_private_decision(&fence, device, &proposal, &proof, boot)
                .unwrap_err()
        ),
        unavailable
    );
    assert_eq!(
        code(
            h.orchestrator
                .bot_consume_private_decision(&fence, device, dispatcher, &proposal, &proof, boot)
                .unwrap_err()
        ),
        unavailable
    );
    assert_eq!(
        code(
            h.orchestrator
                .bot_prepare_private_effect(&fence, device, dispatcher, &proposal, &proof, boot)
                .unwrap_err()
        ),
        unavailable
    );
    assert_eq!(
        code(
            h.orchestrator
                .bot_advance_private_effect(
                    &key,
                    dispatcher,
                    &PrivateEffectTransition::Dispatched {},
                    device
                )
                .unwrap_err()
        ),
        unavailable
    );
    assert_eq!(
        code(
            h.orchestrator
                .bot_read_private_effect(fence.run, proposal.operation_id, dispatcher, device)
                .unwrap_err()
        ),
        unavailable
    );
    // The owner's device is still compared first.
    assert_eq!(
        code(
            h.orchestrator
                .bot_announce_private_dispatcher(
                    fence.owner,
                    dispatcher,
                    koru_delegate_core::DeviceId::new()
                )
                .unwrap_err()
        ),
        "bot_owner_mismatch"
    );
}
