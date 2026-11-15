// SPDX-License-Identifier: MPL-2.0

use super::*;

fn settled() -> DecisionReceipt {
    let mut receipt = receipt();
    receipt.state = DecisionState {
        kind: DecisionKind::KoruEdit,
        authorization: DecisionAuthorization::Canceled,
        phase: DecisionPhase::Settled,
        revision: rev(2),
    };
    receipt.closure = Some(DecisionClosure {
        reason: DecisionClosureReason::Canceled,
        settled_at_ms: 10,
    });
    receipt.updated_at_ms = 10;
    receipt
}

#[test]
fn decision_outcome_golden_commits_literal_terminal_semantics() {
    let receipt = settled();
    let literal = include_str!("outcome-v1.json").trim_end().as_bytes();
    assert_eq!(
        super::super::outcome::canonical_bytes(&receipt).unwrap(),
        literal
    );
    let mut reference = blake3::Hasher::new();
    reference.update(b"koru.bot.decision.outcome.v1");
    reference.update(literal);
    assert_eq!(
        decision_outcome_digest(&receipt).unwrap().as_bytes(),
        reference.finalize().as_bytes()
    );
}

#[test]
fn decision_outcome_rejects_open_or_invalid_binding_and_ignores_mutable_observations() {
    assert!(decision_outcome_digest(&receipt()).is_err());
    let mut bad = settled();
    bad.binding_digest = digest(99);
    assert!(decision_outcome_digest(&bad).is_err());
    let original = settled();
    let expected = decision_outcome_digest(&original).unwrap();
    let mut observed = original.clone();
    observed.updated_at_ms = 999_999;
    observed.last_observed_at_ms = 999_998;
    assert_eq!(decision_outcome_digest(&observed).unwrap(), expected);
    let mut mutations = Vec::new();
    let mut changed = original.clone();
    changed.decision = id(99);
    mutations.push(changed);
    let mut changed = original.clone();
    changed.deadline.created_boot = id(99);
    mutations.push(changed);
    let mut changed = original.clone();
    changed.deadline.created_at_ms += 1;
    changed.deadline.deadline_at_ms += 1;
    mutations.push(changed);
    let mut changed = original.clone();
    changed.state.revision = rev(3);
    mutations.push(changed);
    let mut changed = original.clone();
    changed.state.authorization = DecisionAuthorization::Expired;
    mutations.push(changed);
    let mut changed = original.clone();
    changed.closure.as_mut().unwrap().reason = DecisionClosureReason::ClockUncertain;
    mutations.push(changed);
    let mut changed = original.clone();
    changed.closure.as_mut().unwrap().settled_at_ms += 1;
    mutations.push(changed);
    let mut changed = original.clone();
    if let DecisionBindingV1::KoruEdit(binding) = &mut changed.binding {
        binding.payload = DecisionBytes::new(b"{\"x\":1}".to_vec()).unwrap();
    }
    changed.binding_digest = changed.binding.digest().unwrap();
    mutations.push(changed);
    let mut answered = original.clone();
    answered.answer = Some(DecisionAnswerReceipt {
        decision: answered.decision,
        request: id(40),
        expected_revision: rev(1),
        committed_revision: rev(2),
        binding_digest: answered.binding_digest,
        human_profile: edit().human_profile,
        choice: DecisionAnswerChoice::AllowOnce {},
        answered_at_ms: 0,
        authority_digest: digest(7),
    });
    mutations.push(answered.clone());
    let answer_digest = decision_outcome_digest(&answered).unwrap();
    answered.answer.as_mut().unwrap().authority_digest = digest(8);
    assert_ne!(decision_outcome_digest(&answered).unwrap(), answer_digest);
    answered.state.authorization = DecisionAuthorization::Consumed;
    answered.consumed_at_ms = Some(1);
    let consumed = decision_outcome_digest(&answered).unwrap();
    answered.consumed_at_ms = Some(2);
    assert_ne!(decision_outcome_digest(&answered).unwrap(), consumed);
    for changed in mutations {
        assert_ne!(decision_outcome_digest(&changed).unwrap(), expected);
    }
}
