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
        shared.bots().unwrap().reserve_bot(run).unwrap(),
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
        .activate_bot(run, row.attempt, row.generation)
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
        .prepare_bot_initial_send(run, &send)
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
