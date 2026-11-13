// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::BotPolicyBarrierKey;

fn proof(s: &BotSubmission) -> FreshBotPolicyProof {
    let p = &s.provenance;
    FreshBotPolicyProof {
        global: BotPolicyBarrier {
            key: BotPolicyBarrierKey {
                owner: p.owner,
                bot: p.bot,
                request: p.submission.into(),
            },
            config_revision: p.config_revision,
            policy_revision: p.policy_revision,
            policy: s.policy.clone(),
            effects_allowed: true,
        },
        routine: p.routine.map(|r| RoutinePolicyBarrier {
            owner: p.owner,
            bot: p.bot,
            schedule: r.schedule,
            request: p.submission.into(),
            grant_revision: r.grant_revision,
            policy: s.policy.clone(),
            effects_allowed: true,
        }),
    }
}
#[test]
fn routine_fresh_proof_requires_exact_subject_and_separate_global_fence() {
    let p = super::super::tests::prepared();
    let mut f = proof(&p.submission);
    assert!(f.effects_allowed_for(&p.submission));
    f.routine = None;
    assert!(!f.matches_submission(&p.submission));
    f = proof(&p.submission);
    f.routine.as_mut().unwrap().schedule = ScheduleId::mint();
    assert!(!f.matches_submission(&p.submission));
    f = proof(&p.submission);
    f.global.effects_allowed = false;
    assert!(f.matches_submission(&p.submission));
    assert!(!f.effects_allowed_for(&p.submission));
    f = proof(&p.submission);
    f.routine.as_mut().unwrap().effects_allowed = false;
    assert!(!f.effects_allowed_for(&p.submission));
    f = proof(&p.submission);
    f.routine.as_mut().unwrap().grant_revision = 0;
    assert!(!f.matches_submission(&p.submission));
    f = proof(&p.submission);
    f.global.policy_revision = 0;
    assert!(!f.matches_submission(&p.submission));
}
#[test]
fn routine_grant_is_not_replaced_by_global_selected_pages() {
    let p = super::super::tests::prepared();
    let mut f = proof(&p.submission);
    f.global
        .policy
        .selected_pages
        .insert(crate::DocumentId::from_uuid(Uuid::new_v4()));
    assert!(f.effects_allowed_for(&p.submission));
    assert!(f.routine.as_ref().unwrap().policy.selected_pages.is_empty());
    let debug = format!("{f:?}");
    assert!(!debug.contains("selected_pages"));
    let mut manual = p.submission;
    manual.provenance.routine = None;
    manual.request.origin = RunOrigin::Manual;
    assert!(!f.matches_submission(&manual));
    f.routine = None;
    assert!(f.effects_allowed_for(&manual));
}
#[test]
fn routine_policy_reuses_v8_policy_id_and_strict_object_boundary() {
    let p = super::super::tests::prepared();
    let mut f = proof(&p.submission);
    f.routine.as_mut().unwrap().request = "00000000-0000-8000-8000-000000000001".parse().unwrap();
    let encoded = serde_json::to_string(&f).unwrap();
    assert_eq!(
        serde_json::from_str::<FreshBotPolicyProof>(&encoded).unwrap(),
        f
    );
    let unknown = encoded.replacen(
        "\"grant_revision\":1",
        "\"grant_revision\":1,\"authority\":true",
        1,
    );
    assert!(serde_json::from_str::<FreshBotPolicyProof>(&unknown).is_err());
}
