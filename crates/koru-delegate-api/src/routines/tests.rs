// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::*;
use koru_delegate_schedule::{DelegationTemplate, RecurrenceRule, RoutineAssociation};

fn id(n: u8) -> Uuid {
    format!("abcdef00-0000-4000-8000-0000000000{n:02x}")
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
        user_scope: UserScopeId::from_uuid(id(10)),
        device: DeviceId::from_uuid(id(11)),
    };
    let slot = RoutineSlot {
        owner,
        bot: BotId::from_uuid(id(12)),
        schedule: ScheduleId::from_uuid(id(13)),
        occurrence_ms: 60_000,
    };
    let workspace = WorkspaceId::from_uuid(id(14));
    let definition = RoutineDefinitionIdentity {
        revision: 1,
        digest: RoutineDigest32::from_bytes([1; 32]),
        owner_handoff: None,
    };
    RoutinePrepared {
        slot,
        definition,
        ids,
        semantic_hash: RoutineDigest32::from_bytes([2; 32]),
        preparation_hash: RoutineDigest32::from_bytes([3; 32]),
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
            request: DelegationRequest {
                run: ids.run,
                requested_by: ActorRef::Person {
                    profile: ProfileId::from_uuid(id(15)),
                },
                objective: Objective::new("PRIVATE-BRIDGE-OBJECTIVE").unwrap(),
                agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                requested_model: None,
                scope: WorkspaceScope::BotFolder {
                    folder: BotWorkingFolder::new("/fixture/bot".into()).unwrap(),
                },
                context: vec![],
                grants: [workspace].into(),
                executing_device: owner.device,
                origin: RunOrigin::Scheduled {
                    schedule: slot.schedule,
                    occurrence_ms: slot.occurrence_ms,
                    revision: definition.revision,
                },
            },
            workspace,
            input: PendingInput {
                idempotency_key: IdempotencyKey::mint(),
                text: "PRIVATE-BRIDGE-INPUT".into(),
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
fn prepare(p: &RoutinePrepared) -> PrepareOccurrence {
    let r = &p.submission.request;
    PrepareOccurrence {
        slot: p.slot,
        workspace: p.submission.workspace,
        owner_handoff: p.definition.owner_handoff,
        schedule_definition: ScheduleDefinition {
            revision: p.definition.revision,
            effective_at_ms: 0,
            template: DelegationTemplate {
                requested_by: r.requested_by.clone(),
                objective: r.objective.clone(),
                agent: r.agent.clone(),
                requested_model: r.requested_model.clone(),
                scope: r.scope.clone(),
                grants: r.grants.clone(),
                context: vec![ScheduleContextPointer::Document {
                    workspace: p.submission.workspace,
                    document: DocumentId::from_uuid(id(16)),
                }],
            },
            recurrence: RecurrenceRule::floating("* * * * *".parse().unwrap()),
            lifecycle: ScheduleLifecycle::Active,
            routine: Some(RoutineAssociation {
                owner: p.slot.owner,
                bot: p.slot.bot,
                home: p.submission.workspace,
                creation_request: BotRequestId::from_uuid(id(17)),
            }),
        },
    }
}
fn claim(p: &RoutinePrepared) -> RoutineClaimIdentity {
    RoutineClaimIdentity {
        slot: p.slot,
        definition: p.definition,
        ids: p.ids,
        preparation_hash: p.preparation_hash,
        claimed_at_ms: 60_001,
    }
}
fn roundtrip<T>(envelope: &RoutineBridgeEnvelope<T>)
where
    T: RoutineBridgePayload + serde::Serialize + serde::de::DeserializeOwned + PartialEq,
{
    let bytes = envelope.encode().unwrap();
    assert_eq!(
        RoutineBridgeEnvelope::<T>::decode(&bytes).unwrap(),
        *envelope
    );
}

#[test]
fn routine_bridge_envelope_routes_version_and_canonical_request_identity_are_exact() {
    assert_eq!(ROUTINE_BRIDGE_METHOD, "POST");
    assert_eq!(
        [
            ROUTINE_PREPARE_ROUTE,
            ROUTINE_PREPARATION_ROUTE,
            ROUTINE_FINALIZE_ROUTE,
            ROUTINE_LINK_ROUTE
        ],
        [
            "/internal/bots/routines/v1/prepare",
            "/internal/bots/routines/v1/preparation",
            "/internal/bots/routines/v1/finalize",
            "/internal/bots/routines/v1/link"
        ]
    );
    assert_eq!(ROUTINE_BRIDGE_DEADLINE, std::time::Duration::from_secs(5));
    let request = RoutineBridgeEnvelope::new(prepare(&prepared())).unwrap();
    assert!(v4(request.request_id));
    roundtrip(&request);
    for version in [0, 2, 255] {
        let mut json = serde_json::to_value(&request).unwrap();
        json["version"] = version.into();
        assert!(
            RoutineBridgeEnvelope::<PrepareOccurrence>::decode(&serde_json::to_vec(&json).unwrap())
                .is_err()
        );
    }
    for bad in [
        id(70).to_string().to_uppercase(),
        id(70).simple().to_string(),
        Uuid::nil().to_string(),
        "abcdef00-0000-5000-8000-000000000000".into(),
    ] {
        let mut json = serde_json::to_value(&request).unwrap();
        json["request_id"] = bad.into();
        assert!(
            RoutineBridgeEnvelope::<PrepareOccurrence>::decode(&serde_json::to_vec(&json).unwrap())
                .is_err()
        );
    }
    let mut invalid = request.clone();
    invalid.version = 2;
    assert!(invalid.encode().is_err());
    invalid = request;
    invalid.request_id = Uuid::nil();
    assert!(invalid.encode().is_err());
}

#[test]
fn routine_bridge_closes_nested_source_and_prepared_reply_fields() {
    let p = prepared();
    let request = RoutineBridgeEnvelope::new(prepare(&p)).unwrap();
    for pointer in [
        "",
        "/payload",
        "/payload/slot",
        "/payload/slot/owner",
        "/payload/schedule_definition",
        "/payload/schedule_definition/template",
        "/payload/schedule_definition/template/requested_by",
        "/payload/schedule_definition/template/agent",
        "/payload/schedule_definition/template/scope",
        "/payload/schedule_definition/template/context/0",
        "/payload/schedule_definition/routine",
        "/payload/schedule_definition/recurrence",
    ] {
        let mut json = serde_json::to_value(&request).unwrap();
        json.pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), true.into());
        assert!(
            RoutineBridgeEnvelope::<PrepareOccurrence>::decode(&serde_json::to_vec(&json).unwrap())
                .is_err(),
            "{pointer}"
        );
    }
    let reply = RoutineBridgeEnvelope::reply_to(
        &request,
        PrepareOccurrenceReply::Prepared { preparation: p },
    )
    .unwrap();
    for pointer in [
        "/payload",
        "/payload/preparation",
        "/payload/preparation/definition",
        "/payload/preparation/ids",
        "/payload/preparation/submission",
        "/payload/preparation/submission/provenance",
        "/payload/preparation/submission/provenance/routine",
        "/payload/preparation/submission/policy",
        "/payload/preparation/submission/request",
        "/payload/preparation/submission/request/agent",
        "/payload/preparation/submission/input",
    ] {
        let mut json = serde_json::to_value(&reply).unwrap();
        json.pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), true.into());
        assert!(
            RoutineBridgeEnvelope::<PrepareOccurrenceReply>::decode(
                &serde_json::to_vec(&json).unwrap()
            )
            .is_err(),
            "{pointer}"
        );
    }
    let mut legacy = serde_json::to_value(&request.payload.schedule_definition).unwrap();
    legacy["template"]["agent"]["extra"] = true.into();
    assert!(serde_json::from_value::<ScheduleDefinition>(legacy).is_ok());
    let debug = format!("{request:?} {:?} {reply:?}", request.payload);
    assert!(!debug.contains("PRIVATE-BRIDGE-OBJECTIVE"));
    assert!(!debug.contains("PRIVATE-BRIDGE-INPUT"));
    assert!(!debug.contains(&id(16).to_string()));
}

#[test]
fn routine_bridge_reply_vocabulary_roundtrips_and_missing_is_distinct_from_refusal() {
    let p = prepared();
    let c = claim(&p);
    let prepare = RoutineBridgeEnvelope::new(prepare(&p)).unwrap();
    for reply in [
        PrepareOccurrenceReply::Prepared {
            preparation: p.clone(),
        },
        PrepareOccurrenceReply::Closed {
            slot: p.slot,
            semantic_hash: p.semantic_hash,
            reason: BotRoutineCloseReason::ExpiredUnclaimed,
        },
        PrepareOccurrenceReply::Refused {
            code: RoutineBridgeRefusal::Unavailable,
        },
    ] {
        roundtrip(&RoutineBridgeEnvelope::reply_to(&prepare, reply).unwrap());
    }
    let read = RoutineBridgeEnvelope::new(ReadPreparation {
        slot: p.slot,
        semantic_hash: p.semantic_hash,
    })
    .unwrap();
    for reply in [
        ReadPreparationReply::Prepared {
            preparation: p.clone(),
        },
        ReadPreparationReply::Linked {
            slot: p.slot,
            semantic_hash: p.semantic_hash,
            claim: c.clone(),
        },
        ReadPreparationReply::Closed {
            slot: p.slot,
            semantic_hash: p.semantic_hash,
            reason: BotRoutineCloseReason::Stopped,
        },
        ReadPreparationReply::Missing {
            slot: p.slot,
            semantic_hash: p.semantic_hash,
        },
        ReadPreparationReply::Refused {
            code: RoutineBridgeRefusal::Unavailable,
        },
    ] {
        roundtrip(&RoutineBridgeEnvelope::reply_to(&read, reply).unwrap());
    }
    let finalize = RoutineBridgeEnvelope::new(FinalizeOccurrence { claim: c.clone() }).unwrap();
    for reply in [
        FinalizeOccurrenceReply::Linked { claim: c.clone() },
        FinalizeOccurrenceReply::Terminal {
            claim: c.clone(),
            reason: RoutineTerminalReason::Stopped,
        },
        FinalizeOccurrenceReply::Refused {
            code: RoutineBridgeRefusal::Unavailable,
        },
    ] {
        roundtrip(&RoutineBridgeEnvelope::reply_to(&finalize, reply).unwrap());
    }
    let link = RoutineBridgeEnvelope::new(ReadOccurrenceLink { claim: c.clone() }).unwrap();
    for reply in [
        ReadOccurrenceLinkReply::Linked { claim: c.clone() },
        ReadOccurrenceLinkReply::Terminal {
            claim: c.clone(),
            reason: RoutineTerminalReason::AlreadyTerminal,
        },
        ReadOccurrenceLinkReply::Missing { claim: c.clone() },
        ReadOccurrenceLinkReply::Refused {
            code: RoutineBridgeRefusal::Unavailable,
        },
    ] {
        roundtrip(&RoutineBridgeEnvelope::reply_to(&link, reply).unwrap());
    }
    assert!(
        ReadOccurrenceLinkReply::Missing { claim: c.clone() }
            .acknowledgement()
            .is_none()
    );
    assert!(
        ReadOccurrenceLinkReply::Refused {
            code: RoutineBridgeRefusal::Unavailable
        }
        .acknowledgement()
        .is_none()
    );
    assert!(
        FinalizeOccurrenceReply::Linked { claim: c.clone() }
            .acknowledgement()
            .unwrap()
            .matches_claim(&c)
    );
    for (code, token) in [
        (RoutineBridgeRefusal::Invalid, "invalid"),
        (RoutineBridgeRefusal::Unauthorized, "unauthorized"),
        (RoutineBridgeRefusal::Unavailable, "unavailable"),
        (RoutineBridgeRefusal::Capacity, "capacity"),
        (RoutineBridgeRefusal::Conflict, "conflict"),
        (RoutineBridgeRefusal::Stale, "stale"),
        (RoutineBridgeRefusal::Forbidden, "forbidden"),
        (RoutineBridgeRefusal::ContextTooLarge, "context_too_large"),
    ] {
        assert_eq!(
            serde_json::to_string(&code).unwrap(),
            format!("\"{token}\"")
        );
    }
    assert!(serde_json::from_str::<RoutineBridgeRefusal>("\"timeout\"").is_err());
    assert!(
        serde_json::from_value::<FinalizeOccurrenceReply>(
            serde_json::json!({"kind":"missing","claim":c})
        )
        .is_err()
    );
}

#[test]
fn routine_bridge_completion_rejects_request_slot_hash_and_full_claim_mismatch() {
    let p = prepared();
    let request = RoutineBridgeEnvelope::new(prepare(&p)).unwrap();
    let reply = RoutineBridgeEnvelope::reply_to(
        &request,
        PrepareOccurrenceReply::Prepared {
            preparation: p.clone(),
        },
    )
    .unwrap();
    let mut wrong = reply.clone();
    wrong.request_id = Uuid::new_v4();
    assert_eq!(
        wrong.validate_reply_to(&request),
        Err(RoutineBridgeCodecError::Mismatch)
    );
    for field in ["slot", "owner", "bot", "home", "revision", "handoff"] {
        let mut offered = request.clone();
        match field {
            "slot" => offered.payload.slot.occurrence_ms += 1,
            "owner" => {
                offered.payload.slot.owner.device = DeviceId::from_uuid(id(31));
                offered
                    .payload
                    .schedule_definition
                    .routine
                    .as_mut()
                    .unwrap()
                    .owner = offered.payload.slot.owner;
            }
            "bot" => {
                offered.payload.slot.bot = BotId::from_uuid(id(31));
                offered
                    .payload
                    .schedule_definition
                    .routine
                    .as_mut()
                    .unwrap()
                    .bot = offered.payload.slot.bot;
            }
            "home" => {
                offered.payload.workspace = WorkspaceId::from_uuid(id(31));
                offered
                    .payload
                    .schedule_definition
                    .routine
                    .as_mut()
                    .unwrap()
                    .home = offered.payload.workspace;
                offered.payload.schedule_definition.template.grants =
                    [offered.payload.workspace].into();
                offered.payload.schedule_definition.template.context.clear();
            }
            "revision" => offered.payload.schedule_definition.revision += 1,
            "handoff" => offered.payload.owner_handoff = Some(HandoffId::from_uuid(id(31))),
            _ => unreachable!(),
        }
        assert_eq!(
            reply.validate_reply_to(&offered),
            Err(RoutineBridgeCodecError::Mismatch),
            "{field}"
        );
    }
    let read = RoutineBridgeEnvelope::new(ReadPreparation {
        slot: p.slot,
        semantic_hash: p.semantic_hash,
    })
    .unwrap();
    let wrong_hash = RoutineDigest32::from_bytes([9; 32]);
    for response in [
        ReadPreparationReply::Prepared {
            preparation: RoutinePrepared {
                semantic_hash: wrong_hash,
                ..p.clone()
            },
        },
        ReadPreparationReply::Linked {
            slot: p.slot,
            semantic_hash: wrong_hash,
            claim: claim(&p),
        },
        ReadPreparationReply::Closed {
            slot: p.slot,
            semantic_hash: wrong_hash,
            reason: BotRoutineCloseReason::Stopped,
        },
        ReadPreparationReply::Missing {
            slot: p.slot,
            semantic_hash: wrong_hash,
        },
    ] {
        assert!(RoutineBridgeEnvelope::reply_to(&read, response).is_err());
    }
    let c = claim(&p);
    let finalize = RoutineBridgeEnvelope::new(FinalizeOccurrence { claim: c.clone() }).unwrap();
    let link = RoutineBridgeEnvelope::new(ReadOccurrenceLink { claim: c.clone() }).unwrap();
    for field in ["claim_time", "hash", "run", "chat", "definition", "slot"] {
        let mut changed = c.clone();
        match field {
            "claim_time" => changed.claimed_at_ms += 1,
            "hash" => changed.preparation_hash = wrong_hash,
            "run" => changed.ids.run = RunId::from_uuid(id(31)),
            "chat" => changed.ids.chat = id(31),
            "definition" => changed.definition.digest = wrong_hash,
            "slot" => changed.slot.occurrence_ms += 1,
            _ => unreachable!(),
        }
        assert!(
            RoutineBridgeEnvelope::reply_to(
                &finalize,
                FinalizeOccurrenceReply::Linked {
                    claim: changed.clone()
                }
            )
            .is_err(),
            "{field}"
        );
        assert!(
            RoutineBridgeEnvelope::reply_to(
                &link,
                ReadOccurrenceLinkReply::Missing { claim: changed }
            )
            .is_err(),
            "{field}"
        );
    }
}

#[test]
fn routine_bridge_complete_body_bounds_include_envelope_and_count_before_encoding() {
    let p = prepared();
    let request = RoutineBridgeEnvelope::new(ReadPreparation {
        slot: p.slot,
        semantic_hash: p.semantic_hash,
    })
    .unwrap();
    let mut response = RoutineBridgeEnvelope::reply_to(
        &request,
        ReadPreparationReply::Prepared { preparation: p },
    )
    .unwrap();
    let original = response.encode().unwrap();
    let ReadPreparationReply::Prepared { preparation } = &mut response.payload else {
        unreachable!()
    };
    let length =
        preparation.submission.input.text.len() + ROUTINE_BRIDGE_BODY_LIMIT - original.len();
    preparation.submission.input.text = "x".repeat(length);
    let bytes = response.encode().unwrap();
    assert_eq!(bytes.len(), ROUTINE_BRIDGE_BODY_LIMIT);
    roundtrip(&response);
    let ReadPreparationReply::Prepared { preparation } = &mut response.payload else {
        unreachable!()
    };
    preparation.submission.input.text.push('x');
    assert_eq!(
        response.encode(),
        Err(RoutineBridgeCodecError::ContextTooLarge)
    );
    let over = vec![b' '; ROUTINE_BRIDGE_BODY_LIMIT + 1];
    assert_eq!(
        RoutineBridgeEnvelope::<ReadPreparation>::decode(&over),
        Err(RoutineBridgeCodecError::ContextTooLarge)
    );

    struct Counted<'a> {
        calls: &'a std::cell::Cell<usize>,
        text: &'a str,
    }
    impl codec::sealed::Sealed for Counted<'_> {}
    impl RoutineBridgePayload for Counted<'_> {
        fn valid(&self) -> bool {
            true
        }
    }
    impl Serialize for Counted<'_> {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            self.calls.set(self.calls.get() + 1);
            s.serialize_str(self.text)
        }
    }
    let calls = std::cell::Cell::new(0);
    let text = "x".repeat(ROUTINE_BRIDGE_BODY_LIMIT);
    let oversized = RoutineBridgeEnvelope::new(Counted {
        calls: &calls,
        text: &text,
    })
    .unwrap();
    assert_eq!(
        oversized.encode(),
        Err(RoutineBridgeCodecError::ContextTooLarge)
    );
    assert_eq!(
        calls.get(),
        1,
        "oversize must stop before complete-body encode allocation"
    );
}

#[test]
fn routine_bridge_credential_is_exact_derived_redacted_and_parent_rotated() {
    let parent = "PRIVATE-BRIDGE-PARENT-0123456789abcdef";
    let authority = crate::bots::BotDelegateAuthority::new(parent.into()).unwrap();
    let bridge = authority.routine_bridge_authority();
    let expected = blake3::derive_key("koru.bot.routine.bridge.v1", parent.as_bytes());
    let hex = expected
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let header = bridge.authorization_header();
    assert_eq!(header, format!("Koru-Routine {hex}"));
    assert_eq!(header.len(), 13 + 64);
    assert!(bridge.matches_authorization(header.as_bytes()));
    assert_eq!(
        authority
            .clone()
            .routine_bridge_authority()
            .authorization_header(),
        header
    );
    assert!(
        !crate::bots::BotDelegateAuthority::new("PRIVATE-NEW-PARENT-0123456789abcdef".into())
            .unwrap()
            .routine_bridge_authority()
            .matches_authorization(header.as_bytes())
    );
    for index in 0..header.len() {
        let mut changed = header.as_bytes().to_vec();
        changed[index] ^= 1;
        assert!(!bridge.matches_authorization(&changed));
    }
    for bad in [
        header.to_uppercase(),
        header.to_lowercase(),
        format!("{header} "),
        format!("Bearer {hex}"),
        hex.clone(),
        String::new(),
    ] {
        assert!(!bridge.matches_authorization(bad.as_bytes()));
    }
    let debug = format!("{bridge:?} {authority:?}");
    assert!(!debug.contains(parent));
    assert!(!debug.contains(&hex));
    assert!(!debug.contains(&header));
}
