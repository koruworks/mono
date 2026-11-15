// SPDX-License-Identifier: MPL-2.0
//! Each IPC16 decision arm reaches the real orchestrator and the real
//! in-memory ledger. The fixture's sessions/documents/capture ports panic on
//! any call, so an arm that answered without the orchestrator would be caught
//! by its code, and one that touched a provider would panic.
use super::*;
use koru_delegate_api::bots::{BotCommand, DecisionAnswerRequest, DecisionEditProposal};
use koru_delegate_core::decisions::{
    BotWaitId, DecisionAnswerChoice, DecisionAnswerId, DecisionDigest, DecisionEditAction,
    DecisionId, DecisionPayload, DecisionRevision, DecisionVersionVector,
};
use koru_delegate_core::{
    BotAccess, BotExecutionFence, BotId, BotOwner, BotPolicyBarrier, BotPolicyBarrierKey,
    BotRequestId, CapturedBotPolicy, DelegateBootId, FreshBotPolicyProof, NativeResultKey,
    NativeResultOperation, PrepareNativeResult, RunAttemptId,
};

fn commands(owner: BotOwner) -> Vec<BotCommand> {
    let fence = BotExecutionFence {
        owner,
        bot: BotId::mint(),
        run: RunId::mint(),
        attempt: RunAttemptId::mint(),
        generation: koru_delegate_core::AttemptGeneration::FIRST,
        session: koru_delegate_core::SessionId::mint(),
        turn: Some(koru_delegate_core::AgentTurnId::mint()),
    };
    let proof = FreshBotPolicyProof {
        global: BotPolicyBarrier {
            key: BotPolicyBarrierKey {
                owner,
                bot: fence.bot,
                request: BotRequestId::mint().into(),
            },
            config_revision: 1,
            policy_revision: 1,
            policy: CapturedBotPolicy {
                workspace: WorkspaceId::new(),
                destination: WorkspaceId::new(),
                access: BotAccess::Default,
                selected_pages: Default::default(),
            },
            effects_allowed: true,
        },
        routine: None,
    };
    let boot = DelegateBootId::mint();
    let payload = DecisionPayload::new(b"PRIVATE-EDIT".to_vec()).unwrap();
    vec![
        BotCommand::PrepareNativeDecision {
            fence: fence.clone(),
            approval: koru_delegate_core::ApprovalId::mint(),
            proof: proof.clone(),
            boot,
        },
        BotCommand::PrepareEditDecision {
            fence: fence.clone(),
            edit: DecisionEditProposal {
                target: koru_delegate_core::DocumentId::new(),
                action: DecisionEditAction::EditDocument,
                operation_id: uuid::Uuid::new_v4(),
                edit_fingerprint: [3; 32],
                expected_revision: DecisionVersionVector::new(vec![2, 1, 2, 2, 4]).unwrap(),
                payload: payload.clone(),
            },
            proof: proof.clone(),
            boot,
        },
        BotCommand::AnswerDecision {
            answer: DecisionAnswerRequest {
                owner,
                run: fence.run,
                decision: DecisionId::mint(),
                expected_revision: DecisionRevision::new(2).unwrap(),
                binding_digest: DecisionDigest::from_bytes([1; 32]),
                request: DecisionAnswerId::mint(),
                human: ProfileId::new(),
                choice: DecisionAnswerChoice::AllowOnce {},
                authority_digest: DecisionDigest::from_bytes([2; 32]),
            },
            proof: proof.clone(),
            boot,
        },
        BotCommand::ConsumeDecisionEdit {
            fence: fence.clone(),
            request: PrepareNativeResult {
                key: NativeResultKey {
                    run: fence.run,
                    attempt: fence.attempt,
                    generation: fence.generation,
                    turn: fence.turn.unwrap(),
                    operation_id: uuid::Uuid::new_v4(),
                },
                kind: NativeResultOperation::EditPage,
                destination: WorkspaceId::new(),
                target: Some(koru_delegate_core::DocumentId::new()),
                request_fingerprint: [3; 32],
                expected_revision: Some(vec![2, 1, 2, 2, 4]),
                config_revision: 1,
                policy_revision: 1,
                title: "PRIVATE-TITLE".into(),
            },
            payload,
            proof,
            boot,
        },
        BotCommand::ReadBotDecisionWait {
            owner,
            bot: fence.bot,
            run: fence.run,
            wait_id: BotWaitId::mint(),
            decision_id: DecisionId::mint(),
        },
    ]
}

#[test]
fn decision_arms_reach_the_orchestrator_and_its_ledger_refusal_not_a_placeholder() {
    let (mut actor, _, inbox) = actor();
    actor.device = DeviceId::new();
    let owner = BotOwner {
        user_scope: koru_store_core::UserScopeId::new(),
        device: actor.device,
    };
    for command in commands(owner) {
        let failure = actor.bot_command(command).unwrap_err();
        assert!(
            !matches!(
                failure.code.as_str(),
                "bot_control_unsupported" | "bot_lifecycle_unavailable" | "bot_owner_mismatch"
            ),
            "{}",
            failure.code
        );
        // An unknown Run is refused by the orchestrator/ledger itself.
        assert!(
            failure.code.starts_with("bot_") || failure.code.starts_with("delegate_"),
            "{}",
            failure.code
        );
        assert!(!failure.retryable, "{}", failure.code);
    }
    // The ledger read answers for the exact subject: an unknown owner Run is
    // a refusal, never Missing.
    let read = commands(owner).pop().unwrap();
    assert_eq!(actor.bot_command(read).unwrap_err().code, "bot_fenced");
    assert!(!actor.bot_drain_pending);
    assert!(matches!(inbox.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

#[test]
fn decision_arms_refuse_another_device_before_the_orchestrator() {
    let (mut actor, _, _) = actor();
    actor.device = DeviceId::new();
    let foreign = BotOwner {
        user_scope: koru_store_core::UserScopeId::new(),
        device: DeviceId::new(),
    };
    for command in commands(foreign) {
        assert_eq!(
            actor.bot_command(command).unwrap_err().code,
            "bot_owner_mismatch"
        );
    }
}
