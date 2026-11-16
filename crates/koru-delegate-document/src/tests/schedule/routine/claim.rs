// SPDX-License-Identifier: MPL-2.0

use super::*;

pub(super) fn prepared_document() -> (ScheduleDocument, RoutinePrepared) {
    let doc = ScheduleDocument::create(&routine_header()).unwrap();
    doc.set_lifecycle(device(5), 1, ScheduleLifecycle::Active, 2000)
        .unwrap();
    let header = doc.header().unwrap();
    let association = association();
    let ids = RoutineExecutionIds {
        submission: BotRequestId::mint(),
        run: RunId::mint(),
        chat: uuid::Uuid::new_v4(),
        attempt: uuid::Uuid::new_v4(),
        user_message: uuid::Uuid::new_v4(),
        assistant_message: uuid::Uuid::new_v4(),
    };
    let definition = RoutineDefinitionIdentity {
        revision: header.definition.revision,
        digest: routine_definition_digest(&header.definition, association.home, None).unwrap(),
        owner_handoff: None,
    };
    let ScheduleClaimPlan::Run { pinned_context, .. } = run_plan(60_000, ids.run) else {
        unreachable!()
    };
    let mut request =
        header
            .definition
            .template
            .clone()
            .into_request(ids.run, device(5), pinned_context);
    request.scope = WorkspaceScope::BotFolder {
        folder: BotWorkingFolder::new("/fixture/bot".into()).unwrap(),
    };
    request.origin = RunOrigin::Scheduled {
        schedule: header.schedule,
        occurrence_ms: 60_000,
        revision: definition.revision,
    };
    let prepared = RoutinePrepared {
        slot: RoutineSlot {
            owner: association.owner,
            bot: association.bot,
            schedule: header.schedule,
            occurrence_ms: 60_000,
        },
        definition,
        ids,
        semantic_hash: RoutineDigest32::from_bytes([1; 32]),
        preparation_hash: RoutineDigest32::from_bytes([2; 32]),
        submission_version: 2,
        submission: BotSubmission {
            provenance: BotProvenance {
                owner: association.owner,
                bot: association.bot,
                submission: ids.submission,
                chat_id: ids.chat,
                ai_attempt_id: Some(ids.attempt),
                assistant_message_id: Some(ids.assistant_message),
                config_revision: 1,
                policy_revision: 1,
                continued_from: None,
                routine: Some(RoutineProvenance {
                    schedule: header.schedule,
                    occurrence_ms: 60_000,
                    definition,
                    grant_revision: 1,
                }),
            },
            policy: CapturedBotPolicy {
                workspace: association.home,
                destination: association.home,
                access: BotAccess::Default,
                selected_pages: Default::default(),
                connections: Default::default(),
            },
            workspace: association.home,
            input: PendingInput {
                idempotency_key: IdempotencyKey::mint(),
                text: request.objective.as_str().into(),
                attachment_ids: vec![],
                references: vec![],
                mode: None,
                fast: None,
                reasoning: None,
                skills: vec![],
                model: None,
            },
            request,
        },
    };
    assert!(prepared.valid());
    (doc, prepared)
}
fn authority(doc: &ScheduleDocument, p: &RoutinePrepared) -> ScheduleClaimAuthority {
    doc.claim_authority(
        p.submission.workspace,
        p.slot.owner.device,
        ScheduleClaimPlan::BotRoutine {
            prepared: p.clone(),
            representative_miss: None,
        },
    )
    .unwrap()
    .unwrap()
}

#[test]
fn routine_live_authority_import_rechecks_association_full_definition_and_handoff() {
    for field in 0..9 {
        let (doc, p) = prepared_document();
        let token = authority(&doc, &p);
        let since = doc.version();
        let peer = ScheduleDocument::from_snapshot(&doc.export_snapshot().unwrap()).unwrap();
        if field == 8 {
            let handoff = HandoffId::mint();
            peer.request_handoff(handoff, device(6), device(6), 2100)
                .unwrap();
            peer.confirm_handoff(handoff, device(5), 2200).unwrap();
        } else if field == 7 {
            peer.doc()
                .get_map("schedule")
                .insert("schema", 3i64)
                .unwrap();
            peer.doc().commit();
        } else {
            let mut definition = peer.header().unwrap().definition;
            match field {
                0 => {
                    definition.template.requested_model =
                        Some(ModelName::new("changed-inert-model").unwrap())
                }
                1 => definition.template.grants.clear(),
                2 => definition.routine.as_mut().unwrap().bot = BotId::mint(),
                3 => definition.routine.as_mut().unwrap().home = WorkspaceId::from_uuid(v4(90)),
                4 => definition.routine.as_mut().unwrap().owner.device = device(6),
                5 => definition.routine.as_mut().unwrap().creation_request = BotRequestId::mint(),
                _ => {
                    definition.lifecycle = ScheduleLifecycle::Paused {
                        reason: SchedulePauseReason::Manual,
                    }
                }
            }
            write_definition(&peer, &definition);
        }
        doc.import(&peer.export_updates_since(&since).unwrap())
            .unwrap();
        assert!(token.into_bot_routine().is_err(), "field {field}");
    }
}

#[test]
fn routine_authority_is_not_an_ordinary_run_or_skip_and_binds_the_task_union() {
    let (doc, p) = prepared_document();
    assert!(matches!(
        authority(&doc, &p).into_run(),
        Err(ScheduleDocumentError::RoutineRequiresBotClaim)
    ));
    assert!(matches!(
        authority(&doc, &p).into_skip(),
        Err(ScheduleDocumentError::RoutineRequiresBotClaim)
    ));
    assert!(
        doc.claim_authority(
            p.submission.workspace,
            device(5),
            skip_plan(60_000, RunId::mint())
        )
        .is_err()
    );
    let mut plus = p.clone();
    plus.submission
        .request
        .context
        .push(RunContextItem::Document {
            workspace: plus.submission.workspace,
            document: DocumentId::from_uuid(v4(80)),
            version: DocumentVersionId::from_uuid(v4(81)),
        });
    assert_eq!(
        authority(&doc, &plus)
            .into_bot_routine()
            .unwrap()
            .prepared(),
        &plus
    );
    for change in 0..4 {
        let mut invalid = p.clone();
        match change {
            0 => invalid.submission.request.context.clear(),
            1 => invalid.submission.input.text = "different task".into(),
            2 => invalid.definition.digest = RoutineDigest32::from_bytes([42; 32]),
            _ => invalid.slot.bot = BotId::mint(),
        }
        assert!(
            doc.claim_authority(
                p.submission.workspace,
                device(5),
                ScheduleClaimPlan::BotRoutine {
                    prepared: invalid,
                    representative_miss: None
                }
            )
            .is_err()
        );
    }
    assert!(
        doc.claim_authority(
            WorkspaceId::from_uuid(v4(91)),
            device(5),
            ScheduleClaimPlan::BotRoutine {
                prepared: p,
                representative_miss: None
            }
        )
        .is_err()
    );
}
