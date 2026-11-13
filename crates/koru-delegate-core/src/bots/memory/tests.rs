// SPDX-License-Identifier: MPL-2.0
use super::*;

fn allocation() -> MemoryEditAllocation {
    let request = crate::tests::request();
    MemoryEditAllocation {
        key: NativeResultKey {
            run: request.run,
            attempt: crate::RunAttemptId::mint(),
            generation: crate::AttemptGeneration::FIRST,
            turn: crate::AgentTurnId::mint(),
            operation_id: Uuid::new_v4(),
        },
        caller_fingerprint: [4; 32],
        subject: MemoryEditSubjectV1 {
            version: 1,
            owner: BotOwner {
                user_scope: koru_store_core::UserScopeId::new(),
                device: request.executing_device,
            },
            bot: BotId::mint(),
            submission: BotRequestId::mint(),
            captured_config_revision: 1,
            action: MemoryEditAction::Correct,
        },
        base: MemoryBaseSelector::History {
            version: DocumentVersionId::from_uuid(Uuid::new_v4()),
        },
        ensure: None,
        resolution: MemoryBaseResolution::Unresolved {},
        source_run_document: DocumentId::from_uuid(Uuid::new_v4()),
        fact_id: Uuid::nil(),
        detail_id: Some(Uuid::new_v4()),
        source_id: Some(Uuid::new_v4()),
        created_at_ms: 7,
    }
}

#[test]
fn memory_fact_identity_retains_core_uuid_domain_and_strict_spelling() {
    for raw in [
        "00000000-0000-0000-0000-000000000000",
        "b1fbf22c-1c31-5c07-825d-9cba7e23100b",
        "b1fbf22c-1c31-1c07-025d-9cba7e23100b",
        "ffffffff-ffff-ffff-ffff-ffffffffffff",
    ] {
        let mut value = allocation();
        value.fact_id = Uuid::parse_str(raw).unwrap();
        assert!(value.valid());
        let mut json = serde_json::to_value(&value).unwrap();
        assert_eq!(json["fact_id"], raw);
        assert_eq!(
            serde_json::from_value::<MemoryEditAllocation>(json.clone()).unwrap(),
            value
        );
        json["fact_id"] = serde_json::to_value(vec![0u8; 16]).unwrap();
        assert!(serde_json::from_value::<MemoryEditAllocation>(json).is_err());
    }
    for raw in [
        "B1FBF22C-1C31-5C07-825D-9CBA7E23100B",
        "b1fbf22c1c315c07825d9cba7e23100b",
        "not-a-node",
    ] {
        let mut json = serde_json::to_value(allocation()).unwrap();
        json["fact_id"] = raw.into();
        assert!(serde_json::from_value::<MemoryEditAllocation>(json).is_err());
    }
    assert!(
        "b1fbf22c-1c31-5c07-825d-9cba7e23100b"
            .parse::<crate::BlockNodeId>()
            .is_err()
    );
}

#[test]
fn memory_allocation_domains_refuse_mixed_creation_and_resolution() {
    let mut value = allocation();
    value.base = MemoryBaseSelector::OriginalSeed {};
    assert!(!value.valid());
    value.subject.action = MemoryEditAction::Remember;
    value.fact_id = Uuid::new_v4();
    value.ensure = Some(MemoryEnsureIdentity {
        request_id: BotRequestId::mint(),
        expected_revision: 1,
        title: MEMORY_ENSURE_TITLE.into(),
    });
    assert!(value.valid());
    value.ensure.as_mut().unwrap().title = "PRIVATE-TITLE".into();
    assert!(!value.valid());
    assert!(!format!("{value:?}").contains("PRIVATE-TITLE"));
    value.ensure.as_mut().unwrap().title = MEMORY_ENSURE_TITLE.into();
    value.subject.captured_config_revision = 2;
    assert!(!value.valid());
    value.subject.captured_config_revision = 1;
    value.source_id = value.detail_id;
    assert!(!value.valid());
}

#[test]
fn memory_wire_rejects_unknown_fields_and_missing_purpose() {
    let value = allocation();
    for field in ["subject", "base", "resolution"] {
        let mut json = serde_json::to_value(&value).unwrap();
        json[field]["unvouched"] = true.into();
        assert!(serde_json::from_value::<MemoryEditAllocation>(json).is_err());
    }
    assert!(
        serde_json::from_value::<NativeEffectPurpose>(
            serde_json::json!({"kind":"result","authority":{}})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<NativeEffectPurpose>(serde_json::json!({})).is_err());
    assert!(
        serde_json::from_value::<NativeEffectPurpose>(serde_json::json!({"kind":"future"}))
            .is_err()
    );
}
