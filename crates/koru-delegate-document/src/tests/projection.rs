// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::decisions::*;
use loro::VersionVector;

fn id<T: std::str::FromStr>(n: u8) -> T {
    v4(n).to_string().parse().ok().unwrap()
}
fn rev(n: u64) -> DecisionRevision {
    DecisionRevision::new(n).unwrap()
}
fn bot(routine: bool) -> RunDocument {
    let mut h = bots::bot_header();
    h.status = RunStatus::running();
    if routine {
        let r = RoutineProvenance {
            schedule: ScheduleId::from_uuid(v4(60)),
            occurrence_ms: 20,
            definition: RoutineDefinitionIdentity {
                revision: 1,
                digest: RoutineDigest32::from_bytes([1; 32]),
                owner_handoff: None,
            },
            grant_revision: 1,
        };
        h.origin = RunOrigin::Scheduled {
            schedule: r.schedule,
            occurrence_ms: r.occurrence_ms,
            revision: r.definition.revision,
        };
        h.bot.as_mut().unwrap().routine = Some(r);
    }
    RunDocument::create(&h).unwrap()
}
fn intent(doc: &RunDocument) -> RunProjectionIntent {
    let h = doc.header().unwrap();
    let bot = h.bot.as_ref().unwrap();
    let target = RunProjectionTargetV1::Control {
        version: DecisionVersionV1,
        predecessor: DecisionProjectionPredecessor {
            projection: None,
            status: h.status.clone(),
            failure_reason: h.failure_reason,
        },
        effects: DecisionProjectionEffects {
            event: Some(RunEvent::ApprovalRequired),
            status: transition_bot(&h.status, &RunEvent::ApprovalRequired).unwrap(),
            failure_reason: None,
            economics_at_ms: None,
            steps: DecisionList::new(vec![TimelineEntry {
                id: id(91),
                kind: StepKind::ApprovalRequired,
                refs: TimelineRefs::default(),
                at_ms: 1,
            }])
            .unwrap(),
            models: DecisionList::default(),
        },
        fence: DecisionFence {
            owner: bot.owner,
            bot: bot.bot,
            run: h.run,
            attempt: attempt(80),
            generation: AttemptGeneration::FIRST,
            session: id(81),
            turn: turn(82),
        },
        disposition: None,
        wait_phase: BotWaitPhase::Fenced,
        reason: BotWaitReason::Capacity {},
        action: DecisionControlAction::DecisionWaiting {},
        retained_bookkeeping: None,
    };
    RunProjectionIntent {
        identity: DecisionProjectionIdentity {
            transition: id(70),
            run: h.run,
            wait: id(71),
            decision: None,
            sequence: rev(1),
            workspace: WorkspaceId::from_uuid(v4(4)),
            document: DocumentId::from_uuid(v4(90)),
            source_schema: (doc.schema_version().unwrap() as u8).try_into().unwrap(),
            subject_digest: decision_projection_subject_digest(&h).unwrap(),
            kind: target.kind(),
            target_digest: target.digest().unwrap(),
        },
        target,
    }
}
fn applied(
    stage: &mut RunProjectionStaging,
    doc: &RunDocument,
    prepared: &DecisionPreparedProjection,
) -> AppliedRunProjection {
    stage.apply(doc, prepared).unwrap();
    stage.acknowledge(doc, prepared, &doc.version()).unwrap()
}
fn next(doc: &RunDocument, previous: &RunProjectionIntent, event: RunEvent) -> RunProjectionIntent {
    let mut value = previous.clone();
    value.identity.transition = id(72);
    value.identity.sequence = rev(2);
    value.identity.source_schema = 5u8.try_into().unwrap();
    value.target = RunProjectionTargetV1::Control {
        version: DecisionVersionV1,
        predecessor: DecisionProjectionPredecessor {
            projection: Some(DecisionProjectionPrior {
                transition: previous.identity.transition,
                sequence: previous.identity.sequence,
                target_digest: previous.identity.target_digest,
            }),
            status: doc.header().unwrap().status,
            failure_reason: doc.header().unwrap().failure_reason,
        },
        effects: DecisionProjectionEffects {
            event: Some(event),
            status: transition_bot(&doc.header().unwrap().status, &event).unwrap(),
            failure_reason: None,
            economics_at_ms: None,
            steps: DecisionList::default(),
            models: DecisionList::default(),
        },
        fence: *previous.target.fence(),
        disposition: None,
        wait_phase: BotWaitPhase::CleanupPending,
        reason: BotWaitReason::Capacity {},
        action: DecisionControlAction::Stop {},
        retained_bookkeeping: previous.target.bookkeeping().cloned(),
    };
    value.identity.target_digest = value.target.digest().unwrap();
    value
}

#[test]
fn projection_prepare_is_private_exact_and_bot3_bot4_upgrade_is_specialized() {
    for routine in [false, true] {
        let doc = bot(routine);
        let intent = intent(&doc);
        let before = doc.version();
        let h = doc.header().unwrap();
        let mut stage = RunProjectionStaging::default();
        let p = stage.prepare(&doc, &intent).unwrap();
        assert_eq!(doc.version(), before);
        assert_eq!(doc.header().unwrap(), h);
        assert_eq!(stage.prepare(&doc, &intent).unwrap(), p);
        let receipt = applied(&mut stage, &doc, &p);
        assert_eq!(receipt.identity(), &intent.identity);
        assert_eq!(doc.schema_version(), Some(5));
        assert_eq!(doc.header().unwrap().bot, h.bot);
        assert_eq!(doc.header().unwrap().origin, h.origin);
        assert_eq!(doc.header().unwrap().native_results, h.native_results);
        assert!(matches!(
            doc.set_status(&RunStatus::Cancelled),
            Err(RunDocumentError::ReadOnly {
                found: 5,
                understood: 4
            })
        ));
    }
    assert_eq!(
        RunDocument::create(&header()).unwrap().schema_version(),
        Some(3)
    );
}

#[test]
fn projection_refuses_legacy_newer_wrong_subject_and_detached_without_mutating() {
    for schema in [1, 2, 6] {
        let doc = bot(false);
        let intent = intent(&doc);
        doc.doc().get_map("run").insert("schema", schema).unwrap();
        doc.doc().commit();
        let before = doc.version();
        assert!(
            RunProjectionStaging::default()
                .prepare(&doc, &intent)
                .is_err()
        );
        assert_eq!(doc.version(), before);
    }
    let doc = bot(false);
    let mut offered = intent(&doc);
    offered.identity.run = RunId::from_uuid(v4(99));
    assert!(
        RunProjectionStaging::default()
            .prepare(&doc, &offered)
            .is_err()
    );
    let doc = bot(false);
    let intent = intent(&doc);
    let frontier = doc.doc().state_frontiers();
    doc.set_status(&RunStatus::Cancelled).unwrap();
    doc.doc().checkout(&frontier).unwrap();
    assert!(
        RunProjectionStaging::default()
            .prepare(&doc, &intent)
            .is_err()
    );
}

#[test]
fn projection_ack_requires_actual_coverage_and_revalidates_after_remote_import() {
    let doc = bot(false);
    let intent = intent(&doc);
    let base = doc.version();
    let mut stage = RunProjectionStaging::default();
    let p = stage.prepare(&doc, &intent).unwrap();
    stage.apply(&doc, &p).unwrap();
    assert!(stage.acknowledge(&doc, &p, &base).is_err());
    assert!(
        stage.discard(p.identity.transition).is_err(),
        "unknown applied bytes cannot be discarded"
    );
    doc.doc()
        .get_map("run")
        .insert(
            "status",
            loro::LoroValue::from(serde_json::to_value(RunStatus::Cancelled).unwrap()),
        )
        .unwrap();
    doc.doc().commit();
    assert!(stage.acknowledge(&doc, &p, &doc.version()).is_err());
}

#[test]
fn projection_lost_apply_ack_reuses_identical_delta_without_duplicate_timeline() {
    let doc = bot(false);
    let intent = intent(&doc);
    let mut stage = RunProjectionStaging::default();
    let p = stage.prepare(&doc, &intent).unwrap();
    stage.apply(&doc, &p).unwrap();
    let version = doc.version();
    stage.apply(&doc, &p).unwrap();
    assert_eq!(doc.version(), version);
    assert_eq!(doc.timeline_len(), 1);
    stage.acknowledge(&doc, &p, &version).unwrap();
    // Cold recovery imports the retained bytes; it does not reconstruct target ops.
    let mut cold = RunProjectionStaging::default();
    applied(&mut cold, &doc, &p);
    assert_eq!(doc.version(), version);
    assert_eq!(doc.timeline_len(), 1);
}

#[test]
fn projection_next_target_is_incremental_and_old_waiting_delta_cannot_override_stop() {
    let doc = bot(false);
    let first = intent(&doc);
    let mut stage = RunProjectionStaging::default();
    let p = stage.prepare(&doc, &first).unwrap();
    applied(&mut stage, &doc, &p);
    let stop = next(&doc, &first, RunEvent::Cancel);
    let q = stage.prepare(&doc, &stop).unwrap();
    applied(&mut stage, &doc, &q);
    let version = doc.version();
    doc.import(p.delta.as_slice()).unwrap();
    assert_eq!(doc.version(), version);
    assert_eq!(doc.header().unwrap().status, RunStatus::Cancelled);
    assert!(
        stage.apply(&doc, &p).is_err(),
        "historical idempotence is not current-marker equality"
    );
}

#[test]
fn projection_predecessor_and_retained_bytes_mismatches_refuse_before_apply() {
    let doc = bot(false);
    let first = intent(&doc);
    let mut stage = RunProjectionStaging::default();
    let p = stage.prepare(&doc, &first).unwrap();
    let before = doc.version();
    let mut changed = p.clone();
    changed.delta_digest = DecisionDigest::from_bytes([99; 32]);
    assert!(stage.apply(&doc, &changed).is_err());
    assert_eq!(doc.version(), before);
    assert!(stage.discard(id(99)).is_err());
    stage.discard(p.identity.transition).unwrap();
    let p = stage.prepare(&doc, &first).unwrap();
    applied(&mut stage, &doc, &p);
    let mut next = next(&doc, &first, RunEvent::Cancel);
    if let RunProjectionTargetV1::Control { predecessor, .. } = &mut next.target {
        predecessor.projection.as_mut().unwrap().target_digest =
            DecisionDigest::from_bytes([9; 32]);
    }
    next.identity.target_digest = next.target.digest().unwrap();
    assert!(stage.prepare(&doc, &next).is_err());
}

#[test]
fn projection_pending_ancestry_and_incoherent_result_are_not_positive_apply() {
    let doc = bot(false);
    let first = intent(&doc);
    let p = RunProjectionStaging::default()
        .prepare(&doc, &first)
        .unwrap();
    let empty = loro::LoroDoc::new();
    let imported = empty.import(p.delta.as_slice()).unwrap();
    assert!(imported.pending.is_some());
    let other = bot(false);
    let before = other.version();
    assert!(RunProjectionStaging::default().apply(&other, &p).is_err());
    assert_eq!(other.version(), before);
    let mut corrupted = p;
    corrupted.result_version =
        DecisionVersionVector::new(VersionVector::default().encode()).unwrap();
    assert!(
        RunProjectionStaging::default()
            .apply(&doc, &corrupted)
            .is_err()
    );
}

#[test]
fn projection_cold_preflight_rejects_changed_marker_effects_before_connected_import() {
    let doc = bot(false);
    let first = intent(&doc);
    let base = doc.version();
    let mut p = RunProjectionStaging::default()
        .prepare(&doc, &first)
        .unwrap();
    let forged = loro::LoroDoc::new();
    forged.import(&doc.export_snapshot().unwrap()).unwrap();
    forged.import(p.delta.as_slice()).unwrap();
    forged
        .get_map("run")
        .insert(
            "status",
            loro::LoroValue::from(serde_json::to_value(RunStatus::Cancelled).unwrap()),
        )
        .unwrap();
    forged.commit();
    p.delta = DecisionPreparedDelta::new(forged.export(loro::ExportMode::updates(&base)).unwrap())
        .unwrap();
    p.delta_digest = decision_projection_delta_digest(&p.delta).unwrap();
    p.result_version = DecisionVersionVector::new(forged.oplog_vv().encode()).unwrap();
    assert!(RunProjectionStaging::default().apply(&doc, &p).is_err());
    assert_eq!(doc.version(), base);
    assert_eq!(doc.timeline_len(), 0);
}

#[test]
fn projection_source_bookkeeping_usage_and_economics_survive_later_control() {
    let doc = bot(false);
    let mut source = intent(&doc);
    let bookkeeping = DecisionSourceBookkeeping {
        boundary: DecisionSourceBoundary {
            fence: *source.target.fence(),
            producer_epoch: v4(100),
            stream: DecisionSourceStream::Agent,
            sequence: StreamSequence::new(3).unwrap(),
            resulting: DecisionSourceFrontiers {
                agent_after: Some(StreamSequence::new(3).unwrap()),
                session_after: Some(StreamSequence::new(7).unwrap()),
            },
        },
        unflushed: DecisionList::new(vec![DecisionTurnTally {
            turn: turn(82),
            originator: TurnOriginator::Orchestrator,
            tool_calls: 2,
            tool_failures: 0,
            failure_breakdown: ToolFailures::default(),
        }])
        .unwrap(),
        usage: Some(DecisionUsageSnapshot {
            id: id(101),
            revision: AmendmentRevision::FIRST,
            event: id(102),
            attempt: attempt(80),
            turn: turn(82),
            input: Some(12),
            output: Some(3),
            cached_input: None,
            cached_input_relationship: serde_json::from_str("\"undeclared\"").unwrap(),
            model: model(48),
            completeness: DecisionUsageCompleteness::Complete {},
        }),
        allocated_steps: DecisionList::new(vec![id(91)]).unwrap(),
    };
    let mut effects = source.target.effects().clone();
    effects.economics_at_ms = Some(1234);
    effects.models = DecisionList::new(vec![identity(48)]).unwrap();
    source.target = RunProjectionTargetV1::Source {
        version: DecisionVersionV1,
        predecessor: source.target.predecessor().clone(),
        effects,
        bookkeeping: bookkeeping.clone(),
    };
    source.identity.kind = DecisionProjectionKind::Source;
    source.identity.target_digest = source.target.digest().unwrap();
    let mut stage = RunProjectionStaging::default();
    let p = stage.prepare(&doc, &source).unwrap();
    applied(&mut stage, &doc, &p);
    assert_eq!(doc.timeline_len(), 2);
    assert_eq!(doc.header().unwrap().economics, doc.economics(1234));
    let control = next(&doc, &source, RunEvent::Cancel);
    let mut wrong = control.clone();
    if let RunProjectionTargetV1::Control {
        retained_bookkeeping,
        ..
    } = &mut wrong.target
    {
        *retained_bookkeeping = None;
    }
    wrong.identity.kind = DecisionProjectionKind::Control;
    wrong.identity.target_digest = wrong.target.digest().unwrap();
    assert!(stage.prepare(&doc, &wrong).is_err());
    let mut control = control;
    control.identity.kind = DecisionProjectionKind::Control;
    let q = stage.prepare(&doc, &control).unwrap();
    applied(&mut stage, &doc, &q);
    assert_eq!(
        doc.timeline_len(),
        2,
        "Control must not append retained usage again"
    );
    let marker = doc
        .doc()
        .get_map("run")
        .get("decision_projection")
        .unwrap()
        .get_deep_value();
    let marker: serde_json::Value = marker.into();
    assert_eq!(
        marker["bookkeeping"],
        serde_json::to_value(bookkeeping).unwrap()
    );
}

#[test]
fn projection_immutable_person_capture_and_agent_match_retained_subject() {
    for field in [
        "requested_by",
        "context",
        "agent",
        "objective",
        "grants",
        "created_at_ms",
    ] {
        let doc = bot(false);
        let expected = intent(&doc);
        let value = match field {
            "requested_by" => serde_json::to_value(ActorRef::Person {
                profile: ProfileId::from_uuid(v4(111)),
            })
            .unwrap(),
            "context" | "grants" => serde_json::json!([]),
            "agent" => {
                serde_json::to_value(SessionProfileKey::BuiltIn("claude".parse().unwrap())).unwrap()
            }
            "objective" => serde_json::json!("changed"),
            _ => serde_json::json!(99),
        };
        doc.doc()
            .get_map("run")
            .insert(field, loro::LoroValue::from(value))
            .unwrap();
        doc.doc().commit();
        assert!(
            RunProjectionStaging::default()
                .prepare(&doc, &expected)
                .is_err(),
            "{field}"
        );
    }
    let mut h = bots::bot_header();
    h.context = vec![h.context[0].clone(); 1024];
    assert!(serde_json::to_vec(&h.context).unwrap().len() > 65_536);
    assert!(
        decision_projection_subject_digest(&h).is_ok(),
        "subject is not capped at target64KiB"
    );
}

#[test]
fn projection_partial_staging_failure_resets_before_next_prepare() {
    let doc = bot(false);
    let original = intent(&doc);
    doc.doc().get_map("run").insert("models", 7).unwrap();
    doc.doc().commit();
    let before = doc.version();
    let mut stage = RunProjectionStaging::default();
    // Staging has appended its timeline entry before finding the malformed
    // model container. A failed preparation must not retain that ahead state.
    assert!(stage.prepare(&doc, &original).is_err());
    assert_eq!(doc.version(), before);
    assert_eq!(doc.timeline_len(), 0);
    doc.doc()
        .get_map("run")
        .insert_container("models", loro::LoroMap::new())
        .unwrap();
    doc.doc().commit();
    let q = stage.prepare(&doc, &original).unwrap();
    applied(&mut stage, &doc, &q);
    assert_eq!(doc.timeline_len(), 1);
}

#[test]
fn projection_marker_has_independent_64k_and_depth_bounds() {
    let doc = bot(false);
    let original = intent(&doc);
    let mut oversized = original.clone();
    let boundary = DecisionSourceBoundary {
        fence: *original.target.fence(),
        producer_epoch: v4(100),
        stream: DecisionSourceStream::Agent,
        sequence: StreamSequence::new(1).unwrap(),
        resulting: DecisionSourceFrontiers {
            agent_after: Some(StreamSequence::new(1).unwrap()),
            session_after: None,
        },
    };
    let mut tallies = Vec::new();
    let mut found = false;
    // Fixed bounded search makes the overhead assertion independent of a
    // guessed JSON length. The marker repeats the complete physical identity.
    for n in 1..=512u128 {
        let turn = uuid::Uuid::from_u128(0x11111111222243338444000000000000 + n)
            .to_string()
            .parse()
            .unwrap();
        tallies.push(DecisionTurnTally {
            turn,
            originator: TurnOriginator::Orchestrator,
            tool_calls: 1,
            tool_failures: 0,
            failure_breakdown: ToolFailures::default(),
        });
        let bookkeeping = DecisionSourceBookkeeping {
            boundary: boundary.clone(),
            unflushed: DecisionList::new(tallies.clone()).unwrap(),
            usage: None,
            allocated_steps: DecisionList::default(),
        };
        let mut effects = original.target.effects().clone();
        effects.event = None;
        effects.status = original.target.predecessor().status.clone();
        effects.steps = DecisionList::default();
        oversized.target = RunProjectionTargetV1::Source {
            version: DecisionVersionV1,
            predecessor: original.target.predecessor().clone(),
            effects,
            bookkeeping: bookkeeping.clone(),
        };
        if oversized.target.canonical_bytes().is_err() {
            break;
        }
        oversized.identity.kind = DecisionProjectionKind::Source;
        oversized.identity.target_digest = oversized.target.digest().unwrap();
        let marker = serde_json::json!({ "identity": oversized.identity,
            "status": oversized.target.effects().status, "bookkeeping": bookkeeping,
            "timeline_start": 0, "timeline_end": 0 });
        if serde_json::to_vec(&marker).unwrap().len() > DECISION_PAYLOAD_BYTES {
            found = true;
            break;
        }
    }
    assert!(found, "a legal target can have an oversized marker");
    oversized.validate().unwrap();
    let before = doc.version();
    let mut stage = RunProjectionStaging::default();
    assert!(stage.prepare(&doc, &oversized).is_err());
    assert_eq!(doc.version(), before);
    let prepared = stage.prepare(&doc, &original).unwrap();
    applied(&mut stage, &doc, &prepared);

    let mut nested = loro::LoroValue::Null;
    for _ in 0..64 {
        nested = loro::LoroValue::List(vec![nested].into());
    }
    assert!(
        crate::bounded_value::Budget(DECISION_PAYLOAD_BYTES)
            .value(&nested, 0)
            .is_ok()
    );
    nested = loro::LoroValue::List(vec![nested].into());
    assert!(
        crate::bounded_value::Budget(DECISION_PAYLOAD_BYTES)
            .value(&nested, 0)
            .is_err()
    );
    doc.doc()
        .get_map("run")
        .insert("decision_projection", nested)
        .unwrap();
    doc.doc().commit();
    let control = next(&doc, &original, RunEvent::Cancel);
    assert!(stage.prepare(&doc, &control).is_err());
}
