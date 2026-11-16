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
    let text = continuation_body(
        "TASK-SENTINEL",
        prepared.decision,
        &ContinuationSubject::Edit(edit),
    )
    .unwrap();
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

/// M5 (D8): a private read's continuation names the tool and three ids and
/// nothing else. The whole body is pinned, so no title, account label, link,
/// digest or content can be added without changing this test.
#[test]
fn m5_private_continuation_body_holds_ids_only() {
    use crate::test_support::private_decision_chain;
    let mut chain = private_decision_chain();
    let prepared = chain.prepared.clone();
    let record = chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .bot_decision(prepared.owner, prepared.run, prepared.decision)
        .unwrap()
        .unwrap();
    let DecisionBindingV1::PrivateOperation(read) = &record.receipt.binding else {
        panic!("a private decision")
    };
    let subject = ContinuationSubject::of(&record.receipt.binding).expect("it resumes");
    assert!(matches!(subject, ContinuationSubject::PrivateRead(_)));
    let text = continuation_body("TASK-SENTINEL", prepared.decision, &subject).unwrap();
    assert_eq!(
        text,
        format!(
            "TASK-SENTINEL\n\nKoru resumed this run in a new session after the person allowed decision `{decision}`. The earlier session's conversation is not available here; results it already wrote remain.\n\nThe person approved exactly one read. To perform it, call `read_connection_item` once with operation_id `{operation}`, connection_id `{connection}` and item_id `{item}`. That performs the one approved read and returns its content. It can never read twice. Then continue the task.",
            decision = prepared.decision,
            operation = read.operation_id,
            connection = read.connection,
            item = read.item,
        )
    );
    // The two digests and the fingerprint the binding holds never reach the
    // model: not as hex and not as a byte list.
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    for digest in [
        hex(read.account.as_bytes()),
        hex(read.item_ref.as_bytes()),
        hex(&read.request_fingerprint),
    ] {
        assert!(!text.contains(&digest));
    }
    // It never claims an outcome: the read has not happened when it is sent.
    let lower = text.to_lowercase();
    for claim in ["was read", "has been read", "succeeded", "here is"] {
        assert!(!lower.contains(claim), "the continuation claims {claim:?}");
    }
}

/// The fingerprint a successor reservation binds is a function of retained
/// inputs only: the same for every key it is asked under, and on every call.
#[test]
fn m5_private_continuation_fingerprint_is_stable_and_key_free() {
    use crate::test_support::{decision_chain, private_decision_chain};
    let mut chain = private_decision_chain();
    let prepared = chain.prepared.clone();
    let record = chain
        .orchestrator
        .bot_ledger()
        .unwrap()
        .bot_decision(prepared.owner, prepared.run, prepared.decision)
        .unwrap()
        .unwrap();
    let expected = DecisionExpected {
        owner: prepared.owner,
        run: prepared.run,
        decision: prepared.decision,
        revision: record.receipt.state.revision,
        binding_digest: record.receipt.binding_digest,
    };
    let (first_key, second_key) = (IdempotencyKey::mint(), IdempotencyKey::mint());
    let (first, first_print) = chain
        .orchestrator
        .decision_continuation(prepared.owner, prepared.run, &expected, first_key)
        .unwrap();
    let (second, second_print) = chain
        .orchestrator
        .decision_continuation(prepared.owner, prepared.run, &expected, second_key)
        .unwrap();
    assert_eq!(
        first_print, second_print,
        "the key is not in the fingerprint"
    );
    assert_eq!(first.text, second.text);
    assert_eq!(first.idempotency_key, first_key);
    assert_eq!(second.idempotency_key, second_key);
    assert!(first.attachment_ids.is_empty());
    assert!(first.text.contains("read_connection_item"));
    // A stale binding digest is not continued.
    let mut stale = expected.clone();
    stale.binding_digest = DecisionDigest::from_bytes([0; 32]);
    assert!(
        chain
            .orchestrator
            .decision_continuation(prepared.owner, prepared.run, &stale, first_key)
            .is_err()
    );
    // An edit decision still continues as an edit.
    let mut edit = decision_chain();
    let prepared = edit.prepared.clone();
    let record = edit
        .orchestrator
        .bot_ledger()
        .unwrap()
        .bot_decision(prepared.owner, prepared.run, prepared.decision)
        .unwrap()
        .unwrap();
    assert!(matches!(
        ContinuationSubject::of(&record.receipt.binding),
        Some(ContinuationSubject::Edit(_))
    ));
}
