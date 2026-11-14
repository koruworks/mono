// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::{BotLedger, fixtures};

fn ordinary() -> (RunsDb, koru_delegate_core::BotSubmission) {
    let mut db = crate::bots::routines::tests::memory();
    let s = crate::bots::tests::submission(2);
    db.admit_bot(&s).unwrap();
    (db, s)
}

#[test]
fn routine_marker_valid_ordinary_admission_and_exact_replay_remain_available() {
    let (mut db, s) = ordinary();
    deny_run_metadata(db.conn(), s.request.run).unwrap();
    deny_run(db.conn(), s.request.run).unwrap();
    assert_eq!(db.pending_intents().unwrap().len(), 1);
    assert_eq!(db.admit_bot(&s).unwrap().submission, s);
}

#[test]
fn routine_marker_ordinary_intent_has_no_new_aggregate_payload_limit() {
    let db = crate::bots::routines::tests::memory();
    let request = fixtures::request(3);
    let mut payload = serde_json::to_value(&request).unwrap();
    payload.as_object_mut().unwrap().remove("origin"); // retained legacy default
    payload["future_ordinary_field"] = serde_json::json!("x".repeat(4 * 1024 * 1024 + 1));
    let payload = serde_json::to_string(&payload).unwrap();
    assert!(payload.len() > 4 * 1024 * 1024);
    db.conn().execute("INSERT INTO run_intents(run_id,workspace_id,payload_json,state,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,'pending',0,0)",
        params![request.run.to_storage(),fixtures::workspace(1).to_storage(),payload]).unwrap();
    deny_run(db.conn(), request.run).unwrap();
    assert_eq!(
        db.pending_intents().unwrap()[0].payload.origin,
        RunOrigin::Manual
    );
}

#[test]
fn routine_marker_origin_projection_matches_manual_and_scheduled_owning_serde() {
    let (db, s) = ordinary();
    for origin in [
        serde_json::json!({"origin":"manual","schedule":null,"occurrence_ms":null,"revision":null}),
        serde_json::json!({"origin":"manual","ignored":"legacy"}),
        serde_json::json!({"origin":"scheduled","schedule":fixtures::schedule(0x77),"occurrence_ms":17,"revision":1}),
    ] {
        let expected: RunOrigin = serde_json::from_value(origin.clone()).unwrap();
        let mut payload = serde_json::to_value(&s.request).unwrap();
        payload["origin"] = origin;
        db.conn()
            .execute(
                "UPDATE run_intents SET payload_json=?1",
                [serde_json::to_string(&payload).unwrap()],
            )
            .unwrap();
        deny_run(db.conn(), s.request.run).unwrap();
        assert_eq!(db.pending_intents().unwrap()[0].payload.origin, expected);
    }
}

#[test]
fn routine_marker_invalid_origin_is_corruption_not_ordinary_negative() {
    let (db, s) = ordinary();
    for origin in [
        serde_json::Value::Null,
        serde_json::json!([]),
        serde_json::json!("manual"),
        serde_json::json!({"origin":"scheduled","schedule":fixtures::schedule(0x77),"occurrence_ms":"17","revision":1}),
        serde_json::json!({"origin":"scheduled","schedule":3,"occurrence_ms":17,"revision":1}),
        serde_json::json!({"origin":"other"}),
    ] {
        assert!(serde_json::from_value::<RunOrigin>(origin.clone()).is_err());
        let mut payload = serde_json::to_value(&s.request).unwrap();
        payload["origin"] = origin;
        db.conn()
            .execute(
                "UPDATE run_intents SET payload_json=?1",
                [serde_json::to_string(&payload).unwrap()],
            )
            .unwrap();
        assert!(matches!(
            deny_run(db.conn(), s.request.run),
            Err(LedgerError::Corrupt { .. })
        ));
    }
}

#[test]
fn routine_marker_invalid_provenance_or_payload_is_corruption_not_negative() {
    let (db, s) = ordinary();
    for provenance in [
        serde_json::Value::Null,
        serde_json::json!([]),
        serde_json::json!(true),
        serde_json::json!({}),
    ] {
        let mut payload = serde_json::to_value(&s).unwrap();
        payload["provenance"] = provenance;
        db.conn()
            .execute(
                "UPDATE bot_admissions SET payload_json=?1",
                [serde_json::to_string(&payload).unwrap()],
            )
            .unwrap();
        assert!(matches!(
            deny_run_metadata(db.conn(), s.request.run),
            Err(LedgerError::Corrupt { .. })
        ));
    }
    db.conn()
        .execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    for payload in ["{", "null", "[]", "3"] {
        db.conn()
            .execute("UPDATE bot_admissions SET payload_json=?1", [payload])
            .unwrap();
        assert!(matches!(
            deny_run_metadata(db.conn(), s.request.run),
            Err(LedgerError::Corrupt { .. })
        ));
    }
}

#[test]
fn routine_marker_null_or_malformed_version_is_corruption_before_other_projection() {
    let (db, s) = ordinary();
    // A corrupt-schema fixture permits NULL, which the real NOT NULL column
    // cannot store. It exercises the shared projection's negative semantics.
    db.conn()
        .execute_batch(
            "PRAGMA foreign_keys=OFF; ALTER TABLE bot_admissions RENAME TO saved_admissions;
        CREATE TABLE bot_admissions(run_id TEXT PRIMARY KEY,submission_version,payload_json TEXT);",
        )
        .unwrap();
    db.conn().execute("INSERT INTO bot_admissions(run_id,submission_version,payload_json) VALUES(?1,NULL,'{}')", [s.request.run.to_storage()]).unwrap();
    assert!(matches!(
        deny_run_metadata(db.conn(), s.request.run),
        Err(LedgerError::Corrupt {
            column: "submission_version",
            ..
        })
    ));
    for value in ["bad", "1", "3"] {
        db.conn()
            .execute("UPDATE bot_admissions SET submission_version=?1", [value])
            .unwrap();
        assert!(matches!(
            deny_run_metadata(db.conn(), s.request.run),
            Err(LedgerError::Corrupt {
                column: "submission_version",
                ..
            })
        ));
    }
}

#[test]
fn routine_marker_admission_cap_is_existing_v1_owner_domain_and_routine_provenance_denies_downgrade()
 {
    let (mut db, mut s) = ordinary();
    let route = crate::bots::routines::tests::route(&s);
    crate::bots::routines::tests::routine(&mut s, &route);
    db.conn()
        .execute(
            "UPDATE bot_admissions SET payload_json=?1",
            [serde_json::to_string(&s).unwrap()],
        )
        .unwrap();
    assert!(matches!(
        deny_run_metadata(db.conn(), s.request.run),
        Err(LedgerError::InvalidScheduleAuthority)
    ));
    // The real v1 owner already rejects the same aggregate bound on insertion;
    // migration024's CHECK and repository decode also own it.
    let mut large = crate::bots::tests::submission(7);
    large.input.text = "x".repeat(4 * 1024 * 1024 + 1);
    assert!(db.admit_bot(&large).is_err());
    let oversized =
        serde_json::json!({"provenance":s.provenance,"padding":"x".repeat(4*1024*1024+1)})
            .to_string();
    assert!(
        db.conn()
            .execute("UPDATE bot_admissions SET payload_json=?1", [oversized])
            .is_err()
    );
}
