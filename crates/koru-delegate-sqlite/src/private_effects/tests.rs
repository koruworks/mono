// SPDX-License-Identifier: MPL-2.0

//! The effect table's own cohorts, on in-memory ledgers with a fake clock:
//! `m5_private_effect_stages`, `m5_private_dispatcher_resolution`,
//! `m5_private_revocation_fence` and the granted half of
//! `m5_private_operation_ids`. The decision half is in
//! `decisions/tests/private.rs`.
use super::*;
use crate::{BotLedger, NativeResultLedger, fixtures};
use std::sync::{Arc, atomic::AtomicI64};
use uuid::Uuid;

/// A valid read of one fixed fixture item under `operation`.
pub(crate) fn proposal(operation: Uuid) -> PrivateOperationProposal {
    let connection = ConnectionId::from_uuid(fixtures::v4(0xc1));
    let item = ConnectionItemId::from_uuid(fixtures::v4(0xc2));
    let item_ref = DecisionDigest::from_bytes([0xc3; 32]);
    PrivateOperationProposal {
        operation_id: operation,
        connection,
        connection_revision: DecisionRevision::new(3).unwrap(),
        account: DecisionDigest::from_bytes([0xc4; 32]),
        item,
        item_ref,
        operation: PrivateOperationKind::ReadItem,
        request_fingerprint: private_request_fingerprint(
            PrivateOperationKind::ReadItem,
            connection,
            item,
            item_ref,
        ),
    }
}

/// Put the grant in the Run's captured policy, as an admission made while the
/// Bot held it would have. A fixture edit of the stored admission, the same
/// way the decision fixtures add its AI ids.
pub(crate) fn capture_grant(db: &RunsDb, run: RunId, grant: ConnectionGrantRef) {
    let mut submission = db.bot_admission(run).unwrap().unwrap().submission;
    submission.policy.connections.insert(grant);
    db.conn()
        .execute(
            "UPDATE bot_admissions SET payload_json=?1 WHERE run_id=?2",
            params![
                serde_json::to_string(&submission).unwrap(),
                run.to_storage()
            ],
        )
        .unwrap();
}

/// Install the next Bot barrier, optionally holding the grant, and return it
/// as the fresh proof's global half.
pub(crate) fn next_barrier(
    db: &mut RunsDb,
    current: &BotPolicyBarrier,
    grant: Option<ConnectionGrantRef>,
    effects_allowed: bool,
) -> BotPolicyBarrier {
    let mut policy = current.clone();
    policy.key.request = BotRequestId::mint().into();
    policy.config_revision += 1;
    policy.policy_revision += 1;
    policy.effects_allowed = effects_allowed;
    policy.policy.connections = grant.into_iter().collect();
    db.install_native_policy(&policy).unwrap();
    policy
}

/// An interactive Run whose captured and current policy both hold the grant.
struct Granted {
    db: RunsDb,
    fence: BotExecutionFence,
    proof: FreshBotPolicyProof,
    proposal: PrivateOperationProposal,
    dispatcher: PrivateDispatcherId,
}
impl Granted {
    fn new() -> Self {
        let db = RunsDb::in_memory(Box::new(fixtures::FakeClock(Arc::new(AtomicI64::new(
            1000,
        )))));
        Self::in_db(db, fixtures::run_id(1))
    }
    fn in_db(db: RunsDb, run: RunId) -> Self {
        let (mut db, fence, global) = crate::native_results::tests::decision_base_in(db, run);
        let proposal = proposal(Uuid::new_v4());
        capture_grant(&db, fence.run, proposal.grant());
        let global = next_barrier(&mut db, &global, Some(proposal.grant()), true);
        Self {
            db,
            fence,
            proof: FreshBotPolicyProof {
                global,
                routine: None,
            },
            proposal,
            dispatcher: PrivateDispatcherId::mint(),
        }
    }
    fn prepare(&mut self) -> Result<PrivateEffectOutcome, PrivateLedgerError> {
        let reply = self
            .db
            .prepare_private_effect(self.dispatcher, &self.proposal, &self.fence, &self.proof)
            .unwrap();
        assert!(reply.resolved.is_empty());
        reply.outcome
    }
    fn key(&self) -> PrivateEffectKey {
        PrivateEffectKey {
            run: self.fence.run,
            operation_id: self.proposal.operation_id,
            attempt: self.fence.attempt,
            generation: self.fence.generation,
            turn: self.fence.turn.unwrap(),
        }
    }
    fn advance(
        &mut self,
        transition: PrivateEffectTransition,
    ) -> Result<PrivateEffectOutcome, PrivateLedgerError> {
        let key = self.key();
        self.db
            .advance_private_effect(&key, self.dispatcher, &transition)
            .unwrap()
            .outcome
    }
    fn stage(&self) -> PrivateEffectStage {
        self.db
            .private_effect(self.fence.run, self.proposal.operation_id)
            .unwrap()
            .unwrap()
            .stage
    }
    /// A granted read at the wanted open or final stage.
    fn at(stage: PrivateEffectStage) -> Self {
        let mut g = Self::new();
        g.prepare().unwrap();
        let steps: &[PrivateEffectTransition] = match stage {
            PrivateEffectStage::Prepared => &[],
            PrivateEffectStage::Dispatched => &[DISPATCHED],
            PrivateEffectStage::Confirmed => &[DISPATCHED, CONFIRMED],
            PrivateEffectStage::Refused => &[REFUSED],
            PrivateEffectStage::Unknown => &[DISPATCHED, UNKNOWN],
        };
        for step in steps {
            g.advance(*step).unwrap();
        }
        assert_eq!(g.stage(), stage);
        g
    }
}

const DISPATCHED: PrivateEffectTransition = PrivateEffectTransition::Dispatched {};
const CONFIRMED: PrivateEffectTransition = PrivateEffectTransition::Confirmed {
    outcome_bytes: 12,
    withheld: false,
};
const REFUSED: PrivateEffectTransition = PrivateEffectTransition::Refused {
    code: PrivateEffectRefusal::CredentialLocked,
};
const UNKNOWN: PrivateEffectTransition = PrivateEffectTransition::Unknown {};
const STAGES: [PrivateEffectStage; 5] = [
    PrivateEffectStage::Prepared,
    PrivateEffectStage::Dispatched,
    PrivateEffectStage::Confirmed,
    PrivateEffectStage::Refused,
    PrivateEffectStage::Unknown,
];

#[test]
fn m5_private_effect_stages_granted_prepare_executes_once_and_replays_reconcile_only() {
    let mut g = Granted::new();
    let first = g.prepare().unwrap();
    assert_eq!(first.disposition, PrivateEffectDisposition::Execute);
    assert!(!first.deliver);
    let record = &first.record;
    assert_eq!(record.stage, PrivateEffectStage::Prepared);
    assert_eq!(record.dispatcher, g.dispatcher);
    assert_eq!(record.key, g.key());
    assert_eq!((record.decision, record.binding_digest), (None, None));
    assert_eq!(record.operation, g.proposal);
    assert_eq!(record.barrier, g.proof.global.key.request);
    assert_eq!(
        (record.config_revision, record.policy_revision),
        (
            g.proof.global.config_revision,
            g.proof.global.policy_revision
        )
    );
    assert!(record.routine.is_none());
    let view = first.view();
    assert_eq!(view.disposition, PrivateEffectDisposition::Execute);
    assert_eq!(
        (view.stage, view.deliver),
        (PrivateEffectStage::Prepared, false)
    );

    // A lost reply: every replay is the same row and never a second license,
    // even after the grant was removed and from another attempt's fence.
    for _ in 0..2 {
        let replay = g.prepare().unwrap();
        assert_eq!(replay.disposition, PrivateEffectDisposition::ReconcileOnly);
        assert_eq!(replay.record, first.record);
    }
    let revoked = next_barrier(&mut g.db, &g.proof.global.clone(), None, true);
    g.proof.global = revoked;
    let mut later = g.fence.clone();
    later.attempt = RunAttemptId::mint();
    let replay =
        g.db.prepare_private_effect(g.dispatcher, &g.proposal, &later, &g.proof)
            .unwrap()
            .outcome
            .unwrap();
    assert_eq!(replay.disposition, PrivateEffectDisposition::ReconcileOnly);
    assert_eq!(replay.record, first.record);
    assert_eq!(
        g.db.conn()
            .query_row("SELECT count(*) FROM bot_private_effects", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn m5_private_effect_stages_prepare_is_atomic_and_a_lost_reply_replays() {
    for moment in crate::Moment::ALL {
        let mut g = Granted::new();
        g.db.arm_crash(crate::CrashPoint {
            op: WriteOp::PreparePrivateEffect,
            at: moment,
        });
        let crashed = g.prepare();
        assert!(matches!(
            crashed,
            Err(PrivateLedgerError::Ledger(BotLedgerError::Ledger(_)))
        ));
        let committed = moment == crate::Moment::AfterCommit;
        assert_eq!(
            g.db.private_effect(g.fence.run, g.proposal.operation_id)
                .unwrap()
                .is_some(),
            committed
        );
        g.db.restart_in_memory();
        let retried = g.prepare().unwrap();
        // The license went with the commit; a reply lost after it is not
        // reissued. The dispatcher that holds no obligation refuses the row.
        assert_eq!(
            retried.disposition == PrivateEffectDisposition::Execute,
            !committed
        );
        assert_eq!(
            g.prepare().unwrap().disposition,
            PrivateEffectDisposition::ReconcileOnly
        );
    }
}

#[test]
fn m5_private_effect_stages_grant_must_be_captured_and_current() {
    // Neither side holds it.
    let (mut db, fence, global) = crate::native_results::tests::decision_base();
    let proposal = proposal(Uuid::new_v4());
    let dispatcher = PrivateDispatcherId::mint();
    let mut proof = FreshBotPolicyProof {
        global,
        routine: None,
    };
    let refused = |db: &mut RunsDb, proof: &FreshBotPolicyProof| {
        db.prepare_private_effect(dispatcher, &proposal, &fence, proof)
            .unwrap()
            .outcome
            .unwrap_err()
    };
    assert_eq!(
        refused(&mut db, &proof),
        PrivateLedgerError::ConnectionNotGranted
    );
    // Current only: a grant added after the Run was admitted does not
    // broaden the Run.
    proof.global = next_barrier(&mut db, &proof.global.clone(), Some(proposal.grant()), true);
    assert_eq!(
        refused(&mut db, &proof),
        PrivateLedgerError::ConnectionNotGranted
    );
    // Captured only: the grant was removed since.
    capture_grant(&db, fence.run, proposal.grant());
    proof.global = next_barrier(&mut db, &proof.global.clone(), None, true);
    assert_eq!(
        refused(&mut db, &proof),
        PrivateLedgerError::ConnectionNotGranted
    );
    // A grant for another connection is not this grant.
    let other = ConnectionGrantRef {
        connection: ConnectionId::mint(),
        operation: PrivateOperationKind::ReadItem,
    };
    proof.global = next_barrier(&mut db, &proof.global.clone(), Some(other), true);
    assert_eq!(
        refused(&mut db, &proof),
        PrivateLedgerError::ConnectionNotGranted
    );
    assert_eq!(
        db.conn()
            .query_row("SELECT count(*) FROM bot_private_effects", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    // Both: granted.
    proof.global = next_barrier(&mut db, &proof.global.clone(), Some(proposal.grant()), true);
    let granted = db
        .prepare_private_effect(dispatcher, &proposal, &fence, &proof)
        .unwrap()
        .outcome
        .unwrap();
    assert_eq!(granted.disposition, PrivateEffectDisposition::Execute);
}

#[test]
fn m5_private_effect_stages_prepare_refuses_bad_shape_stale_proof_and_foreign_fence() {
    let mut g = Granted::new();
    // A fingerprint that does not recompute.
    let mut asserted = g.proposal.clone();
    asserted.request_fingerprint = [1; 32];
    let invalid =
        g.db.prepare_private_effect(g.dispatcher, &asserted, &g.fence, &g.proof)
            .unwrap()
            .outcome;
    assert_eq!(
        invalid.unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Invalid)
    );
    // A fence of another Bot, another attempt, or with no turn.
    let mut foreign = g.fence.clone();
    foreign.bot = BotId::mint();
    let mut old_attempt = g.fence.clone();
    old_attempt.attempt = RunAttemptId::mint();
    let mut no_turn = g.fence.clone();
    no_turn.turn = None;
    for fence in [foreign, old_attempt, no_turn] {
        assert!(matches!(
            g.db.prepare_private_effect(g.dispatcher, &g.proposal, &fence, &g.proof)
                .unwrap()
                .outcome,
            Err(PrivateLedgerError::Ledger(_))
        ));
    }
    // A proof that is not the latest barrier.
    let latest = next_barrier(
        &mut g.db,
        &g.proof.global.clone(),
        Some(g.proposal.grant()),
        true,
    );
    assert_eq!(
        g.prepare().unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Fenced)
    );
    // A barrier that forbids effects.
    g.proof.global = next_barrier(&mut g.db, &latest, Some(g.proposal.grant()), false);
    assert_eq!(
        g.prepare().unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Fenced)
    );
    assert!(
        g.db.private_effect(g.fence.run, g.proposal.operation_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn m5_private_effect_stages_every_transition_is_legal_or_stale() {
    let transitions = [DISPATCHED, CONFIRMED, REFUSED, UNKNOWN];
    for from in STAGES {
        for transition in transitions {
            let mut g = Granted::at(from);
            let before =
                g.db.private_effect(g.fence.run, g.proposal.operation_id)
                    .unwrap()
                    .unwrap();
            let result = g.advance(transition);
            let legal = matches!(
                (from, transition),
                (
                    PrivateEffectStage::Prepared,
                    PrivateEffectTransition::Dispatched {}
                        | PrivateEffectTransition::Refused { .. }
                ) | (
                    PrivateEffectStage::Dispatched,
                    PrivateEffectTransition::Confirmed { .. }
                        | PrivateEffectTransition::Refused { .. }
                        | PrivateEffectTransition::Unknown {}
                )
            );
            let replay = is_replay(&before, &transition);
            match result {
                Ok(outcome) => {
                    assert!(legal || replay, "{from:?} {transition:?}");
                    assert_eq!(outcome.disposition, PrivateEffectDisposition::ReconcileOnly);
                    if replay {
                        assert_eq!(outcome.record, before, "a replay changes nothing");
                        assert!(!outcome.deliver);
                    }
                }
                Err(error) => {
                    assert!(!legal && !replay, "{from:?} {transition:?}");
                    assert_eq!(error, PrivateLedgerError::EffectStale);
                    assert_eq!(
                        g.db.private_effect(g.fence.run, g.proposal.operation_id)
                            .unwrap()
                            .unwrap(),
                        before,
                        "a refused advance commits nothing"
                    );
                }
            }
            // No path leads back to `prepared`.
            if from != PrivateEffectStage::Prepared {
                assert_ne!(g.stage(), PrivateEffectStage::Prepared);
            }
        }
    }
}

#[test]
fn m5_private_effect_stages_final_rows_admit_no_update_and_no_delete() {
    for stage in [
        PrivateEffectStage::Confirmed,
        PrivateEffectStage::Refused,
        PrivateEffectStage::Unknown,
    ] {
        let g = Granted::at(stage);
        for statement in [
            // Not even an update that changes nothing.
            "UPDATE bot_private_effects SET updated_at_ms=updated_at_ms",
            "UPDATE bot_private_effects SET stage=stage",
            "UPDATE bot_private_effects SET stage='prepared',refusal=NULL,outcome_bytes=NULL,withheld=NULL,dispatched_at_ms=NULL,resolved_at_ms=NULL",
            "UPDATE bot_private_effects SET stage='dispatched',refusal=NULL,outcome_bytes=NULL,withheld=NULL,dispatched_at_ms=1,resolved_at_ms=NULL",
            // A late answer never turns `unknown` or `refused` into `confirmed`.
            "UPDATE bot_private_effects SET stage='confirmed',refusal=NULL,outcome_bytes=1,withheld=0,dispatched_at_ms=1,resolved_at_ms=2",
        ] {
            let error = g.db.conn().execute(statement, []).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("invalid private effect transition"),
                "{stage:?}: {statement}: {error}"
            );
        }
        let error =
            g.db.conn()
                .execute("DELETE FROM bot_private_effects", [])
                .unwrap_err();
        assert!(error.to_string().contains("retained private effect"));
        assert_eq!(g.stage(), stage);
    }
    // An open row cannot be deleted or have its identity changed either.
    let g = Granted::at(PrivateEffectStage::Dispatched);
    for statement in [
        "DELETE FROM bot_private_effects",
        "UPDATE bot_private_effects SET dispatcher_instance='00000000-0000-4000-8000-0000000000ff'",
        "UPDATE bot_private_effects SET connection_revision=connection_revision+1",
        "UPDATE bot_private_effects SET stage='prepared',dispatched_at_ms=NULL",
        "UPDATE bot_private_effects SET dispatched_at_ms=dispatched_at_ms+1",
    ] {
        assert!(g.db.conn().execute(statement, []).is_err(), "{statement}");
    }
}

#[test]
fn m5_private_effect_stages_deliver_is_answered_once() {
    let mut g = Granted::at(PrivateEffectStage::Dispatched);
    let confirmed = g.advance(CONFIRMED).unwrap();
    assert!(confirmed.deliver);
    assert_eq!(confirmed.record.stage, PrivateEffectStage::Confirmed);
    assert_eq!(
        (confirmed.record.outcome_bytes, confirmed.record.withheld),
        (Some(12), Some(false))
    );
    assert!(confirmed.record.resolved_at_ms.is_some());
    // The lost reply replays the receipt, never the delivery.
    for _ in 0..2 {
        let replay = g.advance(CONFIRMED).unwrap();
        assert!(!replay.deliver);
        assert_eq!(replay.record, confirmed.record);
    }
    // A different confirmation is not a replay.
    assert_eq!(
        g.advance(PrivateEffectTransition::Confirmed {
            outcome_bytes: 13,
            withheld: false
        })
        .unwrap_err(),
        PrivateLedgerError::EffectStale
    );
    let read =
        g.db.read_private_effect(g.fence.run, g.proposal.operation_id, g.dispatcher)
            .unwrap()
            .outcome
            .unwrap()
            .unwrap();
    assert!(!read.deliver);
    assert_eq!(read.disposition, PrivateEffectDisposition::ReconcileOnly);

    // A withheld read is confirmed and never delivered.
    let mut g = Granted::at(PrivateEffectStage::Dispatched);
    let withheld = g
        .advance(PrivateEffectTransition::Confirmed {
            outcome_bytes: 40,
            withheld: true,
        })
        .unwrap();
    assert!(!withheld.deliver);
    assert_eq!(withheld.record.withheld, Some(true));
}

#[test]
fn m5_private_effect_stages_advance_is_atomic_and_idempotent_by_key_and_transition() {
    for moment in crate::Moment::ALL {
        let mut g = Granted::at(PrivateEffectStage::Prepared);
        g.db.arm_crash(crate::CrashPoint {
            op: WriteOp::AdvancePrivateEffect,
            at: moment,
        });
        assert!(g.advance(DISPATCHED).is_err());
        let committed = moment == crate::Moment::AfterCommit;
        assert_eq!(
            g.stage() == PrivateEffectStage::Dispatched,
            committed,
            "{moment:?}"
        );
        g.db.restart_in_memory();
        // The dispatcher retries the same advance until it is acknowledged.
        assert_eq!(
            g.advance(DISPATCHED).unwrap().record.stage,
            PrivateEffectStage::Dispatched
        );
    }
    // A key that names another attempt or turn is fenced; an unknown key is stale.
    let mut g = Granted::at(PrivateEffectStage::Prepared);
    let mut wrong = g.key();
    wrong.turn = AgentTurnId::mint();
    assert_eq!(
        g.db.advance_private_effect(&wrong, g.dispatcher, &DISPATCHED)
            .unwrap()
            .outcome
            .unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Fenced)
    );
    let mut absent = g.key();
    absent.operation_id = Uuid::new_v4();
    assert_eq!(
        g.db.advance_private_effect(&absent, g.dispatcher, &DISPATCHED)
            .unwrap()
            .outcome
            .unwrap_err(),
        PrivateLedgerError::EffectStale
    );
    let mut unknown_run = g.key();
    unknown_run.run = RunId::mint();
    assert_eq!(
        g.db.advance_private_effect(&unknown_run, g.dispatcher, &DISPATCHED)
            .unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Fenced)
    );
    assert_eq!(g.stage(), PrivateEffectStage::Prepared);
}

#[test]
fn m5_private_effect_stages_one_open_effect_per_run() {
    let mut g = Granted::at(PrivateEffectStage::Prepared);
    let first = g.proposal.clone();
    let second = proposal(Uuid::new_v4());
    let try_second = |g: &mut Granted| {
        g.db.prepare_private_effect(g.dispatcher, &second, &g.fence, &g.proof)
            .unwrap()
            .outcome
    };
    assert_eq!(
        try_second(&mut g).unwrap_err(),
        PrivateLedgerError::EffectOpen
    );
    g.advance(DISPATCHED).unwrap();
    assert_eq!(
        try_second(&mut g).unwrap_err(),
        PrivateLedgerError::EffectOpen
    );
    // The index holds it below the owner too.
    assert!(
        g.db.conn()
            .execute(
                "INSERT INTO bot_private_effects SELECT run_id,?1,user_scope,device_id,bot_id,attempt_id,generation,turn_id,dispatcher_instance,decision_id,binding_digest,connection_id,connection_revision,account_digest,item_id,item_ref_digest,operation,request_fingerprint,barrier_request_id,config_revision,policy_revision,policy_digest,schedule_id,routine_barrier_request_id,grant_revision,routine_policy_digest,'prepared',NULL,NULL,NULL,NULL,NULL,created_at_ms,updated_at_ms FROM bot_private_effects",
                [second.operation_id.to_string()],
            )
            .is_err()
    );
    // Once the first is final a new operation is a new, explicit intent.
    g.advance(UNKNOWN).unwrap();
    let next = try_second(&mut g).unwrap();
    assert_eq!(next.disposition, PrivateEffectDisposition::Execute);
    // The unknown one is still there, final, and its id cannot be reused
    // with another proposal.
    let mut changed = first.clone();
    changed.connection_revision = DecisionRevision::new(4).unwrap();
    assert_eq!(
        g.db.prepare_private_effect(g.dispatcher, &changed, &g.fence, &g.proof)
            .unwrap()
            .outcome
            .unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Conflict)
    );
    let unknown =
        g.db.prepare_private_effect(g.dispatcher, &first, &g.fence, &g.proof)
            .unwrap()
            .outcome
            .unwrap();
    assert_eq!(unknown.record.stage, PrivateEffectStage::Unknown);
    assert_eq!(unknown.disposition, PrivateEffectDisposition::ReconcileOnly);
}

#[test]
fn m5_private_effect_stages_unknown_is_final_and_never_resent() {
    let mut g = Granted::at(PrivateEffectStage::Unknown);
    let unknown =
        g.db.private_effect(g.fence.run, g.proposal.operation_id)
            .unwrap()
            .unwrap();
    assert!(unknown.dispatched_at_ms.is_some() && unknown.resolved_at_ms.is_some());
    assert_eq!(
        unknown.retained_effect(),
        Some(DecisionRetainedEffect::PrivateUnknown { key: g.key() })
    );
    // No command reopens it or hands out another license for it.
    assert_eq!(
        g.prepare().unwrap().disposition,
        PrivateEffectDisposition::ReconcileOnly
    );
    for transition in [DISPATCHED, CONFIRMED, REFUSED] {
        assert_eq!(
            g.advance(transition).unwrap_err(),
            PrivateLedgerError::EffectStale
        );
    }
    assert!(!g.advance(UNKNOWN).unwrap().deliver);
    // Another instance finds nothing to resolve: it is not open.
    assert!(
        g.db.announce_private_dispatcher(g.fence.owner, PrivateDispatcherId::mint())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        g.db.private_effect(g.fence.run, g.proposal.operation_id)
            .unwrap()
            .unwrap(),
        unknown
    );
}

#[test]
fn m5_private_dispatcher_resolution_own_instance_commands_change_nothing() {
    for stage in [PrivateEffectStage::Prepared, PrivateEffectStage::Dispatched] {
        let mut g = Granted::at(stage);
        let before =
            g.db.private_effect(g.fence.run, g.proposal.operation_id)
                .unwrap()
                .unwrap();
        // The row's own instance announces, reads, prepares again and replays
        // its advance: the row is never swept.
        assert!(
            g.db.announce_private_dispatcher(g.fence.owner, g.dispatcher)
                .unwrap()
                .is_empty()
        );
        let read =
            g.db.read_private_effect(g.fence.run, g.proposal.operation_id, g.dispatcher)
                .unwrap();
        assert!(read.resolved.is_empty());
        assert_eq!(read.outcome.unwrap().unwrap().record, before);
        assert_eq!(g.prepare().unwrap().record, before);
        // A delegate boot probe and a Start walk carry no dispatcher and reach
        // no private owner; reading the Bot's boot state changes nothing.
        g.db.restart_in_memory();
        assert_eq!(
            g.db.private_effect(g.fence.run, g.proposal.operation_id)
                .unwrap()
                .unwrap(),
            before
        );
    }
}

#[test]
fn m5_private_dispatcher_resolution_second_instance_closes_prepared_and_dispatched() {
    for (stage, closed, refusal) in [
        (
            PrivateEffectStage::Prepared,
            PrivateEffectStage::Refused,
            Some(PrivateEffectRefusal::NotDispatched),
        ),
        (
            PrivateEffectStage::Dispatched,
            PrivateEffectStage::Unknown,
            None,
        ),
    ] {
        // By an announce.
        let mut g = Granted::at(stage);
        let next = PrivateDispatcherId::mint();
        let resolved =
            g.db.announce_private_dispatcher(g.fence.owner, next)
                .unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!((resolved[0].stage, resolved[0].refusal), (closed, refusal));
        assert_eq!(
            resolved[0].dispatcher, g.dispatcher,
            "the row keeps its instance"
        );
        assert!(resolved[0].resolved_at_ms.is_some());
        assert_eq!(
            resolved[0].dispatched_at_ms.is_some(),
            stage == PrivateEffectStage::Dispatched
        );
        // Idempotent: a retried announce finds nothing.
        assert!(
            g.db.announce_private_dispatcher(g.fence.owner, next)
                .unwrap()
                .is_empty()
        );
        // The dead instance's late advance is stale and commits nothing.
        assert_eq!(
            g.advance(CONFIRMED).unwrap_err(),
            PrivateLedgerError::EffectStale
        );
        assert_eq!(g.stage(), closed);

        // By any command that carries another instance: a read.
        let mut g = Granted::at(stage);
        let read =
            g.db.read_private_effect(g.fence.run, g.proposal.operation_id, next)
                .unwrap();
        assert_eq!(read.resolved.len(), 1);
        assert_eq!(read.outcome.unwrap().unwrap().record.stage, closed);

        // By the new instance's advance of the old row: resolution commits
        // first, then the advance answers stale.
        let mut g = Granted::at(stage);
        let key = g.key();
        let reply = g.db.advance_private_effect(&key, next, &UNKNOWN).unwrap();
        assert_eq!(reply.resolved.len(), 1);
        assert_eq!(reply.outcome.unwrap_err(), PrivateLedgerError::EffectStale);
        assert_eq!(g.stage(), closed);

        // By the new instance's prepare of a new operation, which then
        // proceeds: the Run's one open slot is free again.
        let mut g = Granted::at(stage);
        let second = proposal(Uuid::new_v4());
        let reply =
            g.db.prepare_private_effect(next, &second, &g.fence, &g.proof)
                .unwrap();
        assert_eq!(reply.resolved.len(), 1);
        assert_eq!(
            reply.outcome.unwrap().disposition,
            PrivateEffectDisposition::Execute
        );
        assert_eq!(g.stage(), closed);
    }
}

#[test]
fn m5_private_dispatcher_resolution_survives_a_failed_command_and_spares_other_owners() {
    // Two Runs of one owner in one ledger, each with an open effect of the
    // same (dead) instance.
    let a = Granted::at(PrivateEffectStage::Dispatched);
    let dead = a.dispatcher;
    let (first_run, first_op, owner) = (a.fence.run, a.proposal.operation_id, a.fence.owner);
    let mut b = Granted::in_db(a.db, RunId::mint());
    assert_eq!(b.fence.owner, owner);
    b.dispatcher = dead;
    b.prepare().unwrap();

    // Another owner's announce touches neither row.
    let mut stranger = owner;
    stranger.user_scope = serde_json::from_value(fixtures::v4(0xf0).to_string().into()).unwrap();
    assert!(
        b.db.announce_private_dispatcher(stranger, PrivateDispatcherId::mint())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        b.db.private_effect(first_run, first_op)
            .unwrap()
            .unwrap()
            .stage,
        PrivateEffectStage::Dispatched
    );
    assert_eq!(b.stage(), PrivateEffectStage::Prepared);

    // The bounded probe pages by the open index.
    assert_eq!(
        rows::open_of_others(b.db.conn(), owner, PrivateDispatcherId::mint(), 1)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        rows::open_of_others(b.db.conn(), owner, dead, PRIVATE_RESOLUTION_LIMIT)
            .unwrap()
            .len(),
        0
    );
    assert_eq!(PRIVATE_RESOLUTION_LIMIT, 64);

    // A new instance's command that then fails still leaves both resolved.
    let next = PrivateDispatcherId::mint();
    let mut invalid = proposal(Uuid::new_v4());
    invalid.request_fingerprint = [0; 32];
    let reply =
        b.db.prepare_private_effect(next, &invalid, &b.fence, &b.proof)
            .unwrap();
    assert_eq!(reply.resolved.len(), 2);
    assert_eq!(
        reply.outcome.unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Invalid)
    );
    assert_eq!(
        b.db.private_effect(first_run, first_op)
            .unwrap()
            .unwrap()
            .stage,
        PrivateEffectStage::Unknown
    );
    let closed =
        b.db.private_effect(b.fence.run, b.proposal.operation_id)
            .unwrap()
            .unwrap();
    assert_eq!(
        (closed.stage, closed.refusal),
        (
            PrivateEffectStage::Refused,
            Some(PrivateEffectRefusal::NotDispatched)
        )
    );
}

#[test]
fn m5_private_dispatcher_resolution_is_atomic() {
    for moment in crate::Moment::ALL {
        let mut g = Granted::at(PrivateEffectStage::Dispatched);
        g.db.arm_crash(crate::CrashPoint {
            op: WriteOp::ResolvePrivateDispatcher,
            at: moment,
        });
        let next = PrivateDispatcherId::mint();
        assert!(
            g.db.announce_private_dispatcher(g.fence.owner, next)
                .is_err()
        );
        let committed = moment == crate::Moment::AfterCommit;
        assert_eq!(g.stage() == PrivateEffectStage::Unknown, committed);
        g.db.restart_in_memory();
        // The announce is retried until acknowledged.
        assert_eq!(
            g.db.announce_private_dispatcher(g.fence.owner, next)
                .unwrap()
                .len(),
            usize::from(!committed)
        );
        assert_eq!(g.stage(), PrivateEffectStage::Unknown);
    }
}

#[test]
fn m5_private_revocation_fence_bot_barrier_before_dispatch_refuses_as_a_reply() {
    for effects_allowed in [true, false] {
        let mut g = Granted::at(PrivateEffectStage::Prepared);
        // Removing the Bot's grant bumps its policy revision: a newer barrier.
        next_barrier(&mut g.db, &g.proof.global.clone(), None, effects_allowed);
        let fenced = g.advance(DISPATCHED).unwrap();
        assert_eq!(fenced.record.stage, PrivateEffectStage::Refused);
        assert_eq!(
            fenced.record.refusal,
            Some(PrivateEffectRefusal::AccessDenied)
        );
        assert!(!fenced.deliver);
        assert!(fenced.record.dispatched_at_ms.is_none(), "never dispatched");
        // The lost reply replays the same refusal; the vendor is never called.
        assert_eq!(g.advance(DISPATCHED).unwrap().record, fenced.record);
        assert_eq!(
            g.advance(CONFIRMED).unwrap_err(),
            PrivateLedgerError::EffectStale
        );
    }
}

#[test]
fn m5_private_revocation_fence_pending_bot_barrier_refuses_the_dispatch_mark() {
    let mut g = Granted::at(PrivateEffectStage::Prepared);
    // A held page lease makes the next barrier pending rather than effective.
    let page = PrepareNativeResult {
        key: NativeResultKey {
            run: g.fence.run,
            attempt: g.fence.attempt,
            generation: g.fence.generation,
            turn: g.fence.turn.unwrap(),
            operation_id: Uuid::new_v4(),
        },
        kind: NativeResultOperation::CreatePage,
        destination: g.proof.global.policy.workspace,
        target: None,
        request_fingerprint: [3; 32],
        expected_revision: None,
        config_revision: g.proof.global.config_revision,
        policy_revision: g.proof.global.policy_revision,
        title: "Fixture".into(),
    };
    g.db.prepare_native_result(&page).unwrap();
    g.db.lease_native_result(&page.key, page.request_fingerprint, &g.fence, &g.proof)
        .unwrap();
    let pending = next_barrier(
        &mut g.db,
        &g.proof.global.clone(),
        Some(g.proposal.grant()),
        true,
    );
    assert_eq!(
        g.db.native_policy(&pending.key).unwrap().unwrap().state,
        BotPolicyBarrierState::Pending
    );
    let fenced = g.advance(DISPATCHED).unwrap();
    assert_eq!(
        (fenced.record.stage, fenced.record.refusal),
        (
            PrivateEffectStage::Refused,
            Some(PrivateEffectRefusal::AccessDenied)
        )
    );
}

#[test]
fn m5_private_revocation_fence_bot_barrier_after_dispatch_withholds_delivery() {
    let mut g = Granted::at(PrivateEffectStage::Dispatched);
    next_barrier(&mut g.db, &g.proof.global.clone(), None, true);
    // The read may complete at the vendor; its content is dropped.
    let confirmed = g.advance(CONFIRMED).unwrap();
    assert_eq!(confirmed.record.stage, PrivateEffectStage::Confirmed);
    assert!(!confirmed.deliver);
    assert_eq!(confirmed.record.withheld, Some(false));
}

/// A Routine Run whose Routine grant (captured and current) holds the grant.
fn routine_granted() -> (Granted, RoutinePolicyBarrier) {
    let (db, fence, global) = crate::native_results::tests::decision_base();
    let proposal = proposal(Uuid::new_v4());
    capture_grant(&db, fence.run, proposal.grant());
    let mut db = db;
    let mut submission = db.bot_admission(fence.run).unwrap().unwrap().submission;
    let route = crate::bots::routines::tests::convert(&mut db, &mut submission, true);
    let routine = RoutinePolicyBarrier {
        owner: fence.owner,
        bot: fence.bot,
        schedule: route.schedule,
        request: BotRequestId::mint().into(),
        grant_revision: 1,
        policy: submission.policy.clone(),
        effects_allowed: true,
    };
    db.install_routine_policy(&routine).unwrap();
    (
        Granted {
            db,
            fence,
            // The Bot's own ordinary-chat policy holds no grant at all.
            proof: FreshBotPolicyProof {
                global,
                routine: Some(routine.clone()),
            },
            proposal,
            dispatcher: PrivateDispatcherId::mint(),
        },
        routine,
    )
}
fn next_routine_barrier(
    db: &mut RunsDb,
    current: &RoutinePolicyBarrier,
    grant: Option<ConnectionGrantRef>,
) -> RoutinePolicyBarrier {
    let mut next = current.clone();
    next.request = BotRequestId::mint().into();
    next.grant_revision += 1;
    next.policy.connections = grant.into_iter().collect();
    db.install_routine_policy(&next).unwrap();
    next
}

#[test]
fn m5_private_revocation_fence_routine_run_reads_the_routine_grant() {
    let (mut g, routine) = routine_granted();
    assert!(g.proof.global.policy.connections.is_empty());
    let first = g.prepare().unwrap();
    assert_eq!(first.disposition, PrivateEffectDisposition::Execute);
    // Both barriers are bound on the row.
    let bound = first.record.routine.clone().unwrap();
    assert_eq!(
        (bound.schedule, bound.request, bound.grant_revision),
        (routine.schedule, routine.request, 1)
    );
    assert_eq!(bound.digest, routine.digest().unwrap());
    assert_eq!(first.record.barrier, g.proof.global.key.request);
    assert_eq!(
        first.view().routine,
        Some(PrivateRoutineBinding {
            schedule: routine.schedule,
            grant_revision: 1
        })
    );

    // The Bot holding the grant does not grant a Routine Run whose Routine
    // does not.
    let (mut g, routine) = routine_granted();
    let without = next_routine_barrier(&mut g.db, &routine, None);
    let grant = g.proposal.grant();
    g.proof.global = next_barrier(&mut g.db, &g.proof.global.clone(), Some(grant), true);
    g.proof.routine = Some(without);
    assert_eq!(
        g.prepare().unwrap_err(),
        PrivateLedgerError::ConnectionNotGranted
    );
    // A Routine proof is required for a Routine Run.
    g.proof.routine = None;
    assert!(matches!(
        g.prepare().unwrap_err(),
        PrivateLedgerError::Ledger(_)
    ));
}

#[test]
fn m5_private_revocation_fence_removed_routine_grant_fences_dispatch_and_delivery() {
    // Before dispatch: the dispatch mark becomes a refusal reply.
    let (mut g, routine) = routine_granted();
    g.prepare().unwrap();
    next_routine_barrier(&mut g.db, &routine, None);
    let fenced = g.advance(DISPATCHED).unwrap();
    assert_eq!(
        (fenced.record.stage, fenced.record.refusal, fenced.deliver),
        (
            PrivateEffectStage::Refused,
            Some(PrivateEffectRefusal::AccessDenied),
            false
        )
    );
    assert!(fenced.record.dispatched_at_ms.is_none());

    // After dispatch: the receipt is recorded and nothing is delivered.
    let (mut g, routine) = routine_granted();
    g.prepare().unwrap();
    assert_eq!(
        g.advance(DISPATCHED).unwrap().record.stage,
        PrivateEffectStage::Dispatched
    );
    next_routine_barrier(&mut g.db, &routine, None);
    let confirmed = g.advance(CONFIRMED).unwrap();
    assert_eq!(confirmed.record.stage, PrivateEffectStage::Confirmed);
    assert!(!confirmed.deliver);

    // With both barriers standing the same read is delivered.
    let (mut g, _) = routine_granted();
    g.prepare().unwrap();
    g.advance(DISPATCHED).unwrap();
    assert!(g.advance(CONFIRMED).unwrap().deliver);

    // A newer Bot barrier fences a Routine Run's dispatch too.
    let (mut g, _) = routine_granted();
    g.prepare().unwrap();
    next_barrier(&mut g.db, &g.proof.global.clone(), None, true);
    assert_eq!(
        g.advance(DISPATCHED).unwrap().record.refusal,
        Some(PrivateEffectRefusal::AccessDenied)
    );
}

#[test]
fn m5_private_operation_ids_page_and_private_effect_cannot_share_an_id() {
    // A page intent first: the private prepare refuses its id.
    let mut g = Granted::new();
    let page = |g: &Granted, operation: Uuid| PrepareNativeResult {
        key: NativeResultKey {
            run: g.fence.run,
            attempt: g.fence.attempt,
            generation: g.fence.generation,
            turn: g.fence.turn.unwrap(),
            operation_id: operation,
        },
        kind: NativeResultOperation::CreatePage,
        destination: g.proof.global.policy.workspace,
        target: None,
        request_fingerprint: [3; 32],
        expected_revision: None,
        config_revision: g.proof.global.config_revision,
        policy_revision: g.proof.global.policy_revision,
        title: "Fixture".into(),
    };
    let request = page(&g, g.proposal.operation_id);
    g.db.prepare_native_result(&request).unwrap();
    assert_eq!(
        g.prepare().unwrap_err(),
        PrivateLedgerError::Ledger(BotLedgerError::Conflict)
    );
    assert!(
        g.db.private_effect(g.fence.run, g.proposal.operation_id)
            .unwrap()
            .is_none()
    );

    // A private effect first: the page prepare refuses its id, and an
    // unrelated page id is unaffected.
    let mut g = Granted::new();
    g.prepare().unwrap();
    let request = page(&g, g.proposal.operation_id);
    assert_eq!(
        g.db.prepare_native_result(&request).unwrap_err(),
        BotLedgerError::Conflict
    );
    let other = page(&g, Uuid::new_v4());
    g.db.prepare_native_result(&other).unwrap();

    // A Memory allocation under the id is another kind of operation too.
    let mut g = Granted::new();
    assert!(!operation_id_taken_on(g.db.conn(), g.fence.run, g.proposal.operation_id).unwrap());
    g.prepare().unwrap();
    assert!(operation_id_taken_on(g.db.conn(), g.fence.run, g.proposal.operation_id).unwrap());
    assert!(exists_on(g.db.conn(), g.fence.run, g.proposal.operation_id).unwrap());
    assert!(!exists_on(g.db.conn(), g.fence.run, Uuid::new_v4()).unwrap());
}

#[test]
fn m5_private_effect_rows_hold_no_text_a_person_or_vendor_wrote() {
    // Every column of the table is an id, a digest, a closed token, a number
    // or a time: there is nowhere for a label, a title, a link, a secret or
    // content to be stored.
    let g = Granted::at(PrivateEffectStage::Confirmed);
    let mut query =
        g.db.conn()
            .prepare("SELECT * FROM bot_private_effects")
            .unwrap();
    let names: Vec<String> = query.column_names().map(|n| n.to_string()).collect();
    let mut rows = query.query([]).unwrap();
    let row = rows.next().unwrap().unwrap();
    let tokens = [
        "read_item",
        "prepared",
        "dispatched",
        "confirmed",
        "refused",
        "unknown",
    ];
    for (index, name) in names.iter().enumerate() {
        match row.get::<_, rusqlite::types::Value>(index).unwrap() {
            rusqlite::types::Value::Text(text) => assert!(
                text.len() == 36 && text.parse::<Uuid>().is_ok() || tokens.contains(&text.as_str()),
                "{name} holds free text"
            ),
            rusqlite::types::Value::Blob(bytes) => assert_eq!(bytes.len(), 32, "{name}"),
            rusqlite::types::Value::Integer(_) | rusqlite::types::Value::Null => {}
            rusqlite::types::Value::Real(_) => panic!("{name} holds a real"),
        }
    }
    // Refusals and the record's Debug name no connection, account or item.
    for error in [
        PrivateLedgerError::ConnectionGranted,
        PrivateLedgerError::ConnectionNotGranted,
        PrivateLedgerError::EffectOpen,
        PrivateLedgerError::EffectStale,
        PrivateLedgerError::DecisionOwned,
        PrivateLedgerError::RoutineDecisionUnsupported,
    ] {
        let text = format!("{error} {error:?}");
        assert!(!text.chars().any(|c| c.is_ascii_digit()), "{text}");
        assert!(error.code().unwrap().starts_with("bot_"));
    }
    assert_eq!(
        PrivateLedgerError::Ledger(BotLedgerError::Fenced).code(),
        None
    );
}
