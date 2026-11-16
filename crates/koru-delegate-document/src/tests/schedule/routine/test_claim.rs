// SPDX-License-Identifier: MPL-2.0
//! The owner Test authority: minted only for exactly Paused(Manual), and never
//! a way for any other path to claim a paused Routine.
use super::claim::prepared_document;
use super::*;
use crate::{RoutineExpiryPlan, RoutineTestAuthority};

fn manual() -> ScheduleLifecycle {
    ScheduleLifecycle::Paused {
        reason: SchedulePauseReason::Manual,
    }
}

/// Rebind a prepared occurrence to the document's current definition.
fn rebind(doc: &ScheduleDocument, prepared: &mut RoutinePrepared) {
    let header = doc.header().unwrap();
    let epoch = doc.handoff_state().unwrap().epoch;
    prepared.definition = RoutineDefinitionIdentity {
        revision: header.definition.revision,
        digest: routine_definition_digest(&header.definition, association().home, epoch).unwrap(),
        owner_handoff: epoch,
    };
    prepared
        .submission
        .provenance
        .routine
        .as_mut()
        .unwrap()
        .definition = prepared.definition;
    prepared.submission.request.origin = RunOrigin::Scheduled {
        schedule: header.schedule,
        occurrence_ms: prepared.slot.occurrence_ms,
        revision: header.definition.revision,
    };
    assert!(prepared.valid());
}

/// Revision 3: created paused, turned on, paused again by the owner.
fn paused_document() -> (ScheduleDocument, RoutinePrepared) {
    let (doc, mut prepared) = prepared_document();
    doc.set_lifecycle(device(5), 2, manual(), 3000).unwrap();
    rebind(&doc, &mut prepared);
    (doc, prepared)
}

fn mint(doc: &ScheduleDocument, prepared: &RoutinePrepared) -> RoutineTestAuthority {
    doc.test_claim_authority(association().home, device(5), prepared.clone())
        .unwrap()
}

fn ordinary_plan(prepared: &RoutinePrepared) -> ScheduleClaimPlan {
    ScheduleClaimPlan::BotRoutine {
        prepared: prepared.clone(),
        representative_miss: None,
    }
}

fn expiry_plan() -> RoutineExpiryPlan {
    RoutineExpiryPlan {
        occurrence_ms: 60_000,
        planned_run: RunId::mint(),
        representative_miss: None,
    }
}

#[test]
fn routine_test_claim_cannot_be_minted_for_an_active_routine() {
    let (doc, prepared) = prepared_document();
    assert!(doc.may_claim(device(5)).unwrap());
    assert_eq!(
        doc.test_claim_authority(association().home, device(5), prepared.clone())
            .unwrap_err(),
        ScheduleDocumentError::LifecycleConflict
    );
    // The ordinary path still owns the Active Routine, untouched by the refusal.
    assert!(
        doc.claim_authority(association().home, device(5), ordinary_plan(&prepared))
            .unwrap()
            .is_some()
    );
}

#[test]
fn routine_test_claim_leaves_a_paused_routine_unclaimable_by_the_due_and_expiry_paths() {
    let (doc, prepared) = paused_document();
    let unattended_paths_refuse = |doc: &ScheduleDocument| {
        assert!(!doc.may_claim(device(5)).unwrap());
        assert!(
            doc.claim_authority(association().home, device(5), ordinary_plan(&prepared))
                .unwrap()
                .is_none()
        );
        assert!(
            doc.expired_routine_authority(association().home, device(5), expiry_plan())
                .unwrap()
                .is_none()
        );
    };
    unattended_paths_refuse(&doc);
    // Minting and consuming a Test changes nothing the other paths read.
    let authority = mint(&doc, &prepared);
    unattended_paths_refuse(&doc);
    let version = doc.version();
    authority.into_bot_routine_test().unwrap();
    assert_eq!(doc.version(), version, "consuming writes nothing");
    unattended_paths_refuse(&doc);
    assert_eq!(doc.header().unwrap().definition.lifecycle, manual());
}

#[test]
fn routine_test_claim_facts_carry_the_paused_definition_and_no_representative_miss() {
    let (doc, prepared) = paused_document();
    let header = doc.header().unwrap();
    let facts = mint(&doc, &prepared).into_bot_routine_test().unwrap();
    assert_eq!(facts.prepared(), &prepared);
    assert_eq!(facts.prepared().definition.revision, 3);
    assert_eq!(
        facts.prepared().definition.digest,
        routine_definition_digest(&header.definition, association().home, None).unwrap()
    );
    assert_eq!(facts.workspace(), association().home);
    assert_eq!(facts.creation_request(), association().creation_request);
    assert_eq!(facts.representative_miss(), None);
    assert_eq!(header.definition.lifecycle, manual());
}

#[test]
fn routine_test_claim_is_refused_at_consume_after_an_enable() {
    let (doc, prepared) = paused_document();
    let authority = mint(&doc, &prepared);
    doc.set_lifecycle(device(5), 3, ScheduleLifecycle::Active, 4000)
        .unwrap();
    assert_eq!(
        authority.into_bot_routine_test().unwrap_err(),
        ScheduleDocumentError::StaleClaimAuthority
    );
    // An authority minted before the Enable cannot be re-minted after it.
    assert_eq!(
        doc.test_claim_authority(association().home, device(5), prepared)
            .unwrap_err(),
        ScheduleDocumentError::LifecycleConflict
    );
}

#[test]
fn routine_test_claim_is_refused_at_consume_after_any_imported_authority_change() {
    for change in 0..8 {
        let (doc, prepared) = paused_document();
        let authority = mint(&doc, &prepared);
        let since = doc.version();
        let peer = ScheduleDocument::from_snapshot(&doc.export_snapshot().unwrap()).unwrap();
        match change {
            // Enable, then pause again: paused, but no longer the same definition.
            0 => {
                peer.set_lifecycle(device(5), 3, ScheduleLifecycle::Active, 4000)
                    .unwrap();
                peer.set_lifecycle(device(5), 4, manual(), 5000).unwrap();
            }
            1 => {
                let mut definition = peer.header().unwrap().definition;
                definition.revision += 1;
                definition.effective_at_ms += 1;
                definition.recurrence =
                    RecurrenceRule::floating(CronExpression::parse("5 4 * * *").unwrap());
                peer.edit(
                    device(5),
                    3,
                    &peer.name().unwrap(),
                    &peer.name().unwrap(),
                    &definition,
                )
                .unwrap();
            }
            2 => {
                let mut definition = peer.header().unwrap().definition;
                definition.lifecycle = ScheduleLifecycle::Paused {
                    reason: SchedulePauseReason::ProviderNotDelegatable,
                };
                write_definition(&peer, &definition);
            }
            3 => {
                peer.retire(device(5), 4000).unwrap();
            }
            4 => {
                peer.request_handoff(HandoffId::mint(), device(6), device(6), 4000)
                    .unwrap();
            }
            5 => {
                let id = HandoffId::mint();
                peer.request_handoff(id, device(6), device(6), 4000)
                    .unwrap();
                peer.confirm_handoff(id, device(5), 4100).unwrap();
            }
            6 => {
                peer.doc()
                    .get_map("schedule")
                    .insert("schema", 3i64)
                    .unwrap();
                peer.doc().commit();
            }
            _ => {
                let mut definition = peer.header().unwrap().definition;
                definition.routine.as_mut().unwrap().bot = BotId::mint();
                write_definition(&peer, &definition);
            }
        }
        doc.import(&peer.export_updates_since(&since).unwrap())
            .unwrap();
        assert!(
            authority.into_bot_routine_test().is_err(),
            "change {change}"
        );
    }
}

#[test]
fn routine_test_claim_mint_requires_the_exact_paused_owner_and_binding() {
    // Paused for a reason other than the owner's own pause.
    let (doc, mut prepared) = paused_document();
    let mut definition = doc.header().unwrap().definition;
    definition.lifecycle = ScheduleLifecycle::Paused {
        reason: SchedulePauseReason::ScopeUnavailable,
    };
    write_definition(&doc, &definition);
    rebind(&doc, &mut prepared);
    assert_eq!(
        doc.test_claim_authority(association().home, device(5), prepared)
            .unwrap_err(),
        ScheduleDocumentError::LifecycleConflict
    );

    let (doc, prepared) = paused_document();
    // Another workspace, and a device that is not the association's owner.
    assert!(
        doc.test_claim_authority(WorkspaceId::from_uuid(v4(90)), device(5), prepared.clone())
            .is_err()
    );
    assert_eq!(
        doc.test_claim_authority(association().home, device(6), prepared.clone())
            .unwrap_err(),
        ScheduleDocumentError::NotOwner
    );
    // A preparation of an earlier definition, or of a minute before this one
    // took effect, binds nothing.
    let mut stale = prepared.clone();
    stale.definition.digest = RoutineDigest32::from_bytes([9; 32]);
    stale
        .submission
        .provenance
        .routine
        .as_mut()
        .unwrap()
        .definition = stale.definition;
    assert_eq!(
        doc.test_claim_authority(association().home, device(5), stale)
            .unwrap_err(),
        ScheduleDocumentError::ClaimBindingMismatch
    );
    let mut early = prepared.clone();
    early.slot.occurrence_ms = 0;
    early
        .submission
        .provenance
        .routine
        .as_mut()
        .unwrap()
        .occurrence_ms = 0;
    early.submission.request.origin = RunOrigin::Scheduled {
        schedule: early.slot.schedule,
        occurrence_ms: 0,
        revision: early.definition.revision,
    };
    assert!(early.valid());
    assert_eq!(
        doc.test_claim_authority(association().home, device(5), early)
            .unwrap_err(),
        ScheduleDocumentError::ClaimBindingMismatch
    );

    // A pending handoff, then a retirement.
    doc.request_handoff(HandoffId::mint(), device(6), device(6), 4000)
        .unwrap();
    assert_eq!(
        doc.test_claim_authority(association().home, device(5), prepared.clone())
            .unwrap_err(),
        ScheduleDocumentError::HandoffPending
    );
    let (doc, prepared) = paused_document();
    doc.retire(device(5), 4000).unwrap();
    assert_eq!(
        doc.test_claim_authority(association().home, device(5), prepared.clone())
            .unwrap_err(),
        ScheduleDocumentError::Retired
    );
    // An ordinary schedule has no Test.
    assert!(
        schedule()
            .test_claim_authority(association().home, device(5), prepared)
            .is_err()
    );
}
