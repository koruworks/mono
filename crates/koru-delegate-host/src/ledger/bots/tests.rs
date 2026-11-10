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
            request: BotRequestId::mint(),
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
}
