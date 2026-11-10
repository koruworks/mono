// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::{
    BotInitialSend, BotLedger, BotReservation, CrashPoint, LedgerError, Moment, RunsDb, WriteOp,
    fixtures,
};
use std::sync::{Arc, atomic::AtomicI64};

struct Harness {
    db: RunsDb,
    fence: BotExecutionFence,
    request: PrepareNativeResult,
    policy: BotPolicyBarrier,
}
impl Harness {
    fn new() -> Self {
        let mut db = RunsDb::in_memory(Box::new(fixtures::FakeClock(Arc::new(AtomicI64::new(
            1000,
        )))));
        let mut request = fixtures::request(1);
        request.scope = WorkspaceScope::BotFolder {
            folder: BotWorkingFolder::new("/fixture/bot".into()).unwrap(),
        };
        let submission = BotSubmission {
            provenance: BotProvenance {
                owner: BotOwner {
                    user_scope: serde_json::from_value(serde_json::json!(fixtures::v4(0xb0)))
                        .unwrap(),
                    device: request.executing_device,
                },
                bot: BotId::mint(),
                submission: BotRequestId::mint(),
                chat_id: Uuid::new_v4(),
                ai_attempt_id: None,
                assistant_message_id: None,
                config_revision: 1,
                policy_revision: 1,
                continued_from: None,
            },
            policy: CapturedBotPolicy {
                workspace: fixtures::workspace(0xa2),
                destination: fixtures::workspace(0xa2),
                access: BotAccess::Default,
                selected_pages: Default::default(),
            },
            workspace: fixtures::workspace(0xa2),
            request,
            input: PendingInput {
                idempotency_key: IdempotencyKey::mint(),
                text: "PRIVATE-TASK".into(),
                attachment_ids: vec![],
                references: vec![],
                mode: None,
                fast: None,
                reasoning: None,
                skills: vec![],
                model: None,
            },
        };
        db.admit_bot(&submission).unwrap();
        db.satisfy_intent(submission.request.run, fixtures::document(0xd0))
            .unwrap();
        assert_eq!(
            db.reserve_bot(submission.request.run).unwrap(),
            BotReservation::Acquired
        );
        let a = fixtures::running_attempt(&mut db, submission.request.run, 1);
        db.activate_bot(a.run, a.attempt, a.generation).unwrap();
        let mut send = BotInitialSend {
            attempt: a.attempt,
            generation: a.generation,
            key: IdempotencyKey::mint(),
            fingerprint: [2; 32],
            turn: None,
        };
        db.prepare_bot_initial_send(a.run, &send).unwrap();
        let turn = AgentTurnId::mint();
        send.turn = Some(turn);
        db.acknowledge_bot_initial_send(a.run, &send).unwrap();
        let fence = BotExecutionFence {
            owner: submission.provenance.owner,
            bot: submission.provenance.bot,
            run: a.run,
            attempt: a.attempt,
            generation: a.generation,
            session: a.provider_session.unwrap().session.unwrap(),
            turn: Some(turn),
        };
        let request = PrepareNativeResult {
            key: NativeResultKey {
                run: a.run,
                attempt: a.attempt,
                generation: a.generation,
                turn,
                operation_id: Uuid::new_v4(),
            },
            kind: NativeResultOperation::CreatePage,
            destination: submission.workspace,
            target: None,
            request_fingerprint: [3; 32],
            expected_revision: None,
            config_revision: 1,
            policy_revision: 1,
            title: "PRIVATE-TITLE".into(),
        };
        let policy = BotPolicyBarrier {
            key: BotPolicyBarrierKey {
                owner: submission.provenance.owner,
                bot: submission.provenance.bot,
                request: BotRequestId::mint(),
            },
            config_revision: 1,
            policy_revision: 1,
            policy: submission.policy.clone(),
            effects_allowed: true,
        };
        db.install_native_policy(&policy).unwrap();
        Self {
            db,
            fence,
            request,
            policy,
        }
    }
    fn prepare(&mut self) -> NativeIntentRecord {
        self.db.prepare_native_result(&self.request).unwrap()
    }
    fn lease(&mut self) -> NativeLeaseOutcome {
        self.db
            .lease_native_result(
                &self.request.key,
                self.request.request_fingerprint,
                &self.fence,
                &self.policy,
            )
            .unwrap()
    }
    fn evidence(&self) -> NativeResultReference {
        let r = self.db.native_result(&self.request.key).unwrap().unwrap();
        NativeResultReference {
            operation_id: r.key.operation_id,
            workspace: r.destination,
            document: r.target,
            version: vec![1, 2, 3],
            title: "PRIVATE-CONFIRMED".into(),
            confirmed_at_ms: 900,
            kind: if r.kind == NativeResultOperation::CreatePage {
                NativeResultKind::CreatedPage
            } else {
                NativeResultKind::EditedPage
            },
        }
    }
    fn confirm(&mut self, observation: NativeResultObservation) -> NativeIntentRecord {
        self.db
            .confirm_native_result(&ConfirmNativeResult {
                key: self.request.key.clone(),
                request_fingerprint: self.request.request_fingerprint,
                observation,
            })
            .unwrap()
    }
    fn narrow(&self) -> BotPolicyBarrier {
        let mut p = self.policy.clone();
        p.key.request = BotRequestId::mint();
        p.config_revision += 1;
        p.policy_revision += 1;
        p.effects_allowed = false;
        p
    }
}

#[test]
fn prepare_and_first_lease_have_distinct_commit_boundaries_and_never_reexecute() {
    let mut h = Harness::new();
    let first = h.prepare();
    assert_eq!(first.lease_state, NativeLeaseState::None);
    assert_eq!(
        first.view().disposition,
        NativeResultDisposition::ReconcileOnly
    );
    h.db.restart_in_memory();
    assert_eq!(h.prepare(), first);
    let mut stale = h.policy.clone();
    stale.policy.selected_pages.insert(fixtures::document(2));
    assert!(
        h.db.lease_native_result(
            &h.request.key,
            h.request.request_fingerprint,
            &h.fence,
            &stale
        )
        .is_err()
    );
    let leased = h.lease();
    assert!(leased.execute.is_some());
    assert_eq!(leased.intent.target, first.target);
    assert_eq!(leased.intent.store_request, first.store_request);
    h.db.restart_in_memory();
    assert!(h.lease().execute.is_none());
    h.confirm(NativeResultObservation::Unconfirmed);
    assert!(h.lease().execute.is_none());
    assert_eq!(
        h.db.refuse_unleased_native_result(&h.request.key, NativeResultRefusal::Unavailable),
        Err(BotLedgerError::Fenced)
    );
    let e = h.evidence();
    h.confirm(NativeResultObservation::Durable {
        evidence: e.clone(),
    });
    assert!(h.lease().execute.is_none());
    let saved = h.db.register_native_result(&h.request.key, &e).unwrap();
    assert_eq!(saved.stage, NativeResultStage::Registered);
    assert_eq!(
        h.db.register_native_result(&h.request.key, &e).unwrap(),
        saved
    );
    assert!(h.lease().execute.is_none());
    assert!(!format!("{saved:?}").contains("PRIVATE"));
}

#[test]
fn narrowing_stays_pending_through_restart_and_uncertainty_until_proven_settled() {
    let mut h = Harness::new();
    h.prepare();
    h.lease();
    let narrow = h.narrow();
    assert_eq!(
        h.db.install_native_policy(&narrow).unwrap().state,
        BotPolicyBarrierState::Pending
    );
    h.db.restart_in_memory();
    h.confirm(NativeResultObservation::Unconfirmed);
    assert_eq!(
        h.db.settle_native_policy(&narrow.key).unwrap().state,
        BotPolicyBarrierState::Pending
    );
    let old = h.request.clone();
    h.request.key.operation_id = Uuid::new_v4();
    h.prepare();
    assert!(
        h.db.lease_native_result(
            &h.request.key,
            h.request.request_fingerprint,
            &h.fence,
            &h.policy
        )
        .is_err()
    );
    assert!(
        h.db.lease_native_result(
            &h.request.key,
            h.request.request_fingerprint,
            &h.fence,
            &narrow
        )
        .is_err()
    );
    h.request = old;
    h.confirm(NativeResultObservation::NotApplied {
        code: NativeResultRefusal::Unavailable,
    });
    assert_eq!(
        h.db.settle_native_policy(&narrow.key).unwrap().state,
        BotPolicyBarrierState::Effective
    );
    assert_eq!(
        h.db.current_native_policy(narrow.key.owner, narrow.key.bot)
            .unwrap()
            .unwrap()
            .key,
        narrow.key
    );
    assert!(h.lease().execute.is_none());
    let mut changed = h.policy.clone();
    changed.effects_allowed = false;
    assert_eq!(
        h.db.install_native_policy(&changed),
        Err(BotLedgerError::Conflict)
    );
    changed = h.policy.clone();
    changed.key.request = BotRequestId::mint();
    assert_eq!(
        h.db.install_native_policy(&changed),
        Err(BotLedgerError::Stale)
    );
}

#[test]
fn unleased_refusal_is_terminal_and_cannot_drain_a_held_effect() {
    let mut h = Harness::new();
    h.prepare();
    let refused =
        h.db.refuse_unleased_native_result(&h.request.key, NativeResultRefusal::AccessDenied)
            .unwrap();
    assert_eq!(refused.stage, NativeResultStage::Refused);
    assert_eq!(refused.lease_state, NativeLeaseState::None);
    assert!(h.lease().execute.is_none());
    assert!(h.db.pending_native_results(None, 256).unwrap().is_empty());
    assert_eq!(
        h.db.refuse_unleased_native_result(&h.request.key, NativeResultRefusal::AccessDenied)
            .unwrap(),
        refused
    );
    assert_eq!(
        h.db.refuse_unleased_native_result(&h.request.key, NativeResultRefusal::Conflict),
        Err(BotLedgerError::Conflict)
    );
    h.request.key.operation_id = Uuid::new_v4();
    h.prepare();
    h.lease();
    let narrow = h.narrow();
    h.db.install_native_policy(&narrow).unwrap();
    assert_eq!(
        h.db.refuse_unleased_native_result(&h.request.key, NativeResultRefusal::AccessDenied),
        Err(BotLedgerError::Fenced)
    );
    assert_eq!(
        h.db.settle_native_policy(&narrow.key).unwrap().state,
        BotPolicyBarrierState::Pending
    );
}

#[test]
fn changed_payload_cross_fence_and_wrong_evidence_refuse_without_effect_license() {
    let mut h = Harness::new();
    let original = h.prepare();
    for index in 0..5 {
        let mut request = h.request.clone();
        match index {
            0 => request.title.push('x'),
            1 => request.request_fingerprint[0] ^= 1,
            2 => request.destination = fixtures::workspace(2),
            3 => request.key.attempt = RunAttemptId::mint(),
            _ => request.key.turn = AgentTurnId::mint(),
        };
        assert_eq!(
            h.db.prepare_native_result(&request),
            Err(BotLedgerError::Conflict)
        );
    }
    for index in 0..5 {
        let mut fence = h.fence.clone();
        match index {
            0 => fence.bot = BotId::mint(),
            1 => fence.run = RunId::mint(),
            2 => fence.turn = Some(AgentTurnId::mint()),
            3 => fence.session = SessionId::mint(),
            _ => fence.attempt = RunAttemptId::mint(),
        };
        assert!(
            h.db.lease_native_result(
                &h.request.key,
                h.request.request_fingerprint,
                &fence,
                &h.policy
            )
            .is_err()
        );
    }
    assert_eq!(
        h.db.native_result(&h.request.key).unwrap().unwrap(),
        original
    );
    h.lease();
    let mut evidence = h.evidence();
    evidence.document = fixtures::document(2);
    assert_eq!(
        h.db.confirm_native_result(&ConfirmNativeResult {
            key: h.request.key.clone(),
            request_fingerprint: h.request.request_fingerprint,
            observation: NativeResultObservation::Durable { evidence }
        }),
        Err(BotLedgerError::Conflict)
    );
    assert_eq!(
        h.db.native_result(&h.request.key)
            .unwrap()
            .unwrap()
            .lease_state,
        NativeLeaseState::Held
    );
}

#[test]
fn captured_policy_cannot_widen_and_only_verified_creation_conveys_provenance() {
    let mut h = Harness::new();
    h.prepare();
    h.lease();
    let evidence = h.evidence();
    assert!(
        !h.db
            .created_native_page(
                h.fence.owner,
                h.fence.bot,
                evidence.workspace,
                evidence.document
            )
            .unwrap()
    );
    h.confirm(NativeResultObservation::Durable {
        evidence: evidence.clone(),
    });
    assert!(
        h.db.created_native_page(
            h.fence.owner,
            h.fence.bot,
            evidence.workspace,
            evidence.document
        )
        .unwrap()
    );
    assert!(
        !h.db
            .created_native_page(
                h.fence.owner,
                BotId::mint(),
                evidence.workspace,
                evidence.document
            )
            .unwrap()
    );
    h.request.key.operation_id = Uuid::new_v4();
    h.request.kind = NativeResultOperation::EditPage;
    h.request.target = Some(fixtures::document(3));
    h.request.expected_revision = Some(vec![1]);
    h.prepare();
    let mut widened = h.policy.clone();
    widened.key.request = BotRequestId::mint();
    widened.config_revision = 2;
    widened.policy_revision = 2;
    widened.policy.access = BotAccess::Workspace;
    h.db.install_native_policy(&widened).unwrap();
    assert_eq!(
        h.db.lease_native_result(
            &h.request.key,
            h.request.request_fingerprint,
            &h.fence,
            &widened
        ),
        Err(BotLedgerError::Fenced)
    );
    // Creation evidence permits default editing without rewriting captured policy.
    h.request.key.operation_id = Uuid::new_v4();
    h.request.target = Some(evidence.document);
    h.prepare();
    h.policy = widened;
    assert!(h.lease().execute.is_some());
    let edited = h.evidence();
    assert_eq!(edited.kind, NativeResultKind::EditedPage);
    h.confirm(NativeResultObservation::Durable { evidence: edited });
    let count:i64=h.db.conn().query_row("SELECT count(*) FROM native_result_intents WHERE kind='create_page' AND stage IN ('verified','registered')",[],|r|r.get(0)).unwrap();
    assert_eq!(count, 1);
}

#[test]
fn keyset_recovery_passes_more_than_one_page_of_permanent_uncertainty() {
    let mut h = Harness::new();
    for _ in 0..260 {
        h.request.key.operation_id = Uuid::new_v4();
        h.prepare();
    }
    let first = h.db.pending_native_results(None, u16::MAX).unwrap();
    assert_eq!(first.len(), 256);
    let second =
        h.db.pending_native_results(Some(&first.last().unwrap().cursor), 256)
            .unwrap();
    assert_eq!(second.len(), 4);
    assert!(second.iter().all(|r| !first.iter().any(|p| p.key == r.key)));
    assert!(
        h.db.pending_native_results(Some(&second.last().unwrap().cursor), 256)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn terminal_attempt_reconciles_confirmed_partial_output_but_cannot_acquire_new_lease() {
    let mut h = Harness::new();
    h.prepare();
    h.lease();
    let evidence = h.evidence();
    h.db.advance(
        h.fence.run,
        h.fence.generation,
        crate::AttemptPhase::Interrupted,
    )
    .unwrap();
    h.db.retire(h.fence.run, h.fence.generation).unwrap();
    h.confirm(NativeResultObservation::Durable {
        evidence: evidence.clone(),
    });
    h.db.register_native_result(&h.request.key, &evidence)
        .unwrap();
    assert!(h.lease().execute.is_none());
    h.request.key.operation_id = Uuid::new_v4();
    assert_eq!(
        h.db.prepare_native_result(&h.request),
        Err(BotLedgerError::Fenced)
    );
}

#[test]
fn every_native_write_reuses_the_existing_crash_commit_boundary() {
    for op in [
        WriteOp::InstallNativePolicy,
        WriteOp::SettleNativePolicy,
        WriteOp::PrepareNativeResult,
        WriteOp::LeaseNativeResult,
        WriteOp::RefuseUnleasedNativeResult,
        WriteOp::ConfirmNativeResult,
        WriteOp::RegisterNativeResult,
    ] {
        for at in Moment::ALL {
            let mut h = Harness::new();
            let newer = h.narrow();
            if op != WriteOp::InstallNativePolicy && op != WriteOp::PrepareNativeResult {
                h.prepare();
            }
            if matches!(
                op,
                WriteOp::SettleNativePolicy
                    | WriteOp::ConfirmNativeResult
                    | WriteOp::RegisterNativeResult
            ) {
                h.lease();
            }
            if op == WriteOp::SettleNativePolicy {
                h.db.install_native_policy(&newer).unwrap();
                h.confirm(NativeResultObservation::NotApplied {
                    code: NativeResultRefusal::Unavailable,
                });
            }
            let evidence = if matches!(
                op,
                WriteOp::ConfirmNativeResult | WriteOp::RegisterNativeResult
            ) {
                Some(h.evidence())
            } else {
                None
            };
            if op == WriteOp::RegisterNativeResult {
                h.confirm(NativeResultObservation::Durable {
                    evidence: evidence.clone().unwrap(),
                });
            }
            let before = h.db.native_result(&h.request.key).unwrap();
            let before_policy = h.db.native_policy(&newer.key).unwrap();
            let action = |db: &mut RunsDb| -> Result<(), BotLedgerError> {
                match op {
                    WriteOp::InstallNativePolicy => db.install_native_policy(&newer).map(|_| ()),
                    WriteOp::SettleNativePolicy => db.settle_native_policy(&newer.key).map(|_| ()),
                    WriteOp::PrepareNativeResult => {
                        db.prepare_native_result(&h.request).map(|_| ())
                    }
                    WriteOp::LeaseNativeResult => db
                        .lease_native_result(
                            &h.request.key,
                            h.request.request_fingerprint,
                            &h.fence,
                            &h.policy,
                        )
                        .map(|_| ()),
                    WriteOp::RefuseUnleasedNativeResult => db
                        .refuse_unleased_native_result(
                            &h.request.key,
                            NativeResultRefusal::Unavailable,
                        )
                        .map(|_| ()),
                    WriteOp::ConfirmNativeResult => db
                        .confirm_native_result(&ConfirmNativeResult {
                            key: h.request.key.clone(),
                            request_fingerprint: h.request.request_fingerprint,
                            observation: NativeResultObservation::Durable {
                                evidence: evidence.clone().unwrap(),
                            },
                        })
                        .map(|_| ()),
                    WriteOp::RegisterNativeResult => db
                        .register_native_result(&h.request.key, evidence.as_ref().unwrap())
                        .map(|_| ()),
                    _ => unreachable!(),
                }
            };
            let point = CrashPoint { op, at };
            h.db.arm_crash(point);
            assert_eq!(
                action(&mut h.db),
                Err(BotLedgerError::Ledger(LedgerError::Crashed(point)))
            );
            h.db.restart_in_memory();
            let after = h.db.native_result(&h.request.key).unwrap();
            let after_policy = h.db.native_policy(&newer.key).unwrap();
            if at == Moment::AfterCommit {
                assert!(
                    after != before || after_policy != before_policy,
                    "{point:?}"
                );
            } else {
                assert_eq!(after, before, "{point:?}");
                assert_eq!(after_policy, before_policy, "{point:?}");
            }
            action(&mut h.db).unwrap();
            if op == WriteOp::LeaseNativeResult {
                assert!(h.lease().execute.is_none());
            }
        }
    }
}

#[test]
fn sql_closed_states_and_nullable_cohorts_refuse_incomplete_evidence() {
    let mut h = Harness::new();
    h.prepare();
    h.lease();
    for column in [
        "lease_id",
        "lease_user_scope",
        "lease_device_id",
        "lease_bot_id",
        "lease_barrier_request_id",
        "lease_config_revision",
        "lease_policy_revision",
        "lease_payload_digest",
    ] {
        // Static internal column list; no caller SQL is accepted by the ledger.
        assert!(
            h.db.conn()
                .execute(
                    &format!("UPDATE native_result_intents SET {column}=NULL"),
                    []
                )
                .is_err(),
            "{column}"
        );
    }
    for sql in [
        "UPDATE native_result_intents SET stage='future'",
        "UPDATE native_result_intents SET kind='future'",
        "UPDATE native_result_intents SET expected_revision=X'01'",
        "UPDATE native_result_intents SET request_fingerprint=X'01'",
        "UPDATE native_result_intents SET lease_state='settled'",
        "UPDATE native_result_intents SET stage='refused',lease_state='settled'",
        "DELETE FROM bot_policy_barriers",
        "DELETE FROM run_attempts",
    ] {
        assert!(h.db.conn().execute(sql, []).is_err(), "{sql}");
    }
    let e = h.evidence();
    h.confirm(NativeResultObservation::Durable { evidence: e });
    for column in ["verified_version", "verified_title", "confirmed_at_ms"] {
        assert!(
            h.db.conn()
                .execute(
                    &format!("UPDATE native_result_intents SET {column}=NULL"),
                    []
                )
                .is_err(),
            "{column}"
        );
    }
    h.db.conn().execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE native_result_intents SET stage='unknown'; PRAGMA ignore_check_constraints=OFF;").unwrap();
    assert!(matches!(
        h.db.native_result(&h.request.key),
        Err(BotLedgerError::Ledger(LedgerError::Corrupt { .. }))
    ));
}

#[test]
fn migration_preserves_populated_runs_five_repeats_and_refuses_newer_memory_databases() {
    use koru_sqlite::Schema;
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    for step in crate::db::MIGRATIONS.iter().filter(|s| s.to <= 5) {
        conn.execute_batch(step.sql).unwrap();
    }
    conn.execute_batch("INSERT INTO run_intents VALUES('old','workspace','{\"objective\":\"kept\"}','satisfied','doc',1,2); INSERT INTO run_attempts(run_id,generation,attempt_id,state,updated_at_ms) VALUES('old',1,'attempt','ended',3); INSERT INTO pending_publications(event_id,run_id,kind,body_json,attempts,queued_at_ms) VALUES('event','old','lifecycle','{}',0,4); PRAGMA user_version=5;").unwrap();
    let schema = Schema {
        database: crate::DATABASE_NAME,
        understood: 6,
        migrations: crate::db::MIGRATIONS,
    };
    koru_sqlite::prepare(&schema, &mut conn).unwrap();
    koru_sqlite::prepare(&schema, &mut conn).unwrap();
    assert_eq!(
        koru_sqlite::user_version(crate::DATABASE_NAME, &conn).unwrap(),
        6
    );
    assert_eq!(
        conn.query_row(
            "SELECT payload_json FROM run_intents WHERE run_id='old'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "{\"objective\":\"kept\"}"
    );
    for table in ["run_attempts", "pending_publications"] {
        assert_eq!(
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    assert_eq!(
        conn.query_row("SELECT count(*) FROM native_result_intents", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    conn.execute_batch("PRAGMA user_version=7").unwrap();
    assert!(matches!(
        koru_sqlite::prepare(&schema, &mut conn),
        Err(koru_sqlite::OpenError::SchemaTooNew { .. })
    ));
    assert_eq!(
        koru_sqlite::user_version(crate::DATABASE_NAME, &conn).unwrap(),
        7
    );
}

#[test]
fn corrupt_creation_receipt_never_becomes_a_default_edit_grant() {
    let mut h = Harness::new();
    h.prepare();
    h.lease();
    let evidence = h.evidence();
    h.confirm(NativeResultObservation::Durable {
        evidence: evidence.clone(),
    });
    h.db.conn()
        .execute(
            "UPDATE native_result_intents SET store_request_id='not-a-request-id'",
            [],
        )
        .unwrap();
    assert!(matches!(
        h.db.created_native_page(
            h.fence.owner,
            h.fence.bot,
            evidence.workspace,
            evidence.document
        ),
        Err(BotLedgerError::Ledger(LedgerError::Corrupt { .. }))
    ));
}

#[test]
fn new_wrong_destination_and_effective_disabled_policy_never_acquire_a_lease() {
    let mut h = Harness::new();
    let original = h.request.destination;
    h.request.destination = fixtures::workspace(3);
    assert_eq!(
        h.db.prepare_native_result(&h.request),
        Err(BotLedgerError::Fenced)
    );
    assert!(h.db.native_result(&h.request.key).unwrap().is_none());
    h.request.destination = original;
    h.prepare();
    let disabled = h.narrow();
    assert_eq!(
        h.db.install_native_policy(&disabled).unwrap().state,
        BotPolicyBarrierState::Effective
    );
    assert_eq!(
        h.db.lease_native_result(
            &h.request.key,
            h.request.request_fingerprint,
            &h.fence,
            &disabled
        ),
        Err(BotLedgerError::Fenced)
    );
    assert_eq!(
        h.db.native_result(&h.request.key)
            .unwrap()
            .unwrap()
            .lease_state,
        NativeLeaseState::None
    );
}
