// SPDX-License-Identifier: MPL-2.0
//! The current grant, the access edit and its receipt, and the lifecycle a
//! capture is expected to have. Pure values: no storage and no delegate.
use super::*;
use crate::bots::{BotOperationKind, BotOperationStatus, BotReceiptStage};

/// The fixture creation with its grant digest computed, as storage holds it.
fn bound() -> BotRoutineCreationIdentity {
    let mut identity = creation();
    identity.grant.digest = identity.grant.canonical_digest().unwrap();
    identity
}

fn page(home: WorkspaceId, n: u8) -> BotPage {
    BotPage {
        workspace: home,
        document: DocumentId::from_uuid(id(n)),
    }
}

fn update(identity: &BotRoutineCreationIdentity) -> BotRoutineAccessUpdate {
    BotRoutineAccessUpdate {
        request: BotRequestId::from_uuid(id(40)),
        bot: identity.bot,
        schedule: identity.schedule,
        expected_grant_revision: 1,
        access: crate::bots::BotAccess::Workspace,
        selected_pages: vec![page(identity.home, 41), page(identity.home, 42)],
    }
}

#[test]
fn routine_current_grant_is_the_creation_grant_until_an_edit_with_its_own_request() {
    let identity = bound();
    let original = BotRoutineCurrentGrant::original(&identity);
    assert!(!original.is_edited());
    assert_eq!(original.request, identity.creation_request);
    original.validate(&identity).unwrap();

    let command = update(&identity);
    let grant = command.grant(identity.home, 7, &Default::default()).unwrap();
    assert_eq!(grant.grant_revision, 2);
    assert_eq!(grant.source_bot_policy_revision, 7);
    assert_eq!(grant.policy.workspace, identity.home);
    assert_eq!(grant.policy.destination, identity.home);
    assert_eq!(grant.policy.access, BotAccess::Workspace);
    assert_eq!(grant.policy.selected_pages.len(), 2);
    assert_eq!(grant.digest, grant.canonical_digest().unwrap());
    let edited = BotRoutineCurrentGrant {
        grant: grant.clone(),
        request: command.request,
    };
    assert!(edited.is_edited());
    edited.validate(&identity).unwrap();

    for mutation in 0..6 {
        let mut bad = edited.clone();
        match mutation {
            // An edit is never produced by the creation request.
            0 => bad.request = identity.creation_request,
            // Revision 1 is exactly the creation grant, nothing else.
            1 => {
                bad.grant.grant_revision = 1;
                bad.grant.digest = bad.grant.canonical_digest().unwrap();
            }
            // A digest is recomputed, never trusted.
            2 => bad.grant.digest = digest(77),
            3 => bad.grant.policy.access = BotAccess::Default,
            // Another workspace is not this Routine's home.
            4 => {
                bad.grant.policy.workspace = WorkspaceId::from_uuid(id(60));
                bad.grant.digest = bad.grant.canonical_digest().unwrap();
            }
            // The creation grant under an edit's request.
            _ => {
                bad.grant = identity.grant.clone();
            }
        }
        assert_eq!(bad.validate(&identity), Err(BotError::Invalid), "{mutation}");
    }
    // The creation row's own rule is unchanged: its grant is revision 1.
    let mut raised = identity.clone();
    raised.grant = grant;
    assert!(raised.validate().is_err());
    let debug = format!("{edited:?}{command:?}");
    assert!(!debug.contains("selected_pages") && !debug.contains("workspace"));
}

#[test]
fn routine_access_update_uses_the_bot_access_bounds_and_builds_only_the_next_revision() {
    let identity = bound();
    let command = update(&identity);
    command.validate().unwrap();
    let summary = command.grant(identity.home, 3, &Default::default()).unwrap().summary().unwrap();
    assert_eq!(
        summary,
        BotRoutineGrantSummary {
            source_bot_policy_revision: 3,
            grant_revision: 2,
            access: crate::bots::BotAccess::Workspace,
            selected_page_count: 2,
        }
    );
    // The default access with no pages is the approved default.
    let mut default = command.clone();
    default.access = crate::bots::BotAccess::Default;
    default.selected_pages.clear();
    let grant = default.grant(identity.home, 3, &Default::default()).unwrap();
    assert_eq!(grant.policy.access, BotAccess::Default);
    assert!(grant.policy.selected_pages.is_empty());

    for mutation in 0..7 {
        let mut bad = command.clone();
        match mutation {
            0 => bad.selected_pages.reverse(),
            1 => bad.selected_pages.push(page(identity.home, 42)),
            2 => bad.selected_pages[0].workspace = WorkspaceId::from_uuid(id(60)),
            3 => bad.expected_grant_revision = 0,
            4 => bad.request = BotRequestId::from_uuid(uuid::Uuid::nil()),
            5 => {
                bad.selected_pages = (0..1025u32)
                    .map(|n| BotPage {
                        workspace: identity.home,
                        document: DocumentId::from_uuid(uuid::Uuid::from_u128(
                            0x0000_0000_0000_4000_8000_0000_0000_0000u128 + u128::from(n),
                        )),
                    })
                    .collect()
            }
            _ => bad.expected_grant_revision = i64::MAX as u64,
        }
        assert_eq!(
            bad.grant(identity.home, 3, &Default::default()).err(),
            Some(BotError::Invalid),
            "{mutation}"
        );
    }
    assert!(
        command
            .grant(identity.home, 0, &Default::default())
            .is_err()
    );
    assert!(
        command
            .grant(WorkspaceId::from_uuid(id(60)), 3, &Default::default())
            .is_err()
    );
}

#[test]
fn routine_access_receipt_answers_only_its_own_request_and_carries_no_page_list() {
    let identity = bound();
    let command = update(&identity);
    let grant = command.grant(identity.home, 3, &Default::default()).unwrap();
    let receipt = BotRoutineAccessReceipt::updated(&command, &grant).unwrap();
    assert_eq!(receipt.summary(), Some(grant.summary().unwrap()));
    assert!(receipt.answers(&command, identity.home, &Default::default()));
    for mutation in 0..5 {
        let mut other = command.clone();
        match mutation {
            0 => other.selected_pages.pop().map(|_| ()).unwrap(),
            1 => other.access = crate::bots::BotAccess::Default,
            2 => other.expected_grant_revision = 2,
            3 => other.schedule = ScheduleId::from_uuid(id(61)),
            _ => other.bot = BotId::from_uuid(id(62)),
        }
        assert!(
            !receipt.answers(&other, identity.home, &Default::default()),
            "{mutation}"
        );
    }
    assert!(!receipt.answers(
        &command,
        WorkspaceId::from_uuid(id(60)),
        &Default::default()
    ));
    // The request id is the receipt's key, not part of what it answers.
    let mut renamed = command.clone();
    renamed.request = BotRequestId::from_uuid(id(63));
    assert!(receipt.answers(&renamed, identity.home, &Default::default()));

    let encoded = serde_json::to_string(&receipt).unwrap();
    assert!(encoded.len() <= BOT_ROUTINE_RECEIPT_BYTES);
    for page in &command.selected_pages {
        assert!(!encoded.contains(&page.document.to_storage()));
    }
    assert_eq!(
        serde_json::from_str::<BotRoutineAccessReceipt>(&encoded).unwrap(),
        receipt
    );
    for pointer in ["", "/outcome"] {
        let mut json = serde_json::to_value(&receipt).unwrap();
        json.pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), true.into());
        assert!(serde_json::from_value::<BotRoutineAccessReceipt>(json).is_err());
    }
    for mutation in 0..4 {
        let mut bad = receipt.clone();
        match (&mut bad.outcome, mutation) {
            (_, 0) => bad.version = 2,
            (BotRoutineAccessOutcome::Updated { grant_revision, .. }, 1) => *grant_revision = 3,
            (
                BotRoutineAccessOutcome::Updated {
                    source_bot_policy_revision,
                    ..
                },
                2,
            ) => *source_bot_policy_revision = 0,
            (
                BotRoutineAccessOutcome::Updated {
                    selected_page_count,
                    ..
                },
                _,
            ) => *selected_page_count = 1025,
            _ => unreachable!(),
        }
        assert_eq!(bad.validate(), Err(BotError::Invalid), "{mutation}");
    }

    let refused =
        BotRoutineAccessReceipt::refused(&command, BotRoutineAccessRefusal::NotPaused).unwrap();
    assert_eq!(refused.summary(), None);
    assert!(refused.answers(&command, identity.home, &Default::default()));
    assert_eq!(
        serde_json::to_value(&refused).unwrap()["outcome"],
        serde_json::json!({"kind":"refused","reason":"not_paused"})
    );
}

#[test]
fn routine_access_record_pairs_stage_with_outcome_and_reads_as_one_operation() {
    let identity = bound();
    let command = update(&identity);
    let grant = command.grant(identity.home, 3, &Default::default()).unwrap();
    let updated = BotRoutineAccessReceipt::updated(&command, &grant).unwrap();
    let refused =
        BotRoutineAccessReceipt::refused(&command, BotRoutineAccessRefusal::PageForbidden).unwrap();
    let record = |receipt: &BotRoutineAccessReceipt, stage| BotRoutineAccessRecord {
        request: command.request,
        fingerprint: digest(9),
        receipt: receipt.clone(),
        stage,
    };
    for (receipt, stage, valid, status) in [
        (&updated, BotReceiptStage::Prepared, true, BotOperationStatus::Pending),
        (&updated, BotReceiptStage::Completed, true, BotOperationStatus::Accepted),
        (&refused, BotReceiptStage::Rejected, true, BotOperationStatus::Rejected),
        (&updated, BotReceiptStage::Rejected, false, BotOperationStatus::Rejected),
        (&updated, BotReceiptStage::Accepted, false, BotOperationStatus::Accepted),
        (&refused, BotReceiptStage::Prepared, false, BotOperationStatus::Pending),
        (&refused, BotReceiptStage::Completed, false, BotOperationStatus::Accepted),
    ] {
        let record = record(receipt, stage);
        assert_eq!(record.validate().is_ok(), valid, "{stage:?}");
        let operation = record.operation();
        assert_eq!(operation.kind, BotOperationKind::UpdateRoutineAccess);
        assert_eq!(operation.request_id, command.request);
        assert_eq!(operation.bot_id, identity.bot);
        assert_eq!(operation.run_id, None);
        assert_eq!(operation.status, status);
    }
    assert_eq!(
        BotOperationKind::parse("update_routine_access"),
        Some(BotOperationKind::UpdateRoutineAccess)
    );
    assert_eq!(
        serde_json::to_value(BotOperationKind::UpdateRoutineAccess).unwrap(),
        serde_json::json!(BotOperationKind::UpdateRoutineAccess.token())
    );
}

/// Guard 7 of contract 4.3: capture validation requires an Active definition
/// unless the caller states that it expects the owner Test's paused one.
#[test]
fn routine_capture_default_expectation_rejects_a_paused_definition() {
    let active = preparation();
    let manual = ScheduleLifecycle::Paused {
        reason: SchedulePauseReason::Manual,
    };
    let mut test = active.clone();
    test.capture.schedule_definition.lifecycle = manual.clone();

    use BotRoutineCaptureLifecycle::{Active, TestPausedManual};
    assert_eq!(BotRoutineCaptureLifecycle::default(), Active);
    assert_eq!(BotRoutineCaptureLifecycle::of(&ScheduleLifecycle::Active), Some(Active));
    assert_eq!(BotRoutineCaptureLifecycle::of(&manual), Some(TestPausedManual));

    // The default, and the expectation stated as Active, behave as before.
    active.capture.validate(&active.prepared).unwrap();
    active
        .capture
        .validate_expecting(&active.prepared, Active)
        .unwrap();
    assert_eq!(
        test.capture.validate(&test.prepared),
        Err(BotError::Invalid)
    );
    assert_eq!(
        test.capture.validate_expecting(&test.prepared, Active),
        Err(BotError::Invalid)
    );
    // The Test expectation admits exactly the paused definition.
    test.capture
        .validate_expecting(&test.prepared, TestPausedManual)
        .unwrap();
    assert_eq!(
        active
            .capture
            .validate_expecting(&active.prepared, TestPausedManual),
        Err(BotError::Invalid)
    );
    // A capture that was already retained is read back as what it is.
    active.capture.validate_retained(&active.prepared).unwrap();
    test.capture.validate_retained(&test.prepared).unwrap();
    active.validate().unwrap();
    test.validate().unwrap();

    // Paused for any other reason is never a capture, whatever is stated.
    for reason in [
        SchedulePauseReason::ProviderNotDelegatable,
        SchedulePauseReason::ScopeUnavailable,
    ] {
        let lifecycle = ScheduleLifecycle::Paused { reason };
        assert_eq!(BotRoutineCaptureLifecycle::of(&lifecycle), None);
        let mut other = active.clone();
        other.capture.schedule_definition.lifecycle = lifecycle;
        for expected in [Active, TestPausedManual] {
            assert_eq!(
                other.capture.validate_expecting(&other.prepared, expected),
                Err(BotError::Invalid)
            );
        }
        assert_eq!(
            other.capture.validate_retained(&other.prepared),
            Err(BotError::Invalid)
        );
        assert_eq!(other.validate(), Err(BotError::Invalid));
    }
    // Stating the Test expectation relaxes nothing else.
    let mut late = test.clone();
    late.capture.schedule_definition.effective_at_ms = late.prepared.slot.occurrence_ms + 1;
    assert_eq!(
        late.capture
            .validate_expecting(&late.prepared, TestPausedManual),
        Err(BotError::Invalid)
    );
}
