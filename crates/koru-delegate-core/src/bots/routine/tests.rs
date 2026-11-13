// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::*;

fn id(n: u8) -> Uuid {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
        .parse()
        .unwrap()
}
fn prepared() -> RoutinePrepared {
    let ids = RoutineExecutionIds {
        submission: BotRequestId::from_uuid(id(1)),
        run: RunId::from_uuid(id(2)),
        chat: id(3),
        attempt: id(4),
        user_message: id(5),
        assistant_message: id(6),
    };
    let owner = BotOwner {
        user_scope: super::super::UserScopeId::from_uuid(id(10)),
        device: DeviceId::from_uuid(id(11)),
    };
    let slot = RoutineSlot {
        owner,
        bot: BotId::from_uuid(id(12)),
        schedule: ScheduleId::from_uuid(id(13)),
        occurrence_ms: 10,
    };
    let definition = RoutineDefinitionIdentity {
        revision: 1,
        digest: RoutineDigest32::from_bytes([0xab; 32]),
        owner_handoff: None,
    };
    let workspace = WorkspaceId::from_uuid(id(14));
    let mut request = crate::tests::request();
    request.run = ids.run;
    request.executing_device = owner.device;
    request.scope = WorkspaceScope::BotFolder {
        folder: BotWorkingFolder::new("/bot".into()).unwrap(),
    };
    request.context.clear();
    request.grants = [workspace].into();
    request.origin = RunOrigin::Scheduled {
        schedule: slot.schedule,
        occurrence_ms: slot.occurrence_ms,
        revision: definition.revision,
    };
    RoutinePrepared {
        slot,
        definition,
        ids,
        semantic_hash: RoutineDigest32::from_bytes([1; 32]),
        preparation_hash: RoutineDigest32::from_bytes([2; 32]),
        submission_version: 2,
        submission: BotSubmission {
            provenance: BotProvenance {
                owner,
                bot: slot.bot,
                submission: ids.submission,
                chat_id: ids.chat,
                ai_attempt_id: Some(ids.attempt),
                assistant_message_id: Some(ids.assistant_message),
                config_revision: 1,
                policy_revision: 1,
                continued_from: None,
                routine: Some(RoutineProvenance {
                    schedule: slot.schedule,
                    occurrence_ms: slot.occurrence_ms,
                    definition,
                    grant_revision: 1,
                }),
            },
            policy: CapturedBotPolicy {
                workspace,
                destination: workspace,
                access: BotAccess::Default,
                selected_pages: Default::default(),
            },
            workspace,
            request,
            input: PendingInput {
                idempotency_key: id(16).to_string().parse().unwrap(),
                text: "ROUTINE-PRIVATE-TASK".into(),
                attachment_ids: vec![],
                references: vec![],
                mode: None,
                fast: None,
                reasoning: None,
                skills: vec![],
                model: None,
            },
        },
    }
}

#[test]
fn routine_v2_is_explicit_and_cannot_pass_the_ordinary_submission_validator() {
    let value = prepared();
    assert!(value.valid());
    assert!(value.submission.valid_for_version(2));
    assert!(!value.submission.valid());
    assert!(!value.submission.valid_for_version(1));
    assert!(!value.submission.valid_for_version(3));
    let mut manual = value.submission.clone();
    manual.provenance.routine = None;
    manual.request.origin = RunOrigin::Manual;
    assert!(manual.valid());
    assert!(!manual.valid_for_version(2));
    manual.request.origin = value.submission.request.origin;
    assert!(!manual.valid());
    assert!(!manual.valid_for_version(2));
    assert!(!format!("{value:?}").contains("ROUTINE-PRIVATE-TASK"));
}

#[test]
fn routine_preparation_binds_every_repeated_identity_and_claim() {
    let value = prepared();
    let claim = RoutineClaimIdentity {
        slot: value.slot,
        definition: value.definition,
        ids: value.ids,
        preparation_hash: value.preparation_hash,
        claimed_at_ms: 11,
    };
    assert!(claim.matches_prepared(&value));
    let mut cases = vec![];
    let mut changed = value.clone();
    changed.slot.occurrence_ms += 1;
    cases.push(changed);
    let mut changed = value.clone();
    changed.slot.bot = BotId::from_uuid(id(31));
    cases.push(changed);
    let mut changed = value.clone();
    changed.slot.owner.device = DeviceId::from_uuid(id(31));
    cases.push(changed);
    let mut changed = value.clone();
    changed.definition.owner_handoff = Some(HandoffId::from_uuid(id(31)));
    cases.push(changed);
    let mut changed = value.clone();
    changed.definition.digest = RoutineDigest32::from_bytes([4; 32]);
    cases.push(changed);
    let mut changed = value.clone();
    changed.ids.run = RunId::from_uuid(id(31));
    cases.push(changed);
    let mut changed = value.clone();
    changed.ids.attempt = id(31);
    cases.push(changed);
    let mut changed = value.clone();
    changed.ids.assistant_message = id(31);
    cases.push(changed);
    let mut changed = value.clone();
    changed.ids.user_message = changed.ids.assistant_message;
    cases.push(changed);
    let mut changed = value.clone();
    changed
        .submission
        .provenance
        .routine
        .as_mut()
        .unwrap()
        .grant_revision = 0;
    cases.push(changed);
    let mut changed = value.clone();
    changed.submission.provenance.continued_from = Some(RunId::from_uuid(id(31)));
    cases.push(changed);
    for changed in cases {
        assert!(!changed.valid());
        assert!(!claim.matches_prepared(&changed));
    }
    let mut changed = claim;
    changed.preparation_hash = RoutineDigest32::from_bytes([8; 32]);
    assert!(!changed.matches_prepared(&value));
}

#[test]
fn routine_wire_is_closed_and_digest_has_one_fixed_spelling() {
    let value = prepared();
    let json = serde_json::to_value(&value).unwrap();
    assert_eq!(
        serde_json::from_value::<RoutinePrepared>(json.clone()).unwrap(),
        value
    );
    for path in [
        "",
        "/slot",
        "/definition",
        "/ids",
        "/submission/provenance/routine",
        "/submission/provenance/routine/definition",
    ] {
        let mut changed = json.clone();
        changed
            .pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), true.into());
        assert!(
            serde_json::from_value::<RoutinePrepared>(changed).is_err(),
            "{path}"
        );
    }
    for invalid in [
        "ab".repeat(31),
        "AB".repeat(32),
        "ag".repeat(32),
        "ab".repeat(33),
    ] {
        assert!(RoutineDigest32::try_from(invalid).is_err());
    }
    assert_eq!(
        serde_json::to_string(&value.definition.digest).unwrap(),
        format!("\"{}\"", "ab".repeat(32))
    );
}

#[test]
fn routine_slot_preserves_store_owned_uuid_domains_and_rejects_malformed_wire() {
    let mut slot = prepared().slot;
    slot.owner.user_scope = super::super::UserScopeId::from_uuid(Uuid::nil());
    slot.owner.device = DeviceId::from_uuid(Uuid::nil());
    assert!(slot.valid());
    let mut json = serde_json::to_value(slot).unwrap();
    json["owner"]["device"] = "invalid UUID".into();
    assert!(serde_json::from_value::<RoutineSlot>(json).is_err());
    slot.bot = BotId::from_uuid(Uuid::nil());
    assert!(!slot.valid());
}

#[test]
fn routine_addition_preserves_legacy_provenance_json_bytes() {
    let mut p = prepared().submission.provenance;
    p.routine = None;
    let golden = r#"{"owner":{"user_scope":"00000000-0000-4000-8000-00000000000a","device":"00000000-0000-4000-8000-00000000000b"},"bot":"00000000-0000-4000-8000-00000000000c","submission":"00000000-0000-4000-8000-000000000001","chat_id":"00000000-0000-4000-8000-000000000003","ai_attempt_id":"00000000-0000-4000-8000-000000000004","assistant_message_id":"00000000-0000-4000-8000-000000000006","config_revision":1,"policy_revision":1,"continued_from":null}"#;
    assert_eq!(serde_json::to_string(&p).unwrap(), golden);
    assert_eq!(serde_json::from_str::<BotProvenance>(golden).unwrap(), p);
}
