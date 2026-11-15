// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::bots::BotDelegateAuthority;
use koru_delegate_core::decisions::{
    BotWaitCleanupCause, BotWaitCleanupIntentV1, BotWaitId, DecisionId, DecisionRevision,
    DecisionVersionV1,
};
use koru_delegate_core::{
    AgentTurnId, AttemptGeneration, BotId, BotOwner, RunAttemptId, RunId, SessionId,
};

pub(crate) fn enroll() -> EnrollBotWaitCleanup {
    let owner: BotOwner = serde_json::from_value(
        serde_json::json!({"user_scope":Uuid::new_v4(),"device":Uuid::new_v4()}),
    )
    .unwrap();
    let intent = BotWaitCleanupIntentV1 {
        version: DecisionVersionV1,
        owner,
        bot: BotId::mint(),
        run: RunId::mint(),
        wait_id: BotWaitId::mint(),
        decision_id: DecisionId::mint(),
        decision_revision: DecisionRevision::new(3).unwrap(),
        wait_revision: DecisionRevision::new(2).unwrap(),
        origin_attempt: RunAttemptId::mint(),
        origin_generation: AttemptGeneration::from_raw(1).unwrap(),
        origin_session: SessionId::mint(),
        origin_turn: AgentTurnId::mint(),
        cause: BotWaitCleanupCause::DecisionWaitCleanup,
    };
    EnrollBotWaitCleanup {
        key: intent.key().unwrap(),
        cleanup_intent: intent,
    }
}

fn parent() -> BotDelegateAuthority {
    BotDelegateAuthority::new("PRIVATE-DECISION-BRIDGE-0123456789012345".into()).unwrap()
}

#[test]
fn decision_bridge_credential_is_a_separate_domain_from_the_routine_bridge() {
    let parent = parent();
    let decision = parent.decision_bridge_authority();
    let routine = parent.routine_bridge_authority();
    let decision_header = decision.authorization_header();
    let routine_header = routine.authorization_header();
    assert!(decision_header.starts_with("Koru-Decision "));
    assert!(routine_header.starts_with("Koru-Routine "));
    assert!(decision.matches_authorization(decision_header.as_bytes()));
    assert!(routine.matches_authorization(routine_header.as_bytes()));
    assert!(
        !decision.matches_authorization(routine_header.as_bytes()),
        "the Routine credential never authorizes a decision route"
    );
    assert!(
        !routine.matches_authorization(decision_header.as_bytes()),
        "the decision credential never authorizes a Routine route"
    );
    // Same derived key under the other scheme is still refused.
    let swapped = decision_header.replacen("Koru-Decision ", "Koru-Routine  ", 1);
    assert!(!decision.matches_authorization(swapped.as_bytes()));
    let other = BotDelegateAuthority::new("ANOTHER-PARENT-SECRET-0123456789012345".into())
        .unwrap()
        .decision_bridge_authority();
    assert!(!other.matches_authorization(decision_header.as_bytes()));
    assert_eq!(
        format!("{decision:?}"),
        "DecisionBridgeAuthority([REDACTED])"
    );
    assert_ne!(DECISION_ENROLL_ROUTE, DECISION_READ_ROUTE);
    assert!(DECISION_ENROLL_ROUTE.starts_with("/private/bots/decisions/"));
    assert!(DECISION_READ_ROUTE.starts_with("/private/bots/decisions/"));
}

#[test]
fn decision_bridge_envelope_is_strict_versioned_and_bounded() {
    let request = DecisionBridgeEnvelope::new(enroll()).unwrap();
    let bytes = request.encode().unwrap();
    let decoded = DecisionBridgeEnvelope::<EnrollBotWaitCleanup>::decode(&bytes).unwrap();
    assert_eq!(decoded, request);
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["version"], 1);
    let mutations: [fn(&mut serde_json::Value); 6] = [
        |v: &mut serde_json::Value| v["extra"] = serde_json::json!(true),
        |v: &mut serde_json::Value| v["payload"]["extra"] = serde_json::json!(true),
        |v: &mut serde_json::Value| v["version"] = serde_json::json!(2),
        |v: &mut serde_json::Value| {
            let id = v["request_id"].as_str().unwrap().to_uppercase();
            v["request_id"] = serde_json::json!(id)
        },
        |v: &mut serde_json::Value| {
            v["request_id"] = serde_json::json!("00000000-0000-1000-8000-000000000001")
        },
        |v: &mut serde_json::Value| {
            v["payload"]["key"]["receipt_digest"] = serde_json::json!("00".repeat(32))
        },
    ];
    for mutate in mutations {
        let mut changed = value.clone();
        mutate(&mut changed);
        assert!(
            DecisionBridgeEnvelope::<EnrollBotWaitCleanup>::decode(
                &serde_json::to_vec(&changed).unwrap()
            )
            .is_err(),
            "{changed}"
        );
    }
    let oversize = vec![b' '; DECISION_BRIDGE_BODY_LIMIT + 1];
    assert_eq!(
        DecisionBridgeEnvelope::<EnrollBotWaitCleanup>::decode(&oversize).unwrap_err(),
        DecisionBridgeCodecError::TooLarge
    );
    assert!(!format!("{request:?}").contains("cleanup_intent"));
}

#[test]
fn decision_bridge_reply_correlation_never_turns_enroll_into_missing() {
    let enroll = enroll();
    let key = enroll.key.clone();
    let request = DecisionBridgeEnvelope::new(enroll).unwrap();
    let enrolled = BotWaitEnrollmentReply::Enrolled {
        receipt_digest: key.receipt_digest,
        key: key.clone(),
        ai_attempt: Uuid::new_v4(),
    };
    DecisionBridgeEnvelope::reply_to(&request, enrolled.clone()).unwrap();
    DecisionBridgeEnvelope::reply_to(
        &request,
        BotWaitEnrollmentReply::Refused {
            code: koru_delegate_core::decisions::BotWaitRefusal::Stale,
        },
    )
    .unwrap();
    assert_eq!(
        DecisionBridgeEnvelope::reply_to(
            &request,
            BotWaitEnrollmentReply::Missing { key: key.clone() }
        )
        .unwrap_err(),
        DecisionBridgeCodecError::Mismatch,
        "only Read may answer Missing"
    );
    let mut foreign = key.clone();
    foreign.wait_revision = DecisionRevision::new(9).unwrap();
    assert!(
        DecisionBridgeEnvelope::reply_to(
            &request,
            BotWaitEnrollmentReply::Enrolled {
                receipt_digest: foreign.receipt_digest,
                key: foreign,
                ai_attempt: Uuid::new_v4(),
            }
        )
        .is_err()
    );
    let mut stray = DecisionBridgeEnvelope::new(enrolled).unwrap();
    assert_eq!(
        stray.validate_reply_to(&request).unwrap_err(),
        DecisionBridgeCodecError::Mismatch,
        "another request ID is never this request's answer"
    );
    stray.request_id = request.request_id;
    stray.validate_reply_to(&request).unwrap();
    let read = DecisionBridgeEnvelope::new(ReadBotWaitCleanup { key: key.clone() }).unwrap();
    DecisionBridgeEnvelope::reply_to(&read, BotWaitEnrollmentReply::Missing { key }).unwrap();
}
