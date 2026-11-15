// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn decision_migration_runs10_closed_kind_authorization_phase_matrix_and_signed_times() {
    let c = current();
    for kind in ["koru_edit", "native_approval"] {
        for auth in [
            "pending", "granted", "denied", "canceled", "expired", "consumed",
        ] {
            for phase in [
                "publishing",
                "holding",
                "held",
                "native_wait",
                "resume_prepared",
                "await_retry",
                "effect",
                "native_dispatch",
                "native_unknown",
                "stop_pending",
                "settled",
            ] {
                let auth_phases: &[&str] = match auth {
                    "pending" => &["publishing", "holding", "held", "native_wait"],
                    "granted" => &[
                        "held",
                        "resume_prepared",
                        "await_retry",
                        "native_wait",
                        "native_dispatch",
                        "native_unknown",
                    ],
                    "consumed" => &[
                        "effect",
                        "native_dispatch",
                        "native_unknown",
                        "stop_pending",
                        "settled",
                    ],
                    "denied" => &[
                        "native_dispatch",
                        "native_unknown",
                        "stop_pending",
                        "settled",
                    ],
                    _ => &["stop_pending", "settled"],
                };
                let allowed = auth_phases.contains(&phase)
                    && !(kind == "koru_edit"
                        && ["native_wait", "native_dispatch", "native_unknown"].contains(&phase))
                    && !(kind == "native_approval"
                        && ["resume_prepared", "await_retry", "effect"].contains(&phase));
                check(&c, &format!("{kind}/{auth}/{phase}"), allowed, || {
                    insert(&c, "bot_decisions", &decision(kind, auth, phase)).map(|_| ())
                });
            }
        }
    }
    let row = decision("koru_edit", "pending", "publishing");
    for (column, value) in [
        ("kind", text("other")),
        ("authorization", text("other")),
        ("phase", text("other")),
        ("binding_version", integer(2)),
        ("deadline_at_ms", integer(-100)),
        ("deadline_at_ms", integer(599901)),
        ("origin_generation", integer(0)),
        ("origin_generation", integer(4294967296)),
        ("revision", integer(0)),
    ] {
        row_case(&c, "bot_decisions", &row, column, value, false);
    }
    for column in ["created_at_ms", "last_observed_at_ms", "updated_at_ms"] {
        let mut signed = row.clone();
        signed.insert(column, integer(i64::MIN));
        if column == "created_at_ms" {
            signed.insert("deadline_at_ms", integer(i64::MIN + 600000));
        }
        check(&c, column, true, || {
            insert(&c, "bot_decisions", &signed).map(|_| ())
        });
    }
}

#[test]
fn decision_migration_runs10_decision_null_types_utf8_and_exact_payload_boundaries() {
    let c = current();
    let row = decision("koru_edit", "granted", "held");
    // Each required field is tested at insertion; immutable UPDATE guards cannot
    // accidentally mask a missing NOT NULL or storage-type constraint here.
    for column in [
        "decision_id",
        "run_id",
        "user_scope",
        "device_id",
        "bot_id",
        "origin_attempt_id",
        "origin_generation",
        "origin_session_id",
        "origin_turn_id",
        "kind",
        "human_profile_id",
        "binding_version",
        "binding_digest",
        "binding_json",
        "created_boot_id",
        "created_at_ms",
        "deadline_at_ms",
        "last_observed_at_ms",
        "revision",
        "authorization",
        "phase",
        "updated_at_ms",
    ] {
        row_case(&c, "bot_decisions", &row, column, Value::Null, false);
    }
    for column in [
        "created_at_ms",
        "deadline_at_ms",
        "last_observed_at_ms",
        "revision",
        "origin_generation",
        "updated_at_ms",
    ] {
        row_case(&c, "bot_decisions", &row, column, Value::Real(1.5), false);
    }
    for column in ["binding_digest", "answer_proof_digest"] {
        for value in [blob(31), blob(33), text("x".repeat(32))] {
            row_case(&c, "bot_decisions", &row, column, value, false);
        }
    }
    for column in [
        "decision_id",
        "user_scope",
        "device_id",
        "bot_id",
        "origin_session_id",
        "origin_turn_id",
        "human_profile_id",
        "created_boot_id",
        "operation_id",
        "answer_request_id",
        "answer_profile_id",
    ] {
        for length in [35, 37] {
            row_case(
                &c,
                "bot_decisions",
                &row,
                column,
                text("x".repeat(length)),
                false,
            );
        }
    }
    for (column, limit) in [
        ("binding_json", 4194304),
        ("answer_choice_json", 16384),
        ("answer_receipt_json", 65536),
    ] {
        for value in [
            text(""),
            text("not json"),
            Value::Blob(b"{}".to_vec()),
            text(format!("\"{}\"", "x".repeat(limit - 1))),
        ] {
            row_case(&c, "bot_decisions", &row, column, value, false);
        }
        row_case(
            &c,
            "bot_decisions",
            &row,
            column,
            text(format!("\"{}\"", "x".repeat(limit - 2))),
            true,
        );
    }
    // SQL limits count UTF-8 bytes, not Unicode scalar values.
    let mut native = decision("native_approval", "pending", "native_wait");
    native.insert("native_deadline_at_ms", integer(599899));
    check(
        &c,
        "native deadline before decision deadline",
        false,
        || insert(&c, "bot_decisions", &native).map(|_| ()),
    );
    native.remove("native_deadline_at_ms");
    row_case(
        &c,
        "bot_decisions",
        &native,
        "native_prompt_id",
        text("é".repeat(2048)),
        true,
    );
    row_case(
        &c,
        "bot_decisions",
        &native,
        "native_prompt_id",
        text("é".repeat(2049)),
        false,
    );
    for column in [
        "answer_request_id",
        "answer_profile_id",
        "answer_choice_json",
        "answer_at_ms",
        "answer_proof_digest",
        "answer_receipt_json",
    ] {
        row_case(&c, "bot_decisions", &row, column, Value::Null, false);
    }
}

#[test]
fn decision_migration_runs10_answer_identity_receipts_and_retention_are_immutable() {
    let c = current();
    insert(
        &c,
        "bot_decisions",
        &decision("koru_edit", "granted", "held"),
    )
    .unwrap();
    for sql in [
        "UPDATE bot_decisions SET decision_id=NULL",
        "UPDATE bot_decisions SET answer_request_id=NULL",
        "UPDATE bot_decisions SET answer_choice_json='{\"changed\":true}'",
        "UPDATE bot_decisions SET answer_receipt_json='{\"changed\":true}'",
        "UPDATE bot_decisions SET answer_at_ms=0",
        "UPDATE bot_decisions SET binding_json='{\"changed\":true}'",
        "DELETE FROM bot_decisions",
        "DELETE FROM bot_admissions",
        "DELETE FROM run_attempts",
    ] {
        check(&c, sql, false, || c.execute_batch(sql));
    }
    check(&c, "exact no-op receipt update", true, || {
        c.execute_batch(
            "UPDATE bot_decisions SET answer_receipt_json=answer_receipt_json,revision=2",
        )
    });
    check(&c, "new second outstanding decision", false, || {
        let mut second = decision("koru_edit", "pending", "publishing");
        second.insert("decision_id", text(id(22)));
        second.insert("operation_id", text(id(23)));
        insert(&c, "bot_decisions", &second).map(|_| ())
    });
}

#[test]
fn decision_migration_runs10_wait_request_ack_bounds_and_immutable_retained_release() {
    let c = current();
    insert(
        &c,
        "bot_decisions",
        &decision("koru_edit", "granted", "held"),
    )
    .unwrap();
    let row = wait();
    for column in [
        "wait_id",
        "run_id",
        "user_scope",
        "device_id",
        "bot_id",
        "attempt_id",
        "generation",
        "session_id",
        "turn_id",
        "reason",
        "phase",
        "revision",
        "created_at_ms",
        "updated_at_ms",
    ] {
        row_case(&c, "bot_wait_runtime", &row, column, Value::Null, false);
    }
    for column in [
        "ai_enrollment_request_json",
        "release_evidence_json",
        "cleanup_evidence_json",
    ] {
        for value in [
            text(""),
            text("bad"),
            Value::Blob(b"{}".to_vec()),
            text(format!("\"{}\"", "x".repeat(4095))),
        ] {
            row_case(&c, "bot_wait_runtime", &row, column, value, false);
        }
        row_case(
            &c,
            "bot_wait_runtime",
            &row,
            column,
            text(format!("\"{}\"", "x".repeat(4094))),
            true,
        );
    }
    row_case(
        &c,
        "bot_wait_runtime",
        &row,
        "ai_cleanup_receipt_json",
        text("{}"),
        false,
    );
    let mut acknowledged = row.clone();
    acknowledged.insert("ai_enrollment_request_json", text("{}"));
    for value in [
        text(""),
        text("bad"),
        Value::Blob(b"{}".to_vec()),
        text(format!("\"{}\"", "x".repeat(4095))),
    ] {
        row_case(
            &c,
            "bot_wait_runtime",
            &acknowledged,
            "ai_cleanup_receipt_json",
            value,
            false,
        );
    }
    row_case(
        &c,
        "bot_wait_runtime",
        &acknowledged,
        "ai_cleanup_receipt_json",
        text(format!("\"{}\"", "x".repeat(4094))),
        true,
    );
    for column in [
        "generation",
        "revision",
        "created_at_ms",
        "updated_at_ms",
        "observed_sequence",
    ] {
        row_case(
            &c,
            "bot_wait_runtime",
            &row,
            column,
            Value::Real(1.5),
            false,
        );
    }
    row_case(
        &c,
        "bot_wait_runtime",
        &row,
        "observed_sequence",
        integer(-1),
        false,
    );
    insert(&c, "bot_wait_runtime", &row).unwrap();
    c.execute_batch("UPDATE bot_wait_runtime SET ai_enrollment_request_json='{}',ai_cleanup_receipt_json='{}'; UPDATE bot_wait_runtime SET phase='hold_requested'; UPDATE bot_wait_runtime SET phase='held',release_evidence_json='{}'; UPDATE bot_wait_runtime SET phase='resume_reserved';").unwrap();
    for sql in [
        "UPDATE bot_wait_runtime SET ai_enrollment_request_json=NULL",
        "UPDATE bot_wait_runtime SET ai_enrollment_request_json='{\"changed\":true}'",
        "UPDATE bot_wait_runtime SET ai_cleanup_receipt_json='{\"changed\":true}'",
        "UPDATE bot_wait_runtime SET release_evidence_json=NULL",
        "UPDATE bot_wait_runtime SET release_evidence_json='{\"changed\":true}'",
        "UPDATE bot_wait_runtime SET phase='held'",
        "UPDATE bot_wait_runtime SET phase='retired'",
        "DELETE FROM bot_wait_runtime",
    ] {
        check(&c, sql, false, || c.execute_batch(sql));
    }
    assert_eq!(
        c.query_row(
            "SELECT release_evidence_json FROM bot_wait_runtime",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "{}"
    );
    c.execute_batch("UPDATE bot_wait_runtime SET phase='cleanup_pending'; UPDATE bot_wait_runtime SET phase='settled',settled_at_ms=-70;").unwrap();
    check(&c, "settled rollback", false, || {
        c.execute_batch("UPDATE bot_wait_runtime SET phase='cleanup_pending',settled_at_ms=NULL")
    });
}

#[test]
fn decision_migration_runs10_wait_closed_phase_edges_and_retained_resume_refuse_rehold() {
    let c = current();
    insert(
        &c,
        "bot_decisions",
        &decision("koru_edit", "granted", "held"),
    )
    .unwrap();
    for old in [
        "fenced",
        "hold_requested",
        "held",
        "retired",
        "resume_reserved",
        "cleanup_pending",
        "settled",
    ] {
        for new in [
            "fenced",
            "hold_requested",
            "held",
            "retired",
            "resume_reserved",
            "cleanup_pending",
            "settled",
        ] {
            let allowed = old == new
                || matches!(
                    (old, new),
                    ("fenced", "hold_requested" | "cleanup_pending" | "settled")
                        | ("hold_requested", "held" | "retired" | "cleanup_pending")
                        | ("held" | "retired", "resume_reserved" | "cleanup_pending")
                        | ("resume_reserved", "cleanup_pending" | "settled")
                        | ("cleanup_pending", "settled")
                );
            check(&c, &format!("wait {old}->{new}"), allowed, || {
                // Insertion state is an owning-writer invariant, not a SQL claim.
                let mut row = wait();
                row.insert("phase", text(old));
                row.insert("release_evidence_json", text("{}"));
                if old == "settled" {
                    row.insert("settled_at_ms", integer(-70));
                }
                insert(&c, "bot_wait_runtime", &row)?;
                c.execute(
                    "UPDATE bot_wait_runtime SET phase=?1,settled_at_ms=?2",
                    rusqlite::params![new, (new == "settled").then_some(-70_i64)],
                )?;
                Ok(())
            });
        }
    }
    insert(&c, "bot_decision_resumes", &resume(false)).unwrap();
    for phase in ["held", "retired"] {
        check(&c, phase, false, || {
            insert(&c, "bot_wait_runtime", &wait())?;
            c.execute_batch("UPDATE bot_wait_runtime SET phase='hold_requested'")?;
            c.execute(
                "UPDATE bot_wait_runtime SET phase=?1,release_evidence_json='{}'",
                [phase],
            )
            .map(|_| ())
        });
    }
}

#[test]
fn decision_migration_runs10_resume_full_key_nullability_receipt_and_lease_foreign_keys() {
    let c = current();
    insert(
        &c,
        "bot_decisions",
        &decision("koru_edit", "granted", "resume_prepared"),
    )
    .unwrap();
    row_case(
        &c,
        "bot_decision_resumes",
        &resume(false),
        "consumer_session_id",
        Value::Null,
        false,
    );
    row_case(
        &c,
        "bot_decision_resumes",
        &resume(true),
        "consumer_session_id",
        Value::Null,
        true,
    );
    for column in [
        "decision_id",
        "run_id",
        "kind",
        "origin_attempt_id",
        "origin_generation",
        "origin_session_id",
        "consumer_attempt_id",
        "consumer_generation",
        "session_creation_key",
        "input_key",
        "input_fingerprint",
        "state",
        "created_at_ms",
        "updated_at_ms",
    ] {
        row_case(
            &c,
            "bot_decision_resumes",
            &resume(false),
            column,
            Value::Null,
            false,
        );
    }
    row_case(
        &c,
        "bot_decision_resumes",
        &resume(false),
        "consumer_attempt_id",
        text(id(12)),
        false,
    );
    row_case(
        &c,
        "bot_decision_resumes",
        &resume(true),
        "consumer_generation",
        integer(1),
        false,
    );
    row_case(
        &c,
        "bot_decision_resumes",
        &resume(true),
        "consumer_generation",
        integer(3),
        false,
    );
    row_case(
        &c,
        "bot_decision_resumes",
        &resume(false),
        "state",
        text("input_acknowledged"),
        false,
    );
    let mut acknowledged = resume(false);
    acknowledged.extend([
        ("state", text("input_acknowledged")),
        ("accepted_turn_id", text(id(33))),
        ("receipt_json", text("{}")),
    ]);
    for column in ["accepted_turn_id", "receipt_json"] {
        row_case(
            &c,
            "bot_decision_resumes",
            &acknowledged,
            column,
            Value::Null,
            false,
        );
    }
    for value in [
        text(""),
        text("bad"),
        Value::Blob(b"{}".to_vec()),
        text(format!("\"{}\"", "x".repeat(65535))),
    ] {
        row_case(
            &c,
            "bot_decision_resumes",
            &acknowledged,
            "receipt_json",
            value,
            false,
        );
    }
    row_case(
        &c,
        "bot_decision_resumes",
        &acknowledged,
        "receipt_json",
        text(format!("\"{}\"", "x".repeat(65534))),
        true,
    );
    insert(&c, "bot_decision_resumes", &resume(false)).unwrap();
    for sql in [
        "UPDATE bot_decision_resumes SET input_key='changed'",
        "UPDATE bot_decision_resumes SET consumer_generation=2",
        "DELETE FROM bot_decision_resumes",
    ] {
        check(&c, sql, false, || c.execute_batch(sql));
    }
    c.execute("UPDATE bot_decision_resumes SET state='input_acknowledged',accepted_turn_id=?1,receipt_json='{}'",[id(33)]).unwrap();
    check(&c, "acknowledged resume immutable", false, || {
        c.execute_batch("UPDATE bot_decision_resumes SET receipt_json='{\"changed\":true}'")
    });
    let lease = Row::from([
        ("run_id", text(id(1))),
        ("operation_id", text(id(14))),
        ("decision_id", text(id(20))),
        ("binding_digest", blob(32)),
        ("consumer_attempt_id", text(id(11))),
        ("consumer_generation", integer(1)),
        ("consumer_turn_id", text(id(33))),
        ("consumed_at_ms", integer(-70)),
    ]);
    for column in lease.keys() {
        row_case(
            &c,
            "native_decision_leases",
            &lease,
            column,
            Value::Null,
            false,
        );
    }
    for value in [blob(31), blob(33), text("x".repeat(32))] {
        row_case(
            &c,
            "native_decision_leases",
            &lease,
            "binding_digest",
            value,
            false,
        );
    }
    row_case(
        &c,
        "native_decision_leases",
        &lease,
        "operation_id",
        text(id(99)),
        false,
    );
    insert(&c, "native_decision_leases", &lease).unwrap();
    for sql in [
        "UPDATE native_decision_leases SET consumed_at_ms=0",
        "DELETE FROM native_decision_leases",
        "DELETE FROM native_result_intents",
        "DELETE FROM run_attempts",
    ] {
        check(&c, sql, false, || c.execute_batch(sql));
    }
    assert_fk(&c);
}

#[test]
fn decision_migration_runs10_native_answer_choice_groups_bounds_and_unknown_ack_retention() {
    let c = current();
    insert(
        &c,
        "bot_decisions",
        &decision("native_approval", "granted", "native_dispatch"),
    )
    .unwrap();
    let row = Row::from([
        ("decision_id", text(id(20))),
        ("session_id", text(id(12))),
        ("turn_id", text(id(13))),
        ("prompt_id", text("prompt")),
        ("response_key", text(id(50))),
        ("choice_id", text("allow")),
        ("choice_intent", text("allow")),
        ("native_decision", text("approve")),
        ("state", text("unconfirmed")),
        ("created_at_ms", integer(-100)),
        ("updated_at_ms", integer(-90)),
    ]);
    for column in row.keys() {
        row_case(
            &c,
            "bot_decision_native_answers",
            &row,
            column,
            Value::Null,
            false,
        );
    }
    for (intent, answer) in [
        ("allow", "approve"),
        ("allow_remembered", "approve"),
        ("reject", "reject"),
        ("reject_remembered", "reject"),
    ] {
        let mut choice = row.clone();
        choice.insert("choice_intent", text(intent));
        choice.insert("native_decision", text(answer));
        check(&c, intent, true, || {
            insert(&c, "bot_decision_native_answers", &choice).map(|_| ())
        });
    }
    row_case(
        &c,
        "bot_decision_native_answers",
        &row,
        "native_decision",
        text("reject"),
        false,
    );
    row_case(
        &c,
        "bot_decision_native_answers",
        &row,
        "choice_intent",
        text("other"),
        false,
    );
    row_case(
        &c,
        "bot_decision_native_answers",
        &row,
        "state",
        text("acknowledged"),
        false,
    );
    for column in ["prompt_id", "choice_id"] {
        row_case(
            &c,
            "bot_decision_native_answers",
            &row,
            column,
            text("é".repeat(2048)),
            true,
        );
        row_case(
            &c,
            "bot_decision_native_answers",
            &row,
            column,
            text("é".repeat(2049)),
            false,
        );
        row_case(
            &c,
            "bot_decision_native_answers",
            &row,
            column,
            text(""),
            false,
        );
    }
    let mut acknowledged = row.clone();
    acknowledged.insert("state", text("acknowledged"));
    for value in [
        text(""),
        text("bad"),
        Value::Blob(b"{}".to_vec()),
        text(format!("\"{}\"", "x".repeat(65535))),
    ] {
        row_case(
            &c,
            "bot_decision_native_answers",
            &acknowledged,
            "acknowledgment_json",
            value,
            false,
        );
    }
    row_case(
        &c,
        "bot_decision_native_answers",
        &acknowledged,
        "acknowledgment_json",
        text(format!("\"{}\"", "x".repeat(65534))),
        true,
    );
    insert(&c, "bot_decision_native_answers", &row).unwrap();
    c.execute_batch(
        "UPDATE bot_decision_native_answers SET state='acknowledged',acknowledgment_json='{}'",
    )
    .unwrap();
    for sql in [
        "UPDATE bot_decision_native_answers SET acknowledgment_json=NULL",
        "UPDATE bot_decision_native_answers SET response_key='changed'",
        "UPDATE bot_decision_native_answers SET state='unconfirmed',acknowledgment_json=NULL",
        "DELETE FROM bot_decision_native_answers",
    ] {
        check(&c, sql, false, || c.execute_batch(sql));
    }
}
