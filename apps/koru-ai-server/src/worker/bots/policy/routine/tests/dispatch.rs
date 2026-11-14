// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::worker::bots::policy::dispatch::RoutineDispatch;
use std::sync::atomic::{AtomicBool, Ordering};

struct Human {
    owner: BotOwner,
    home: WorkspaceId,
    profile: ProfileId,
    moved: Arc<AtomicBool>,
    inspections: AtomicUsize,
}
impl BotAuthority for Human {
    fn owner(&self) -> Result<BotOwner, BotError> {
        Ok(self.owner)
    }
    fn workspace(&self, home: WorkspaceId) -> Result<ProfileId, BotError> {
        if home != self.home {
            return Err(BotError::Forbidden);
        }
        Ok(if self.moved.load(Ordering::SeqCst) {
            ProfileId::new()
        } else {
            self.profile
        })
    }
    fn page(&self, p: &BotPage) -> Result<(), BotError> {
        self.workspace(p.workspace).map(|_| ())
    }
    fn inspect(
        &self,
        homes: &[WorkspaceId],
        pages: &[BotPage],
    ) -> Result<BotAuthoritySnapshot, BotError> {
        self.inspections.fetch_add(1, Ordering::SeqCst);
        Ok(BotAuthoritySnapshot {
            owner: self.owner,
            workspaces: homes.iter().map(|w| (*w, self.workspace(*w))).collect(),
            pages: pages.iter().map(|p| (p.clone(), self.page(p))).collect(),
        })
    }
}
struct SyncPort {
    claim: RoutineClaimRecord,
    calls: Mutex<Vec<&'static str>>,
    mode: u8,
    moved: Arc<AtomicBool>,
}
impl BotDelegatePort for SyncPort {
    fn execute(&self, _: DelegateCommand) -> ApiResult<BotReply> {
        panic!("finite owner calls only")
    }
    fn execute_within(&self, command: DelegateCommand, within: Duration) -> ApiResult<BotReply> {
        assert!(!within.is_zero() && within <= Duration::from_secs(5));
        Ok(match command {
            DelegateCommand::ReadRoutineClaim { slot } => {
                self.calls.lock().unwrap().push("read");
                assert_eq!(slot, self.claim.claim.slot);
                let mut record = self.claim.clone();
                if self.mode == 1 {
                    record.link_state = RoutineLinkState::Held {};
                }
                BotReply::RoutineClaim {
                    observation: RoutineClaimObservation::Claimed { record },
                }
            }
            DelegateCommand::SynchronizePolicy { mut barrier } => {
                self.calls.lock().unwrap().push("global");
                if self.mode == 2 {
                    barrier.config_revision += 1;
                }
                BotReply::PolicyBarrier {
                    receipt: koru_delegate_core::BotPolicyBarrierReceipt {
                        barrier,
                        state: if self.mode == 3 {
                            BotPolicyBarrierState::Pending
                        } else {
                            BotPolicyBarrierState::Effective
                        },
                    },
                }
            }
            DelegateCommand::SynchronizeRoutinePolicy { mut barrier } => {
                self.calls.lock().unwrap().push("routine");
                if self.mode == 4 {
                    barrier.schedule = koru_delegate_core::ScheduleId::mint();
                }
                if self.mode == 5 {
                    self.moved.store(true, Ordering::SeqCst);
                }
                BotReply::RoutinePolicyBarrier {
                    barrier,
                    state: BotPolicyBarrierState::Effective,
                }
            }
            _ => panic!("unexpected command"),
        })
    }
}
fn ports(f: &Fixture, mode: u8) -> (Human, SyncPort) {
    let moved = Arc::new(AtomicBool::new(false));
    (
        Human {
            owner: owner(&f.bot),
            home: f.bot.home_workspace,
            profile: f.profile,
            moved: moved.clone(),
            inspections: AtomicUsize::new(0),
        },
        SyncPort {
            claim: f.claim.clone(),
            calls: Mutex::new(Vec::new()),
            mode,
            moved,
        },
    )
}

pub(in crate::worker::bots) fn dispatch_job_fixture() -> (
    Repository,
    koru_delegate_core::BotSubmission,
    Arc<dyn BotAuthority>,
    Arc<dyn BotDelegatePort>,
) {
    let f = Fixture::new(true);
    let (human, port) = ports(&f, 0);
    (
        f.repository,
        f.claim.submission,
        Arc::new(human),
        Arc::new(port),
    )
}

#[test]
fn routine_dispatch_sync_reuses_full_link_global_then_routine_and_writer_recheck() {
    let f = Fixture::new(true);
    let proof = RoutineDispatch::snapshot(&f.repository, &f.claim.submission).unwrap();
    let (human, port) = ports(&f, 0);
    proof
        .synchronize(&human, &port, Instant::now() + Duration::from_secs(5))
        .unwrap();
    proof.recheck(&f.repository).unwrap();
    assert_eq!(*port.calls.lock().unwrap(), ["read", "global", "routine"]);
    assert_eq!(human.inspections.load(Ordering::SeqCst), 2);
}

#[test]
fn routine_dispatch_wrong_profile_before_or_after_sync_refuses() {
    for after in [false, true] {
        let f = Fixture::new(true);
        let proof = RoutineDispatch::snapshot(&f.repository, &f.claim.submission).unwrap();
        let (human, port) = ports(&f, if after { 5 } else { 0 });
        if !after {
            human.moved.store(true, Ordering::SeqCst);
        }
        assert!(
            proof
                .synchronize(&human, &port, Instant::now() + Duration::from_secs(5))
                .is_err()
        );
        assert_eq!(port.calls.lock().unwrap().len(), if after { 3 } else { 0 });
    }
}

#[test]
fn routine_dispatch_held_pending_and_mismatched_sync_never_become_readiness() {
    for mode in 1..=4 {
        let f = Fixture::new(true);
        let proof = RoutineDispatch::snapshot(&f.repository, &f.claim.submission).unwrap();
        let (human, port) = ports(&f, mode);
        assert!(
            proof
                .synchronize(&human, &port, Instant::now() + Duration::from_secs(5))
                .is_err()
        );
        assert_eq!(
            port.calls.lock().unwrap().len(),
            if mode == 1 {
                1
            } else if mode < 4 {
                2
            } else {
                3
            }
        );
    }
}

#[test]
fn routine_dispatch_full_submission_and_expired_deadline_fail_before_owner_work() {
    let f = Fixture::new(true);
    let mut changed = f.claim.submission.clone();
    changed.input.text.push_str(" changed");
    assert!(RoutineDispatch::snapshot(&f.repository, &changed).is_err());
    let proof = RoutineDispatch::snapshot(&f.repository, &f.claim.submission).unwrap();
    let (human, port) = ports(&f, 0);
    assert!(proof.synchronize(&human, &port, Instant::now()).is_err());
    assert!(port.calls.lock().unwrap().is_empty());
    assert_eq!(human.inspections.load(Ordering::SeqCst), 0);
}

#[test]
fn routine_dispatch_writer_change_refuses_and_pause_does_not_change_policy_epoch() {
    let mut f = Fixture::new(true);
    let proof = RoutineDispatch::snapshot(&f.repository, &f.claim.submission).unwrap();
    f.bot.revision += 1;
    f.bot.config.name = "Changed after synchronization".into();
    f.repository
        .save_bot(
            &f.bot,
            f.profile,
            BotRequestId::mint(),
            &"b".repeat(64),
            Some(1),
        )
        .unwrap();
    assert!(proof.recheck(&f.repository).is_err());
    let before = barrier(&f.bot).unwrap();
    f.bot.paused = true;
    assert_eq!(barrier(&f.bot).unwrap(), before);
}
