// SPDX-License-Identifier: MPL-2.0

//! Real SQLite backend proofs with explicitly seeded executing attempts. These
//! fixtures do not themselves prove a live worker or authorize dispatch.
use super::*;
use rusqlite::params;

fn bind(h: &mut Harness, schedule: ScheduleId, occurrence: i64) -> FreshBotPolicyProof {
    let mut s = h.db.bot_admission(h.fence.run).unwrap().unwrap().submission;
    let route = RoutineRouteIdentity {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        schedule,
        workspace: s.workspace,
        creation_request: BotRequestId::mint(),
        seed_hash: RoutineDigest32::from_bytes([4; 32]),
        created_at_ms: 800,
    };
    if h.db.routine_route(schedule).unwrap().is_none() {
        h.db.register_routine_route(&route).unwrap();
    }
    crate::bots::routines::tests::routine(&mut s, &route);
    s.provenance.routine.as_mut().unwrap().occurrence_ms = occurrence;
    s.request.origin = RunOrigin::Scheduled {
        schedule,
        occurrence_ms: occurrence,
        revision: 1,
    };
    h.db.conn()
        .execute(
            "UPDATE bot_admissions SET submission_version=2,payload_json=?2 WHERE run_id=?1",
            params![h.fence.run.to_storage(), serde_json::to_string(&s).unwrap()],
        )
        .unwrap();
    h.db.conn()
        .execute(
            "UPDATE run_intents SET payload_json=?2 WHERE run_id=?1",
            params![
                h.fence.run.to_storage(),
                serde_json::to_string(&s.request).unwrap()
            ],
        )
        .unwrap();
    h.db.conn().execute("INSERT INTO schedule_claims(schedule_id,occurrence_ms,revision,run_id,outcome,claimed_at_ms) VALUES(?1,?2,1,?3,'claimed',1000)",params![schedule.to_storage(),occurrence,h.fence.run.to_storage()]).unwrap();
    h.db.conn().execute("INSERT INTO bot_routine_links(run_id,schedule_id,occurrence_ms,user_scope,device_id,bot_id,definition_revision,definition_digest,grant_revision,preparation_hash,submission_request_id,chat_id,attempt_id,user_message_id,assistant_message_id,state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,1,?7,1,?8,?9,?10,?11,?12,?13,'linked',1000,1000)",params![h.fence.run.to_storage(),schedule.to_storage(),occurrence,route.owner.user_scope.to_storage(),route.owner.device.to_storage(),route.bot.to_storage(),[5u8;32].as_slice(),[6u8;32].as_slice(),s.provenance.submission.to_storage(),s.provenance.chat_id.to_string(),s.provenance.ai_attempt_id.unwrap().to_string(),Uuid::new_v4().to_string(),s.provenance.assistant_message_id.unwrap().to_string()]).unwrap();
    let offered = RoutinePolicyBarrier {
        owner: route.owner,
        bot: route.bot,
        schedule,
        request: BotRequestId::mint().into(),
        grant_revision: 1,
        policy: s.policy,
        effects_allowed: true,
    };
    let offered = if let Some(old) =
        h.db.current_routine_policy(route.owner, route.bot, schedule)
            .unwrap()
    {
        RoutinePolicyBarrier {
            request: old.request,
            ..offered
        }
    } else {
        offered
    };
    h.db.install_routine_policy(&offered).unwrap();
    FreshBotPolicyProof {
        global: h.policy.clone(),
        routine: Some(offered),
    }
}
fn fixture() -> (Harness, FreshBotPolicyProof) {
    let mut h = Harness::new();
    let proof = bind(&mut h, ScheduleId::from_uuid(Uuid::new_v4()), 900);
    (h, proof)
}
fn lease(
    h: &mut Harness,
    proof: &FreshBotPolicyProof,
) -> Result<NativeLeaseOutcome, BotLedgerError> {
    h.db.lease_native_result(
        &h.request.key,
        h.request.request_fingerprint,
        &h.fence,
        proof,
    )
}
fn execution(h: &Harness) -> BotRunKey {
    BotRunKey {
        owner: h.fence.owner,
        bot: h.fence.bot,
        run: h.fence.run,
    }
}
fn finish(h: &mut Harness) -> DocumentId {
    let evidence = h.evidence();
    h.confirm(NativeResultObservation::Durable {
        evidence: evidence.clone(),
    });
    h.db.register_native_result(&h.request.key, &evidence)
        .unwrap();
    evidence.document
}
fn next(h: &mut Harness) {
    let mut s = h.db.bot_admission(h.fence.run).unwrap().unwrap().submission;
    h.db.advance(
        h.fence.run,
        h.fence.generation,
        crate::AttemptPhase::Interrupted,
    )
    .unwrap();
    h.db.retire(h.fence.run, h.fence.generation).unwrap();
    h.db.settle_bot(h.fence.run, crate::BotSettlement::Settled)
        .unwrap();
    s.provenance.routine = None;
    s.provenance.submission = BotRequestId::mint();
    s.provenance.chat_id = Uuid::new_v4();
    s.provenance.ai_attempt_id = None;
    s.provenance.assistant_message_id = None;
    s.request.run = RunId::from_uuid(Uuid::new_v4());
    s.request.origin = RunOrigin::Manual;
    s.input.idempotency_key = IdempotencyKey::mint();
    h.db.admit_bot(&s).unwrap();
    h.db.satisfy_intent(s.request.run, DocumentId::from_uuid(Uuid::new_v4()))
        .unwrap();
    assert_eq!(
        h.db.reserve_bot(s.request.run, None).unwrap(),
        BotReservation::Acquired
    );
    let a = fixtures::running_attempt(&mut h.db, s.request.run, 2);
    h.db.activate_bot(a.run, a.attempt, a.generation, None)
        .unwrap();
    let mut send = BotInitialSend {
        attempt: a.attempt,
        generation: a.generation,
        key: IdempotencyKey::mint(),
        fingerprint: [9; 32],
        turn: None,
    };
    h.db.prepare_bot_initial_send(a.run, &send, None).unwrap();
    send.turn = Some(AgentTurnId::mint());
    h.db.acknowledge_bot_initial_send(a.run, &send).unwrap();
    h.fence = BotExecutionFence {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        run: a.run,
        attempt: a.attempt,
        generation: a.generation,
        session: a.provider_session.unwrap().session.unwrap(),
        turn: send.turn,
    };
    h.request.key = NativeResultKey {
        run: a.run,
        attempt: a.attempt,
        generation: a.generation,
        turn: send.turn.unwrap(),
        operation_id: Uuid::new_v4(),
    };
}

#[test]
fn routine_native_atomic_companion_lease_crash_replay_and_terminal_history() {
    for at in Moment::ALL {
        let (mut h, proof) = fixture();
        h.prepare();
        h.db.arm_crash(CrashPoint {
            op: WriteOp::LeaseNativeResult,
            at,
        });
        assert!(lease(&mut h, &proof).is_err());
        h.db.restart_in_memory();
        let count: i64 =
            h.db.conn()
                .query_row("SELECT count(*) FROM native_routine_leases", [], |r| {
                    r.get(0)
                })
                .unwrap();
        assert_eq!(count, if at == Moment::AfterCommit { 1 } else { 0 });
        let leased = lease(&mut h, &proof).unwrap();
        assert_eq!(leased.execute.is_none(), at == Moment::AfterCommit);
        assert_eq!(
            leased
                .intent
                .lease
                .as_ref()
                .unwrap()
                .routine
                .as_ref()
                .unwrap()
                .schedule,
            proof.routine.as_ref().unwrap().schedule
        );
        let key = h.request.key.clone();
        finish(&mut h);
        next(&mut h);
        let retained = h.db.native_result(&key).unwrap().unwrap();
        assert_eq!(retained.stage, NativeResultStage::Registered);
        assert!(retained.lease.unwrap().routine.is_some());
    }
}

#[test]
fn routine_native_composite_subject_current_policy_and_attempt_defenses() {
    for field in 0..11 {
        let (mut h, mut proof) = fixture();
        h.prepare();
        match field {
            0 => proof.routine = None,
            1 => proof.routine.as_mut().unwrap().schedule = ScheduleId::from_uuid(Uuid::new_v4()),
            2 => proof.routine.as_mut().unwrap().bot = BotId::mint(),
            3 => proof.routine.as_mut().unwrap().request = BotRequestId::mint().into(),
            4 => proof.routine.as_mut().unwrap().grant_revision = 2,
            5 => proof.routine.as_mut().unwrap().effects_allowed = false,
            6 => proof.global.effects_allowed = false,
            7 => h.fence.session = SessionId::mint(),
            8 => {
                h.db.conn()
                    .execute("UPDATE bot_routine_links SET state='held'", [])
                    .unwrap();
            }
            9 => {
                let newer = RoutinePolicyBarrier {
                    request: BotRequestId::mint().into(),
                    grant_revision: 2,
                    ..proof.routine.clone().unwrap()
                };
                h.db.install_routine_policy(&newer).unwrap();
            }
            _ => {
                h.db.conn()
                    .execute(
                        "UPDATE run_intents SET state='pending',run_document_id=NULL",
                        [],
                    )
                    .unwrap();
            }
        }
        assert!(lease(&mut h, &proof).is_err(), "field {field}");
        assert_eq!(
            h.db.conn()
                .query_row("SELECT count(*) FROM native_routine_leases", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            h.db.conn()
                .query_row("SELECT lease_state FROM native_result_intents", [], |r| {
                    r.get::<_, String>(0)
                })
                .unwrap(),
            "none"
        );
    }
}

#[test]
fn routine_native_retained_marker_refuses_v1_manual_downgrade_for_result_and_memory() {
    for (memory, orphan) in [(false, false), (true, false), (false, true), (true, true)] {
        let (mut h, proof) = fixture();
        // Capture the valid subject before deliberately corrupting its admission.
        let mut fresh_memory = super::memory::request(&h, true);
        if memory {
            let request = super::memory::request(&h, true);
            h.db.allocate_memory_edit(&request, fixtures::document(0xd0), &h.fence, &proof)
                .unwrap();
            let resolution = super::memory::resolution(&request);
            h.db.resolve_memory_edit_base(
                &request.key,
                request.caller_fingerprint,
                &resolution,
                &h.fence,
                &proof,
            )
            .unwrap();
            super::memory::prepare(&mut h, &request, &resolution);
        } else {
            h.prepare();
        }
        let mut s = h.db.bot_admission(h.fence.run).unwrap().unwrap().submission;
        s.provenance.routine = None;
        s.request.origin = RunOrigin::Manual;
        assert!(s.valid_for_version(1));
        h.db.conn()
            .execute(
                "UPDATE bot_admissions SET submission_version=1,payload_json=?2 WHERE run_id=?1",
                params![h.fence.run.to_storage(), serde_json::to_string(&s).unwrap()],
            )
            .unwrap();
        h.db.conn()
            .execute(
                "UPDATE run_intents SET payload_json=?2 WHERE run_id=?1",
                params![
                    h.fence.run.to_storage(),
                    serde_json::to_string(&s.request).unwrap()
                ],
            )
            .unwrap();
        if orphan {
            h.db.conn().execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM schedule_claims; DELETE FROM bot_routine_routes;").unwrap();
            assert_eq!(
                h.db.conn()
                    .query_row("SELECT count(*) FROM bot_routine_links", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
        let ordinary = FreshBotPolicyProof {
            global: proof.global,
            routine: None,
        };
        assert!(lease(&mut h, &ordinary).is_err(), "memory {memory}");
        assert!(h.db.native_result(&h.request.key).is_err());
        h.request.key.operation_id = Uuid::new_v4();
        assert!(h.db.prepare_native_result(&h.request).is_err());
        fresh_memory.key = h.request.key.clone();
        assert!(
            h.db.allocate_memory_edit(&fresh_memory, fixtures::document(0xd0), &h.fence, &ordinary)
                .is_err()
        );
        assert_eq!(
            h.db.conn()
                .query_row("SELECT count(*) FROM native_routine_leases", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn routine_native_created_authority_is_partitioned_and_prior_occurrence_survives() {
    for (from, to, expected) in [
        (0, 1, false),
        (1, 0, false),
        (1, 2, false),
        (2, 1, false),
        (1, 1, true),
        (0, 0, true),
    ] {
        let mut h = Harness::new();
        let a = ScheduleId::from_uuid(Uuid::new_v4());
        let b = ScheduleId::from_uuid(Uuid::new_v4());
        let origin = match from {
            0 => FreshBotPolicyProof {
                global: h.policy.clone(),
                routine: None,
            },
            1 => bind(&mut h, a, 900),
            _ => bind(&mut h, b, 900),
        };
        h.prepare();
        lease(&mut h, &origin).unwrap();
        let target = finish(&mut h);
        next(&mut h);
        let offered = match to {
            0 => FreshBotPolicyProof {
                global: h.policy.clone(),
                routine: None,
            },
            1 => bind(&mut h, a, 1900),
            _ => bind(&mut h, b, 1900),
        };
        assert_eq!(
            h.db.created_native_page(&execution(&h), h.request.destination, target)
                .unwrap(),
            expected,
            "{from}->{to}"
        );
        h.request.kind = NativeResultOperation::EditPage;
        h.request.target = Some(target);
        h.request.expected_revision = Some(vec![1]);
        h.prepare();
        assert_eq!(
            lease(&mut h, &offered).is_ok(),
            expected,
            "{from}->{to} lease"
        );
    }
}

#[test]
fn routine_native_created_fallback_requires_complete_source_link_and_provenance() {
    for field in 0..3 {
        let (mut h, proof) = fixture();
        h.prepare();
        lease(&mut h, &proof).unwrap();
        let source = h.fence.run;
        let source_key = h.request.key.clone();
        let target = finish(&mut h);
        next(&mut h);
        let offered = bind(&mut h, proof.routine.as_ref().unwrap().schedule, 1900);
        assert!(
            h.db.created_native_page(&execution(&h), h.request.destination, target)
                .unwrap()
        );
        match field {
            0 => {
                h.db.conn()
                    .execute_batch("PRAGMA foreign_keys=OFF;")
                    .unwrap();
                h.db.conn()
                    .execute(
                        "DELETE FROM bot_routine_links WHERE run_id=?1",
                        [source.to_storage()],
                    )
                    .unwrap();
            }
            1 => {
                h.db.conn()
                    .execute(
                        "UPDATE bot_routine_links SET definition_digest=?2 WHERE run_id=?1",
                        params![source.to_storage(), [9u8; 32].as_slice()],
                    )
                    .unwrap();
            }
            _ => {
                let mut s = h.db.bot_admission(source).unwrap().unwrap().submission;
                s.provenance.routine.as_mut().unwrap().grant_revision = 2;
                h.db.conn()
                    .execute(
                        "UPDATE bot_admissions SET payload_json=?2 WHERE run_id=?1",
                        params![source.to_storage(), serde_json::to_string(&s).unwrap()],
                    )
                    .unwrap();
            }
        }
        assert!(h.db.native_result(&source_key).is_err());
        assert!(
            !h.db
                .created_native_page(&execution(&h), h.request.destination, target)
                .unwrap_or(false)
        );
        h.request.kind = NativeResultOperation::EditPage;
        h.request.target = Some(target);
        h.request.expected_revision = Some(vec![1]);
        h.prepare();
        assert!(lease(&mut h, &offered).is_err());
    }
}

#[test]
fn routine_native_global_pages_do_not_supply_routine_permission() {
    let (mut h, mut proof) = fixture();
    let target = DocumentId::from_uuid(Uuid::new_v4());
    proof.global.key.request = BotRequestId::mint().into();
    proof.global.config_revision = 2;
    proof.global.policy_revision = 2;
    proof.global.policy.access = BotAccess::Workspace;
    h.db.install_native_policy(&proof.global).unwrap();
    h.request.kind = NativeResultOperation::EditPage;
    h.request.target = Some(target);
    h.request.expected_revision = Some(vec![1]);
    h.prepare();
    assert_eq!(lease(&mut h, &proof), Err(BotLedgerError::Fenced));
    assert_eq!(
        h.db.reserve_bot(h.fence.run, None),
        Ok(crate::BotReservation::AlreadyOwned)
    );
    assert!(h.db.open_attempt(h.fence.run).is_err());
}

#[test]
fn routine_native_explicit_grants_require_both_captured_and_current_permissions() {
    for workspace in [false, true] {
        for captured in [false, true] {
            let (mut h, mut proof) = fixture();
            let target = DocumentId::from_uuid(Uuid::new_v4());
            let mut permission = proof.routine.as_ref().unwrap().policy.clone();
            if workspace {
                permission.access = BotAccess::Workspace;
            } else {
                permission.selected_pages.insert(target);
            }
            if captured {
                let mut s = h.db.bot_admission(h.fence.run).unwrap().unwrap().submission;
                s.policy = permission.clone();
                h.db.conn()
                    .execute(
                        "UPDATE bot_admissions SET payload_json=?2 WHERE run_id=?1",
                        params![h.fence.run.to_storage(), serde_json::to_string(&s).unwrap()],
                    )
                    .unwrap();
            }
            let current = proof.routine.as_mut().unwrap();
            current.request = BotRequestId::mint().into();
            current.grant_revision = 2;
            current.policy = permission;
            h.db.install_routine_policy(current).unwrap();
            h.request.kind = NativeResultOperation::EditPage;
            h.request.target = Some(target);
            h.request.expected_revision = Some(vec![1]);
            h.prepare();
            assert_eq!(
                lease(&mut h, &proof).is_ok(),
                captured,
                "workspace {workspace} captured {captured}"
            );
        }
    }
}

#[test]
fn routine_native_update_failure_rolls_back_preceding_companion_insert() {
    let (mut h, proof) = fixture();
    h.prepare();
    h.db.conn().execute_batch("CREATE TEMP TRIGGER fail_native_held BEFORE UPDATE OF lease_state ON native_result_intents WHEN NEW.lease_state='held' BEGIN SELECT RAISE(ABORT,'injected_after_companion'); END;").unwrap();
    assert!(lease(&mut h, &proof).is_err());
    assert_eq!(
        h.db.conn()
            .query_row("SELECT count(*) FROM native_routine_leases", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        h.db.native_result(&h.request.key)
            .unwrap()
            .unwrap()
            .lease_state,
        NativeLeaseState::None
    );
    h.db.conn()
        .execute_batch("DROP TRIGGER fail_native_held;")
        .unwrap();
    assert!(lease(&mut h, &proof).unwrap().execute.is_some());
}

#[test]
fn routine_native_memory_uses_composite_fence_and_retains_independent_purpose() {
    let (mut h, proof) = fixture();
    let request = super::memory::request(&h, true);
    let allocation =
        h.db.allocate_memory_edit(&request, fixtures::document(0xd0), &h.fence, &proof)
            .unwrap();
    let authority = MemoryEditAuthorityV1 {
        version: 1,
        owner: h.fence.owner,
        bot: h.fence.bot,
        submission: request.subject.submission,
        captured_config_revision: 1,
        creation_request: BotRequestId::mint(),
        workspace: h.request.destination,
        memory_document: DocumentId::from_uuid(Uuid::new_v4()),
        action: MemoryEditAction::Remember,
    };
    let resolution = MemoryBaseResolution::Resolved {
        authority: authority.clone(),
        version: DocumentVersionId::from_uuid(Uuid::new_v4()),
        expected_revision: vec![2, 1, 2, 2, 4],
    };
    h.db.resolve_memory_edit_base(
        &allocation.key,
        request.caller_fingerprint,
        &resolution,
        &h.fence,
        &proof,
    )
    .unwrap();
    h.request.kind = NativeResultOperation::EditPage;
    h.request.target = Some(authority.memory_document);
    h.request.expected_revision = Some(vec![2, 1, 2, 2, 4]);
    h.db.prepare_memory_edit(&h.request, request.caller_fingerprint, &authority)
        .unwrap();
    let acquired = lease(&mut h, &proof).unwrap();
    assert!(acquired.execute.unwrap().routine.is_some());
    assert!(matches!(
        acquired.intent.purpose,
        NativeEffectPurpose::Memory { .. }
    ));
    finish(&mut h);
    assert!(
        !h.db
            .created_native_page(
                &execution(&h),
                h.request.destination,
                authority.memory_document
            )
            .unwrap()
    );
}

#[test]
fn routine_native_corrupt_companions_never_read_drain_or_make_policy_effective() {
    for field in 0..7 {
        let (mut h, proof) = fixture();
        h.prepare();
        lease(&mut h, &proof).unwrap();
        let p = proof.routine.as_ref().unwrap();
        let next = RoutinePolicyBarrier {
            request: BotRequestId::mint().into(),
            grant_revision: 2,
            effects_allowed: false,
            ..p.clone()
        };
        assert_eq!(
            h.db.install_routine_policy(&next).unwrap().state,
            BotPolicyBarrierState::Pending
        );
        let retained = h.db.native_result(&h.request.key).unwrap().unwrap();
        let binding = retained.pending_effect().unwrap().binding;
        let evidence = NativeResultReference {
            operation_id: retained.key.operation_id,
            workspace: retained.destination,
            document: retained.target,
            version: vec![1],
            title: "Result".into(),
            confirmed_at_ms: 1000,
            kind: NativeResultKind::CreatedPage,
        };
        // Inject malformed retained storage, never a production repair path.
        h.db.conn().execute_batch("PRAGMA foreign_keys=OFF; DROP TRIGGER native_routine_lease_retained; DROP TRIGGER native_routine_lease_immutable;").unwrap();
        match field {
            0 => {
                h.db.conn()
                    .execute("DELETE FROM native_routine_leases", [])
                    .unwrap();
            }
            1 => {
                h.db.conn()
                    .execute_batch(
                        "DELETE FROM native_routine_leases; DELETE FROM bot_routine_links;",
                    )
                    .unwrap();
            }
            2 => {
                let other = ScheduleId::from_uuid(Uuid::new_v4()).to_storage();
                h.db.conn()
                    .execute("UPDATE native_routine_leases SET schedule_id=?1", [&other])
                    .unwrap();
                h.db.conn()
                    .execute("UPDATE bot_routine_links SET schedule_id=?1", [&other])
                    .unwrap();
            }
            3 => {
                h.db.conn()
                    .execute(
                        "UPDATE native_routine_leases SET payload_digest=?1",
                        [[0u8; 32].as_slice()],
                    )
                    .unwrap();
            }
            4 => {
                h.db.conn()
                    .execute("UPDATE native_routine_leases SET grant_revision=2", [])
                    .unwrap();
            }
            5 => {
                h.db.conn()
                    .execute(
                        "UPDATE native_routine_leases SET bot_id=?1",
                        [BotId::mint().to_storage()],
                    )
                    .unwrap();
            }
            _ => {
                h.db.conn()
                    .execute(
                        "UPDATE bot_routine_links SET state='terminal',terminal_reason='stopped'",
                        [],
                    )
                    .unwrap();
            }
        }
        assert!(h.db.native_result(&h.request.key).is_err(), "read {field}");
        assert!(
            h.db.pending_native_results(None, 16).is_err(),
            "recovery {field}"
        );
        assert!(
            h.db.pending_native_effects(p.owner, Some(p.bot), None, 16)
                .is_err(),
            "global {field}"
        );
        assert!(
            h.db.pending_routine_native_effects(p.owner, p.bot, p.schedule, None, 16)
                .is_err(),
            "subject {field}"
        );
        assert!(
            h.db.settle_routine_policy(p.owner, p.bot, p.schedule, next.request)
                .is_err(),
            "effective {field}"
        );
        assert!(lease(&mut h, &proof).is_err(), "replay {field}");
        assert!(
            h.db.register_native_result(&h.request.key, &evidence)
                .is_err(),
            "register {field}"
        );
        assert!(
            h.db.confirm_native_result(&ConfirmNativeResult {
                key: h.request.key.clone(),
                request_fingerprint: h.request.request_fingerprint,
                observation: NativeResultObservation::DrainedUnknown {
                    receipt: NativeEffectDrainReceipt {
                        binding,
                        fenced_at: NativeEffectTimestamp::from_millis(1000),
                        drained_at: NativeEffectTimestamp::from_millis(1001)
                    }
                }
            })
            .is_err(),
            "confirm {field}"
        );
        assert_eq!(
            h.db.routine_policy(p.owner, p.bot, p.schedule, next.request)
                .unwrap()
                .unwrap()
                .state,
            BotPolicyBarrierState::Pending
        );
    }
}

#[test]
fn routine_native_scoped_drain_pages_and_effective_fences_preserve_retained_truth() {
    let (mut h, proof) = fixture();
    h.prepare();
    lease(&mut h, &proof).unwrap();
    let p = proof.routine.as_ref().unwrap();
    let other = ScheduleId::from_uuid(Uuid::new_v4());
    let page =
        h.db.pending_routine_native_effects(p.owner, p.bot, other, None, 1)
            .unwrap();
    assert!(page.targets.is_empty());
    let cursor = page.next.expect("examined another subject, must continue");
    assert!(
        h.db.pending_routine_native_effects(p.owner, p.bot, other, Some(&cursor), 1)
            .unwrap()
            .next
            .is_none()
    );
    let next = RoutinePolicyBarrier {
        request: BotRequestId::mint().into(),
        grant_revision: 2,
        effects_allowed: false,
        ..p.clone()
    };
    assert_eq!(
        h.db.install_routine_policy(&next).unwrap().state,
        BotPolicyBarrierState::Pending
    );
    let original = h.request.clone();
    h.request.key.operation_id = Uuid::new_v4();
    h.prepare();
    let pending_proof = FreshBotPolicyProof {
        global: proof.global.clone(),
        routine: Some(next.clone()),
    };
    assert_eq!(lease(&mut h, &pending_proof), Err(BotLedgerError::Fenced));
    h.request = original;
    let pending =
        h.db.pending_routine_native_effects(p.owner, p.bot, p.schedule, None, 16)
            .unwrap();
    assert_eq!(pending.targets.len(), 1);
    assert_eq!(
        h.db.pending_native_effects(p.owner, Some(p.bot), None, 16)
            .unwrap()
            .targets,
        pending.targets
    );
    let mut stale = proof.clone();
    stale.global.effects_allowed = false;
    stale.routine = None;
    assert!(
        lease(&mut h, &stale).unwrap().execute.is_none(),
        "retained replay precedes fresh proof"
    );
    let target = &pending.targets[0];
    h.confirm(NativeResultObservation::DrainedUnknown {
        receipt: NativeEffectDrainReceipt {
            binding: target.binding.clone(),
            fenced_at: NativeEffectTimestamp::from_millis(1000),
            drained_at: NativeEffectTimestamp::from_millis(1001),
        },
    });
    assert_eq!(
        h.db.settle_routine_policy(p.owner, p.bot, p.schedule, next.request)
            .unwrap()
            .state,
        BotPolicyBarrierState::Effective
    );
    assert_eq!(
        h.db.conn()
            .query_row("SELECT count(*) FROM native_routine_leases", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(lease(&mut h, &proof).unwrap().execute.is_none());
}

#[test]
fn routine_native_query_plans_keep_target_lookup_and_held_history_indexes() {
    let (mut h, proof) = fixture();
    h.prepare();
    lease(&mut h, &proof).unwrap();
    let target = finish(&mut h);
    for _ in 0..32 {
        h.request.key.operation_id = Uuid::new_v4();
        h.prepare();
        lease(&mut h, &proof).unwrap();
        finish(&mut h);
    }
    h.request.key.operation_id = Uuid::new_v4();
    h.prepare();
    lease(&mut h, &proof).unwrap();
    assert_eq!(
        h.db.conn()
            .query_row(
                "SELECT count(*) FROM native_result_intents WHERE lease_state='settled'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        33
    );
    let p = proof.routine.as_ref().unwrap();
    let query = format!("EXPLAIN QUERY PLAN {}", rows::CREATED_ROUTINE_QUERY);
    let mut statement = h.db.conn().prepare(&query).unwrap();
    let details = statement
        .query_map(
            params![
                h.request.destination.to_storage(),
                target.to_storage(),
                p.owner.user_scope.to_storage(),
                p.owner.device.to_storage(),
                p.bot.to_storage(),
                p.schedule.to_storage()
            ],
            |r| r.get::<_, String>(3),
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    assert!(details.contains("native_result_created"), "{details}");
    assert!(
        !details.contains("SCAN n") && !details.contains("SCAN b") && !details.contains("SCAN l"),
        "{details}"
    );
    let query = format!("EXPLAIN QUERY PLAN {}", super::super::drain::BOT_QUERY);
    let mut statement = h.db.conn().prepare(&query).unwrap();
    let details = statement
        .query_map(
            params![
                p.owner.user_scope.to_storage(),
                p.owner.device.to_storage(),
                p.bot.to_storage(),
                i64::MIN,
                "",
                "",
                16
            ],
            |r| r.get::<_, String>(3),
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    assert!(
        details.contains("native_result_held_bot_recovery"),
        "{details}"
    );
    assert!(!details.contains("SCAN native_result_intents"), "{details}");
}
