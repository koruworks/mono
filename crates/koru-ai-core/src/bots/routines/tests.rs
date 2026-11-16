// SPDX-License-Identifier: MPL-2.0
use super::super::UserScopeId;
use super::super::{BotCapturedMemory, BotKnowledgeCapture, BotPage};
use super::*;
use koru_delegate_schedule::{DelegationTemplate, RecurrenceRule, ScheduleContextPointer};
use uuid::Uuid;
mod access;

fn id(n: u8) -> Uuid {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
        .parse()
        .unwrap()
}
fn digest(n: u8) -> RoutineDigest32 {
    RoutineDigest32::from_bytes([n; 32])
}
fn creation() -> BotRoutineCreationIdentity {
    let owner = BotOwner {
        user_scope: UserScopeId::from_uuid(id(1)),
        device: DeviceId::from_uuid(id(2)),
    };
    let home = WorkspaceId::from_uuid(id(3));
    let bot = BotId::from_uuid(id(4));
    let schedule = ScheduleId::from_uuid(id(5));
    let creation_request = BotRequestId::from_uuid(id(6));
    let association = RoutineAssociation {
        owner,
        bot,
        home,
        creation_request,
    };
    let folder = if cfg!(windows) {
        "C:/fixture/work"
    } else {
        "/fixture/work"
    };
    BotRoutineCreationIdentity {
        owner,
        bot,
        home,
        schedule,
        creation_request,
        store_request: id(7),
        expected_bot_revision: 2,
        command_fingerprint: digest(1),
        seed_hash: digest(2),
        seed: BotRoutineSeed {
            version: 1,
            name: ScheduleName::new("PRIVATE-ROUTINE-NAME").unwrap(),
            header: ScheduleHeader {
                schedule,
                created_at_ms: 0,
                initial_owner: owner.device,
                definition: ScheduleDefinition {
                    revision: 1,
                    effective_at_ms: 0,
                    template: DelegationTemplate {
                        requested_by: ActorRef::Person {
                            profile: ProfileId::from_uuid(id(8)),
                        },
                        objective: Objective::new("PRIVATE-ROUTINE-OBJECTIVE").unwrap(),
                        agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                        requested_model: Some(ModelName::new("fixture-model").unwrap()),
                        scope: WorkspaceScope::BotFolder {
                            folder: BotWorkingFolder::new(folder.into()).unwrap(),
                        },
                        grants: [home].into(),
                        context: vec![ScheduleContextPointer::Document {
                            workspace: home,
                            document: DocumentId::from_uuid(id(9)),
                        }],
                    },
                    recurrence: serde_json::from_str::<RecurrenceRule>(
                        r#"{"type":"floating","cron":"0 9 * * *"}"#,
                    )
                    .unwrap(),
                    lifecycle: ScheduleLifecycle::Paused {
                        reason: SchedulePauseReason::Manual,
                    },
                    routine: Some(association),
                },
            },
        },
        grant: BotRoutineGrant {
            source_bot_policy_revision: 2,
            grant_revision: 1,
            policy: CapturedBotPolicy {
                workspace: home,
                destination: home,
                access: BotAccess::Default,
                selected_pages: Default::default(),
            },
            digest: digest(3),
        },
    }
}
pub(super) fn preparation() -> BotRoutinePreparation {
    let c = creation();
    let mut definition = c.seed.header.definition.clone();
    definition.revision = 2;
    definition.lifecycle = ScheduleLifecycle::Active;
    let ids = RoutineExecutionIds {
        submission: BotRequestId::from_uuid(id(11)),
        run: RunId::from_uuid(id(12)),
        chat: id(13),
        attempt: id(14),
        user_message: id(15),
        assistant_message: id(16),
    };
    let role = BotKnowledgeCapture {
        page: BotPage {
            workspace: c.home,
            document: DocumentId::from_uuid(id(17)),
        },
        version: DocumentVersionId::from_uuid(id(18)),
    };
    let task = BotKnowledgeCapture {
        page: BotPage {
            workspace: c.home,
            document: DocumentId::from_uuid(id(9)),
        },
        version: DocumentVersionId::from_uuid(id(19)),
    };
    let context: Vec<_> = [&role, &task]
        .into_iter()
        .map(|p| RunContextItem::Document {
            workspace: p.page.workspace,
            document: p.page.document,
            version: p.version,
        })
        .collect();
    let canonical = BotRoutineCanonical {
        home: c.home,
        config_revision: 2,
        policy_revision: 2,
        config: BotConfig {
            name: "PRIVATE-BOT-NAME".into(),
            role: role.page.clone(),
            working_folder: match &definition.template.scope {
                WorkspaceScope::BotFolder { folder } => folder.as_str().into(),
                _ => unreachable!(),
            },
            model: crate::ModelSelection {
                provider: crate::AiProviderId::parse("agent-codex").unwrap(),
                requested_model: "fixture-model".into(),
                default_reasoning: crate::ReasoningChoice::Default,
            },
            mode: None,
            fast: None,
        },
    };
    let identity = RoutineDefinitionIdentity {
        revision: definition.revision,
        digest: digest(4),
        owner_handoff: None,
    };
    let prepared = RoutinePrepared {
        slot: RoutineSlot {
            owner: c.owner,
            bot: c.bot,
            schedule: c.schedule,
            occurrence_ms: 60_000,
        },
        definition: identity,
        ids,
        semantic_hash: digest(5),
        preparation_hash: digest(6),
        submission_version: 2,
        submission: BotSubmission {
            provenance: BotProvenance {
                owner: c.owner,
                bot: c.bot,
                submission: ids.submission,
                chat_id: ids.chat,
                ai_attempt_id: Some(ids.attempt),
                assistant_message_id: Some(ids.assistant_message),
                config_revision: 2,
                policy_revision: 2,
                continued_from: None,
                routine: Some(RoutineProvenance {
                    schedule: c.schedule,
                    occurrence_ms: 60_000,
                    definition: identity,
                    grant_revision: 1,
                }),
            },
            policy: c.grant.policy.clone(),
            workspace: c.home,
            request: definition
                .template
                .clone()
                .into_request(ids.run, c.owner.device, context),
            input: PendingInput {
                idempotency_key: id(20).to_string().parse().unwrap(),
                text: definition.template.objective.as_str().into(),
                attachment_ids: vec![],
                references: vec![],
                mode: None,
                fast: None,
                reasoning: None,
                skills: vec![],
                model: Some("fixture-model".parse().unwrap()),
            },
        },
    };
    let mut value = BotRoutinePreparation {
        prepared,
        capture: BotRoutineCaptureV1 {
            version: 1,
            creation: c.route(),
            schedule_definition: definition,
            canonical,
            grant: c.grant,
            library: BotLibraryCapture {
                version: 1,
                owner: super::super::BotOwner {
                    user_scope: c.owner.user_scope,
                    device: c.owner.device,
                },
                bot: c.bot,
                submission: ids.submission,
                config_revision: 2,
                knowledge: vec![],
                memory: BotCapturedMemory::Absent {},
                index_digest: [1; 32],
            },
            role,
            task: vec![task],
        },
        stage: BotRoutinePreparationStage::Prepared {},
        created_at_ms: 60_001,
        updated_at_ms: 60_001,
    };
    value.prepared.submission.request.origin = RunOrigin::Scheduled {
        schedule: c.schedule,
        occurrence_ms: 60_000,
        revision: 2,
    };
    value
}

#[test]
fn routine_seed_freezes_complete_paused_header_and_independent_grant() {
    let c = creation();
    c.validate().unwrap();
    let bytes = serde_json::to_vec(&c).unwrap();
    assert_eq!(
        serde_json::from_slice::<BotRoutineCreationIdentity>(&bytes).unwrap(),
        c
    );
    for mutation in 0..6 {
        let mut bad = c.clone();
        match mutation {
            0 => bad.seed.header.initial_owner = DeviceId::from_uuid(id(22)),
            1 => bad.seed.header.definition.lifecycle = ScheduleLifecycle::Active,
            2 => bad.seed.header.definition.routine = None,
            3 => bad.seed.header.definition.effective_at_ms += 1,
            4 => bad.grant.grant_revision = 2,
            _ => bad.seed.header.schedule = ScheduleId::mint(),
        }
        assert!(bad.validate().is_err());
    }
    let json = serde_json::to_value(&c.grant).unwrap();
    assert!(json.get("effects_allowed").is_none());
    assert!(json.get("paused").is_none());
}
#[test]
fn routine_private_seed_rejects_unknown_nested_fields_without_changing_ordinary_schedule() {
    let c = creation();
    for pointer in [
        "",
        "/seed",
        "/seed/header",
        "/seed/header/definition",
        "/seed/header/definition/template",
        "/seed/header/definition/template/agent",
        "/seed/header/definition/template/context/0",
        "/seed/header/definition/lifecycle/paused/reason",
        "/grant",
        "/grant/policy",
    ] {
        let mut json = serde_json::to_value(&c).unwrap();
        json.pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unexpected".into(), true.into());
        assert!(
            serde_json::from_value::<BotRoutineCreationIdentity>(json).is_err(),
            "{pointer}"
        );
    }
    let mut legacy = serde_json::to_value(&c.seed.header.definition).unwrap();
    legacy["template"]["unexpected"] = true.into();
    assert!(serde_json::from_value::<ScheduleDefinition>(legacy).is_ok());
}
#[test]
fn routine_creation_state_requires_exact_bound_vv_and_conflict_refusal() {
    let c = creation();
    let mut value = BotRoutineCreation {
        identity: c.clone(),
        state: BotRoutineCreationState::Prepared,
        origin_edit_revision: None,
        refusal: None,
        created_at_ms: 0,
        updated_at_ms: 1,
    };
    value.validate().unwrap();
    value.state = BotRoutineCreationState::Bound;
    assert!(value.validate().is_err());
    value.origin_edit_revision = Some(vec![1]);
    value.validate().unwrap();
    value.origin_edit_revision = Some(vec![0; BOT_ROUTINE_VV_BYTES + 1]);
    assert!(value.validate().is_err());
    value.origin_edit_revision = None;
    value.state = BotRoutineCreationState::Conflict;
    assert!(value.validate().is_err());
    value.refusal = Some(BotRoutineCreationRefusal::DocumentChanged);
    value.validate().unwrap();
    assert_eq!(value.observed().updated_at_ms, 1);
    let confirmation = BotRoutineStoreConfirmation {
        identity: c.clone(),
        origin_edit_revision: vec![1],
    };
    confirmation.validate(&c).unwrap();
    let mut changed = c;
    changed.seed_hash = digest(99);
    assert!(confirmation.validate(&changed).is_err());
}
#[test]
fn routine_creation_receipt_retains_original_candidate_without_seed_content() {
    let c = creation();
    let receipt = BotRoutineCreationReceipt {
        version: 1,
        route: c.route(),
        state: BotRoutineCreationState::Bound,
        refusal: None,
    };
    receipt.validate(&c).unwrap();
    let encoded = serde_json::to_string(&receipt).unwrap();
    assert!(!encoded.contains("PRIVATE"));
    assert!(!encoded.contains("policy"));
    let mut changed = c;
    changed.schedule = ScheduleId::mint();
    assert!(receipt.validate(&changed).is_err());
}
#[test]
fn routine_capture_matches_exact_role_task_union_and_canonical_runtime() {
    let p = preparation();
    p.validate().unwrap();
    assert_eq!(
        serde_json::from_slice::<BotRoutinePreparation>(&serde_json::to_vec(&p).unwrap()).unwrap(),
        p
    );
    for mutation in 0..8 {
        let mut bad = p.clone();
        match mutation {
            0 => bad
                .prepared
                .submission
                .request
                .context
                .pop()
                .map(|_| ())
                .unwrap(),
            1 => bad.capture.task[0].version = DocumentVersionId::from_uuid(id(33)),
            2 => bad
                .capture
                .canonical
                .config
                .working_folder
                .push_str("/other"),
            3 => bad.capture.canonical.config.model.requested_model = "other".into(),
            4 => bad.capture.library.submission = BotRequestId::mint(),
            5 => bad
                .capture
                .grant
                .policy
                .selected_pages
                .insert(DocumentId::from_uuid(id(34)))
                .then_some(())
                .unwrap(),
            6 => bad.capture.creation.seed_hash = digest(33), // A digest's shape alone is not recomputation proof.
            _ => bad.prepared.submission.input.fast = Some(true),
        }
        if mutation == 6 {
            bad.validate().unwrap();
        } else {
            assert!(bad.validate().is_err(), "mutation {mutation}");
        }
    }
    let debug = format!("{p:?}");
    for secret in [
        "PRIVATE-ROUTINE-OBJECTIVE",
        "PRIVATE-BOT-NAME",
        "fixture-model",
        "selected_pages",
    ] {
        assert!(!debug.contains(secret), "{secret}");
    }
}
#[test]
fn routine_capture_refuses_memory_as_general_context_and_duplicate_pins() {
    let p = preparation();
    let mut bad = p.clone();
    bad.capture.library.memory = BotCapturedMemory::Captured {
        page: bad.capture.role.page.clone(),
        version: bad.capture.role.version,
        creation_request: BotRequestId::mint(),
        nodes: vec![],
        omitted_count: 0,
        invalid_count: 0,
    };
    assert!(bad.validate().is_err());
    let mut bad = p;
    bad.prepared.submission.request.context[1] = bad.prepared.submission.request.context[0].clone();
    assert!(bad.validate().is_err());
}

#[test]
fn routine_capture_accepts_union33_and_refuses_a_seventeenth_task() {
    let mut p = preparation();
    p.capture.library.knowledge.clear();
    p.capture.task.clear();
    p.capture.schedule_definition.template.context.clear();
    p.prepared.submission.request.context.truncate(1);
    for (offset, knowledge) in [(40, true), (80, false)] {
        for n in 0..16 {
            let pin = BotKnowledgeCapture {
                page: BotPage {
                    workspace: p.capture.canonical.home,
                    document: DocumentId::from_uuid(id(offset + n)),
                },
                version: DocumentVersionId::from_uuid(id(offset + n + 16)),
            };
            p.prepared
                .submission
                .request
                .context
                .push(RunContextItem::Document {
                    workspace: pin.page.workspace,
                    document: pin.page.document,
                    version: pin.version,
                });
            if knowledge {
                p.capture.library.knowledge.push(pin);
            } else {
                p.capture.schedule_definition.template.context.push(
                    ScheduleContextPointer::Document {
                        workspace: pin.page.workspace,
                        document: pin.page.document,
                    },
                );
                p.capture.task.push(pin);
            }
        }
    }
    assert_eq!(p.prepared.submission.request.context.len(), 33);
    p.validate().unwrap();
    p.capture.task.push(p.capture.task[0].clone());
    p.capture
        .schedule_definition
        .template
        .context
        .push(p.capture.schedule_definition.template.context[0].clone());
    assert!(p.validate().is_err());
}
#[test]
fn routine_close_needs_positive_exact_slot_evidence_and_real_deadline() {
    let p = preparation();
    let absent = |time| RoutineClaimObservation::Absent {
        slot: p.prepared.slot,
        observed_at_ms: time,
    };
    assert!(!p.can_close(
        BotRoutineCloseReason::ExpiredUnclaimed,
        &absent(119_999),
        120_000
    ));
    assert!(p.can_close(
        BotRoutineCloseReason::ExpiredUnclaimed,
        &absent(120_000),
        120_000
    ));
    assert!(!p.can_close(
        BotRoutineCloseReason::SlotSkipped,
        &absent(120_000),
        120_000
    ));
    let mut claim = RoutineClaimIdentity {
        slot: p.prepared.slot,
        definition: p.prepared.definition,
        ids: p.prepared.ids,
        preparation_hash: p.prepared.preparation_hash,
        claimed_at_ms: 60_010,
    };
    let record = |claim| RoutineClaimObservation::Claimed {
        record: RoutineClaimRecord {
            claim,
            submission_version: 2,
            submission: p.prepared.submission.clone(),
            link_state: RoutineLinkState::Held {},
            updated_at_ms: 60_010,
        },
    };
    assert!(!p.can_close(
        BotRoutineCloseReason::OtherWinner,
        &record(claim.clone()),
        120_000
    ));
    claim.preparation_hash = digest(77);
    assert!(p.can_close(BotRoutineCloseReason::OtherWinner, &record(claim), 120_000));
    let mut closed = p.clone();
    closed.stage = BotRoutinePreparationStage::Closed {
        reason: BotRoutineCloseReason::ExpiredUnclaimed,
    };
    assert!(!closed.can_close(
        BotRoutineCloseReason::ExpiredUnclaimed,
        &absent(120_000),
        120_000
    ));
}
#[test]
fn routine_linked_preparation_keeps_complete_claim_identity() {
    let mut p = preparation();
    let mut claim = RoutineClaimIdentity {
        slot: p.prepared.slot,
        definition: p.prepared.definition,
        ids: p.prepared.ids,
        preparation_hash: p.prepared.preparation_hash,
        claimed_at_ms: 60_010,
    };
    p.stage = BotRoutinePreparationStage::Linked {
        acknowledgement: RoutineLinkAcknowledgement {
            claim: claim.clone(),
            outcome: RoutineLinkOutcome::Linked {},
        },
    };
    p.validate().unwrap();
    claim.ids.user_message = Uuid::new_v4();
    p.stage = BotRoutinePreparationStage::Linked {
        acknowledgement: RoutineLinkAcknowledgement {
            claim,
            outcome: RoutineLinkOutcome::Linked {},
        },
    };
    assert!(p.validate().is_err());
}
#[test]
fn routine_counting_bound_and_title_byte_bound_are_exact() {
    assert!(bounded(&"abcd", 6).is_ok());
    assert!(bounded(&"abcd", 5).is_err());
    let mut c = creation();
    c.seed.name = ScheduleName::new("é".repeat(128)).unwrap();
    c.validate().unwrap();
    c.seed.name = ScheduleName::new("界".repeat(86)).unwrap();
    assert!(c.validate().is_err());
    let encoded = serde_json::to_vec(&preparation().capture).unwrap();
    assert!(bounded(&preparation().capture, encoded.len()).is_ok());
    assert!(bounded(&preparation().capture, encoded.len() - 1).is_err());
}
#[test]
fn routine_neutral_dependency_edge_keeps_delegate_core_and_schedule_free_of_ai() {
    let ai = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
    assert!(ai.contains("koru-delegate-schedule/schema"));
    for manifest in [
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../koru-delegate-core/Cargo.toml"
        )),
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../koru-delegate-schedule/Cargo.toml"
        )),
    ] {
        assert!(!manifest.contains("koru-ai-"));
    }
}
