// SPDX-License-Identifier: MPL-2.0

//! Runtime decision entry points over the real in-memory ledger and scripted
//! session host. Scripted runtime composition, not provider qualification.
use super::*;

pub(super) fn prompt(
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

pub(super) fn choice(intent: &str) -> koru_session_protocol::ApprovalChoice {
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
