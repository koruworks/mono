// SPDX-License-Identifier: MPL-2.0
use super::*;

fn populated_eight() -> Connection {
    let mut h = Harness::new();
    // Actual repository-written Result stages and held/released/drained evidence.
    for cohort in 0..9 {
        h.request.key.operation_id = Uuid::new_v4();
        h.prepare();
        if cohort != 0 {
            h.lease();
        }
        if cohort == 2 {
            h.confirm(NativeResultObservation::Unconfirmed);
        }
        if cohort == 5 {
            h.confirm(NativeResultObservation::NotApplied {
                code: NativeResultRefusal::AccessDenied,
            });
        }
        if cohort >= 6 {
            let record = h.db.native_result(&h.request.key).unwrap().unwrap();
            let receipt = NativeEffectDrainReceipt {
                binding: record.pending_effect().unwrap().binding,
                fenced_at: NativeEffectTimestamp::from_millis(900),
                drained_at: NativeEffectTimestamp::from_millis(800),
            };
            h.confirm(NativeResultObservation::DrainedUnknown { receipt });
        }
        if matches!(cohort, 3 | 4 | 7 | 8) {
            let evidence = h.evidence();
            h.confirm(NativeResultObservation::Durable {
                evidence: evidence.clone(),
            });
            if matches!(cohort, 4 | 8) {
                h.db.register_native_result(&h.request.key, &evidence)
                    .unwrap();
            }
        }
    }
    for cohort in 0..3 {
        h.request.key.operation_id = Uuid::new_v4();
        let r = super::super::request(&h, true);
        super::super::allocate(&mut h, &r);
        let resolved = super::super::resolution(&r);
        super::super::resolve(&mut h, &r, &resolved);
        super::super::prepare(&mut h, &r, &resolved);
        if cohort > 0 {
            h.lease();
        }
        if cohort == 2 {
            let evidence = h.evidence();
            h.confirm(NativeResultObservation::Durable { evidence });
        }
    }
    let original = h.db.bot_admission(h.fence.run).unwrap().unwrap().submission;
    for cohort in 0..4 {
        let mut submission = original.clone();
        submission.request.run = RunId::mint();
        submission.provenance.bot = BotId::mint();
        submission.provenance.submission = BotRequestId::mint();
        h.db.admit_bot(&submission).unwrap();
        match cohort {
            1 => {
                assert_eq!(
                    h.db.reserve_bot(submission.request.run, None).unwrap(),
                    BotReservation::Acquired
                );
            }
            2 => {
                h.db.settle_bot(submission.request.run, crate::BotSettlement::Settled)
                    .unwrap()
            }
            3 => {
                h.db.settle_bot(submission.request.run, crate::BotSettlement::Refused)
                    .unwrap()
            }
            _ => {}
        }
    }
    h.db.apply_bot_barrier(&koru_delegate_core::BotAdmissionBarrier {
        owner: original.provenance.owner,
        bot: original.provenance.bot,
        request: BotRequestId::mint(),
        epoch: 1,
        paused: false,
    })
    .unwrap();
    let ordinary = fixtures::request(0x40);
    let schedule = fixtures::schedule(0x41);
    h.db.claim_scheduled_run_at(schedule, 100, 1, &ordinary, original.workspace, 1000)
        .unwrap();
    h.db.record_outcome(schedule, 100, crate::SlotOutcome::Fired)
        .unwrap();
    h.db.claim_skip_at(
        schedule,
        200,
        1,
        RunId::mint(),
        crate::SlotOutcome::SkippedMissed,
        None,
        1000,
    )
    .unwrap();
    h.db.claim_skip_at(
        schedule,
        300,
        1,
        RunId::mint(),
        crate::SlotOutcome::SkippedBusy,
        Some(ordinary.run),
        1000,
    )
    .unwrap();
    for cohort in 0..3 {
        let event = fixtures::event(0x50 + cohort);
        h.db.prepare_schedule_publication(
            event,
            crate::SchedulePublicationRef {
                schedule: fixtures::schedule(0x50 + cohort),
                workspace: original.workspace,
                document: fixtures::document(0x50 + cohort),
            },
            crate::SchedulePublicationFact::Created { revision: 1 },
            r#"{"preserve":"schedule publication bytes"}"#,
        )
        .unwrap();
        if cohort > 0 {
            h.db.mark_schedule_publication_ready(event).unwrap();
        }
        if cohort == 2 {
            h.db.acknowledge_schedule_publication(event).unwrap();
        }
    }
    let mut conn = Connection::open_in_memory().unwrap();
    koru_sqlite::prepare(&schema(), &mut conn).unwrap();
    conn.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    for table in tables(&conn) {
        let cols = columns(&conn, &table);
        for row in rows(h.db.conn(), &table, &cols) {
            let marks = vec!["?"; cols.len()].join(",");
            conn.execute(
                &format!("INSERT INTO {table}({}) VALUES({marks})", cols.join(",")),
                rusqlite::params_from_iter(row),
            )
            .unwrap();
        }
    }
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    for (query, expected) in [
        ("SELECT count(DISTINCT state) FROM bot_admissions", 5),
        ("SELECT count(*) FROM schedule_claims", 3),
        ("SELECT count(*) FROM schedule_publications", 3),
        ("SELECT count(*) FROM bot_admission_barriers", 1),
        ("SELECT count(*) FROM run_attempts", 1),
        (
            "SELECT count(*) FROM native_result_intents WHERE effect_purpose='result'",
            9,
        ),
        (
            "SELECT count(*) FROM native_result_intents WHERE effect_purpose='memory'",
            3,
        ),
        (
            "SELECT count(*) FROM bot_memory_edit_allocations WHERE resolution_state='resolved'",
            3,
        ),
    ] {
        assert_eq!(
            conn.query_row(query, [], |r| r.get::<_, i64>(0)).unwrap(),
            expected,
            "{query}"
        );
    }
    for state in ["none", "held", "settled"] {
        assert!(
            conn.query_row(
                "SELECT count(*) FROM native_result_intents WHERE lease_state=?1",
                [state],
                |r| r.get::<_, i64>(0)
            )
            .unwrap()
                > 0
        );
    }
    for stage in [
        "prepared",
        "unconfirmed",
        "verified",
        "registered",
        "refused",
        "settled_unknown",
    ] {
        assert!(
            conn.query_row(
                "SELECT count(*) FROM native_result_intents WHERE stage=?1",
                [stage],
                |r| r.get::<_, i64>(0)
            )
            .unwrap()
                > 0
        );
    }
    assert_fk(&conn);
    conn
}
fn nine() -> koru_sqlite::Schema {
    koru_sqlite::Schema {
        database: crate::DATABASE_NAME,
        understood: 9,
        migrations: &crate::db::MIGRATIONS[..9],
    }
}
#[test]
fn runs_nine_preserves_populated_eight_bytes_indexes_and_memory_authority() {
    let mut conn = populated_eight();
    let before = snapshot(&conn);
    let indexes: Vec<String> = conn
        .prepare(
            "SELECT sql FROM sqlite_master WHERE type='index' AND sql IS NOT NULL ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    koru_sqlite::prepare(&nine(), &mut conn).unwrap();
    koru_sqlite::prepare(&nine(), &mut conn).unwrap();
    for (table, cols, data) in before {
        assert_eq!(rows(&conn, &table, &cols), data, "{table}");
    }
    for index in indexes {
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND sql=?1",
                [index],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM bot_admissions WHERE submission_version<>1",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM native_result_intents WHERE effect_purpose='memory'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        3
    );
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        9
    );
    assert_fk(&conn);
}
#[test]
fn runs_nine_failure_rolls_back_alter_new_objects_stamp_and_all_eight_rows() {
    let mut conn = populated_eight();
    // Conflict after ALTER and the first three new tables/indexes, inside the real runner.
    conn.execute_batch("CREATE TABLE native_routine_leases(sentinel TEXT); INSERT INTO native_routine_leases VALUES('kept');").unwrap();
    let before = snapshot(&conn);
    assert!(koru_sqlite::prepare(&nine(), &mut conn).is_err());
    assert_eq!(snapshot(&conn), before);
    assert!(!columns(&conn, "bot_admissions").contains(&"submission_version".into()));
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        8
    );
    assert_fk(&conn);
}

#[test]
fn decision_migration_runs10_preserves_result_memory_schedule_and_routine_history_cohorts() {
    let mut conn = populated_eight();
    koru_sqlite::prepare(&nine(), &mut conn).unwrap();
    conn.execute_batch("INSERT INTO bot_routine_routes(schedule_id,user_scope,device_id,bot_id,workspace_id,creation_request_id,seed_hash,created_at_ms) VALUES('schedule-fixture','scope-fixture','device-fixture','bot-fixture','workspace-fixture','creation-fixture',zeroblob(32),-100);
      INSERT INTO bot_routine_policy_barriers(user_scope,device_id,bot_id,schedule_id,request_id,grant_revision,payload_digest,effects_allowed,state,created_at_ms,updated_at_ms) VALUES('scope-fixture','device-fixture','bot-fixture','schedule-fixture','policy-fixture',1,zeroblob(32),0,'pending',-100,-90);").unwrap();
    let before = snapshot(&conn);
    let old_objects: Vec<(String,String)> = conn.prepare("SELECT name,coalesce(sql,'') FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name").unwrap()
        .query_map([],|r|Ok((r.get(0)?,r.get(1)?))).unwrap().collect::<Result<_,_>>().unwrap();
    koru_sqlite::prepare(
        &koru_sqlite::Schema {
            database: crate::DATABASE_NAME,
            understood: 10,
            migrations: &crate::db::MIGRATIONS[..10],
        },
        &mut conn,
    )
    .unwrap();
    for (table, cols, data) in before {
        assert_eq!(rows(&conn, &table, &cols), data, "old bytes: {table}");
    }
    for (name, sql) in old_objects {
        assert_eq!(
            conn.query_row(
                "SELECT coalesce(sql,'') FROM sqlite_master WHERE name=?1",
                [&name],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            sql,
            "old object: {name}"
        );
    }
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        10
    );
    assert_fk(&conn);
}
