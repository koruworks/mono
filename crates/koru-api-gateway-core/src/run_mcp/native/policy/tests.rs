// SPDX-License-Identifier: MPL-2.0
//! Composite checks and exact private acknowledgements, without execution.
use super::*;
use koru_store_core::UserScopeId;
use std::sync::Mutex;
use uuid::Uuid;

fn fixture() -> (BotProvenance, CapturedBotPolicy, FreshBotPolicyProof) {
    let workspace = WorkspaceId::new();
    let owner = BotOwner {
        user_scope: UserScopeId::new(),
        device: DeviceId::new(),
    };
    let bot = BotId::mint();
    let provenance = BotProvenance {
        owner,
        bot,
        submission: BotRequestId::mint(),
        chat_id: Uuid::new_v4(),
        ai_attempt_id: Some(Uuid::new_v4()),
        assistant_message_id: Some(Uuid::new_v4()),
        config_revision: 3,
        policy_revision: 2,
        continued_from: None,
        routine: Some(RoutineProvenance {
            schedule: ScheduleId::mint(),
            occurrence_ms: 60_000,
            definition: RoutineDefinitionIdentity {
                revision: 2,
                digest: RoutineDigest32::from_bytes([4; 32]),
                owner_handoff: None,
            },
            grant_revision: 1,
        }),
    };
    let captured = CapturedBotPolicy {
        workspace,
        destination: workspace,
        access: BotAccess::Default,
        selected_pages: Default::default(),
    };
    let proof = FreshBotPolicyProof {
        global: BotPolicyBarrier {
            key: BotPolicyBarrierKey {
                owner,
                bot,
                request: Uuid::new_v4().try_into().unwrap(),
            },
            config_revision: 4,
            policy_revision: 3,
            policy: captured.clone(),
            effects_allowed: true,
        },
        routine: Some(RoutinePolicyBarrier {
            owner,
            bot,
            schedule: provenance.routine.unwrap().schedule,
            request: Uuid::new_v4().try_into().unwrap(),
            grant_revision: 1,
            policy: captured.clone(),
            effects_allowed: true,
        }),
    };
    (provenance, captured, proof)
}

#[derive(Clone, Copy)]
enum Answer {
    Exact,
    PendingGlobal,
    PendingRoutine,
    AlterGlobal,
    AlterRoutine,
    Wrong,
    Lost,
}
struct Owner {
    answer: Answer,
    calls: Mutex<Vec<&'static str>>,
}
impl Owner {
    fn new(answer: Answer) -> Self {
        Self {
            answer,
            calls: Mutex::new(Vec::new()),
        }
    }
    fn synchronize(&self, proof: &FreshBotPolicyProof) -> Result<(), ToolFailure> {
        synchronize(proof, |command| {
            self.execute(command)
                .map_err(|_| dispatch::failure("native_owner_unavailable"))
        })
    }
}
impl BotDelegatePort for Owner {
    fn execute(&self, command: BotCommand) -> Result<BotReply, koru_delegate_api::Failure> {
        let mut calls = self.calls.lock().unwrap();
        match command {
            BotCommand::SynchronizePolicy { mut barrier } => {
                calls.push("global");
                match self.answer {
                    Answer::Wrong => return Ok(BotReply::Missing),
                    Answer::Lost => {
                        return Err(koru_delegate_api::Failure::new("unknown", "lost", true));
                    }
                    Answer::AlterGlobal => barrier.policy.selected_pages.insert(DocumentId::new()),
                    _ => false,
                };
                Ok(BotReply::PolicyBarrier {
                    receipt: BotPolicyBarrierReceipt {
                        barrier,
                        state: if matches!(self.answer, Answer::PendingGlobal) {
                            BotPolicyBarrierState::Pending
                        } else {
                            BotPolicyBarrierState::Effective
                        },
                    },
                })
            }
            BotCommand::SynchronizeRoutinePolicy { mut barrier } => {
                calls.push("routine");
                if matches!(self.answer, Answer::AlterRoutine) {
                    barrier.request = Uuid::new_v4().try_into().unwrap();
                }
                Ok(BotReply::RoutinePolicyBarrier {
                    barrier,
                    state: if matches!(self.answer, Answer::PendingRoutine) {
                        BotPolicyBarrierState::Pending
                    } else {
                        BotPolicyBarrierState::Effective
                    },
                })
            }
            _ => panic!("policy synchronization must not dispatch or acquire effects"),
        }
    }
}

#[test]
fn composite_policy_requires_both_subjects_and_preserves_ordinary_shape() {
    let (mut p, c, mut proof) = fixture();
    assert!(matches(&proof, &p, &c, c.workspace));
    let routine = proof.routine.take();
    assert!(!matches(&proof, &p, &c, c.workspace));
    p.routine = None;
    assert!(matches(&proof, &p, &c, c.workspace));
    proof.routine = routine;
    assert!(!matches(&proof, &p, &c, c.workspace));
}

#[test]
fn composite_policy_rejects_foreign_subject_stale_revisions_and_wrong_home() {
    let (p, c, proof) = fixture();
    for change in 0..9 {
        let mut other = proof.clone();
        match change {
            0 => other.global.key.owner.device = DeviceId::new(),
            1 => other.global.key.bot = BotId::mint(),
            2 => other.global.config_revision = p.config_revision - 1,
            3 => other.global.policy_revision = p.policy_revision - 1,
            4 => other.global.policy.workspace = WorkspaceId::new(),
            5 => other.routine.as_mut().unwrap().schedule = ScheduleId::mint(),
            6 => other.routine.as_mut().unwrap().owner.user_scope = UserScopeId::new(),
            7 => other.routine.as_mut().unwrap().grant_revision = 0,
            _ => other.routine.as_mut().unwrap().policy.destination = WorkspaceId::new(),
        }
        assert!(!matches(&other, &p, &c, c.workspace), "composite {change}");
    }
}

#[test]
fn composite_policy_historical_identity_read_does_not_require_effects_permission() {
    let (p, c, mut proof) = fixture();
    for global in [false, true] {
        proof.global.effects_allowed = global;
        proof.routine.as_mut().unwrap().effects_allowed = !global;
        assert!(matches(&proof, &p, &c, c.workspace));
        let owner = Owner::new(Answer::Exact);
        assert!(owner.synchronize(&proof).is_err());
        assert!(owner.calls.lock().unwrap().is_empty());
    }
}

#[test]
fn composite_policy_synchronizes_global_then_routine_with_exact_effective_echoes() {
    let (_, _, mut proof) = fixture();
    let owner = Owner::new(Answer::Exact);
    owner.synchronize(&proof).unwrap();
    assert_eq!(*owner.calls.lock().unwrap(), ["global", "routine"]);
    proof.routine = None;
    let ordinary = Owner::new(Answer::Exact);
    ordinary.synchronize(&proof).unwrap();
    assert_eq!(*ordinary.calls.lock().unwrap(), ["global"]);
}

#[test]
fn composite_policy_pending_changed_or_unknown_global_never_installs_routine() {
    let (_, _, proof) = fixture();
    for answer in [
        Answer::PendingGlobal,
        Answer::AlterGlobal,
        Answer::Wrong,
        Answer::Lost,
    ] {
        let owner = Owner::new(answer);
        assert_eq!(
            owner.synchronize(&proof).unwrap_err().code,
            if matches!(answer, Answer::Lost) {
                "native_owner_unavailable"
            } else {
                "native_policy_pending"
            }
        );
        assert_eq!(*owner.calls.lock().unwrap(), ["global"]);
    }
}

#[test]
fn composite_policy_pending_or_changed_routine_never_yields_fresh_permission() {
    let (_, _, proof) = fixture();
    for answer in [Answer::PendingRoutine, Answer::AlterRoutine] {
        let owner = Owner::new(answer);
        assert_eq!(
            owner.synchronize(&proof).unwrap_err().code,
            "native_policy_pending"
        );
        assert_eq!(*owner.calls.lock().unwrap(), ["global", "routine"]);
    }
}
