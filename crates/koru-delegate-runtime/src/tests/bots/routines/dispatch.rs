// SPDX-License-Identifier: MPL-2.0

//! Real live Loro authority, production orchestrator and in-memory SQL. Session
//! operations are deterministic ports; no actual provider or Store is opened.
use super::*;

struct FailPolicyAfterMcp {
    inner: StubMcp,
    ledger: Arc<Mutex<LedgerScript>>,
    sessions: Arc<Mutex<SessionScript>>,
    sql: RecordingStore,
    uncertain: bool,
}
impl McpEndpoints for FailPolicyAfterMcp {
    fn launch(&mut self, launch: McpLaunch<'_>) -> PortResult<DelegationLaunch> {
        let run = launch.run;
        let result = self.inner.launch(launch)?;
        self.ledger.lock().unwrap().fail = Some("bot_current_routine_policy");
        if self.uncertain {
            let row = self
                .sql
                .inner
                .lock()
                .unwrap()
                .current_attempt(run)
                .unwrap()
                .unwrap();
            self.sessions.lock().unwrap().lookups.insert(
                row.provider_session.unwrap().idempotency,
                (host(1), CreationLookup::Creating),
            );
        }
        Ok(result)
    }
    fn revoke(&mut self, run: RunId) {
        self.inner.revoke(run);
    }
}

fn fail_start_policy(after_mcp: bool, uncertain: bool) {
    let (mut h, p, fresh) = linked();
    global(&mut h, &p, &fresh);
    h.orchestrator
        .synchronize_routine_policy(fresh.routine.unwrap(), p.slot.owner.device)
        .unwrap();
    if after_mcp {
        h.orchestrator.ports_mut().mcp = Box::new(FailPolicyAfterMcp {
            inner: StubMcp {
                log: h.authority.clone(),
            },
            ledger: h.ledger.clone(),
            sessions: h.sessions.clone(),
            sql: h.schedule_store.clone(),
            uncertain,
        });
    } else {
        h.ledger.lock().unwrap().arm_failure_after =
            Some(("bind_session:key", "bot_current_routine_policy"));
    }
    let result = h
        .orchestrator
        .bot_dispatch(&key(&p.submission), p.slot.owner.device);
    let failed = RunStatus::Failed {
        stage: FailureStage::Authority,
    };
    if uncertain {
        assert!(result.is_err());
    } else {
        assert_eq!(result.unwrap().unwrap().status, failed);
    }
    assert_eq!(h.orchestrator.status(p.ids.run).unwrap(), failed);
    assert!(
        h.sessions
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|call| !matches!(call, Call::Create { .. }))
    );
    assert_eq!(
        h.authority.lock().unwrap().issued.len(),
        usize::from(after_mcp)
    );
    assert!(h.authority.lock().unwrap().revoked.contains(&p.ids.run));
    let row = h
        .schedule_store
        .inner
        .lock()
        .unwrap()
        .current_attempt(p.ids.run)
        .unwrap()
        .unwrap();
    assert_eq!(row.retired, !uncertain);
    let state = h
        .schedule_store
        .inner
        .lock()
        .unwrap()
        .bot_execution(p.ids.run)
        .unwrap()
        .unwrap()
        .state;
    assert_eq!(
        state,
        if uncertain {
            BotAdmissionState::Executing
        } else {
            BotAdmissionState::Settled
        }
    );
    assert_eq!(
        h.orchestrator.bot_budget_runs().contains(&p.ids.run),
        uncertain
    );
    if uncertain {
        // Cleanup owns this attempt and remains scheduled, rather than leaving
        // an untracked Starting row or treating unknown creation as absence.
        assert!(h.orchestrator.bot_budget_delay_ms().unwrap().is_some());
    }
    let _ = h
        .orchestrator
        .bot_dispatch(&key(&p.submission), p.slot.owner.device);
    assert_eq!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .attempts(p.ids.run)
            .unwrap()
            .len(),
        1
    );
    assert!(
        h.sessions
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|call| !matches!(call, Call::Create { .. }))
    );
    assert_eq!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .routine_firing(&p.slot)
            .unwrap(),
        None
    );
}

#[test]
fn routine_dispatch_policy_failure_before_mcp_records_failed_and_retires() {
    fail_start_policy(false, false);
}

#[test]
fn routine_dispatch_policy_failure_after_mcp_revokes_and_preserves_unknown_ownership() {
    fail_start_policy(true, false);
    fail_start_policy(true, true);
}

fn linked() -> (Harness, RoutinePrepared, FreshBotPolicyProof) {
    let (h, doc, p, fresh) = fixture();
    let record = claim(&h, &doc, &p, &fresh, false);
    h.schedule_store
        .inner
        .lock()
        .unwrap()
        .acknowledge_routine_link(&RoutineLinkAcknowledgement {
            claim: record.claim,
            outcome: RoutineLinkOutcome::Linked {},
        })
        .unwrap();
    (h, p, fresh)
}
fn global(h: &mut Harness, p: &RoutinePrepared, fresh: &FreshBotPolicyProof) {
    let boot = h
        .orchestrator
        .bot_hello(p.slot.owner, BotBootScan::Probe {}, p.slot.owner.device)
        .unwrap()
        .boot;
    let receipt = h
        .orchestrator
        .synchronize_boot_bot(
            boot,
            admission(p),
            fresh.global.clone(),
            p.slot.owner.device,
        )
        .unwrap();
    assert_eq!(receipt.state, BotBootState::Ready);
}
fn start(h: &mut Harness, p: &RoutinePrepared, fresh: &FreshBotPolicyProof) -> BotExecutionFence {
    global(h, p, fresh);
    h.orchestrator
        .synchronize_routine_policy(fresh.routine.clone().unwrap(), p.slot.owner.device)
        .unwrap();
    let started = h
        .orchestrator
        .bot_dispatch(&key(&p.submission), p.slot.owner.device)
        .unwrap()
        .unwrap();
    assert_eq!(started.status, RunStatus::Starting);
    h.orchestrator
        .bot_reconcile(&submission_key(&p.submission), p.slot.owner.device)
        .unwrap()
        .unwrap()
        .fence
        .unwrap()
}

#[test]
fn routine_dispatch_uses_existing_bot_session_and_fires_only_on_acknowledged_input() {
    let (mut h, p, fresh) = linked();
    let fence = start(&mut h, &p, &fresh);
    assert_eq!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .routine_firing(&p.slot)
            .unwrap(),
        None
    );
    assert_eq!(count_submits(&h), 0);
    observe(&mut h, &fence);
    let turn = h
        .orchestrator
        .bot_begin_turn(&fence, p.slot.owner.device, p.submission.input.clone())
        .unwrap();
    assert!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .routine_firing(&p.slot)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        h.orchestrator
            .bot_begin_turn(&fence, p.slot.owner.device, p.submission.input.clone())
            .unwrap(),
        turn
    );
    assert_eq!(count_submits(&h), 1);
    assert!(
        h.orchestrator
            .start(p.ids.run, StartOrigin::Manual)
            .is_err()
    );
}

#[test]
fn routine_dispatch_global_boot_never_substitutes_for_per_run_routine_cache() {
    let (mut h, p, fresh) = linked();
    global(&mut h, &p, &fresh);
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&p.submission), p.slot.owner.device)
            .is_err()
    );
    assert!(h.sessions.lock().unwrap().calls.is_empty());
    assert_eq!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .bot_execution(p.ids.run)
            .unwrap()
            .unwrap()
            .state,
        BotAdmissionState::Queued
    );
    // Global readiness still permits ordinary work for the same Bot.
    let ordinary = submission(2);
    h.orchestrator
        .bot_submit(ordinary.clone(), p.slot.owner.device)
        .unwrap();
    global(&mut h, &p, &fresh);
    assert!(
        h.orchestrator
            .bot_dispatch(&key(&ordinary), p.slot.owner.device)
            .unwrap()
            .is_some()
    );
}

#[test]
fn routine_dispatch_lost_first_input_reply_is_not_fired_and_replay_keeps_ids() {
    let (mut h, p, fresh) = linked();
    let fence = start(&mut h, &p, &fresh);
    observe(&mut h, &fence);
    h.sessions.lock().unwrap().bot_reply_lost = true;
    assert!(
        h.orchestrator
            .bot_begin_turn(&fence, p.slot.owner.device, p.submission.input.clone())
            .is_err()
    );
    assert_eq!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .routine_firing(&p.slot)
            .unwrap(),
        None
    );
    let before = h
        .schedule_store
        .inner
        .lock()
        .unwrap()
        .current_attempt(p.ids.run)
        .unwrap()
        .unwrap();
    h.orchestrator
        .bot_begin_turn(&fence, p.slot.owner.device, p.submission.input.clone())
        .unwrap();
    let after = h
        .schedule_store
        .inner
        .lock()
        .unwrap()
        .current_attempt(p.ids.run)
        .unwrap()
        .unwrap();
    assert_eq!(before.attempt, after.attempt);
    assert_eq!(before.provider_session, after.provider_session);
    assert_eq!(count_submits(&h), 1);
}

#[test]
fn routine_dispatch_current_policy_change_blocks_new_input_but_not_acknowledged_replay() {
    for acknowledged in [false, true] {
        let (mut h, p, fresh) = linked();
        let fence = start(&mut h, &p, &fresh);
        observe(&mut h, &fence);
        if acknowledged {
            h.orchestrator
                .bot_begin_turn(&fence, p.slot.owner.device, p.submission.input.clone())
                .unwrap();
        }
        let mut later = fresh.routine.unwrap();
        later.request = BotRequestId::mint().into();
        later.grant_revision += 1;
        later.effects_allowed = false;
        h.orchestrator
            .synchronize_routine_policy(later, p.slot.owner.device)
            .unwrap();
        assert_eq!(
            h.orchestrator
                .bot_begin_turn(&fence, p.slot.owner.device, p.submission.input.clone())
                .is_ok(),
            acknowledged
        );
        assert_eq!(count_submits(&h), usize::from(acknowledged));
        let mut changed = p.submission.input.clone();
        changed.text.push_str(" changed");
        assert!(
            h.orchestrator
                .bot_begin_turn(&fence, p.slot.owner.device, changed)
                .is_err()
        );
    }
}

#[test]
fn routine_dispatch_boot_inventory_retains_each_run_of_same_bot() {
    let (mut h, p, fresh) = linked();
    let ordinary = submission(2);
    h.orchestrator
        .bot_submit(ordinary, p.slot.owner.device)
        .unwrap();
    let page = h
        .orchestrator
        .bot_hello(p.slot.owner, BotBootScan::Start {}, p.slot.owner.device)
        .unwrap();
    assert_eq!(page.entries.len(), 2);
    assert!(
        page.entries
            .iter()
            .all(|entry| entry.identity.bot == p.slot.bot)
    );
    assert_ne!(page.entries[0].identity.run, page.entries[1].identity.run);
    global(&mut h, &p, &fresh);
}

#[test]
fn routine_dispatch_live_result_and_memory_use_composite_lease_and_preserve_replay() {
    let (mut h, p, fresh) = linked();
    let mut fence = start(&mut h, &p, &fresh);
    observe(&mut h, &fence);
    fence.turn = Some(
        h.orchestrator
            .bot_begin_turn(&fence, p.slot.owner.device, p.submission.input.clone())
            .unwrap(),
    );
    let request = PrepareNativeResult {
        key: NativeResultKey {
            run: fence.run,
            attempt: fence.attempt,
            generation: fence.generation,
            turn: fence.turn.unwrap(),
            operation_id: uuid::Uuid::new_v4(),
        },
        kind: NativeResultOperation::CreatePage,
        destination: p.submission.workspace,
        target: None,
        request_fingerprint: [42; 32],
        expected_revision: None,
        config_revision: 1,
        policy_revision: 1,
        title: "Routine result".into(),
    };
    let result = h
        .orchestrator
        .prepare_native_effect(&request, p.slot.owner.device)
        .unwrap();
    assert_eq!(result.disposition, NativeResultDisposition::Execute);
    let allocation = AllocateMemoryEditRequest {
        key: NativeResultKey {
            operation_id: uuid::Uuid::new_v4(),
            ..request.key.clone()
        },
        caller_fingerprint: [43; 32],
        subject: MemoryEditSubjectV1 {
            version: 1,
            owner: p.slot.owner,
            bot: p.slot.bot,
            submission: p.ids.submission,
            captured_config_revision: 1,
            action: MemoryEditAction::Remember,
        },
        base: MemoryBaseSelector::OriginalSeed {},
        current_config_revision: 1,
        current_policy_revision: 1,
        existing_fact_id: None,
    };
    let retained = h
        .orchestrator
        .allocate_memory_edit(&allocation, p.slot.owner.device)
        .unwrap();
    let mut newer = fresh.routine.unwrap();
    newer.grant_revision += 1;
    newer.request = BotRequestId::mint().into();
    newer.effects_allowed = false;
    assert_eq!(
        h.orchestrator
            .synchronize_routine_policy(newer, p.slot.owner.device)
            .unwrap(),
        BotPolicyBarrierState::Pending
    );
    assert_eq!(
        h.orchestrator
            .prepare_native_effect(&request, p.slot.owner.device)
            .unwrap()
            .disposition,
        NativeResultDisposition::ReconcileOnly
    );
    assert_eq!(
        h.orchestrator
            .allocate_memory_edit(&allocation, p.slot.owner.device)
            .unwrap(),
        retained
    );
    let mut new = request;
    new.key.operation_id = uuid::Uuid::new_v4();
    assert!(
        h.orchestrator
            .prepare_native_effect(&new, p.slot.owner.device)
            .is_err()
    );
}

#[test]
fn routine_dispatch_restart_reattaches_retained_session_without_new_attempt_or_input() {
    let (mut h, p, fresh) = linked();
    let fence = start(&mut h, &p, &fresh);
    let mut h = restarted(
        h,
        (
            host(1),
            CreationLookup::Known {
                session: summary(ManagedState::Idle, None),
            },
        ),
    );
    let report = h.orchestrator.restart().unwrap();
    assert_eq!(report.reattached.len(), 1);
    assert_eq!(count_submits(&h), 0);
    observe(&mut h, &fence);
    assert!(
        h.orchestrator
            .bot_begin_turn(&fence, p.slot.owner.device, p.submission.input.clone())
            .is_err()
    );
    global(&mut h, &p, &fresh);
    h.orchestrator
        .synchronize_routine_policy(fresh.routine.unwrap(), p.slot.owner.device)
        .unwrap();
    h.orchestrator
        .bot_begin_turn(&fence, p.slot.owner.device, p.submission.input)
        .unwrap();
    assert_eq!(
        h.schedule_store
            .inner
            .lock()
            .unwrap()
            .attempts(p.ids.run)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(count_submits(&h), 1);
}
