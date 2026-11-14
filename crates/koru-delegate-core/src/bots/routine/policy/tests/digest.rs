// SPDX-License-Identifier: MPL-2.0
use super::*;

const GOLDEN: &str = r#"{"version":1,"barrier":{"owner":{"user_scope":"00000000-0000-4000-8000-00000000000a","device":"00000000-0000-4000-8000-00000000000b"},"bot":"00000000-0000-4000-8000-00000000000c","schedule":"00000000-0000-4000-8000-00000000000d","request":"00000000-0000-4000-8000-000000000001","grant_revision":1,"policy":{"workspace":"00000000-0000-4000-8000-00000000000e","access":"default","selected_pages":[],"destination":"00000000-0000-4000-8000-00000000000e"},"effects_allowed":true}}"#;
fn barrier() -> RoutinePolicyBarrier {
    proof(&super::super::super::tests::prepared().submission)
        .routine
        .unwrap()
}

#[test]
fn routine_barrier_digest_has_complete_frozen_versioned_json_and_hash() {
    let b = barrier();
    assert_eq!(
        serde_json::to_string(&BarrierDigestV1 {
            version: 1,
            barrier: &b
        })
        .unwrap(),
        GOLDEN
    );
    let digest = b.digest().unwrap();
    let mut reference = blake3::Hasher::new();
    reference.update(b"koru.bot.routine.barrier.v1");
    reference.update(GOLDEN.as_bytes());
    assert_eq!(digest.as_bytes(), reference.finalize().as_bytes());
    eprintln!("barrier_fixture_digest={}", String::from(digest));
}

#[test]
fn routine_barrier_digest_binds_every_field_and_canonicalizes_only_sets() {
    let base = barrier();
    let digest = base.digest().unwrap();
    let mut cases = vec![];
    let mut b = base.clone();
    b.owner.user_scope = super::super::super::super::UserScopeId::new();
    cases.push(b);
    let mut b = base.clone();
    b.owner.device = crate::DeviceId::new();
    cases.push(b);
    let mut b = base.clone();
    b.bot = crate::BotId::mint();
    cases.push(b);
    let mut b = base.clone();
    b.schedule = crate::ScheduleId::mint();
    cases.push(b);
    let mut b = base.clone();
    b.request = crate::BotRequestId::mint().into();
    cases.push(b);
    let mut b = base.clone();
    b.grant_revision += 1;
    cases.push(b);
    let mut b = base.clone();
    b.policy.access = crate::BotAccess::Workspace;
    cases.push(b);
    let mut b = base.clone();
    b.policy.selected_pages.insert(crate::DocumentId::new());
    cases.push(b);
    let mut b = base.clone();
    b.policy.workspace = crate::WorkspaceId::new();
    b.policy.destination = b.policy.workspace;
    cases.push(b);
    let mut b = base.clone();
    b.effects_allowed = false;
    cases.push(b);
    for b in cases {
        assert_ne!(b.digest().unwrap(), digest)
    }
    let a = crate::DocumentId::new();
    let z = crate::DocumentId::new();
    let mut b = base.clone();
    b.policy.selected_pages = [a, z].into();
    let mut c = b.clone();
    c.policy.selected_pages = [z, a].into();
    assert_eq!(b.digest(), c.digest());
    let mut invalid = base.clone();
    invalid.grant_revision = 0;
    assert_eq!(invalid.digest(), Err(RoutineDigestError::Invalid));
    let mut invalid = base.clone();
    invalid.policy.destination = crate::WorkspaceId::new();
    assert!(invalid.digest().is_err());
    let mut full_domain = base;
    full_domain.owner.user_scope = serde_json::from_value(serde_json::json!(Uuid::nil())).unwrap();
    full_domain.owner.device = crate::DeviceId::from_uuid(Uuid::nil());
    full_domain.request = "00000000-0000-8000-8000-000000000001".parse().unwrap();
    assert!(
        full_domain.digest().is_ok(),
        "Store UUID domains and native-policy v8 remain valid"
    );
}
