// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::ports::UpdateOrigin;

#[test]
fn cloud_origin_round_trips_through_commit_and_a_new_repository() {
    let vectors: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../contracts/vectors/cloud-sync-admission-v1/foundation.json"
    ))
    .unwrap();
    let origin: UpdateOrigin =
        serde_json::from_str(vectors["provenance"][1]["json"].as_str().unwrap()).unwrap();
    let (mut workspace, document) = with_document();
    let mut update = batch(document, vec![vec![1, 2, 3]]);
    update.origins = vec![origin.clone()];
    workspace.commit_updates(&update).unwrap();
    // A new repository has no prior in-memory origin state. The database is
    // kept in memory for this default test; disk/process restart is separate.
    let SqliteWorkspace { connection } = workspace;
    let reopened = SqliteWorkspace::from_connection(connection).unwrap();
    assert_eq!(
        reopened.updates_after(document, None).unwrap()[0].origin,
        origin
    );
}

#[test]
fn null_legacy_rows_stay_null_but_unknown_nonnull_origins_are_corrupt() {
    let (mut workspace, document) = with_document();
    workspace
        .commit_updates(&batch(document, vec![vec![1]]))
        .unwrap();
    let client = ClientId::new();
    workspace
        .connection
        .execute(
            "UPDATE loro_updates SET origin_json=NULL, from_client=?1",
            [client.to_storage()],
        )
        .unwrap();
    assert_eq!(
        workspace.updates_after(document, None).unwrap()[0].origin,
        UpdateOrigin::LocalClient { client_id: client }
    );
    let value: Option<Vec<u8>> = workspace
        .connection
        .query_row("SELECT origin_json FROM loro_updates", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, None);
    for invalid in [
        b"null".as_slice(),
        b"{",
        br#"{"kind":"future"}"#,
        br#"{"kind":"cloud","logical_replica_id":[]}"#,
        br#"{"kind":"store","unexpected":true}"#,
        br#"{"kind":"store","kind":"store"}"#,
        br#"{"kind":"cloud","logical_replica_id":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"unexpected":true}"#,
        br#"{"kind":"cloud","logical_replica_id":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],"logical_replica_id":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}"#,
    ] {
        workspace
            .connection
            .execute("UPDATE loro_updates SET origin_json=?1", [invalid])
            .unwrap();
        assert_eq!(
            workspace.updates_after(document, None).unwrap_err().code(),
            Code::CorruptState
        );
    }
}

#[test]
fn schema_twelve_has_no_structural_change_and_old_writer_refuses_before_mutation() {
    let mut connection = db::open_in_memory().unwrap();
    let old = &MIGRATIONS[..MIGRATIONS
        .iter()
        .position(|migration| migration.to == 12)
        .unwrap()];
    db::migrate(&mut connection, "workspace.db", old, 11).unwrap();
    fn shape(connection: &Connection) -> Vec<(String, Option<String>)> {
        connection
            .prepare("SELECT name, sql FROM sqlite_master ORDER BY name")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    }
    let before = shape(&connection);
    let through_twelve = &MIGRATIONS[..MIGRATIONS
        .iter()
        .position(|migration| migration.to == 12)
        .unwrap()
        + 1];
    db::migrate(&mut connection, "workspace.db", through_twelve, 12).unwrap();
    let mut upgraded = SqliteWorkspace { connection };
    assert_eq!(upgraded.schema_version().unwrap(), 12);
    assert_eq!(shape(&upgraded.connection), before);
    let error = db::migrate(&mut upgraded.connection, "workspace.db", old, 11).unwrap_err();
    assert_eq!(error.code(), Code::SchemaTooNew);
    assert_eq!(upgraded.schema_version().unwrap(), 12);
    assert_eq!(shape(&upgraded.connection), before);
}
