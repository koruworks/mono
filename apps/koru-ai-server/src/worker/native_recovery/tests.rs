// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_delegate_core::*;
use koru_store_core::{NativeEffectBinding, NativeEffectKind};
use koru_store_server::memory::MemoryStore;

fn owner() -> BotOwner {
    BotOwner {
        user_scope: koru_store_core::UserScopeId::new(),
        device: DeviceId::new(),
    }
}
fn cursor() -> NativeRecoveryCursor {
    NativeRecoveryCursor {
        created_at_ms: 10,
        run: RunId::mint(),
        operation_id: Uuid::new_v4(),
    }
}
fn ready_gate(owner: BotOwner) -> Arc<NativeMutationGate> {
    let gate = Arc::new(NativeMutationGate::closed());
    let startup = gate.next_sweep(Instant::now()).unwrap();
    assert!(gate.complete(NativeDrainResult {
        id: startup.id,
        progress: Ok((owner, None, false))
    }));
    assert!(gate.ready());
    gate
}
pub(in crate::worker) fn saturated_gate(owner: BotOwner) -> Arc<NativeMutationGate> {
    let gate = ready_gate(owner);
    for _ in 0..MAX_RECOVERY_SCOPES {
        gate.enqueue(scoped(owner, BotId::mint(), NativeDrainReason::OwnerStop))
            .unwrap();
    }
    gate
}
fn scoped(owner: BotOwner, bot: BotId, reason: NativeDrainReason) -> Sweep {
    Sweep::new(Some(owner), Some(bot), Some(RunId::mint()), None, reason)
}

#[test]
fn same_gate_retains_startup_cursor_and_backoff_without_restarting_open_readiness() {
    let gate = Arc::new(NativeMutationGate::closed());
    let owner = owner();
    let start = gate.next_sweep(Instant::now()).unwrap();
    let after = cursor();
    gate.complete(NativeDrainResult {
        id: start.id,
        progress: Ok((owner, Some(after.clone()), false)),
    });
    let restarted = gate.clone();
    let resumed = restarted.next_sweep(Instant::now()).unwrap();
    assert_eq!(resumed.id, start.id);
    assert_eq!(resumed.after, Some(after));
    assert!(!restarted.ready());
    restarted.complete(NativeDrainResult {
        id: resumed.id,
        progress: Err(()),
    });
    assert!(restarted.next_sweep(Instant::now()).is_none());
    let retry = restarted
        .next_sweep(Instant::now() + Duration::from_secs(32))
        .unwrap();
    assert_eq!(retry.id, start.id);
    assert!(retry.after.is_some());
    restarted.complete(NativeDrainResult {
        id: retry.id,
        progress: Ok((owner, None, false)),
    });
    assert!(gate.ready());
    assert!(
        gate.next_sweep(Instant::now() + Duration::from_secs(64))
            .is_none()
    );
    // Reusing the gate never constructs another unconditional process sweep.
    assert!(restarted.ready());
    assert_eq!(
        gate.enqueue(Sweep::new(
            Some(owner),
            None,
            None,
            None,
            NativeDrainReason::GatewayStartup
        )),
        Err(NativeRecoveryRefusal::ForeignOwner)
    );
}

#[test]
fn coalesced_wake_after_cursor_keeps_a_followup_pass_and_stale_ticket_cannot_skip_it() {
    let owner = owner();
    let gate = ready_gate(owner);
    let bot = BotId::mint();
    let scope = scoped(owner, bot, NativeDrainReason::UnconfirmedOperation);
    gate.enqueue(scope.clone()).unwrap();
    let first = gate.next_sweep(Instant::now()).unwrap();
    gate.complete(NativeDrainResult {
        id: first.id,
        progress: Ok((owner, Some(cursor()), false)),
    });
    let eof_job = gate.next_sweep(Instant::now()).unwrap();
    assert_eq!(gate.enqueue(scope), Ok(NativeRecoveryAdmission::Coalesced));
    gate.complete(NativeDrainResult {
        id: eof_job.id,
        progress: Ok((owner, None, false)),
    });
    let rescan = gate.next_sweep(Instant::now()).unwrap();
    assert!(rescan.after.is_none());
    let wider = scoped(owner, bot, NativeDrainReason::UnconfirmedOperation);
    gate.enqueue(wider).unwrap();
    let widened = gate.next_sweep(Instant::now()).unwrap();
    assert_ne!(widened.id, rescan.id);
    assert!(!gate.complete(NativeDrainResult {
        id: rescan.id,
        progress: Ok((owner, None, false))
    }));
    assert_eq!(gate.next_sweep(Instant::now()).unwrap().id, widened.id);
    assert!(gate.ready());
}

#[test]
fn saturation_coalesces_same_owner_uncertainty_and_sticky_overflow_is_fair_and_bounded() {
    let owner = owner();
    let gate = ready_gate(owner);
    for _ in 0..MAX_RECOVERY_SCOPES {
        gate.enqueue(scoped(
            owner,
            BotId::mint(),
            NativeDrainReason::UnconfirmedOperation,
        ))
        .unwrap();
    }
    assert_eq!(
        gate.enqueue(scoped(
            owner,
            BotId::mint(),
            NativeDrainReason::UnconfirmedOperation
        )),
        Ok(NativeRecoveryAdmission::Coalesced)
    );
    {
        let state = gate.recovery.lock().unwrap();
        assert_eq!(state.queue.len(), 1);
        assert!(state.queue[0].bot.is_none());
    }
    let gate = ready_gate(owner);
    for _ in 0..MAX_RECOVERY_SCOPES {
        gate.enqueue(scoped(owner, BotId::mint(), NativeDrainReason::OwnerStop))
            .unwrap();
    }
    assert_eq!(
        gate.enqueue(scoped(owner, BotId::mint(), NativeDrainReason::OwnerStop)),
        Err(NativeRecoveryRefusal::Busy)
    );
    assert_eq!(
        gate.enqueue(scoped(
            owner,
            BotId::mint(),
            NativeDrainReason::UnconfirmedOperation
        )),
        Ok(NativeRecoveryAdmission::Pending)
    );
    let overflow = gate.next_sweep(Instant::now()).unwrap();
    assert!(overflow.bot.is_none());
    gate.complete(NativeDrainResult {
        id: overflow.id,
        progress: Ok((owner, Some(cursor()), false)),
    });
    let queued = gate.next_sweep(Instant::now()).unwrap();
    assert_eq!(queued.reason, NativeDrainReason::OwnerStop);
    assert!(queued.bot.is_some());
    assert_eq!(
        gate.recovery.lock().unwrap().queue.len(),
        MAX_RECOVERY_SCOPES
    );
    assert!(gate.recovery.lock().unwrap().unconfirmed_overflow.is_some());
    assert_eq!(
        gate.enqueue(scoped(
            super::tests::owner(),
            BotId::mint(),
            NativeDrainReason::UnconfirmedOperation
        )),
        Err(NativeRecoveryRefusal::ForeignOwner)
    );
}

#[test]
fn a_full_worker_mailbox_does_not_lose_the_admitted_uncertain_scope() {
    let owner = owner();
    let gate = ready_gate(owner);
    let (sender, _receiver) = sync_channel(1);
    assert!(sender.try_send(Envelope::NativeDrainWake).is_ok());
    let client = AiWorkerClient {
        routine_calls: Default::default(),
        sender,
        native_mutations: Some(gate.clone()),
        attachments: AttachmentStore::new(&PathBuf::from("unused-native-recovery")),
        store_context: None,
    };
    let after = cursor();
    let key = NativeResultKey {
        run: after.run,
        attempt: RunAttemptId::mint(),
        generation: AttemptGeneration::FIRST,
        turn: AgentTurnId::mint(),
        operation_id: after.operation_id,
    };
    assert_eq!(
        client.reconcile_native_operation(owner, BotId::mint(), key.clone()),
        Ok(NativeRecoveryAdmission::Queued)
    );
    assert_eq!(gate.next_sweep(Instant::now()).unwrap().key, Some(key));
}

struct Port {
    target: NativePendingEffect,
    client: StoreWorkerClient,
    eligible: AtomicBool,
    fail_confirm: AtomicBool,
    confirmations: Mutex<Vec<koru_store_core::NativeEffectDrainReceipt>>,
    calls: Mutex<Vec<&'static str>>,
}
impl BotDelegatePort for Port {
    fn execute(&self, command: BotCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        match command {
            BotCommand::PendingNativeEffects {
                owner,
                after,
                limit,
                ..
            } => {
                assert_eq!(owner, self.target.owner);
                assert_eq!(limit, 1);
                self.calls.lock().unwrap().push("discover");
                Ok(BotReply::NativePendingEffects {
                    page: NativePendingEffects {
                        targets: if after.is_none() {
                            vec![self.target.clone()]
                        } else {
                            Vec::new()
                        },
                        next: after.is_none().then(|| self.target.cursor.clone()),
                    },
                })
            }
            BotCommand::CheckNativeEffectDrain { key, .. } => {
                assert_eq!(key, self.target.key);
                self.calls.lock().unwrap().push("check");
                Ok(BotReply::NativeDrainEligibility {
                    target: self
                        .eligible
                        .load(Ordering::SeqCst)
                        .then(|| self.target.clone()),
                })
            }
            BotCommand::ConfirmNativeResult { request } => {
                self.calls.lock().unwrap().push("confirm");
                let NativeResultObservation::DrainedUnknown { receipt } = request.observation
                else {
                    panic!("exact receipt required")
                };
                assert_eq!(receipt.binding, self.target.binding);
                let binding = receipt.binding.clone();
                // This round trip would fail if confirmation held the Store owner.
                assert!(
                    self.client
                        .with_store(move |store| store.check_native_effect(&binding).is_err())
                        .unwrap()
                );
                self.confirmations.lock().unwrap().push(receipt);
                if self.fail_confirm.swap(false, Ordering::SeqCst) {
                    return Err(koru_delegate_api::Failure::new(
                        "fixture_lost_ack",
                        "fixture",
                        true,
                    ));
                }
                Ok(BotReply::NativeResult {
                    intent: NativeResultIntentView {
                        purpose: koru_delegate_core::NativeEffectPurpose::Result {},
                        key: self.target.key.clone(),
                        kind: NativeResultOperation::CreatePage,
                        destination: self.target.binding.workspace,
                        request_fingerprint: self.target.binding.request_fingerprint,
                        target: self.target.binding.document,
                        store_request: self.target.binding.request_key,
                        stage: NativeResultStage::SettledUnknown,
                        verified: None,
                        disposition: NativeResultDisposition::ReconcileOnly,
                    },
                })
            }
            BotCommand::Hello { owner, .. } => {
                assert_eq!(owner, self.target.owner);
                // The independent Bot boot scan starts for each worker. This
                // drain fixture supplies no inventory or execution authority.
                Err(koru_delegate_api::Failure::new(
                    "fixture_boot_unavailable",
                    "Bot boot inventory is unavailable in this drain fixture",
                    true,
                ))
            }
            _ => panic!("no execution or ordinary read command belongs in recovery"),
        }
    }
}
fn job_fixture() -> (MemoryStore, NativeDrainJob, Arc<Port>) {
    let memory = MemoryStore::open("native-drain-job-memory").unwrap();
    let scope = memory
        .with_store(|store| store.local_user_scope().unwrap())
        .unwrap();
    let owner = BotOwner {
        user_scope: scope,
        device: DeviceId::new(),
    };
    let cursor = cursor();
    let key = NativeResultKey {
        run: cursor.run,
        attempt: RunAttemptId::mint(),
        generation: AttemptGeneration::FIRST,
        turn: AgentTurnId::mint(),
        operation_id: cursor.operation_id,
    };
    let target = NativePendingEffect {
        owner,
        bot: BotId::mint(),
        key,
        cursor,
        binding: NativeEffectBinding {
            user_scope: scope,
            scope: "native-job".into(),
            request_key: Uuid::new_v4(),
            workspace: koru_store_core::WorkspaceId::new(),
            document: koru_store_core::DocumentId::new(),
            kind: NativeEffectKind::CreateDocument,
            request_fingerprint: [3; 32],
        },
    };
    let port = Arc::new(Port {
        target,
        client: memory.client(),
        eligible: AtomicBool::new(true),
        fail_confirm: AtomicBool::new(false),
        confirmations: Mutex::new(Vec::new()),
        calls: Mutex::new(Vec::new()),
    });
    let authority = Arc::new(koru_ai_core::bots::BotAuthoritySnapshot {
        owner: koru_ai_core::bots::BotOwner {
            user_scope: scope,
            device: owner.device,
        },
        workspaces: BTreeMap::new(),
        pages: BTreeMap::new(),
    });
    let job = NativeDrainJob {
        sweep: scoped(
            owner,
            port.target.bot,
            NativeDrainReason::UnconfirmedOperation,
        ),
        config: NativeRecoveryConfig {
            gate: ready_gate(owner),
            store: memory.client(),
        },
        authority,
        delegate: port.clone(),
    };
    (memory, job, port)
}

#[test]
fn replacing_the_ai_thread_reuses_open_process_gate_without_startup_discovery() {
    let (memory, job, port) = job_fixture();
    let gate = job.config.gate;
    for _ in 0..2 {
        let mut config = WorkerConfig::new(
            PathBuf::from("unused-native-ai-restart"),
            "internal-debug",
            ProviderRegistry::default(),
        )
        .with_bot_authority(job.authority.clone())
        .with_bot_delegate(port.clone())
        .with_native_effect_recovery(memory.client(), gate.clone());
        config.test_repository = Some(Repository::in_memory().unwrap());
        let worker = AiWorker::new(config).spawn().unwrap();
        worker.ping().unwrap();
        assert!(Arc::ptr_eq(
            worker.client().native_mutations.as_ref().unwrap(),
            &gate
        ));
        assert!(gate.ready());
        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
    }
    assert!(port.calls.lock().unwrap().is_empty());
}

#[test]
fn one_target_job_checks_eligibility_and_replays_store_proof_after_actor_ack_loss() {
    let (_memory, mut job, port) = job_fixture();
    job.sweep.run = Some(port.target.key.run);
    port.fail_confirm.store(true, Ordering::SeqCst);
    let first = job.execute().unwrap();
    assert!(first.1.is_some());
    assert!(first.2);
    let second = job.execute().unwrap();
    assert!(!second.2);
    let proofs = port.confirmations.lock().unwrap();
    assert_eq!(proofs.len(), 2);
    assert_eq!(proofs[0], proofs[1]);
    assert_eq!(
        port.calls.lock().unwrap().as_slice(),
        [
            "discover", "check", "confirm", "discover", "check", "confirm"
        ]
    );
    drop(proofs);
    job.sweep.after = second.1;
    assert_eq!(job.execute().unwrap(), (port.target.owner, None, false));
}

#[test]
fn ineligible_healthy_target_and_foreign_owner_never_touch_store() {
    let (memory, mut job, port) = job_fixture();
    job.sweep.run = Some(port.target.key.run);
    port.eligible.store(false, Ordering::SeqCst);
    assert!(!job.execute().unwrap().2);
    let binding = port.target.binding.clone();
    assert!(
        memory
            .with_store(move |store| store.check_native_effect(&binding).is_ok())
            .unwrap()
    );
    assert!(port.confirmations.lock().unwrap().is_empty());
    assert_eq!(port.calls.lock().unwrap().as_slice(), ["discover", "check"]);
    job.sweep.owner = Some(owner());
    assert!(job.execute().is_err());
    assert_eq!(port.calls.lock().unwrap().len(), 2);
}
