// SPDX-License-Identifier: MPL-2.0

//! Partial owner qualification: private transaction bodies, real in-memory
//! ledger/admission/policy owners. C3 publication composition remains absent.
use super::*;
use crate::{BotLedger, NativeResultLedger, WriteOutcome, fixtures};
use write::*;

mod answers;
mod corruption;
mod enrollment;
mod native_completion;
mod observation;
mod occupancy;
mod lifecycle;
mod preparation;
mod projection;
mod resume;
mod consume;
mod completion;

struct Harness {
    db: RunsDb,
    binding: DecisionBindingV1,
    proof: FreshBotPolicyProof,
    boot: DelegateBootId,
}
impl Harness {
    fn new() -> Self {
        let (db, fence, global) = crate::native_results::tests::decision_base();
        Self::subject(db,fence,global)
    }
    fn with_db(db:RunsDb)->Self{
        let (db,fence,global)=crate::native_results::tests::decision_base_in(db,RunId::mint());
        Self::subject(db,fence,global)
    }
    fn subject(db:RunsDb,fence:BotExecutionFence,global:BotPolicyBarrier)->Self{
        let admission = db.bot_admission(fence.run).unwrap().unwrap();
        let ActorRef::Person { profile } = admission.submission.request.requested_by else {
            panic!("fixture person")
        };
        let binding = DecisionBindingV1::KoruEdit(DecisionEditBinding {
            version: DecisionVersionV1,
            owner: fence.owner,
            bot: fence.bot,
            run: fence.run,
            origin_attempt: fence.attempt,
            origin_generation: fence.generation,
            origin_session: fence.session,
            origin_turn: fence.turn.unwrap(),
            submission: admission.submission.provenance.submission,
            routine: None,
            human_profile: profile,
            workspace: admission.submission.workspace,
            target: fixtures::document(200),
            action: DecisionEditAction::EditDocument,
            operation_id: uuid::Uuid::new_v4(),
            edit_fingerprint: [3; 32],
            // A real semantic VV, also used by the consume owner's comparison.
            expected_revision: DecisionBytes::new(vec![2,1,2,2,4]).unwrap(),
            payload_version: DecisionVersionV1,
            payload: DecisionBytes::new(b"PRIVATE-EDIT".to_vec()).unwrap(),
            captured_policy: admission.submission.policy,
            global_policy: DecisionGlobalPolicy {
                key: global.key.clone(),
                config_revision: rev(global.config_revision),
                policy_revision: rev(global.policy_revision),
                digest: DecisionDigest::from_bytes(
                    crate::native_results::policy_digest(&global).unwrap(),
                ),
            },
            routine_policy: None,
        });
        Self {
            db,
            binding,
            proof: FreshBotPolicyProof {
                global,
                routine: None,
            },
            boot: DelegateBootId::mint(),
        }
    }
    fn origin(&self) -> DecisionFence {
        self.binding.origin()
    }
    fn edit(&mut self) -> &mut DecisionEditBinding {
        let DecisionBindingV1::KoruEdit(v) = &mut self.binding else {
            panic!("fixture edit")
        };
        v
    }
    fn prepare_at(&mut self, now: i64) -> Result<DecisionMutation, BotLedgerError> {
        let tx = self.db.conn().unchecked_transaction().unwrap();
        let result = prepare_on(&tx, &self.binding, &self.proof, self.boot, now);
        match result {
            Ok(value) => {
                tx.commit().unwrap();
                Ok(value)
            }
            Err(e) => Err(e),
        }
    }
    fn prepare(&mut self) -> DecisionRecord {
        self.prepare_at(1000).unwrap().record
    }
    fn read(&self, id: DecisionId) -> Result<DecisionRecord, BotLedgerError> {
        let f = self.origin();
        self.db.bot_decision(f.owner, f.run, id).map(|r| r.unwrap())
    }
    fn native(&mut self) {
        let DecisionBindingV1::KoruEdit(v) = self.binding.clone() else {
            panic!("fixture edit")
        };
        self.binding = DecisionBindingV1::NativeApproval(DecisionNativeBinding {
            version: v.version,
            owner: v.owner,
            bot: v.bot,
            run: v.run,
            origin_attempt: v.origin_attempt,
            origin_generation: v.origin_generation,
            origin_session: v.origin_session,
            origin_turn: v.origin_turn,
            submission: v.submission,
            routine: v.routine,
            human_profile: v.human_profile,
            workspace: v.workspace,
            prompt: ApprovalId::mint(),
            choices: DecisionList::new(vec![
                DecisionNativeChoice {
                    id: ApprovalChoiceId::mint(),
                    intent: serde_json::from_str("\"allow\"").unwrap(),
                },
                DecisionNativeChoice {
                    id: ApprovalChoiceId::mint(),
                    intent: serde_json::from_str("\"reject\"").unwrap(),
                },
            ])
            .unwrap(),
            native_deadline_at_ms: None,
            captured_policy: v.captured_policy,
            global_policy: v.global_policy,
            routine_policy: v.routine_policy,
        });
    }
    fn routine(&mut self, linked: bool) {
        let f = self.origin();
        let mut s = self.db.bot_admission(f.run).unwrap().unwrap().submission;
        let route = crate::bots::routines::tests::convert(&mut self.db, &mut s, linked);
        let retained = crate::bots::routines::validate_admission(
            self.db.conn(),
            &self.db.bot_admission(f.run).unwrap().unwrap(),
        )
        .unwrap();
        let policy = RoutinePolicyBarrier {
            owner: f.owner,
            bot: f.bot,
            schedule: route.schedule,
            request: BotRequestId::mint().into(),
            grant_revision: 1,
            policy: s.policy.clone(),
            effects_allowed: true,
        };
        self.db.install_routine_policy(&policy).unwrap();
        let subject = DecisionRoutineSubject {
            provenance: s.provenance.routine.unwrap(),
            claim: retained.claim,
            route,
        };
        let rp = DecisionRoutinePolicy {
            owner: f.owner,
            bot: f.bot,
            schedule: policy.schedule,
            request: policy.request,
            grant_revision: rev(1),
            digest: policy.digest().unwrap(),
        };
        match &mut self.binding {
            DecisionBindingV1::KoruEdit(v) => {
                v.routine = Some(subject);
                v.routine_policy = Some(rp);
            }
            DecisionBindingV1::NativeApproval(v) => {
                v.routine = Some(subject);
                v.routine_policy = Some(rp);
            }
        }
        self.proof.routine = Some(policy);
    }
    fn revoke(&mut self) {
        let mut policy = self.proof.global.clone();
        policy.key.request = BotRequestId::mint().into();
        policy.config_revision += 1;
        policy.policy_revision += 1;
        policy.effects_allowed = false;
        self.db.install_native_policy(&policy).unwrap();
        self.proof.global = policy;
    }
    fn count(&self, table: &str) -> i64 {
        self.db
            .conn()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    /// C3 publication and C2 retirement are deliberately not simulated as
    /// production success. Handwritten rows isolate C1's readiness predicates.
    fn published(&self, id: DecisionId, native: bool) {
        self.db
            .conn()
            .execute(
                "UPDATE bot_decisions SET phase=?2,revision=2 WHERE decision_id=?1",
                params![
                    id.to_storage(),
                    if native { "native_wait" } else { "holding" }
                ],
            )
            .unwrap();
    }
    fn answer_input(&self, record: &DecisionRecord, allow: bool) -> AnswerDecision {
        let (human, choice) = match &self.binding {
            DecisionBindingV1::KoruEdit(v) => (
                v.human_profile,
                if allow {
                    DecisionAnswerChoice::AllowOnce {}
                } else {
                    DecisionAnswerChoice::Deny {}
                },
            ),
            DecisionBindingV1::NativeApproval(v) => (
                v.human_profile,
                DecisionAnswerChoice::Native {
                    offered: v.choices.as_slice()[usize::from(!allow)].clone(),
                },
            ),
        };
        AnswerDecision {
            owner: self.origin().owner,
            run: self.origin().run,
            decision: record.receipt.decision,
            expected_revision: record.receipt.state.revision,
            binding_digest: record.receipt.binding_digest,
            request: DecisionAnswerId::mint(),
            human,
            choice,
            authority_digest: DecisionDigest::from_bytes([9; 32]),
        }
    }
    fn answer(
        &mut self,
        input: &AnswerDecision,
        now: i64,
    ) -> Result<DecisionMutation, BotLedgerError> {
        let tx = self.db.conn().unchecked_transaction().unwrap();
        let out = answer_on(&tx, input, &self.proof, self.boot, now);
        match out {
            Ok(value) => {
                tx.commit().unwrap();
                Ok(value)
            }
            Err(e) => Err(e),
        }
    }
}
fn rev(n: u64) -> DecisionRevision {
    DecisionRevision::new(n).unwrap()
}
