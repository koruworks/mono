// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_runtime::RunStore;
use std::collections::BTreeSet;

fn submission() -> BotSubmission {
    let workspace = WorkspaceId::new();
    let owner = BotOwner {
        user_scope: koru_store_core::UserScopeId::new(),
        device: DeviceId::new(),
    };
    BotSubmission {
        provenance: BotProvenance {
            parent: None,
            routine: None,
            owner,
            bot: BotId::mint(),
            submission: BotRequestId::mint(),
            chat_id: uuid::Uuid::new_v4(),
            ai_attempt_id: None,
            assistant_message_id: None,
            config_revision: 1,
            policy_revision: 1,
            continued_from: None,
        },
        policy: CapturedBotPolicy {
            workspace,
            destination: workspace,
            access: BotAccess::Default,
            selected_pages: Default::default(),
            connections: Default::default(),
        },
        workspace,
        request: DelegationRequest {
            run: RunId::mint(),
            requested_by: ActorRef::Person {
                profile: ProfileId::new(),
            },
            objective: Objective::new("fixture").unwrap(),
            agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
            requested_model: None,
            scope: WorkspaceScope::BotFolder {
                folder: BotWorkingFolder::new("C:/fixture/never-opened".into()).unwrap(),
            },
            context: vec![],
            grants: BTreeSet::from([workspace]),
            executing_device: owner.device,
            origin: RunOrigin::Manual,
        },
        input: PendingInput {
            idempotency_key: IdempotencyKey::mint(),
            text: "fixture".into(),
            attachment_ids: vec![],
            references: vec![],
            mode: None,
            fast: None,
            reasoning: None,
            skills: vec![],
            model: None,
        },
    }
}

#[test]
fn routine_foundation_shared_owner_forwards_route_and_bounded_readers_without_execution() {
    let mut shared = SharedLedger::new(RunsDb::open(std::path::Path::new(":memory:")).unwrap());
    let s = submission();
    shared.bots().unwrap().admit_bot(&s).unwrap();
    let route = RoutineRouteIdentity {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        schedule: ScheduleId::mint(),
        workspace: s.workspace,
        creation_request: BotRequestId::mint(),
        seed_hash: RoutineDigest32::from_bytes([9; 32]),
        created_at_ms: 0,
    };
    shared
        .bots()
        .unwrap()
        .register_routine_route(&route)
        .unwrap();
    let reader: &dyn koru_delegate_runtime::RoutineLedger = &shared;
    assert_eq!(
        reader.routine_route(route.schedule).unwrap(),
        Some(route.clone())
    );
    assert!(reader.held_routine_page(None).unwrap().entries.is_empty());
    let page = reader.queued_routine_page(None).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert!(matches!(&page.entries[0].1, Ok(None)));
    assert!(
        reader
            .routine_slots_page(&route, None)
            .unwrap()
            .entries
            .is_empty()
    );
    let mut changed = route;
    changed.creation_request = BotRequestId::mint();
    assert!(reader.routine_slots_page(&changed, None).is_err());
}

#[test]
fn actual_shared_owner_forwards_admission_policy_and_native_intent_on_one_memory_database() {
    let mut shared = SharedLedger::new(RunsDb::open(std::path::Path::new(":memory:")).unwrap());
    let mut reader = shared.clone();
    let s = submission();
    let run = s.request.run;
    shared.bots().unwrap().admit_bot(&s).unwrap();
    let key = BotSubmissionKey {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        request: s.provenance.submission,
    };
    assert_eq!(
        reader
            .bots()
            .unwrap()
            .reconcile_bot(&key)
            .unwrap()
            .unwrap()
            .submission,
        s
    );
    assert!(reader.bots().unwrap().bot_execution(run).unwrap().is_some());
    shared.satisfy_intent(run, DocumentId::new()).unwrap();
    assert_eq!(
        shared.bots().unwrap().reserve_bot(run, None).unwrap(),
        BotReservation::Acquired
    );
    let row = shared.open_attempt(run).unwrap();
    shared
        .advance(run, row.generation, AttemptPhase::Starting)
        .unwrap();
    let native = ProviderSessionKey {
        idempotency: IdempotencyKey::mint(),
        session: Some(SessionId::mint()),
    };
    shared
        .bind_session(
            run,
            row.generation,
            native,
            HostInstanceId::from_uuid(uuid::Uuid::new_v4()),
        )
        .unwrap();
    shared
        .advance(run, row.generation, AttemptPhase::Running)
        .unwrap();
    shared
        .bots()
        .unwrap()
        .activate_bot(run, row.attempt, row.generation, None)
        .unwrap();
    let mut send = BotInitialSend {
        attempt: row.attempt,
        generation: row.generation,
        key: IdempotencyKey::mint(),
        fingerprint: [2; 32],
        turn: None,
    };
    shared
        .bots()
        .unwrap()
        .prepare_bot_initial_send(run, &send, None)
        .unwrap();
    send.turn = Some(AgentTurnId::mint());
    shared
        .bots()
        .unwrap()
        .acknowledge_bot_initial_send(run, &send)
        .unwrap();
    let barrier = BotPolicyBarrier {
        key: BotPolicyBarrierKey {
            owner: s.provenance.owner,
            bot: s.provenance.bot,
            request: BotRequestId::mint().into(),
        },
        config_revision: 1,
        policy_revision: 1,
        policy: s.policy,
        effects_allowed: true,
    };
    shared
        .native_results()
        .unwrap()
        .install_native_policy(&barrier)
        .unwrap();
    assert_eq!(
        reader
            .native_results()
            .unwrap()
            .native_policy(&barrier.key)
            .unwrap()
            .unwrap()
            .state,
        BotPolicyBarrierState::Effective
    );
    let routine = RoutinePolicyBarrier {
        owner: barrier.key.owner,
        bot: barrier.key.bot,
        schedule: ScheduleId::from_uuid(uuid::Uuid::new_v4()),
        request: BotRequestId::mint().into(),
        grant_revision: 1,
        policy: barrier.policy.clone(),
        effects_allowed: true,
    };
    shared
        .bots()
        .unwrap()
        .register_routine_route(&RoutineRouteIdentity {
            owner: routine.owner,
            bot: routine.bot,
            schedule: routine.schedule,
            workspace: routine.policy.workspace,
            creation_request: BotRequestId::mint(),
            seed_hash: RoutineDigest32::from_bytes([9; 32]),
            created_at_ms: 0,
        })
        .unwrap();
    let installed = shared
        .native_results()
        .unwrap()
        .install_routine_native_policy(&routine)
        .unwrap();
    assert_eq!(
        reader
            .native_results()
            .unwrap()
            .current_routine_native_policy(routine.owner, routine.bot, routine.schedule,)
            .unwrap(),
        Some(installed.clone())
    );
    assert_eq!(
        reader
            .native_results()
            .unwrap()
            .settle_routine_native_policy(&routine)
            .unwrap(),
        installed
    );
    let request = PrepareNativeResult {
        key: NativeResultKey {
            run,
            attempt: row.attempt,
            generation: row.generation,
            turn: send.turn.unwrap(),
            operation_id: uuid::Uuid::new_v4(),
        },
        kind: NativeResultOperation::CreatePage,
        destination: s.workspace,
        target: None,
        request_fingerprint: [3; 32],
        expected_revision: None,
        config_revision: 1,
        policy_revision: 1,
        title: "Result".into(),
    };
    let prepared = shared
        .native_results()
        .unwrap()
        .prepare_native_result(&request)
        .unwrap();
    let actual = reader
        .native_results()
        .unwrap()
        .native_result(&request.key)
        .unwrap()
        .unwrap();
    assert_eq!(actual.target, prepared.target);
    assert_eq!(actual.stage, NativeResultStage::Prepared);
    assert!(
        !reader
            .native_results()
            .unwrap()
            .native_effects_pending(run)
            .unwrap()
    );
    // The same production mutex/connection forwards the Memory identity lane.
    let allocation_request = AllocateMemoryEditRequest {
        key: NativeResultKey {
            run,
            attempt: row.attempt,
            generation: row.generation,
            turn: send.turn.unwrap(),
            operation_id: uuid::Uuid::new_v4(),
        },
        caller_fingerprint: [9; 32],
        subject: MemoryEditSubjectV1 {
            version: 1,
            owner: s.provenance.owner,
            bot: s.provenance.bot,
            submission: s.provenance.submission,
            captured_config_revision: 1,
            action: MemoryEditAction::Remember,
        },
        base: MemoryBaseSelector::OriginalSeed {},
        current_config_revision: 1,
        current_policy_revision: 1,
        existing_fact_id: None,
    };
    let memory_fence = BotExecutionFence {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        run,
        attempt: row.attempt,
        generation: row.generation,
        session: native.session.unwrap(),
        turn: send.turn,
    };
    let source = shared.intent(run).unwrap().unwrap().run_document.unwrap();
    let allocation = shared
        .native_results()
        .unwrap()
        .allocate_memory_edit(
            &allocation_request,
            source,
            &memory_fence,
            &koru_delegate_core::FreshBotPolicyProof {
                global: (barrier).clone(),
                routine: None,
            },
        )
        .unwrap();
    assert_eq!(
        reader
            .native_results()
            .unwrap()
            .memory_edit_allocation(run, allocation_request.key.operation_id, [9; 32])
            .unwrap(),
        Some(allocation)
    );
    let authority = MemoryEditAuthorityV1 {
        version: 1,
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        submission: s.provenance.submission,
        captured_config_revision: 1,
        creation_request: BotRequestId::mint(),
        workspace: s.workspace,
        memory_document: DocumentId::new(),
        action: MemoryEditAction::Remember,
    };
    let resolution = MemoryBaseResolution::Resolved {
        authority: authority.clone(),
        version: DocumentVersionId::from_uuid(uuid::Uuid::new_v4()),
        expected_revision: vec![1, 1, 2],
    };
    shared
        .native_results()
        .unwrap()
        .resolve_memory_edit_base(
            &allocation_request.key,
            [9; 32],
            &resolution,
            &memory_fence,
            &koru_delegate_core::FreshBotPolicyProof {
                global: (barrier).clone(),
                routine: None,
            },
        )
        .unwrap();
    let edit = PrepareNativeResult {
        key: allocation_request.key,
        kind: NativeResultOperation::EditPage,
        destination: s.workspace,
        target: Some(authority.memory_document),
        request_fingerprint: [8; 32],
        expected_revision: Some(vec![1, 1, 2]),
        config_revision: 1,
        policy_revision: 1,
        title: "Memory".into(),
    };
    let intent = shared
        .native_results()
        .unwrap()
        .prepare_memory_edit(&edit, [9; 32], &authority)
        .unwrap();
    assert_eq!(
        reader
            .native_results()
            .unwrap()
            .native_result(&edit.key)
            .unwrap(),
        Some(intent)
    );
    let fence = BotExecutionFence {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        run,
        attempt: row.attempt,
        generation: row.generation,
        session: reader
            .current_attempt(run)
            .unwrap()
            .unwrap()
            .provider_session
            .unwrap()
            .session
            .unwrap(),
        turn: send.turn,
    };
    shared
        .native_results()
        .unwrap()
        .lease_native_result(
            &request.key,
            request.request_fingerprint,
            &fence,
            &koru_delegate_core::FreshBotPolicyProof {
                global: (barrier).clone(),
                routine: None,
            },
        )
        .unwrap();
    let page = reader
        .native_results()
        .unwrap()
        .pending_native_effects(fence.owner, Some(fence.bot), None, 1)
        .unwrap();
    assert_eq!(page.targets.len(), 1);
    let proof = NativeEffectDrainReceipt {
        binding: page.targets[0].binding.clone(),
        fenced_at: NativeEffectTimestamp::from_millis(10),
        drained_at: NativeEffectTimestamp::from_millis(9),
    };
    shared
        .native_results()
        .unwrap()
        .confirm_native_result(&ConfirmNativeResult {
            key: request.key,
            request_fingerprint: request.request_fingerprint,
            observation: NativeResultObservation::DrainedUnknown { receipt: proof },
        })
        .unwrap();
    assert!(
        reader
            .native_results()
            .unwrap()
            .native_outcomes_unknown(run)
            .unwrap()
    );
    assert!(
        !reader
            .native_results()
            .unwrap()
            .native_effects_pending(run)
            .unwrap()
    );
    assert!(
        reader
            .native_results()
            .unwrap()
            .pending_native_effects(fence.owner, None, None, 1)
            .unwrap()
            .targets
            .is_empty()
    );
}

#[test]
fn actual_shared_ledger_retains_route_across_clones_without_run_or_session() {
    let mut shared = SharedLedger::new(RunsDb::open(std::path::Path::new(":memory:")).unwrap());
    let mut retry = shared.clone();
    let s = submission();
    let route = RoutineRouteIdentity {
        owner: s.provenance.owner,
        bot: s.provenance.bot,
        schedule: ScheduleId::mint(),
        workspace: s.workspace,
        creation_request: BotRequestId::mint(),
        seed_hash: RoutineDigest32::from_bytes([9; 32]),
        created_at_ms: 1,
    };
    assert_eq!(
        shared
            .bots()
            .unwrap()
            .register_routine_route(&route)
            .unwrap(),
        route
    );
    assert_eq!(
        retry
            .bots()
            .unwrap()
            .register_routine_route(&route)
            .unwrap(),
        route
    );
    let mut changed = route.clone();
    changed.workspace = WorkspaceId::new();
    assert!(
        retry
            .bots()
            .unwrap()
            .register_routine_route(&changed)
            .is_err()
    );
    assert_eq!(
        shared
            .0
            .lock()
            .unwrap()
            .routine_route(route.schedule)
            .unwrap(),
        Some(route)
    );
}

/// Cohort `m6_child_actor` (piece R): this owner forwards every child-request
/// method to the one production ledger. Before piece R each answered the
/// trait's refusing default; now a read reaches the rows and a write is
/// refused or accepted by the ledger itself, never by a default.
#[test]
fn m6_child_actor_shared_owner_forwards_every_child_request_method() {
    use koru_delegate_core::decisions::{BotWaitId, DecisionRevision, DecisionWaitExpected};
    let mut shared = SharedLedger::new(RunsDb::open(std::path::Path::new(":memory:")).unwrap());
    let s = submission();
    shared.bots().unwrap().admit_bot(&s).unwrap();
    let (owner, bot, run) = (s.provenance.owner, s.provenance.bot, s.request.run);
    let guard = shared.bots().unwrap();
    let ledger: &mut dyn BotLedger = guard;
    // Reads answer what the ledger holds: nothing yet, and never `Fenced`.
    let read = ledger.read_child_requests(owner, run, 0).unwrap();
    assert!(read.wait.is_none() && read.requests.is_empty());
    assert_eq!(ledger.child_request_of(owner, run, 0), Ok(None));
    assert_eq!(ledger.child_wait_execution(owner, run), Ok(None));
    assert_eq!(
        ledger.read_bot_child_wait(owner, bot, run, BotWaitId::mint()),
        Ok(None)
    );
    assert_eq!(
        ledger.settle_child_requests(owner, DelegateBootId::mint(), &[]),
        Ok(vec![])
    );
    assert_eq!(
        ledger.fold_child_clocks(owner.device, DelegateBootId::mint(), 0, 0),
        Ok(vec![])
    );
    assert_eq!(ledger.child_resume_runs(owner.device, None, 16), Ok(vec![]));
    assert_eq!(ledger.child_acknowledged_continuation(owner, run), Ok(None));
    // A step for a wait that does not exist is the ledger's own `Stale`,
    // not the default's `Fenced`.
    let expected = DecisionWaitExpected {
        owner,
        run,
        wait: BotWaitId::mint(),
        revision: DecisionRevision::new(1).unwrap(),
        decision: None,
    };
    assert_eq!(
        ledger
            .reserve_child_successor(
                &expected,
                HostInstanceId::from_uuid(uuid::Uuid::new_v4()),
                [0; 32],
                None
            )
            .unwrap_err(),
        BotLedgerError::Stale
    );
    assert_eq!(
        ledger
            .bind_child_successor(
                &expected,
                IdempotencyKey::mint(),
                HostInstanceId::from_uuid(uuid::Uuid::new_v4()),
                SessionId::mint()
            )
            .unwrap_err(),
        BotLedgerError::Stale
    );
}
