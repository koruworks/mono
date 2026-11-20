// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn retired_bot_backlog_cannot_hide_ordinary_publications_at_the_limit() {
    let mut db = RunsDb::open(std::path::Path::new(":memory:")).unwrap();
    let retired = created_run(&mut db, 1);
    let ordinary = created_run(&mut db, 2);
    // Only the scope discriminator is relevant to this bounded query. Its
    // payload must never be decoded or replayed by the new path.
    db.conn().execute("UPDATE run_intents SET payload_json=json_set(payload_json, '$.scope.kind', 'bot_folder') WHERE run_id=?1", [retired.to_storage()]).unwrap();
    db.enqueue(event(1), retired, PublicationKind::Lifecycle, &body(1))
        .unwrap();
    db.enqueue(event(2), ordinary, PublicationKind::Lifecycle, &body(2))
        .unwrap();
    let rows = db.open_publications_for(None, 1).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].run, ordinary);
    let anchor = db.pending_publication_run_anchor().unwrap().unwrap();
    assert!(
        matches!(db.next_pending_publication_run(None, anchor, None).unwrap(),
        PendingPublicationRunSeek::Candidate { run, .. } if run == ordinary)
    );
    assert_eq!(
        db.publications(retired).unwrap().len() + db.publications(ordinary).unwrap().len(),
        2,
        "retirement must not acknowledge or delete anything"
    );
}

#[test]
fn recovery_excludes_retired_bot_rows_without_changing_them() {
    let mut db = RunsDb::open(std::path::Path::new(":memory:")).unwrap();
    let retired = created_run(&mut db, 1);
    let ordinary = created_run(&mut db, 2);
    db.open_attempt(retired).unwrap();
    db.open_attempt(ordinary).unwrap();
    db.conn().execute("UPDATE run_intents SET payload_json=json_set(payload_json, '$.scope.kind', 'bot_folder') WHERE run_id=?1", [retired.to_storage()]).unwrap();
    let rows = db.intents().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].run, ordinary);
    let live = db.live_attempts().unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].run, ordinary);
    assert!(db.intent_facts(retired).unwrap().is_none());
    assert!(db.open_attempt(retired).is_err());
    let count: i64 = db
        .conn()
        .query_row("SELECT count(*) FROM run_attempts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 2);
    let mut request = crate::fixtures::request(3);
    request.scope = koru_delegate_core::WorkspaceScope::BotFolder {
        folder: koru_delegate_core::BotWorkingFolder::new("D:/retained/bot".into()).unwrap(),
    };
    assert!(
        db.record_intent(&request, crate::fixtures::workspace(4))
            .is_err()
    );
    assert!(
        db.validate_intent(&request, crate::fixtures::workspace(4))
            .is_err()
    );
}

#[test]
fn corrupt_unknown_inventory_still_reaches_recovery_fencing() {
    let mut db = RunsDb::open(std::path::Path::new(":memory:")).unwrap();
    let run = created_run(&mut db, 1);
    db.open_attempt(run).unwrap();
    db.conn()
        .execute(
            "UPDATE run_intents SET payload_json='{' WHERE run_id=?1",
            [run.to_storage()],
        )
        .unwrap();
    assert_eq!(db.intents().unwrap().len(), 1);
    assert_eq!(db.live_attempts().unwrap().len(), 1);
    assert!(db.intent_facts(run).is_err());
    db.enqueue(event(1), run, PublicationKind::Lifecycle, &body(1))
        .unwrap();
    assert!(db.open_publications(10).unwrap().is_empty());
}
