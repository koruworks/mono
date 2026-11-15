// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn decision_binding_roundtrips_both_closed_kinds() {
    for b in [binding(), DecisionBindingV1::NativeApproval(native())] {
        assert_eq!(
            decode_decision_frame::<DecisionBindingV1>(&b.canonical_bytes().unwrap()).unwrap(),
            b
        );
    }
    let mut b = json(&binding());
    b["kind"] = "native_approval".into();
    assert!(decode::<DecisionBindingV1>(&b).is_err());
}
#[test]
fn decision_binding_checks_policy_subject_and_routine_pair() {
    let base = json(&binding());
    let mut b = base.clone();
    b["global_policy"]["key"]["bot"] = uuid(80).to_string().into();
    assert!(decode::<DecisionBindingV1>(&b).is_err());
    let mut b = base.clone();
    b["captured_policy"]["destination"] = uuid(80).to_string().into();
    assert!(decode::<DecisionBindingV1>(&b).is_err());
    let mut e = edit();
    e.routine = Some(routine_subject());
    assert!(DecisionBindingV1::KoruEdit(e.clone()).validate().is_err());
    e.routine_policy = Some(DecisionRoutinePolicy {
        owner: e.owner,
        bot: e.bot,
        schedule: id(81),
        request: id::<BotRequestId>(82).into(),
        grant_revision: rev(1),
        digest: digest(2),
    });
    assert!(DecisionBindingV1::KoruEdit(e.clone()).validate().is_ok());
    e.routine_policy.as_mut().unwrap().schedule = id(83);
    assert!(DecisionBindingV1::KoruEdit(e).validate().is_err());
}
#[test]
fn decision_binding_preserves_original_operation_across_consumer_turns() {
    let b = binding();
    let bytes = b.canonical_bytes().unwrap();
    for r in [resume(false), resume(true)] {
        let key = r.candidate_native_key(&b).unwrap();
        assert_eq!(key.operation_id, uuid(12));
        assert_eq!(key.turn, id(34));
        assert_ne!(key.turn, b.origin().turn);
        assert_eq!(b.canonical_bytes().unwrap(), bytes);
    }
    let e = edit();
    assert!(b.matches_edit_except_revision(
        e.operation_id,
        e.target,
        e.edit_fingerprint,
        e.payload.as_slice()
    ));
    assert!(!b.matches_edit_except_revision(
        uuid(99),
        e.target,
        e.edit_fingerprint,
        e.payload.as_slice()
    ));
    assert!(!b.matches_edit_except_revision(
        e.operation_id,
        e.target,
        e.edit_fingerprint,
        b"changed"
    ));
}
#[test]
fn decision_native_choices_keep_exact_offered_remembered_intents() {
    let n = native();
    let b = DecisionBindingV1::NativeApproval(n.clone());
    for c in n.choices.as_slice() {
        assert!(DecisionAnswerChoice::Native { offered: c.clone() }.matches_binding(&b));
        assert_eq!(c.decision(), c.intent.decision());
        let mut changed = c.clone();
        changed.id = id(100);
        assert!(!DecisionAnswerChoice::Native { offered: changed }.matches_binding(&b));
    }
    assert!(!DecisionAnswerChoice::AllowOnce {}.matches_binding(&b));
    assert!(
        !DecisionAnswerChoice::Native {
            offered: n.choices.as_slice()[0].clone()
        }
        .matches_binding(&binding())
    );
}
#[test]
fn decision_native_many_unique_choices_and_duplicate_late() {
    let mut n = native();
    let choices = (0..20_000)
        .map(|i| DecisionNativeChoice {
            id: id(1000 + i),
            intent: koru_session_protocol::ApprovalIntent::Allow,
        })
        .collect::<Vec<_>>();
    n.choices = DecisionList::new(choices.clone()).unwrap();
    let b = DecisionBindingV1::NativeApproval(n.clone());
    assert!(b.canonical_bytes().unwrap().len() < DECISION_FRAME_BYTES);
    assert!(b.validate().is_ok());
    let mut duplicate = choices;
    duplicate.push(duplicate[0].clone());
    n.choices = DecisionList::new(duplicate).unwrap();
    assert!(DecisionBindingV1::NativeApproval(n).validate().is_err());
}
#[test]
fn decision_binding_payload_and_vv_boundaries_are_independent() {
    let mut e = edit();
    e.payload = DecisionBytes::new(vec![255; 65_536]).unwrap();
    e.expected_revision = DecisionBytes::new(vec![255; 65_536]).unwrap();
    assert!(DecisionBindingV1::KoruEdit(e).canonical_bytes().is_ok());
    assert!(DecisionPayload::new(vec![0; 65_537]).is_err());
    assert!(DecisionVersionVector::new(vec![]).is_err());
}
#[test]
fn decision_binding_hash_changes_for_protected_operation_and_authority() {
    let e = edit();
    let original = binding().digest().unwrap();
    let mut cases = Vec::new();
    let mut b = e.clone();
    b.payload = DecisionBytes::new(b"changed".to_vec()).unwrap();
    cases.push(b);
    let mut b = e.clone();
    b.expected_revision = DecisionBytes::new(vec![9]).unwrap();
    cases.push(b);
    let mut b = e.clone();
    b.edit_fingerprint[0] = 1;
    cases.push(b);
    let mut b = e.clone();
    b.global_policy.digest = digest(1);
    cases.push(b);
    let mut b = e.clone();
    b.human_profile = ProfileId::from_uuid(uuid(70));
    cases.push(b);
    for b in cases {
        assert_ne!(DecisionBindingV1::KoruEdit(b).digest().unwrap(), original);
    }
}

#[test]
fn decision_complete_routine_subject_rejects_repeated_identity_mismatches() {
    let b = routine_binding();
    assert!(b.validate().is_ok());
    let base = json(&b);
    for path in [
        ["routine", "claim", "ids", "run"],
        ["routine", "claim", "ids", "submission"],
        ["routine", "route", "workspace", ""],
        ["routine", "route", "bot", ""],
    ] {
        let mut v = base.clone();
        if path[3].is_empty() {
            v[path[0]][path[1]][path[2]] = uuid(99).to_string().into();
        } else {
            v[path[0]][path[1]][path[2]][path[3]] = uuid(99).to_string().into();
        }
        assert!(decode::<DecisionBindingV1>(&v).is_err());
    }
    let mut v = base;
    v["routine"]["provenance"]["definition"]["digest"] = String::from(digest(9)).into();
    assert!(decode::<DecisionBindingV1>(&v).is_err());
    let key = resume(true).candidate_native_key(&b).unwrap();
    assert_eq!(key.attempt, id(30));
    let DecisionBindingV1::KoruEdit(e) = b else {
        unreachable!()
    };
    assert_eq!(e.routine.unwrap().claim.ids.attempt, uuid(102));
}
