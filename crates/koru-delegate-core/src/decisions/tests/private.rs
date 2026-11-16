// SPDX-License-Identifier: MPL-2.0

//! Cohorts `m5_private_binding` and `m5_private_state`.
use super::*;

pub(super) fn private() -> DecisionPrivateBinding {
    let e = edit();
    let connection = ConnectionId::from_uuid(uuid(40));
    let item = ConnectionItemId::from_uuid(uuid(41));
    let item_ref = digest(7);
    DecisionPrivateBinding {
        version: e.version,
        owner: e.owner,
        bot: e.bot,
        run: e.run,
        origin_attempt: e.origin_attempt,
        origin_generation: e.origin_generation,
        origin_session: e.origin_session,
        origin_turn: e.origin_turn,
        submission: e.submission,
        routine: None,
        human_profile: e.human_profile,
        workspace: e.workspace,
        connection,
        connection_revision: rev(3),
        account: digest(6),
        item,
        item_ref,
        operation: PrivateOperationKind::ReadItem,
        operation_id: uuid(12),
        request_fingerprint: private_request_fingerprint(
            PrivateOperationKind::ReadItem,
            connection,
            item,
            item_ref,
        ),
        captured_policy: e.captured_policy,
        global_policy: e.global_policy,
        routine_policy: None,
    }
}
fn private_binding() -> DecisionBindingV1 {
    DecisionBindingV1::PrivateOperation(private())
}
fn reference(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(domain);
    hash.update(bytes);
    *hash.finalize().as_bytes()
}

// Authored literal, in the canonical field order the contract states: the
// shared subject, the eight private scalars, then the shared policy. The one
// computed part is the fingerprint, whose own preimage is pinned beside the
// vocabulary (`connections/tests.rs`).
const PRIVATE_HEAD: &str = r#"{"version":1,"kind":"private_operation","owner":{"user_scope":"00000000-0000-4000-8000-000000000001","device":"00000000-0000-4000-8000-000000000002"},"bot":"00000000-0000-4000-8000-000000000003","run":"00000000-0000-4000-8000-000000000004","origin_attempt":"00000000-0000-4000-8000-000000000005","origin_generation":1,"origin_session":"00000000-0000-4000-8000-000000000006","origin_turn":"00000000-0000-4000-8000-000000000007","submission":"00000000-0000-4000-8000-000000000008","routine":null,"human_profile":"00000000-0000-4000-8000-000000000009","workspace":"00000000-0000-4000-8000-00000000000a","connection":"00000000-0000-4000-8000-000000000028","connection_revision":3,"account":"0606060606060606060606060606060606060606060606060606060606060606","item":"00000000-0000-4000-8000-000000000029","item_ref":"0707070707070707070707070707070707070707070707070707070707070707","operation":"read_item","operation_id":"00000000-0000-4000-8000-00000000000c","request_fingerprint":"#;
const PRIVATE_TAIL: &str = r#","captured_policy":{"workspace":"00000000-0000-4000-8000-00000000000a","access":"default","selected_pages":[],"destination":"00000000-0000-4000-8000-00000000000a"},"global_policy":{"key":{"owner":{"user_scope":"00000000-0000-4000-8000-000000000001","device":"00000000-0000-4000-8000-000000000002"},"bot":"00000000-0000-4000-8000-000000000003","request":"00000000-0000-4000-8000-00000000000d"},"config_revision":1,"policy_revision":2,"digest":"0000000000000000000000000000000000000000000000000000000000000000"},"routine_policy":null}"#;
const FINGERPRINT_PREIMAGE: &str = r#"{"version":1,"operation":"read_item","connection":"00000000-0000-4000-8000-000000000028","item":"00000000-0000-4000-8000-000000000029","item_ref":"0707070707070707070707070707070707070707070707070707070707070707"}"#;

fn expected_private_bytes() -> Vec<u8> {
    let fingerprint = reference(
        b"koru.bot.private.request.v1",
        FINGERPRINT_PREIMAGE.as_bytes(),
    );
    let numbers = fingerprint
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!("{PRIVATE_HEAD}[{numbers}]{PRIVATE_TAIL}").into_bytes()
}

#[test]
fn m5_private_binding_golden_bytes_and_domain_digest() {
    let expected = expected_private_bytes();
    let b = private_binding();
    assert_eq!(
        String::from_utf8(b.canonical_bytes().unwrap()).unwrap(),
        String::from_utf8(expected.clone()).unwrap()
    );
    let expected_digest = reference(b"koru.bot.decision.binding.v1", &expected);
    assert_eq!(*b.digest().unwrap().as_bytes(), expected_digest);
    assert_eq!(
        String::from(DecisionDigest::from_bytes(expected_digest)),
        "1d77794ccb99601fe244bb5a3a93570f9fe71e8ae034ca683baf651d100b6a00"
    );
    assert_eq!(b.kind(), DecisionKind::PrivateOperation);
    assert_eq!(b.operation_id(), Some(uuid(12)));
    assert_eq!(b.human_profile(), edit().human_profile);
}

#[test]
fn m5_private_binding_leaves_both_existing_variants_bytes_unchanged() {
    // The authored pre-M5 literals are the fixtures of `golden.rs`; this
    // repeats their digests beside the new variant so one cohort shows all
    // three.
    assert_eq!(
        String::from(binding().digest().unwrap()),
        "fc7f1478f1873cfbfcf0c8c1e09b7df1642a5ce6d34db2fe3ae51064d05f839c"
    );
    assert_eq!(
        String::from(routine_binding().digest().unwrap()),
        "b6f0d0c28bf3046d8990aa7008d4026c5fd58ea8717c42cfa7d92000132dfe9f"
    );
    let native = DecisionBindingV1::NativeApproval(native());
    let bytes = String::from_utf8(native.canonical_bytes().unwrap()).unwrap();
    assert!(!bytes.contains("connections"));
    assert!(bytes.contains(
        r#""captured_policy":{"workspace":"00000000-0000-4000-8000-00000000000a","access":"default","selected_pages":[],"destination":"00000000-0000-4000-8000-00000000000a"},"global_policy""#
    ));
}

#[test]
fn m5_private_binding_edit_whose_policy_holds_one_grant_has_pinned_bytes() {
    let mut e = edit();
    e.captured_policy.connections.insert(ConnectionGrantRef {
        connection: ConnectionId::from_uuid(uuid(40)),
        operation: PrivateOperationKind::ReadItem,
    });
    let b = DecisionBindingV1::KoruEdit(e);
    let golden = include_str!("binding-v1.json").trim_end().replacen(
        r#""destination":"00000000-0000-4000-8000-00000000000a"}"#,
        r#""destination":"00000000-0000-4000-8000-00000000000a","connections":[{"connection":"00000000-0000-4000-8000-000000000028","operation":"read_item"}]}"#,
        1,
    );
    assert_eq!(
        String::from_utf8(b.canonical_bytes().unwrap()).unwrap(),
        golden
    );
    assert_eq!(
        *b.digest().unwrap().as_bytes(),
        reference(b"koru.bot.decision.binding.v1", golden.as_bytes())
    );
    assert_ne!(b.digest().unwrap(), binding().digest().unwrap());
    assert_eq!(
        decode_decision_frame::<DecisionBindingV1>(golden.as_bytes()).unwrap(),
        b
    );
}

#[test]
fn m5_private_binding_decodes_strictly_and_recomputes_the_fingerprint() {
    let b = private_binding();
    assert_eq!(
        decode_decision_frame::<DecisionBindingV1>(&b.canonical_bytes().unwrap()).unwrap(),
        b
    );
    let base = json(&b);
    let mut v = base.clone();
    v["title"] = "x".into();
    assert!(decode::<DecisionBindingV1>(&v).is_err());
    let mut v = base.clone();
    v.as_object_mut().unwrap().remove("account");
    assert!(decode::<DecisionBindingV1>(&v).is_err());
    // A private body under another kind token is refused, and so is another
    // kind's body under this token.
    let mut v = base.clone();
    v["kind"] = "koru_edit".into();
    assert!(decode::<DecisionBindingV1>(&v).is_err());
    let mut v = json(&binding());
    v["kind"] = "private_operation".into();
    assert!(decode::<DecisionBindingV1>(&v).is_err());
    // A token this build does not know is refused, never read as another kind.
    let mut v = base.clone();
    v["kind"] = "browser_origin".into();
    assert!(decode::<DecisionBindingV1>(&v).is_err());

    let mut asserted = private();
    asserted.request_fingerprint = [9; 32];
    assert!(
        DecisionBindingV1::PrivateOperation(asserted)
            .validate()
            .is_err()
    );
    let mut moved = private();
    moved.item = ConnectionItemId::from_uuid(uuid(99));
    assert!(
        DecisionBindingV1::PrivateOperation(moved)
            .validate()
            .is_err()
    );
    let mut nil = private();
    nil.operation_id = Uuid::nil();
    assert!(DecisionBindingV1::PrivateOperation(nil).validate().is_err());
    // The connection revision is bound by the digest though not by the
    // fingerprint.
    let mut later = private();
    later.connection_revision = rev(4);
    let later = DecisionBindingV1::PrivateOperation(later);
    assert!(later.validate().is_ok());
    assert_ne!(later.digest().unwrap(), b.digest().unwrap());
    assert!(b.matches_private(&private().proposal()));
    let DecisionBindingV1::PrivateOperation(l) = &later else {
        unreachable!()
    };
    assert!(!b.matches_private(&l.proposal()));
    assert!(!binding().matches_private(&private().proposal()));
}

#[test]
fn m5_private_binding_debug_is_redacted() {
    let debug = format!("{:?}", private_binding());
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("00000000-0000-4000-8000-000000000028"));
}

#[test]
fn m5_private_state_matrix_matches_a_koru_edit_and_has_no_native_phase() {
    use DecisionAuthorization as A;
    use DecisionPhase as P;
    let authorizations = [
        A::Pending,
        A::Granted,
        A::Denied,
        A::Canceled,
        A::Expired,
        A::Consumed,
    ];
    let phases = [
        P::Publishing,
        P::Holding,
        P::Held,
        P::NativeWait,
        P::ResumePrepared,
        P::AwaitRetry,
        P::Effect,
        P::NativeDispatch,
        P::NativeUnknown,
        P::StopPending,
        P::Settled,
    ];
    let mut admitted = 0;
    for authorization in authorizations {
        for phase in phases {
            let state = |kind| DecisionState {
                kind,
                authorization,
                phase,
                revision: rev(1),
            };
            let private = state(DecisionKind::PrivateOperation).validate().is_ok();
            assert_eq!(
                private,
                state(DecisionKind::KoruEdit).validate().is_ok(),
                "{authorization:?}/{phase:?}"
            );
            if matches!(phase, P::NativeWait | P::NativeDispatch | P::NativeUnknown) {
                assert!(!private);
            }
            admitted += usize::from(private);
        }
    }
    assert_eq!(admitted, 15);
    assert!(DecisionKind::PrivateOperation.resumes_by_successor());
    assert!(DecisionKind::KoruEdit.resumes_by_successor());
    assert!(!DecisionKind::NativeApproval.resumes_by_successor());
    assert_eq!(
        serde_json::to_value(DecisionKind::PrivateOperation).unwrap(),
        "private_operation"
    );
    assert!(serde_json::from_str::<DecisionKind>("\"browser_origin\"").is_err());
}

#[test]
fn m5_private_state_answer_choices_and_receipt_follow_the_edit_rules() {
    let b = private_binding();
    assert!(DecisionAnswerChoice::AllowOnce {}.matches_binding(&b));
    assert!(DecisionAnswerChoice::Deny {}.matches_binding(&b));
    assert!(
        !DecisionAnswerChoice::Native {
            offered: native().choices.as_slice()[0].clone()
        }
        .matches_binding(&b)
    );

    let mut r = receipt();
    r.binding_digest = b.digest().unwrap();
    r.binding = b;
    r.state.kind = DecisionKind::PrivateOperation;
    assert!(r.validate().is_ok());
    // No native deadline, and the kind must be the binding's.
    let mut native_deadline = r.clone();
    native_deadline.deadline.native_deadline_at_ms = Some(599_990);
    assert!(native_deadline.validate().is_err());
    let mut wrong_kind = r.clone();
    wrong_kind.state.kind = DecisionKind::KoruEdit;
    assert!(wrong_kind.validate().is_err());
    // The answer's human is the binding's.
    let mut answered = r.clone();
    answered.state.authorization = DecisionAuthorization::Granted;
    answered.state.phase = DecisionPhase::Held;
    answered.state.revision = rev(2);
    answered.answer = Some(DecisionAnswerReceipt {
        decision: answered.decision,
        request: id(60),
        expected_revision: rev(1),
        committed_revision: rev(2),
        binding_digest: answered.binding_digest,
        human_profile: edit().human_profile,
        choice: DecisionAnswerChoice::AllowOnce {},
        answered_at_ms: 0,
        authority_digest: digest(9),
    });
    assert!(answered.validate().is_ok());
    answered.answer.as_mut().unwrap().human_profile = ProfileId::from_uuid(uuid(70));
    assert!(answered.validate().is_err());
}

#[test]
fn m5_private_state_retained_effects_keep_existing_bytes_and_name_a_private_outcome() {
    let key = PrivateEffectKey {
        run: id(4),
        operation_id: uuid(12),
        attempt: id(30),
        generation: AttemptGeneration::from_raw(2).unwrap(),
        turn: id(34),
    };
    let native_key = NativeResultKey {
        run: id(4),
        attempt: id(30),
        generation: AttemptGeneration::from_raw(2).unwrap(),
        turn: id(34),
        operation_id: uuid(12),
    };
    // An existing variant's bytes are what they were.
    assert_eq!(
        serde_json::to_string(&DecisionRetainedEffect::NotApplied {
            key: native_key,
            code: NativeResultRefusal::Conflict,
        })
        .unwrap(),
        r#"{"outcome":"not_applied","key":{"run":"00000000-0000-4000-8000-000000000004","attempt":"00000000-0000-4000-8000-00000000001e","generation":2,"turn":"00000000-0000-4000-8000-000000000022","operation_id":"00000000-0000-4000-8000-00000000000c"},"code":"conflict"}"#
    );
    let cases = [
        (
            DecisionRetainedEffect::PrivateNotApplied {
                key: key.clone(),
                code: PrivateEffectRefusal::CredentialLocked,
            },
            "private_not_applied",
            DecisionClosureReason::EffectNotApplied,
        ),
        (
            DecisionRetainedEffect::PrivateConfirmed {
                key: key.clone(),
                outcome_bytes: 12,
                withheld: true,
            },
            "private_confirmed",
            DecisionClosureReason::Completed,
        ),
        (
            DecisionRetainedEffect::PrivateUnknown { key: key.clone() },
            "private_unknown",
            DecisionClosureReason::EffectUnknown,
        ),
    ];
    for (effect, token, reason) in cases {
        let value = json(&effect);
        assert_eq!(value["outcome"], token);
        assert_eq!(
            serde_json::from_value::<DecisionRetainedEffect>(value.clone()).unwrap(),
            effect
        );
        let mut extra = value;
        extra["content"] = "x".into();
        assert!(serde_json::from_value::<DecisionRetainedEffect>(extra).is_err());
        assert_eq!(effect.closure_reason(), reason);
        assert_eq!(
            effect.consumer(),
            (
                key.run,
                key.operation_id,
                key.attempt,
                key.generation,
                key.turn
            )
        );
    }
}
