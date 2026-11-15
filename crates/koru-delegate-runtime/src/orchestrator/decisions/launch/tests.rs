// SPDX-License-Identifier: MPL-2.0

//! The successor's continuation text under O133 design B, built from a real
//! retained binding (the public decision-chain builder over the in-memory
//! ledger). No disk, Store or provider.

use super::*;
use crate::test_support::decision_chain;
use koru_delegate_core::decisions::DecisionBindingV1;

#[test]
fn the_continuation_states_approval_target_operation_and_do_not_repeat_never_saved() {
    let mut chain = decision_chain();
    let prepared = chain.prepared.clone();
    let record = chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .bot_decision(prepared.owner, prepared.run, prepared.decision)
        .unwrap()
        .unwrap();
    let DecisionBindingV1::KoruEdit(edit) = &record.receipt.binding else {
        panic!("a Koru edit decision")
    };
    let text = continuation_body("TASK-SENTINEL", prepared.decision, edit).unwrap();
    assert!(text.starts_with("TASK-SENTINEL"));
    assert!(text.contains("approved exactly one edit"));
    assert!(text.contains(&edit.target.to_string()));
    assert!(text.contains(&edit.operation_id.to_string()));
    assert!(text.contains("Koru is applying it for you now"));
    assert!(text.contains("Do not repeat it"));
    assert!(text.contains("never applies it twice"));
    // The exact arguments stay, so the existing exact retry reads the outcome
    // (or saves once if Koru could not).
    assert!(text.contains("PRIVATE-EDIT"));
    // It is sent before any outcome exists: it never claims one.
    let lower = text.to_lowercase();
    for claim in ["saved", "succeeded", "was applied", "has been applied"] {
        assert!(!lower.contains(claim), "the continuation claims {claim:?}");
    }
}
