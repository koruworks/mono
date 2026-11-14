// SPDX-License-Identifier: MPL-2.0
//! Real actor/cache and in-memory SQL; no Store, scope filesystem or provider.
use super::*;
use koru_delegate_core::*;

pub(crate) struct Fixture {
    pub actor: Actor,
    pub prepared: RoutinePrepared,
    pub admission: BotAdmissionBarrier,
    pub proof: FreshBotPolicyProof,
    pub scope: ScopeIdentity,
    inbox: mpsc::Receiver<Command>,
}
impl Fixture {
    pub fn new() -> Self {
        let (mut actor, _, inbox) = actor();
        let mut s = bot_submission();
        s.input.text = s.request.objective.as_str().to_owned();
        let slot = RoutineSlot {
            owner: s.provenance.owner,
            bot: s.provenance.bot,
            schedule: ScheduleId::mint(),
            occurrence_ms: 900,
        };
        let definition = RoutineDefinitionIdentity {
            revision: 1,
            digest: RoutineDigest32::from_bytes([1; 32]),
            owner_handoff: None,
        };
        let ids = RoutineExecutionIds {
            submission: s.provenance.submission,
            run: s.request.run,
            chat: s.provenance.chat_id,
            attempt: uuid::Uuid::new_v4(),
            user_message: uuid::Uuid::new_v4(),
            assistant_message: uuid::Uuid::new_v4(),
        };
        s.provenance.ai_attempt_id = Some(ids.attempt);
        s.provenance.assistant_message_id = Some(ids.assistant_message);
        s.provenance.routine = Some(RoutineProvenance {
            schedule: slot.schedule,
            occurrence_ms: slot.occurrence_ms,
            definition,
            grant_revision: 1,
        });
        s.request.origin = RunOrigin::Scheduled {
            schedule: slot.schedule,
            occurrence_ms: slot.occurrence_ms,
            revision: 1,
        };
        let admission = BotAdmissionBarrier {
            owner: slot.owner,
            bot: slot.bot,
            request: BotRequestId::mint(),
            epoch: 1,
            paused: false,
        };
        let proof = FreshBotPolicyProof {
            global: BotPolicyBarrier {
                key: BotPolicyBarrierKey {
                    owner: slot.owner,
                    bot: slot.bot,
                    request: BotRequestId::mint().into(),
                },
                config_revision: 1,
                policy_revision: 1,
                policy: s.policy.clone(),
                effects_allowed: true,
            },
            routine: Some(RoutinePolicyBarrier {
                owner: slot.owner,
                bot: slot.bot,
                schedule: slot.schedule,
                request: BotRequestId::mint().into(),
                grant_revision: 1,
                policy: s.policy.clone(),
                effects_allowed: true,
            }),
        };
        let prepared = RoutinePrepared {
            slot,
            definition,
            ids,
            semantic_hash: RoutineDigest32::from_bytes([2; 32]),
            preparation_hash: RoutineDigest32::from_bytes([3; 32]),
            submission_version: 2,
            submission: s,
        };
        assert!(prepared.valid());
        actor.device = slot.owner.device;
        actor
            .orchestrator
            .ports_mut()
            .store
            .bots()
            .unwrap()
            .register_routine_route(&RoutineRouteIdentity {
                owner: slot.owner,
                bot: slot.bot,
                schedule: slot.schedule,
                workspace: prepared.submission.workspace,
                creation_request: BotRequestId::mint(),
                seed_hash: RoutineDigest32::from_bytes([4; 32]),
                created_at_ms: 0,
            })
            .unwrap();
        actor
            .orchestrator
            .bot_barrier(&admission, actor.device)
            .unwrap();
        actor
            .orchestrator
            .synchronize_native_policy(proof.global.clone(), actor.device)
            .unwrap();
        actor
            .orchestrator
            .synchronize_routine_policy(proof.routine.clone().unwrap(), actor.device)
            .unwrap();
        Self {
            actor,
            prepared,
            admission,
            proof,
            scope: ScopeIdentity::fixture_canonical(std::path::Path::new("C:/routine-fixture")),
            inbox,
        }
    }
    pub fn handle(&mut self, command: Command) {
        self.actor.handle(command);
    }
    pub fn held(&self) -> bool {
        self.actor.schedule_admission == Some(ScheduleLease::Routine(self.prepared.ids.run))
    }
    pub fn command(
        &self,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
        reply: mpsc::SyncSender<PortResult<ScheduleAdmission>>,
    ) -> Command {
        Command::RoutineBegin {
            prepared: Box::new(self.prepared.clone()),
            admission: self.admission.clone(),
            proof: self.proof.clone(),
            scope: self.scope.clone(),
            authored_scope: self.prepared.submission.request.scope.clone(),
            deadline,
            cancelled,
            reply,
        }
    }
    fn begin(&mut self) -> PortResult<ScheduleAdmission> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.handle(self.command(
            Instant::now() + Duration::from_secs(2),
            Arc::new(AtomicBool::new(false)),
            reply,
        ));
        answer.recv().unwrap()
    }
}

#[test]
fn routine_actor_grants_only_current_exact_subject_and_does_not_create_run() {
    let mut f = Fixture::new();
    for change in 0..4 {
        let old = f.admission.clone();
        let proof = f.proof.clone();
        let device = f.actor.device;
        match change {
            0 => f.admission.request = BotRequestId::mint(),
            1 => f.proof.global.policy_revision += 1,
            2 => f.actor.device = DeviceId::new(),
            _ => f.proof.routine.as_mut().unwrap().grant_revision += 1,
        }
        assert!(f.begin().is_err());
        assert!(!f.held());
        f.admission = old;
        f.proof = proof;
        f.actor.device = device;
    }
    assert_eq!(f.begin().unwrap(), ScheduleAdmission::Granted);
    assert!(f.held());
    assert!(
        f.actor
            .orchestrator
            .ports_mut()
            .store
            .bots()
            .unwrap()
            .bot_admission(f.prepared.ids.run)
            .unwrap()
            .is_none()
    );
    assert!(f.actor.release_routine(f.prepared.ids.run).is_ok());
    assert!(f.actor.release_routine(f.prepared.ids.run).is_ok());
}

#[test]
fn routine_actor_expired_cancelled_and_failed_delivery_never_retain_grant() {
    for condition in 0..3 {
        let mut f = Fixture::new();
        let (reply, answer) = mpsc::sync_channel(1);
        let deadline = if condition == 0 {
            Instant::now()
        } else {
            Instant::now() + Duration::from_secs(2)
        };
        let cancelled = Arc::new(AtomicBool::new(condition == 1));
        if condition == 2 {
            drop(answer);
            f.handle(f.command(deadline, cancelled, reply));
        } else {
            f.handle(f.command(deadline, cancelled, reply));
            assert!(answer.recv().unwrap().is_err());
        }
        assert!(!f.held());
    }
}

#[test]
fn routine_actor_foreign_scope_busy_is_only_arbitration_without_skip_or_lease() {
    let mut f = Fixture::new();
    let holder = RunId::mint();
    f.actor
        .claimed_schedule_retries
        .push(ScheduledScopeReservation {
            run: holder,
            scope: WorkspaceScope::Temporary,
            identity: Some(f.scope.clone()),
            resolution: ScopeReservationResolution::Exact,
        });
    assert_eq!(f.begin().unwrap(), ScheduleAdmission::Busy(holder));
    assert!(!f.held());
    assert!(matches!(
        f.actor
            .orchestrator
            .read_routine_claim(&f.prepared.slot, f.actor.device)
            .unwrap(),
        RoutineClaimObservation::Absent { .. }
    ));
}

#[test]
fn routine_actor_wrong_phase_cannot_finish_abort_reconcile_or_release() {
    let mut f = Fixture::new();
    let run = f.prepared.ids.run;
    assert_eq!(f.begin().unwrap(), ScheduleAdmission::Granted);
    let claim = koru_delegate_sqlite::SlotClaim {
        schedule: f.prepared.slot.schedule,
        occurrence_ms: 900,
        revision: 1,
        run,
        state: koru_delegate_sqlite::SlotState::Claimed,
        busy_holder: None,
        claimed_at_ms: 1000,
    };
    let (reply, answer) = mpsc::sync_channel(1);
    f.handle(Command::ScheduleReconcile {
        claim: claim.clone(),
        authored_scope: f.prepared.submission.request.scope.clone(),
        reply,
    });
    assert!(answer.recv().unwrap().is_err());
    assert!(f.held());
    let (reply, answer) = mpsc::sync_channel(1);
    f.handle(Command::ScheduleFinish {
        run,
        claim,
        scope: koru_delegate_scope::ResolvedScope {
            identity: f.scope.clone(),
            directory: PathBuf::from("C:/routine-fixture"),
        },
        authored_scope: f.prepared.submission.request.scope.clone(),
        reply,
    });
    assert!(answer.recv().unwrap().is_err());
    f.handle(Command::ScheduleAbort { run });
    assert!(f.held());
    assert!(f.actor.release_routine(RunId::mint()).is_err());
    assert!(f.held());
    f.actor.release_routine(run).unwrap();
    f.actor.schedule_admission = Some(ScheduleLease::Ordinary(run));
    assert!(f.actor.release_routine(run).is_err());
    assert_eq!(
        f.actor.schedule_admission,
        Some(ScheduleLease::Ordinary(run))
    );
}

#[test]
fn routine_actor_release_drains_manual_then_restart_then_wakes_bots() {
    let mut f = Fixture::new();
    assert_eq!(f.begin().unwrap(), ScheduleAdmission::Granted);
    let (reply, answer) = mpsc::sync_channel(1);
    f.handle(Command::Resume {
        run: RunId::mint(),
        confirm: true,
        reply,
    });
    assert_eq!(f.actor.deferred_starts.len(), 1);
    let restart = RunId::mint();
    f.actor.deferred_restart_starts.push_back(restart);
    let mut attempts = 0;
    f.actor
        .release_routine_with(f.prepared.ids.run, |actor, run| {
            assert_eq!(run, restart);
            assert!(answer.try_recv().is_ok());
            assert!(actor.deferred_starts.is_empty());
            assert!(!actor.bot_drain_pending);
            attempts += 1;
            DeferredStartDisposition::Complete
        })
        .unwrap();
    assert_eq!(attempts, 1);
    assert!(f.actor.deferred_restart_starts.is_empty());
    assert!(f.actor.bot_drain_pending);
    assert!(matches!(f.inbox.try_recv().unwrap(), Command::BotDrain));
}

#[test]
fn routine_actor_scheduler_stopped_ack_clears_unknown_routine_lease() {
    let (mut actor, commands, inbox) = actor();
    actor.schedule_admission = Some(ScheduleLease::Routine(RunId::mint()));
    actor.attach_scheduler(crate::scheduler::test_control(commands));
    let (stop, stopped) = mpsc::channel();
    stop.send("routine test stop").unwrap();
    actor.run(inbox, stopped);
    assert_eq!(actor.schedule_admission, None);
}

#[test]
fn routine_actor_rechecks_cancellation_after_refreshed_scope_arbitration() {
    struct CancelRoots(Arc<AtomicBool>);
    impl koru_delegate_scope::ScopeRoots for CancelRoots {
        fn project_root(&self, _: ProjectId) -> koru_delegate_scope::RootLookup {
            koru_delegate_scope::RootLookup::UnknownProject
        }
        fn temporary_directory(&self, _: RunId) -> Option<PathBuf> {
            None
        }
        fn bot_folder(
            &self,
            _: &BotWorkingFolder,
        ) -> Result<koru_delegate_scope::ResolvedScope, koru_delegate_scope::ScopeRefusal> {
            self.0.store(true, Ordering::Release);
            Ok(koru_delegate_scope::ResolvedScope {
                identity: ScopeIdentity::fixture_canonical(std::path::Path::new(
                    "C:/disjoint-fixture",
                )),
                directory: PathBuf::from("C:/disjoint-fixture"),
            })
        }
    }
    let mut f = Fixture::new();
    let cancelled = Arc::new(AtomicBool::new(false));
    f.actor.orchestrator.ports_mut().roots = Box::new(CancelRoots(cancelled.clone()));
    f.actor
        .claimed_schedule_retries
        .push(ScheduledScopeReservation {
            run: RunId::mint(),
            scope: WorkspaceScope::BotFolder {
                folder: BotWorkingFolder::new("C:/disjoint-fixture".into()).unwrap(),
            },
            identity: None,
            resolution: ScopeReservationResolution::Refresh,
        });
    let (reply, answer) = mpsc::sync_channel(1);
    f.handle(f.command(
        Instant::now() + Duration::from_secs(2),
        cancelled.clone(),
        reply,
    ));
    assert!(cancelled.load(Ordering::Acquire));
    assert!(answer.recv().unwrap().is_err());
    assert!(!f.held());
}
