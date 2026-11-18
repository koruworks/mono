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
    let rows = db.open_publications_for_mode(None, 1, false).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].run, ordinary);
    let anchor = db.pending_publication_run_anchor().unwrap().unwrap();
    assert!(
        matches!(db.next_pending_publication_run_for_mode(None, anchor, None, false).unwrap(),
        PendingPublicationRunSeek::Candidate { run, .. } if run == ordinary)
    );
    assert_eq!(
        db.open_publications(10).unwrap().len(),
        2,
        "retirement must not acknowledge or delete anything"
    );
}
