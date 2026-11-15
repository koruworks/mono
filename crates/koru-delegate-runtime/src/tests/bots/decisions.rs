// SPDX-License-Identifier: MPL-2.0

//! Runtime decision entry points over the real in-memory ledger and scripted
//! session host. Scripted runtime composition, not provider qualification.
use super::*;

fn prompt(
    h: &mut Harness,
    fence: &BotExecutionFence,
    choices: Vec<koru_session_protocol::ApprovalChoice>,
) -> ApprovalId {
    let approval = ApprovalId::mint();
    h.agent(
        fence.run,
        AgentEvent::ApprovalRequired {
            turn_id: fence.turn.unwrap(),
            approval_id: approval,
            summary: "PRIVATE_PROMPT_SUMMARY".into(),
            choices,
        },
    );
    approval
}

fn choice(intent: &str) -> koru_session_protocol::ApprovalChoice {
    koru_session_protocol::ApprovalChoice {
        id: ApprovalChoiceId::mint(),
        intent: serde_json::from_str(&format!("\"{intent}\"")).unwrap(),
        label: "PRIVATE_CHOICE_LABEL".into(),
    }
}

#[test]
fn native_prompt_retains_only_choice_identity_and_intent_until_resolved() {
    let (mut h, _, fence, _, _) = native_results::active();
    let offered = vec![choice("allow"), choice("reject")];
    let approval = prompt(&mut h, &fence, offered.clone());
    let retained = h.orchestrator.runs[&fence.run]
        .attempt
        .as_ref()
        .unwrap()
        .native_prompts[&approval]
        .clone();
    assert_eq!(retained.len(), 2);
    assert_eq!(retained[0].id, offered[0].id);
    assert!(!format!("{retained:?}").contains("PRIVATE"));
    // A choiceless prompt is not decision-eligible; resolution drops choices.
    let bare = prompt(&mut h, &fence, Vec::new());
    assert!(
        !h.orchestrator.runs[&fence.run]
            .attempt
            .as_ref()
            .unwrap()
            .native_prompts
            .contains_key(&bare)
    );
    h.agent(
        fence.run,
        AgentEvent::ApprovalResolved {
            turn_id: fence.turn.unwrap(),
            approval_id: approval,
            decision: ApprovalDecision::Approve,
            choice: None,
        },
    );
    assert!(
        !h.orchestrator.runs[&fence.run]
            .attempt
            .as_ref()
            .unwrap()
            .native_prompts
            .contains_key(&approval)
    );
}

#[test]
fn native_decision_answer_is_forwarded_with_the_journaled_key_and_replays_it() {
    let (mut h, s, fence, policy, _) = native_results::active();
    h.orchestrator
        .synchronize_native_policy(policy.clone(), fence.owner.device)
        .unwrap();
    let offered = vec![choice("allow"), choice("reject")];
    let approval = prompt(&mut h, &fence, offered.clone());
    let proof = FreshBotPolicyProof {
        global: policy,
        routine: None,
    };
    let boot = DelegateBootId::mint();
    let prepared = h
        .orchestrator
        .bot_prepare_native_decision(&fence, fence.owner.device, approval, &proof, boot)
        .unwrap();
    let DecisionBindingV1::NativeApproval(binding) = &prepared.receipt.binding else {
        panic!("native binding")
    };
    assert_eq!(binding.prompt, approval);
    assert_eq!(binding.choices.as_slice().len(), 2);
    assert_eq!(
        binding.native_deadline_at_ms, None,
        "O116: no native expiry"
    );
    // A second preparation for the same Run is owned by the open decision.
    let ActorRef::Person { profile } = s.request.requested_by else {
        panic!("person")
    };
    let input = AnswerDecision {
        owner: fence.owner,
        run: fence.run,
        decision: prepared.receipt.decision,
        expected_revision: prepared.receipt.state.revision,
        binding_digest: prepared.receipt.binding_digest,
        request: DecisionAnswerId::mint(),
        human: profile,
        choice: DecisionAnswerChoice::Native {
            offered: binding.choices.as_slice()[0].clone(),
        },
        authority_digest: DecisionDigest::from_bytes([9; 32]),
    };
    let answered = h
        .orchestrator
        .bot_answer_decision(&input, fence.owner.device, &proof, boot)
        .unwrap();
    let Some(DecisionNativeResponse::Acknowledged { key, .. }) = answered.native_response else {
        panic!("acknowledged native answer")
    };
    let forwarded: Vec<_> = h
        .sessions
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter_map(|call| match call {
            Call::AnswerApproval {
                key, approval: a, ..
            } if *a == approval => Some(*key),
            _ => None,
        })
        .collect();
    assert_eq!(forwarded, vec![key.response_key]);
    // The legacy unkeyed route cannot bypass the decision owner while the
    // Run's publication is held for it.
    if h.orchestrator.run_publication_pending(fence.run) {
        assert!(
            h.orchestrator
                .bot_answer_approval(
                    &fence,
                    fence.owner.device,
                    approval,
                    ApprovalDecision::Approve,
                    None
                )
                .is_err()
        );
    }
}
