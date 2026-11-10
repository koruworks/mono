// SPDX-License-Identifier: MPL-2.0
use super::*;

fn proof(h: &Harness) -> NativeEffectDrainReceipt {
    let r = h.db.native_result(&h.request.key).unwrap().unwrap();
    NativeEffectDrainReceipt {
        binding: r.pending_effect().unwrap().binding,
        fenced_at: NativeEffectTimestamp::from_millis(900),
        drained_at: NativeEffectTimestamp::from_millis(800),
    }
}
#[test]
fn drain_receipt_crashes_rollback_or_replay_the_exact_committed_proof() {
    for moment in Moment::ALL {
        let mut h = Harness::new();
        h.prepare();
        h.lease();
        let receipt = proof(&h);
        let request = ConfirmNativeResult {
            key: h.request.key.clone(),
            request_fingerprint: h.request.request_fingerprint,
            observation: NativeResultObservation::DrainedUnknown {
                receipt: receipt.clone(),
            },
        };
        h.db.arm_crash(CrashPoint {
            op: WriteOp::ConfirmNativeResult,
            at: moment,
        });
        assert!(h.db.confirm_native_result(&request).is_err());
        let retained = h.db.native_result(&request.key).unwrap().unwrap();
        assert_eq!(retained.drain.is_some(), moment == Moment::AfterCommit);
        h.db.restart_in_memory();
        let replay = h.db.confirm_native_result(&request).unwrap();
        assert_eq!(replay.drain, Some(receipt));
        assert_eq!(replay.stage, NativeResultStage::SettledUnknown);
        assert!(h.lease().execute.is_none());
    }
}
#[test]
fn drain_is_exact_absorbing_and_only_durable_evidence_can_upgrade() {
    let mut h = Harness::new();
    h.prepare();
    h.lease();
    let receipt = proof(&h);
    let r = h.confirm(NativeResultObservation::DrainedUnknown {
        receipt: receipt.clone(),
    });
    assert_eq!(r.stage, NativeResultStage::SettledUnknown);
    assert_eq!(r.drain, Some(receipt.clone()));
    assert!(!h.db.native_effects_pending(r.key.run).unwrap());
    assert!(h.db.native_outcomes_unknown(r.key.run).unwrap());
    assert!(h.lease().execute.is_none());
    assert_eq!(
        h.confirm(NativeResultObservation::DrainedUnknown {
            receipt: receipt.clone()
        }),
        r
    );
    assert_eq!(
        h.confirm(NativeResultObservation::NotApplied {
            code: NativeResultRefusal::AccessDenied
        }),
        r
    );
    assert_eq!(h.confirm(NativeResultObservation::Unconfirmed), r);
    for field in 0..4 {
        let mut changed = receipt.clone();
        match field {
            0 => changed.fenced_at = NativeEffectTimestamp::from_millis(1),
            1 => changed.drained_at = NativeEffectTimestamp::from_millis(1),
            2 => changed.binding.request_fingerprint = [9; 32],
            _ => changed.binding.document = DocumentId::from_uuid(Uuid::new_v4()),
        };
        assert!(
            h.db.confirm_native_result(&ConfirmNativeResult {
                key: r.key.clone(),
                request_fingerprint: r.fingerprint,
                observation: NativeResultObservation::DrainedUnknown { receipt: changed }
            })
            .is_err()
        );
    }
    let evidence = h.evidence();
    let verified = h.confirm(NativeResultObservation::Durable {
        evidence: evidence.clone(),
    });
    assert_eq!(verified.drain, Some(receipt.clone()));
    assert!(h.db.native_effects_pending(r.key.run).unwrap());
    h.db.register_native_result(&r.key, &evidence).unwrap();
    assert_eq!(
        h.confirm(NativeResultObservation::DrainedUnknown { receipt })
            .stage,
        NativeResultStage::Registered
    );
    assert!(!h.db.native_outcomes_unknown(r.key.run).unwrap());
}

#[test]
fn held_pages_seek_both_indexes_and_advance_past_each_full_page() {
    let mut h = Harness::new();
    for _ in 0..67 {
        h.request.key.operation_id = Uuid::new_v4();
        h.prepare();
        h.lease();
    }
    let owner = h.fence.owner;
    for bot in [None, Some(h.fence.bot)] {
        let first = h.db.pending_native_effects(owner, bot, None, 64).unwrap();
        assert_eq!(first.targets.len(), 64);
        let second =
            h.db.pending_native_effects(owner, bot, first.next.as_ref(), 64)
                .unwrap();
        assert_eq!(second.targets.len(), 3);
        assert!(second.next.is_none());
        let all = first
            .targets
            .iter()
            .chain(&second.targets)
            .map(|t| t.key.operation_id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(all.len(), 67);
        let query = if bot.is_some() {
            super::super::drain::BOT_QUERY
        } else {
            super::super::drain::OWNER_QUERY
        };
        let mut statement =
            h.db.conn()
                .prepare(&format!("EXPLAIN QUERY PLAN {query}"))
                .unwrap();
        let mut values: Vec<rusqlite::types::Value> = vec![
            owner.user_scope.to_storage().into(),
            owner.device.to_storage().into(),
        ];
        if let Some(bot) = bot {
            values.push(bot.to_storage().into());
        }
        let prefix = values.clone();
        values.extend([
            i64::MIN.into(),
            String::new().into(),
            String::new().into(),
            64i64.into(),
        ]);
        let plan = statement
            .query_map(rusqlite::params_from_iter(values), |r| {
                r.get::<_, String>(3)
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join(" ");
        assert!(
            plan.contains(if bot.is_some() {
                "native_result_held_bot_recovery"
            } else {
                "native_result_held_owner_recovery"
            }),
            "{plan}"
        );
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
        let cursor = first.next.as_ref().unwrap();
        let mut values = prefix;
        values.extend([
            cursor.created_at_ms.into(),
            cursor.run.to_storage().into(),
            cursor.operation_id.to_string().into(),
            64i64.into(),
        ]);
        let plan = statement
            .query_map(rusqlite::params_from_iter(values), |r| {
                r.get::<_, String>(3)
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join(" ");
        assert!(
            plan.contains(if bot.is_some() {
                "native_result_held_bot_recovery"
            } else {
                "native_result_held_owner_recovery"
            }),
            "{plan}"
        );
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
    }
}

#[test]
fn recovery_reads_identity_columns_without_deserializing_task_payloads() {
    let mut h = Harness::new();
    h.prepare();
    h.lease();
    h.db.conn()
        .execute("UPDATE bot_admissions SET payload_json='{}'", [])
        .unwrap();
    assert_eq!(
        h.db.pending_native_effects(h.fence.owner, Some(h.fence.bot), None, 1)
            .unwrap()
            .targets
            .len(),
        1
    );
    h.db.conn()
        .execute(
            "UPDATE bot_admissions SET bot_id=?1",
            [BotId::mint().to_storage()],
        )
        .unwrap();
    assert!(
        h.db.pending_native_effects(h.fence.owner, None, None, 1)
            .is_err()
    );
    for limit in [0, 65] {
        assert!(
            h.db.pending_native_effects(h.fence.owner, None, None, limit)
                .is_err()
        );
    }
}

#[test]
fn migration_preserves_populated_six_and_nullable_drain_guards_refuse_partial_proof() {
    let mut h = Harness::new();
    h.prepare();
    h.lease();
    // Rebuild a real v6 table from its immutable migration, then reopen through
    // the current migration owner. All ordinary ledger/admission rows remain.
    let before = h.db.native_result(&h.request.key).unwrap().unwrap();
    let sql = include_str!("../../../migrations/025_runs_native_results_migration.sql");
    let start = sql.find("CREATE TABLE native_result_intents (").unwrap();
    let end = sql
        .find("CREATE UNIQUE INDEX native_result_created_identity")
        .unwrap();
    let table = sql[start..end].replace(
        "CREATE TABLE native_result_intents (",
        "CREATE TABLE old_native (",
    );
    h.db.conn().execute_batch(&table).unwrap();
    let columns: Vec<String> =
        h.db.conn()
            .prepare("SELECT name FROM pragma_table_info('old_native')")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
    let columns = columns.join(",");
    h.db.conn().execute_batch(&format!("INSERT INTO old_native({columns}) SELECT {columns} FROM native_result_intents; DROP TABLE native_result_intents; ALTER TABLE old_native RENAME TO native_result_intents; PRAGMA user_version=6;")).unwrap();
    h.db.migrate_in_memory();
    h.db.migrate_in_memory();
    assert_eq!(h.db.schema_version().unwrap(), 7);
    assert_eq!(h.db.native_result(&h.request.key).unwrap().unwrap(), before);
    assert!(
        h.db.conn()
            .execute("UPDATE native_result_intents SET effect_fenced_at_ms=1", [])
            .is_err()
    );
    assert!(
        h.db.conn()
            .execute(
                "UPDATE native_result_intents SET stage='settled_unknown',lease_state='settled'",
                []
            )
            .is_err()
    );
    let mut older_reader = rusqlite::Connection::open_in_memory().unwrap();
    older_reader
        .execute_batch(
            "CREATE TABLE kept(value); INSERT INTO kept VALUES(1); PRAGMA user_version=7;",
        )
        .unwrap();
    let older = koru_sqlite::Schema {
        database: crate::DATABASE_NAME,
        understood: 6,
        migrations: &crate::db::MIGRATIONS[..6],
    };
    assert!(matches!(
        koru_sqlite::prepare(&older, &mut older_reader),
        Err(koru_sqlite::OpenError::SchemaTooNew {
            discovered: 7,
            supported: 6,
            ..
        })
    ));
    assert_eq!(
        older_reader
            .query_row("SELECT value FROM kept", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
