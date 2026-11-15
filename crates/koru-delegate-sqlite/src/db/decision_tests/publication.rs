// SPDX-License-Identifier: MPL-2.0
use super::*;

fn publication(sequence: i64, kind: &str, state: &str) -> Row {
    let mut row = Row::from([
        ("transition_id", text(id(100 + sequence as u32))),
        ("run_id", text(id(1))),
        ("wait_id", text(id(40))),
        ("run_sequence", integer(sequence)),
        ("workspace_id", text(id(2))),
        ("document_id", text(id(3))),
        ("source_schema", integer(5)),
        ("subject_digest", blob(32)),
        ("target_json", text("{}")),
        ("target_digest", blob(32)),
        ("projection_kind", text(kind)),
        ("state", text(state)),
        ("created_at_ms", integer(-50)),
    ]);
    if state == "prepared" {
        row.extend([
            ("base_version", blob(1)),
            ("result_version", blob(1)),
            ("prepared_delta", blob(1)),
            ("delta_digest", blob(32)),
        ]);
    }
    row
}
fn parents(c: &Connection) {
    insert(
        c,
        "bot_decisions",
        &decision("koru_edit", "pending", "held"),
    )
    .unwrap();
    insert(c, "bot_wait_runtime", &wait()).unwrap();
}

#[test]
fn decision_migration_runs10_publication_classes_partial_indexes_and_owner_only_shape_boundary() {
    let c = current();
    parents(&c);
    let shapes: &[&[(&str, &str)]] = &[
        &[],
        &[("source", "pending")],
        &[("source", "prepared")],
        &[("control", "pending")],
        &[("control", "prepared")],
        &[("source", "pending"), ("control", "pending")],
        &[("source", "prepared"), ("control", "pending")],
        &[("control", "prepared"), ("control", "pending")],
    ];
    for rows in shapes {
        check(&c, "reachable publication shape", true, || {
            for (i, (kind, state)) in rows.iter().enumerate() {
                insert(
                    &c,
                    "bot_decision_publications",
                    &publication(i as i64 + 1, kind, state),
                )?;
            }
            Ok(())
        });
    }
    for (a, b) in [
        (("source", "pending"), ("source", "pending")),
        (("control", "pending"), ("control", "pending")),
        (("source", "prepared"), ("control", "prepared")),
    ] {
        check(&c, "duplicate open index", false, || {
            insert(&c, "bot_decision_publications", &publication(1, a.0, a.1))?;
            insert(&c, "bot_decision_publications", &publication(2, b.0, b.1)).map(|_| ())
        });
    }
    // The final contract deliberately assigns max2/order to the future sole
    // writer+decoder. These scalar rows prove indexes are not that authority.
    check(
        &c,
        "third row is an owning decoder obligation",
        true,
        || {
            insert(
                &c,
                "bot_decision_publications",
                &publication(1, "control", "prepared"),
            )?;
            insert(
                &c,
                "bot_decision_publications",
                &publication(2, "source", "pending"),
            )?;
            insert(
                &c,
                "bot_decision_publications",
                &publication(3, "control", "pending"),
            )
            .map(|_| ())
        },
    );
    for kind in [Value::Null, text("other")] {
        row_case(
            &c,
            "bot_decision_publications",
            &publication(1, "source", "pending"),
            "projection_kind",
            kind,
            false,
        );
    }
}

#[test]
fn decision_migration_runs10_publication_payload_null_cohorts_and_exact_byte_limits() {
    let c = current();
    parents(&c);
    let row = publication(1, "source", "prepared");
    for column in [
        "transition_id",
        "run_id",
        "wait_id",
        "run_sequence",
        "workspace_id",
        "document_id",
        "source_schema",
        "subject_digest",
        "projection_kind",
        "target_json",
        "target_digest",
        "state",
        "created_at_ms",
        "base_version",
        "result_version",
        "prepared_delta",
        "delta_digest",
    ] {
        row_case(
            &c,
            "bot_decision_publications",
            &row,
            column,
            Value::Null,
            false,
        );
    }
    for column in ["base_version", "result_version", "prepared_delta"] {
        let limit = if column == "prepared_delta" {
            262144
        } else {
            65536
        };
        row_case(
            &c,
            "bot_decision_publications",
            &row,
            column,
            blob(limit),
            true,
        );
        for value in [blob(0), blob(limit + 1), text("x")] {
            row_case(&c, "bot_decision_publications", &row, column, value, false);
        }
    }
    for column in ["subject_digest", "target_digest", "delta_digest"] {
        for value in [blob(31), blob(33), text("x".repeat(32))] {
            row_case(&c, "bot_decision_publications", &row, column, value, false);
        }
    }
    row_case(
        &c,
        "bot_decision_publications",
        &row,
        "target_json",
        text(format!("\"{}\"", "x".repeat(65534))),
        true,
    );
    for value in [
        text(""),
        text("bad"),
        Value::Blob(b"{}".to_vec()),
        text(format!("\"{}\"", "x".repeat(65535))),
    ] {
        row_case(
            &c,
            "bot_decision_publications",
            &row,
            "target_json",
            value,
            false,
        );
    }
    for (column, value) in [
        ("decision_revision", integer(1)),
        ("run_sequence", integer(0)),
        ("run_sequence", Value::Real(1.5)),
        ("source_schema", integer(6)),
        ("state", text("unknown")),
    ] {
        row_case(&c, "bot_decision_publications", &row, column, value, false);
    }
}

#[test]
fn decision_migration_runs10_source_never_superseded_control_deferred_fk_and_prepared_receipts_immutable()
 {
    let c = current();
    parents(&c);
    for kind in ["source", "control"] {
        check(
            &c,
            &format!("{kind} supersession"),
            kind == "control",
            || {
                insert(
                    &c,
                    "bot_decision_publications",
                    &publication(1, kind, "pending"),
                )?;
                c.execute_batch("UPDATE bot_decision_publications SET state='superseded',completed_at_ms=-40,superseded_by_sequence=2")?;
                insert(
                    &c,
                    "bot_decision_publications",
                    &publication(2, "control", "pending"),
                )?;
                assert_fk(&c);
                Ok(())
            },
        );
    }
    // The deferred successor FK is checked by a real transaction COMMIT, not
    // by a savepoint rollback that would silently discard the missing parent.
    c.execute_batch("BEGIN").unwrap();
    insert(
        &c,
        "bot_decision_publications",
        &publication(1, "control", "pending"),
    )
    .unwrap();
    c.execute_batch("UPDATE bot_decision_publications SET state='superseded',completed_at_ms=-40,superseded_by_sequence=2").unwrap();
    assert!(c.execute_batch("COMMIT").is_err());
    c.execute_batch("ROLLBACK").unwrap();
    insert(
        &c,
        "bot_decision_publications",
        &publication(1, "source", "prepared"),
    )
    .unwrap();
    for sql in [
        "UPDATE bot_decision_publications SET projection_kind='control'",
        "UPDATE bot_decision_publications SET prepared_delta=X'01'",
        "UPDATE bot_decision_publications SET result_version=X'01'",
        "UPDATE bot_decision_publications SET state='pending',base_version=NULL,result_version=NULL,prepared_delta=NULL,delta_digest=NULL",
        "DELETE FROM bot_decision_publications",
        "DELETE FROM bot_wait_runtime",
    ] {
        check(&c, sql, false, || c.execute_batch(sql));
    }
    c.execute_batch("UPDATE bot_decision_publications SET state='applied',observed_version=X'01',completed_at_ms=-30").unwrap();
    check(&c, "applied receipt immutable", false, || {
        c.execute_batch("UPDATE bot_decision_publications SET observed_version=X'02'")
    });
    assert_fk(&c);
}

#[test]
fn decision_migration_runs10_pending_bus_index_seeks_skip_long_prefix_and_keep_rowid_fifo() {
    let c = current();
    let unavailable = "a".repeat(36);
    let eligible = "b".repeat(36);
    c.execute_batch("SAVEPOINT bus_fixture").unwrap();
    for n in 0..4096 {
        c.execute("INSERT INTO pending_publications(event_id,run_id,kind,body_json,queued_at_ms,acknowledged_at_ms) VALUES(?1,?2,'fixture','{}',?3,?4)",rusqlite::params![format!("acked-{n}"),eligible,n,1]).unwrap();
        c.execute("INSERT INTO pending_publications(event_id,run_id,kind,body_json,queued_at_ms) VALUES(?1,?2,'fixture','{}',?3)",rusqlite::params![format!("blocked-{n}"),unavailable,n]).unwrap();
    }
    for n in (0..128).rev() {
        c.execute("INSERT INTO pending_publications(event_id,run_id,kind,body_json,queued_at_ms) VALUES(?1,?2,'fixture','{}',?3)",rusqlite::params![format!("eligible-{n}"),eligible,n]).unwrap();
    }
    const ANCHOR: &str = "SELECT max(run_id) FROM pending_publications INDEXED BY pending_publications_open_run WHERE acknowledged_at_ms IS NULL";
    const SEEK: &str = "SELECT min(run_id) FROM pending_publications INDEXED BY pending_publications_open_run WHERE acknowledged_at_ms IS NULL AND run_id>?1 AND run_id<=?2";
    const FIFO: &str = "SELECT event_id,run_id,kind,body_json,attempts,queued_at_ms FROM pending_publications INDEXED BY pending_publications_open_run WHERE acknowledged_at_ms IS NULL AND run_id=?1 ORDER BY rowid LIMIT ?2";
    for (sql, params) in [
        (ANCHOR, Vec::<Value>::new()),
        (SEEK, vec![text(&unavailable), text(&eligible)]),
        (FIFO, vec![text(&eligible), integer(64)]),
    ] {
        let plan: Vec<String> = c
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map(params_from_iter(params), |r| r.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(
            plan.iter()
                .all(|p| !p.contains("SCAN") && !p.contains("TEMP B-TREE")),
            "{plan:?}"
        );
        assert!(
            plan.iter()
                .any(|p| p.contains("SEARCH") && p.contains("pending_publications_open_run")),
            "{plan:?}"
        );
    }
    let anchor: String = c.query_row(ANCHOR, [], |r| r.get(0)).unwrap();
    assert_eq!(anchor, eligible);
    let next: String = c
        .query_row(SEEK, [&unavailable, &anchor], |r| r.get(0))
        .unwrap();
    assert_eq!(next, eligible);
    let rows: Vec<String> = c
        .prepare(FIFO)
        .unwrap()
        .query_map(rusqlite::params![next, 64], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        (64..128)
            .rev()
            .map(|n| format!("eligible-{n}"))
            .collect::<Vec<_>>()
    );
    c.execute_batch("ROLLBACK TO bus_fixture; RELEASE bus_fixture")
        .unwrap();
}
