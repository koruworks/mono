// SPDX-License-Identifier: MPL-2.0
//! Actual client/worker/no-path Store composition, with a strict finite owner peer.
use super::*;
use koru_ai_sqlite::LifecycleCapture;
use koru_delegate_core::{BotBootAdmission, BotPolicyBarrierReceipt, BotPolicyBarrierState};

#[derive(Default)]
pub(super) struct Peer {
    enabled: AtomicBool,
    calls: Mutex<Vec<&'static str>>,
    fail: Mutex<Option<&'static str>>,
    held: Mutex<Option<(&'static str, Gate)>>,
    cancel_after_last: Mutex<Option<Cancellation>>,
    hint: Mutex<Option<BotBootAdmission>>,
    newer_global: Mutex<Option<koru_delegate_core::BotPolicyBarrier>>,
}
impl Peer {
    pub(super) fn answer(
        &self,
        command: &DelegateCommand,
        within: Duration,
    ) -> Option<Result<BotReply, koru_delegate_api::Failure>> {
        if !self.enabled.load(Ordering::Acquire) {
            return None;
        }
        let phase = match command {
            DelegateCommand::ReadAdmissionBarrier { .. } => "hint",
            DelegateCommand::Barrier { .. } => "admission",
            DelegateCommand::SynchronizePolicy { .. } => "global",
            DelegateCommand::SynchronizeRoutinePolicy { .. } => "routine",
            _ => return None,
        };
        assert!(!within.is_zero() && within <= ROUTINE_BRIDGE_DEADLINE);
        self.calls.lock().unwrap().push(phase);
        let gate = {
            let mut held = self.held.lock().unwrap();
            if held.as_ref().is_some_and(|(at, _)| *at == phase) {
                held.take().map(|(_, gate)| gate)
            } else {
                None
            }
        };
        if let Some(gate) = gate {
            gate.wait();
        }
        let fail = *self.fail.lock().unwrap();
        if fail == Some("unknown") || fail == Some(phase) {
            return Some(Err(unavailable()));
        }
        let reply = match command {
            DelegateCommand::ReadAdmissionBarrier { owner, bot } => {
                if fail == Some("generic_missing") {
                    return Some(Ok(BotReply::Missing));
                }
                BotReply::AdmissionBarrier {
                    owner: *owner,
                    bot: if fail == Some("foreign_hint") {
                        BotId::mint()
                    } else {
                        *bot
                    },
                    admission: self
                        .hint
                        .lock()
                        .unwrap()
                        .clone()
                        .unwrap_or(BotBootAdmission::Missing {}),
                }
            }
            DelegateCommand::Barrier { barrier } => {
                let mut barrier = barrier.clone();
                if fail == Some("wrong_admission") {
                    barrier.request = BotRequestId::mint();
                }
                BotReply::Barrier { barrier }
            }
            DelegateCommand::SynchronizePolicy { barrier } => {
                let mut barrier = barrier.clone();
                if fail == Some("historical_effective") {
                    let mut newer = barrier.clone();
                    newer.config_revision += 1;
                    newer.policy_revision += 1;
                    newer.key.request = policy::epoch::key(
                        BotOwner {
                            user_scope: newer.key.owner.user_scope,
                            device: newer.key.owner.device,
                        },
                        newer.key.bot,
                        newer.config_revision,
                    )
                    .unwrap();
                    *self.newer_global.lock().unwrap() = Some(newer);
                    // Retain the newer simulated owner state while returning
                    // the exact historical receipt, as the real ledger may.
                }
                if fail == Some("wrong_global") {
                    barrier.policy_revision += 1;
                }
                BotReply::PolicyBarrier {
                    receipt: BotPolicyBarrierReceipt {
                        barrier,
                        state: if fail == Some("pending_global") {
                            BotPolicyBarrierState::Pending
                        } else {
                            BotPolicyBarrierState::Effective
                        },
                    },
                }
            }
            DelegateCommand::SynchronizeRoutinePolicy { barrier } => {
                let mut barrier = barrier.clone();
                if fail == Some("wrong_routine") {
                    barrier.grant_revision += 1;
                }
                if let Some(cancel) = self.cancel_after_last.lock().unwrap().take() {
                    cancel.cancel();
                }
                BotReply::RoutinePolicyBarrier {
                    barrier,
                    state: if fail == Some("pending_routine") {
                        BotPolicyBarrierState::Pending
                    } else {
                        BotPolicyBarrierState::Effective
                    },
                }
            }
            _ => unreachable!(),
        };
        Some(Ok(reply))
    }
}

fn prepared() -> (Harness, SynchronizeRoutinePreclaim) {
    let h = Harness::new();
    let p = h.prepare();
    // Avoid recovery changing the draft while this test exercises preclaim.
    // This is a real unavailable owner read, never a readiness override.
    h.delegate.unavailable.store(true, Ordering::Release);
    h.delegate.preclaim.enabled.store(true, Ordering::Release);
    (
        h,
        SynchronizeRoutinePreclaim {
            slot: p.slot,
            semantic_hash: p.semantic_hash,
            preparation_hash: p.preparation_hash,
        },
    )
}
fn synchronize(
    h: &Harness,
    request: &SynchronizeRoutinePreclaim,
) -> SynchronizeRoutinePreclaimReply {
    wait(
        h.client()
            .synchronize_routine_preclaim(context(), request.clone())
            .unwrap(),
    )
    .unwrap()
}
fn refused(reply: SynchronizeRoutinePreclaimReply, code: RoutineBridgeRefusal) {
    assert_eq!(reply, SynchronizeRoutinePreclaimReply::Refused { code });
}
fn ready(reply: SynchronizeRoutinePreclaimReply) {
    assert!(
        matches!(reply, SynchronizeRoutinePreclaimReply::Ready { .. }),
        "{reply:?}"
    );
}
fn calls(h: &Harness) -> Vec<&'static str> {
    h.delegate.preclaim.calls.lock().unwrap().clone()
}
fn restart(h: &mut Harness, repository: Repository) {
    h.restart(repository);
    // Exact foreign owner refusal proves the real owner probe completed even
    // if intentionally damaged current execution fields cannot be read.
    let mut foreign = h.read_request();
    foreign.slot.owner.device = koru_delegate_core::DeviceId::new();
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let answer = wait(
            h.client()
                .read_routine_preparation(context(), foreign.clone())
                .unwrap(),
        )
        .unwrap();
        if answer
            == (ReadPreparationReply::Refused {
                code: RoutineBridgeRefusal::Forbidden,
            })
        {
            break;
        }
        assert!(Instant::now() < end, "actual owner probe did not complete");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn alter(h: &mut Harness, sql: &str) {
    let repo = h.finish();
    repo.fixture_execute_batch(sql).unwrap();
    restart(h, repo);
}
fn owner_context() -> RequestContext {
    RequestContext {
        principal: koru_ai_api::AiPrincipal {
            label: "preclaim fixture".into(),
            scopes: vec![AiScope::Chat, AiScope::ManageBots],
        },
        trace_id: "preclaim".into(),
        deadline_millis: None,
    }
}
fn update(h: &Harness) {
    let mut config = h.fixture.bot.config.clone();
    config.name = "Changed during synchronization".into();
    let answer = h
        .client()
        .bots(
            owner_context(),
            BotCommand::Update {
                request_id: BotRequestId::mint(),
                bot_id: h.fixture.bot.id,
                expected_revision: h.fixture.bot.revision,
                config,
            },
        )
        .unwrap();
    assert!(matches!(answer, BotResponse::Bot { .. }));
}

#[test]
fn preclaim_worker_installs_exact_admission_global_and_routine_in_order() {
    let (mut h, request) = prepared();
    let result = synchronize(&h, &request);
    assert!(result.matches_request(&request));
    let SynchronizeRoutinePreclaimReply::Ready {
        admission, proof, ..
    } = result
    else {
        panic!("Ready")
    };
    assert_eq!(admission.owner, request.slot.owner);
    assert_eq!(admission.bot, request.slot.bot);
    assert!(!admission.paused);
    assert_eq!(proof.global.config_revision, h.fixture.bot.revision);
    assert_eq!(proof.global.policy, bot_native_policy(&h.fixture.bot));
    assert_eq!(
        proof.routine.unwrap(),
        policy::initial_routine_barrier(&h.fixture.creation.identity)
    );
    assert_eq!(calls(&h), ["hint", "admission", "global", "routine"]);
    let repo = h.finish();
    let snapshot = repo
        .routine_preclaim_snapshot(&request.slot, request.semantic_hash)
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot.0.prepared.preparation_hash,
        request.preparation_hash
    );
    assert!(
        repo.attempts_for_chat(ChatId::from_uuid(snapshot.0.prepared.ids.chat))
            .unwrap()
            .is_empty()
    );
    assert_eq!(h.catalog.reads.load(Ordering::Acquire), 1);
    assert_eq!(h.authority.captures.load(Ordering::Acquire), 1);
    assert_eq!(h.authority.verifies.load(Ordering::Acquire), 1);
}

#[test]
fn preclaim_worker_uses_metadata_with_invalid_current_model_and_preserves_prepare() {
    let (mut h, request) = prepared();
    let original = h.prepare();
    alter(
        &mut h,
        "UPDATE bots SET model=zeroblob(1048577),working_folder=zeroblob(1048577)",
    );
    ready(synchronize(&h, &request));
    let repo = h.finish();
    assert_eq!(
        repo.routine_preclaim_snapshot(&request.slot, request.semantic_hash)
            .unwrap()
            .unwrap()
            .0
            .prepared,
        original
    );
    assert_eq!(h.authority.captures.load(Ordering::Acquire), 1);
    assert_eq!(h.catalog.reads.load(Ordering::Acquire), 1);
}

#[test]
fn preclaim_worker_requires_prepared_and_both_hashes() {
    let (mut h, request) = prepared();
    let mut wrong = request.clone();
    wrong.preparation_hash = RoutineDigest32::from_bytes([9; 32]);
    refused(synchronize(&h, &wrong), RoutineBridgeRefusal::Conflict);
    wrong = request.clone();
    wrong.semantic_hash = RoutineDigest32::from_bytes([8; 32]);
    refused(synchronize(&h, &wrong), RoutineBridgeRefusal::Conflict);
    wrong = request.clone();
    wrong.slot.occurrence_ms += 60_000;
    refused(synchronize(&h, &wrong), RoutineBridgeRefusal::Stale);
    let p = h.prepare();
    let claim = h.delegate.claim(&p);
    h.delegate.unavailable.store(false, Ordering::Release);
    assert!(matches!(
        wait(
            h.client()
                .finalize_routine_occurrence(context(), FinalizeOccurrence { claim })
                .unwrap()
        )
        .unwrap(),
        FinalizeOccurrenceReply::Linked { .. }
    ));
    refused(synchronize(&h, &request), RoutineBridgeRefusal::Stale);
    h.finish();

    let (mut h, request) = prepared();
    let p = h.prepare();
    let claim = h.delegate.claim_with_state(
        &p,
        RoutineLinkState::Terminal {
            reason: RoutineTerminalReason::AlreadyTerminal,
        },
    );
    h.delegate.unavailable.store(false, Ordering::Release);
    assert!(matches!(
        wait(
            h.client()
                .finalize_routine_occurrence(context(), FinalizeOccurrence { claim })
                .unwrap()
        )
        .unwrap(),
        FinalizeOccurrenceReply::Terminal {
            reason: RoutineTerminalReason::AlreadyTerminal,
            ..
        }
    ));
    refused(synchronize(&h, &request), RoutineBridgeRefusal::Stale);
}

#[test]
fn preclaim_worker_refuses_both_revision_drifts() {
    for column in ["revision", "policy_revision"] {
        let (mut h, request) = prepared();
        alter(&mut h, &format!("UPDATE bots SET {column}={column}+1"));
        refused(synchronize(&h, &request), RoutineBridgeRefusal::Conflict);
        assert!(calls(&h).is_empty());
    }
}

#[test]
fn preclaim_worker_requires_bound_exact_route_and_grant() {
    for sql in [
        "UPDATE bot_routines SET origin_edit_revision=x''",
        "UPDATE bot_routines SET grant_digest=zeroblob(32)",
        "UPDATE bot_routines SET seed_hash=zeroblob(32)",
        "UPDATE bot_routines SET owner_device_id='00000000-0000-4000-8000-000000000001'",
    ] {
        let (mut h, request) = prepared();
        alter(
            &mut h,
            &format!("PRAGMA ignore_check_constraints=ON; {sql}"),
        );
        assert!(matches!(
            synchronize(&h, &request),
            SynchronizeRoutinePreclaimReply::Refused { .. }
        ));
        assert!(calls(&h).is_empty());
    }
}

#[test]
fn preclaim_worker_fences_pause_access_and_whole_bot_lifecycle_but_not_chat_lifecycle() {
    for state in ["paused", "access", "whole", "chat"] {
        let (mut h, request) = prepared();
        let mut repo = h.finish();
        match state {
            "paused" => {
                repo.fixture_execute_batch("UPDATE bots SET paused=1")
                    .unwrap();
            }
            "access" => {
                let mut bot = h.fixture.bot.clone();
                let expected = bot.revision;
                bot.revision += 1;
                bot.policy_revision += 1;
                bot.access = BotAccess::Workspace;
                let barrier = policy::barrier(&bot).unwrap();
                repo.prepare_bot_access(
                    &bot,
                    h.fixture.profile,
                    BotRequestId::mint(),
                    &"a".repeat(64),
                    expected,
                    &barrier,
                )
                .unwrap();
            }
            _ => {
                repo.capture_bot_lifecycle(&LifecycleCapture {
                    operation: BotRequestId::mint(),
                    owner: h.fixture.owner,
                    bot: h.fixture.bot.id,
                    kind: if state == "chat" {
                        BotLifecycleKind::TrashChat
                    } else {
                        BotLifecycleKind::Archive
                    },
                    chat: (state == "chat").then_some(h.fixture.bot.main_chat),
                    fingerprint: "a".repeat(64),
                    expected_revision: h.fixture.bot.revision,
                    source: None,
                    members: vec![],
                    now: 10,
                })
                .unwrap();
            }
        }
        restart(&mut h, repo);
        if state == "chat" {
            ready(synchronize(&h, &request));
        } else {
            refused(synchronize(&h, &request), RoutineBridgeRefusal::Unavailable);
            assert!(calls(&h).is_empty());
        }
    }
}

#[test]
fn preclaim_worker_requires_current_store_owner_home_and_both_frozen_profiles() {
    let (h, request) = prepared();
    let home = h.request.workspace;
    let other = h.fixture.other;
    h.fixture
        .memory
        .with_store(move |store| store.move_workspace(home, other).unwrap())
        .unwrap();
    refused(synchronize(&h, &request), RoutineBridgeRefusal::Forbidden);
    assert!(calls(&h).is_empty());
    let mut foreign = request.clone();
    foreign.slot.owner.device = koru_delegate_core::DeviceId::new();
    refused(synchronize(&h, &foreign), RoutineBridgeRefusal::Forbidden);
}

#[test]
fn preclaim_worker_targeted_admission_unknown_never_becomes_missing() {
    for fail in ["unknown", "generic_missing", "foreign_hint"] {
        let (h, request) = prepared();
        *h.delegate.preclaim.fail.lock().unwrap() = Some(fail);
        refused(synchronize(&h, &request), RoutineBridgeRefusal::Unavailable);
        assert_eq!(calls(&h), ["hint"]);
    }
    let (h, request) = prepared();
    *h.delegate.preclaim.hint.lock().unwrap() = Some(BotBootAdmission::Present {
        barrier: koru_delegate_core::BotAdmissionBarrier {
            owner: request.slot.owner,
            bot: request.slot.bot,
            request: BotRequestId::mint(),
            epoch: h.fixture.bot.revision + 1,
            paused: false,
        },
    });
    refused(synchronize(&h, &request), RoutineBridgeRefusal::Conflict);
    assert_eq!(calls(&h), ["hint"]);
}

#[test]
fn preclaim_worker_stops_on_wrong_or_pending_sync_ack() {
    for (fail, count, code) in [
        ("wrong_admission", 2, RoutineBridgeRefusal::Conflict),
        ("wrong_global", 3, RoutineBridgeRefusal::Conflict),
        ("pending_global", 3, RoutineBridgeRefusal::Unavailable),
        ("wrong_routine", 4, RoutineBridgeRefusal::Conflict),
        ("pending_routine", 4, RoutineBridgeRefusal::Unavailable),
        ("global", 3, RoutineBridgeRefusal::Unavailable),
    ] {
        let (h, request) = prepared();
        *h.delegate.preclaim.fail.lock().unwrap() = Some(fail);
        refused(synchronize(&h, &request), code);
        assert_eq!(calls(&h).len(), count);
    }
}

#[test]
fn preclaim_worker_exact_historical_effective_is_only_synchronization_evidence() {
    let (h, request) = prepared();
    *h.delegate.preclaim.fail.lock().unwrap() = Some("historical_effective");
    let SynchronizeRoutinePreclaimReply::Ready { proof, .. } = synchronize(&h, &request) else {
        panic!("exact sync evidence")
    };
    let newer = h
        .delegate
        .preclaim
        .newer_global
        .lock()
        .unwrap()
        .clone()
        .unwrap();
    assert!(newer.config_revision > proof.global.config_revision);
    assert_ne!(newer, proof.global);
    assert_eq!(calls(&h), ["hint", "admission", "global", "routine"]);
    // This peer proves the worker's precise epistemic boundary, not runtime
    // acceptance: actual runtime/SQL must compare against durable latest proof.
}

fn drift_at(phase: &'static str) {
    let (h, request) = prepared();
    let (held, entered, mut release) = gate();
    *h.delegate.preclaim.held.lock().unwrap() = Some((phase, held));
    let call = h
        .client()
        .synchronize_routine_preclaim(context(), request)
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    update(&h);
    release.open();
    refused(wait(call).unwrap(), RoutineBridgeRefusal::Conflict);
    assert_eq!(calls(&h).len(), if phase == "hint" { 1 } else { 4 });
}
#[test]
fn preclaim_worker_rechecks_complete_snapshot_after_admission_read() {
    drift_at("hint");
}
#[test]
fn preclaim_worker_rechecks_complete_snapshot_after_last_sync() {
    drift_at("routine");
}

#[test]
fn preclaim_worker_current_profile_drift_after_last_ack_refuses() {
    let (h, request) = prepared();
    let (held, entered, mut release) = gate();
    *h.delegate.preclaim.held.lock().unwrap() = Some(("routine", held));
    let call = h
        .client()
        .synchronize_routine_preclaim(context(), request)
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let home = h.request.workspace;
    let other = h.fixture.other;
    h.fixture
        .memory
        .with_store(move |store| store.move_workspace(home, other).unwrap())
        .unwrap();
    release.open();
    refused(wait(call).unwrap(), RoutineBridgeRefusal::Forbidden);
}

#[test]
fn preclaim_worker_cancellation_after_final_ack_never_returns_ready() {
    let (h, request) = prepared();
    let context = context();
    *h.delegate.preclaim.cancel_after_last.lock().unwrap() = Some(context.cancellation.clone());
    let call = h
        .client()
        .synchronize_routine_preclaim(context, request)
        .unwrap();
    refused(wait(call).unwrap(), RoutineBridgeRefusal::Unavailable);
    assert_eq!(calls(&h), ["hint", "admission", "global", "routine"]);
}

#[test]
fn preclaim_worker_expired_deadline_never_sends_next_phase_or_ready() {
    for phase in ["hint", "routine"] {
        let (h, request) = prepared();
        let (held, entered, mut release) = gate();
        *h.delegate.preclaim.held.lock().unwrap() = Some((phase, held));
        let deadline = Instant::now() + Duration::from_secs(1);
        let call = h
            .client()
            .synchronize_routine_preclaim(
                RoutineCallContext::new(Uuid::new_v4(), deadline, Cancellation::default()),
                request,
            )
            .unwrap();
        entered.recv_timeout(Duration::from_secs(3)).unwrap();
        std::thread::sleep(
            deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(5),
        );
        release.open();
        refused(wait(call).unwrap(), RoutineBridgeRefusal::Unavailable);
        assert_eq!(calls(&h).len(), if phase == "hint" { 1 } else { 4 });
    }
}

#[test]
fn preclaim_worker_drop_holds_mutation_capacity_until_actual_completion() {
    let (h, request) = prepared();
    let (held, entered, mut release) = gate();
    *h.delegate.preclaim.held.lock().unwrap() = Some(("routine", held));
    let call = h
        .client()
        .synchronize_routine_preclaim(context(), request.clone())
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(call);
    let queued = h
        .client()
        .synchronize_routine_preclaim(context(), request.clone())
        .unwrap();
    assert!(matches!(
        h.client().synchronize_routine_preclaim(context(), request),
        Err(RoutineBridgeRefusal::Capacity)
    ));
    release.open();
    ready(wait(queued).unwrap());
}

#[test]
fn preclaim_worker_mutation_continuations_leave_reads_and_stop_responsive() {
    let (h, request) = prepared();
    let (held, entered, mut release) = gate();
    *h.delegate.preclaim.held.lock().unwrap() = Some(("routine", held));
    let call = h
        .client()
        .synchronize_routine_preclaim(context(), request.clone())
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    h.worker.as_ref().unwrap().ping().unwrap();
    assert!(matches!(
        wait(
            h.client()
                .read_routine_preparation(context(), h.read_request())
                .unwrap()
        )
        .unwrap(),
        ReadPreparationReply::Prepared { .. }
    ));
    // A draft is not an interactive API task. The real Stop entry must answer
    // its existing not-found refusal while synchronization remains held.
    let error = h
        .client()
        .bots(
            owner_context(),
            BotCommand::StopTask {
                request_id: BotRequestId::mint(),
                bot_id: request.slot.bot,
            },
        )
        .unwrap_err();
    assert_eq!(error.code, "not_found");
    release.open();
    ready(wait(call).unwrap());
}

#[test]
fn preclaim_worker_shutdown_joins_running_sync_and_resolves_waiters() {
    let (mut h, request) = prepared();
    let (held, entered, mut release) = gate();
    *h.delegate.preclaim.held.lock().unwrap() = Some(("routine", held));
    let context = context();
    let cancel = context.cancellation.clone();
    let call = h
        .client()
        .synchronize_routine_preclaim(context, request)
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let gates = h.client().routine_calls.clone();
    h.worker.as_ref().unwrap().shutdown();
    let end = Instant::now() + Duration::from_secs(2);
    while !cancel.is_cancelled() {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(5));
    }
    let spare = gates.acquire(false).unwrap();
    assert!(gates.acquire(false).is_err());
    release.open();
    assert_eq!(wait(call), Err(RoutineBridgeRefusal::Unavailable));
    h.finish();
    assert!(gates.acquire(false).is_ok());
    drop(spare);
}
