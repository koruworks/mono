// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_store_core::fixtures::{BIDI_LINE, SAMPLES, UNICODE_FILENAME};

mod client_receive;
mod cloud_origin;
mod commit_retry;

fn at() -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000)
}

fn repo() -> SqliteWorkspace {
    SqliteWorkspace::in_memory().expect("open")
}

fn has_column(connection: &Connection, table: &str, column: &str) -> bool {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .any(|name| name.unwrap() == column)
}

fn has_table(connection: &Connection, table: &str) -> bool {
    connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        > 0
}

fn schema_seven(origin_json: bool, mime: bool) -> Connection {
    let connection = db::open_in_memory().unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TABLE loro_updates (from_client TEXT{});
             CREATE TABLE documents (doc_id TEXT{});
             -- A real schema-7 database has the blobs table from migration 1;
             -- leaving it out would make later blob migrations fail on a shape
             -- no real database ever had.
             CREATE TABLE blobs (hash TEXT PRIMARY KEY, size INTEGER NOT NULL,
               mime TEXT NOT NULL, filename TEXT, created_at INTEGER NOT NULL);
             PRAGMA user_version = 7;",
            if origin_json {
                ", origin_json BLOB"
            } else {
                ""
            },
            if mime { ", mime TEXT" } else { "" },
        ))
        .unwrap();
    connection
}

#[test]
fn schema_eight_repairs_the_origin_only_schema_seven_shape() {
    let workspace = SqliteWorkspace::from_connection(schema_seven(true, false)).unwrap();

    assert_eq!(
        workspace.schema_version().unwrap(),
        WORKSPACE_SCHEMA_VERSION
    );
    assert!(has_column(
        &workspace.connection,
        "loro_updates",
        "origin_json"
    ));
    assert!(has_column(&workspace.connection, "documents", "mime"));
    // Migration 9 lands on the same open.
    assert!(has_column(&workspace.connection, "blobs", "attributes"));
}

#[test]
fn schema_eight_repairs_the_mime_only_schema_seven_shape() {
    let workspace = SqliteWorkspace::from_connection(schema_seven(false, true)).unwrap();

    assert_eq!(
        workspace.schema_version().unwrap(),
        WORKSPACE_SCHEMA_VERSION
    );
    assert!(has_column(
        &workspace.connection,
        "loro_updates",
        "origin_json"
    ));
    assert!(has_column(&workspace.connection, "documents", "mime"));
}

#[test]
fn a_schema_eight_database_missing_a_required_column_names_it_on_open() {
    let connection = schema_seven(true, false);
    connection.pragma_update(None, "user_version", 8).unwrap();

    let error = SqliteWorkspace::from_connection(connection)
        .err()
        .expect("incomplete schema must be refused");
    assert_eq!(error.code(), Code::CorruptState);
    assert!(error.message().contains("documents.mime"), "{error}");
}

/// The exact on-disk shape of a workspace stamped 9: everything the fixture
/// above builds, plus `blobs.attributes`.
fn schema_nine() -> Connection {
    let connection = schema_seven(true, true);
    connection
        .execute_batch("ALTER TABLE blobs ADD COLUMN attributes TEXT;")
        .unwrap();
    connection.pragma_update(None, "user_version", 9).unwrap();
    connection
}

#[test]
fn schema_nine_gains_blob_origins() {
    let workspace = SqliteWorkspace::from_connection(schema_nine()).unwrap();

    assert_eq!(
        workspace.schema_version().unwrap(),
        WORKSPACE_SCHEMA_VERSION
    );
    assert!(has_table(&workspace.connection, "blob_origins"));
}

#[test]
fn a_fresh_database_has_the_blob_origins_table() {
    // Upgrade fixtures do not catch invalid current DDL: this is the clean
    // install's shape, opened through production code.
    let workspace = repo();
    assert_eq!(
        workspace.schema_version().unwrap(),
        WORKSPACE_SCHEMA_VERSION
    );
    assert!(has_table(&workspace.connection, "blob_origins"));
}

/// A workspace with one document, ready to be written to.
fn with_document() -> (SqliteWorkspace, DocumentId) {
    let mut w = repo();
    let d = DocumentId::new();
    w.create_document(d, DocumentKind::Manifest, at()).unwrap();
    (w, d)
}

fn batch(document: DocumentId, updates: Vec<Vec<u8>>) -> UpdateBatch {
    let origins = vec![koru_store_core::ports::UpdateOrigin::Store; updates.len()];
    UpdateBatch {
        document,
        updates,
        version_vector: vec![1],
        frontiers: vec![2],
        received_at: at(),
        origins,
    }
}

fn checkpoint(seq: u64, snapshot: Vec<u8>) -> Checkpoint {
    Checkpoint {
        checksum: checksum(&snapshot),
        seq,
        snapshot,
        version_vector: vec![7],
        frontiers: vec![8],
        created_at: at(),
    }
}

// ------------------------------------------------------------------ documents

#[test]
fn a_document_starts_with_no_checkpoint_and_no_updates() {
    let (w, d) = with_document();
    let state = w.document(d).unwrap().expect("created");
    assert_eq!(state.kind, DocumentKind::Manifest);
    assert_eq!(state.latest_checkpoint_seq, None);
    assert_eq!(w.update_count(d).unwrap(), 0);
    assert!(w.updates_after(d, None).unwrap().is_empty());
}

#[test]
fn creating_the_same_document_twice_is_refused() {
    let (mut w, d) = with_document();
    assert_eq!(
        w.create_document(d, DocumentKind::Document, at())
            .unwrap_err()
            .code(),
        Code::DocumentExists
    );
}

fn seed(candidate: DocumentId, key: &str, hash: &str, created_at: Timestamp) -> SeedDocument {
    SeedDocument {
        candidate,
        recover_candidate: false,
        scope: "local:desktop-user".into(),
        key: key.into(),
        request_hash: hash.into(),
        initial_update: vec![1, 2, 3],
        version_vector: vec![4],
        frontiers: vec![5],
        created_at,
        expires_before: Timestamp::from_millis(created_at.millis() - 7 * 24 * 60 * 60 * 1_000),
    }
}

fn candidate_seed(
    candidate: DocumentId,
    key: &str,
    hash: &str,
    created_at: Timestamp,
) -> SeedDocument {
    let mut seed = seed(candidate, key, hash, created_at);
    seed.recover_candidate = true;
    seed
}

#[test]
fn seeded_creation_plants_content_and_idempotency_in_one_transaction() {
    let mut w = repo();
    let candidate = DocumentId::new();
    let first = w
        .seed_document(&seed(candidate, "same-key", "same-request", at()))
        .unwrap();
    assert_eq!(first.document, candidate);
    assert!(!first.replayed);
    assert_eq!(first.initial_update, vec![1, 2, 3]);
    assert_eq!(w.update_count(candidate).unwrap(), 1);
    assert_eq!(
        w.updates_after(candidate, None).unwrap()[0].bytes,
        vec![1, 2, 3]
    );

    let replay = w
        .seed_document(&seed(DocumentId::new(), "same-key", "same-request", at()))
        .unwrap();
    assert_eq!(replay.document, candidate);
    assert!(replay.replayed);
    assert_eq!(replay.initial_update, vec![1, 2, 3]);
    assert_eq!(w.documents().unwrap().len(), 1);
}

#[test]
fn an_idempotency_key_cannot_name_two_live_requests() {
    let mut w = repo();
    w.seed_document(&seed(DocumentId::new(), "same-key", "first", at()))
        .unwrap();
    let error = w
        .seed_document(&seed(DocumentId::new(), "same-key", "different", at()))
        .unwrap_err();
    assert_eq!(error.code(), Code::IdempotencyConflict);
    assert_eq!(w.documents().unwrap().len(), 1);
}

#[test]
fn an_expired_idempotency_key_can_name_a_new_request() {
    let mut w = repo();
    let first = DocumentId::new();
    w.seed_document(&seed(first, "reusable-key", "first", at()))
        .unwrap();

    let eight_days_later = Timestamp::from_millis(at().millis() + 8 * 24 * 60 * 60 * 1_000);
    let second = DocumentId::new();
    let fresh = w
        .seed_document(&seed(second, "reusable-key", "different", eight_days_later))
        .unwrap();

    assert_eq!(fresh.document, second);
    assert!(!fresh.replayed);
    assert_eq!(w.documents().unwrap().len(), 2);
}

#[test]
fn a_canonical_candidate_replays_original_bytes_after_receipt_expiry() {
    let mut w = repo();
    let candidate = DocumentId::new();
    let first = w
        .seed_document(&candidate_seed(candidate, "stable-key", "first", at()))
        .unwrap();

    let eight_days_later = Timestamp::from_millis(at().millis() + 8 * 24 * 60 * 60 * 1_000);
    let mut retry = candidate_seed(candidate, "stable-key", "different", eight_days_later);
    retry.initial_update = vec![9, 9, 9];
    let replay = w.seed_document(&retry).unwrap();

    assert!(replay.replayed);
    assert_eq!(replay.document, candidate);
    assert_eq!(replay.initial_update, first.initial_update);
    assert_eq!(w.documents().unwrap().len(), 1);
    assert_eq!(w.updates_after(candidate, None).unwrap().len(), 1);
}

#[test]
fn a_canonical_candidate_wins_before_a_live_receipt_hash() {
    let mut w = repo();
    let candidate = DocumentId::new();
    let first = w
        .seed_document(&candidate_seed(candidate, "stable-key", "first", at()))
        .unwrap();
    let mut retry = candidate_seed(candidate, "stable-key", "different", at());
    retry.initial_update = vec![8, 8, 8];

    let replay = w.seed_document(&retry).unwrap();

    assert!(replay.replayed);
    assert_eq!(replay.initial_update, first.initial_update);
    assert_eq!(w.updates_after(candidate, None).unwrap().len(), 1);
}

#[test]
fn a_candidate_owned_by_an_internal_document_kind_fails_closed() {
    let mut w = repo();
    let candidate = DocumentId::new();
    w.create_document(candidate, DocumentKind::Manifest, at())
        .unwrap();
    w.commit_updates(&batch(candidate, vec![vec![7]])).unwrap();

    let error = w
        .seed_document(&candidate_seed(candidate, "stable-key", "first", at()))
        .unwrap_err();

    assert_eq!(error.code(), Code::IdempotencyConflict);
}

#[test]
fn writing_to_an_unknown_document_is_not_found() {
    let mut w = repo();
    let ghost = DocumentId::new();
    assert_eq!(
        w.commit_updates(&batch(ghost, vec![vec![1]]))
            .unwrap_err()
            .code(),
        Code::DocumentNotFound
    );
    assert_eq!(
        w.commit_checkpoint(ghost, &checkpoint(1, vec![1]))
            .unwrap_err()
            .code(),
        Code::DocumentNotFound
    );
}

#[test]
fn dropping_a_document_takes_its_journal_with_it() {
    let (mut w, d) = with_document();
    w.commit_updates(&batch(d, vec![vec![1], vec![2]])).unwrap();
    w.commit_checkpoint(d, &checkpoint(2, vec![9])).unwrap();

    w.drop_document(d).unwrap();
    assert_eq!(w.document(d).unwrap(), None);
    // The cascade is what stops a dropped document leaving its history behind.
    assert_eq!(w.update_count(d).unwrap(), 0);
    assert!(w.checkpoints(d).unwrap().is_empty());
}

// ------------------------------------------------- document-private state

#[test]
fn private_rows_cascade_with_the_document_and_survive_a_reindex() {
    use koru_store_core::DocumentIndex;
    let (mut w, d) = with_document();
    let reader = ProfileId::new();
    assert_eq!(w.private_snapshot(d, reader).unwrap(), None);
    w.put_private_snapshot(d, reader, &[1, 2], at()).unwrap();
    w.put_private_snapshot(d, reader, &[3], at()).unwrap();
    assert_eq!(w.private_snapshot(d, reader).unwrap(), Some(vec![3]));

    // The listing is rebuilt wholesale; private state is not part of it.
    w.reindex(&[]).unwrap();
    assert_eq!(w.private_snapshot(d, reader).unwrap(), Some(vec![3]));

    // An empty snapshot is refused by the table itself.
    assert!(w.put_private_snapshot(d, reader, &[], at()).is_err());
    // A private row for a document with no storage row is refused too.
    assert!(
        w.put_private_snapshot(DocumentId::new(), reader, &[1], at())
            .is_err()
    );

    w.drop_document(d).unwrap();
    assert_eq!(w.private_snapshot(d, reader).unwrap(), None);
}

#[test]
fn pruning_keeps_listed_documents_and_removes_the_rest() {
    let (mut w, kept) = with_document();
    let gone = DocumentId::new();
    w.create_document(gone, DocumentKind::Document, at())
        .unwrap();
    let (a, b) = (ProfileId::new(), ProfileId::new());
    for document in [kept, gone] {
        for reader in [a, b] {
            w.put_private_snapshot(document, reader, &[1], at())
                .unwrap();
        }
    }
    assert_eq!(w.prune_private_snapshots(&[kept]).unwrap(), 2);
    assert_eq!(w.private_snapshots(kept).unwrap().len(), 2);
    assert!(w.private_snapshots(gone).unwrap().is_empty());
    // The shared bytes are not the cleanup's to delete.
    assert!(w.document(gone).unwrap().is_some());
    assert_eq!(w.prune_private_snapshots(&[kept]).unwrap(), 0, "idempotent");
}

// ------------------------------------------------------------------ updates

#[test]
fn updates_are_stored_verbatim_and_come_back_in_order() {
    let (mut w, d) = with_document();
    let sent = vec![vec![1, 2, 3], vec![4], vec![5, 6]];
    let head = w.commit_updates(&batch(d, sent.clone())).unwrap();
    assert_eq!(head, 3);

    let got = w.updates_after(d, None).unwrap();
    assert_eq!(got.iter().map(|u| u.seq).collect::<Vec<_>>(), [1, 2, 3]);
    // Verbatim: what is replayed on recovery is what was acknowledged, not a
    // re-encoding of it.
    assert_eq!(
        got.iter().map(|u| u.bytes.clone()).collect::<Vec<_>>(),
        sent
    );
}

#[test]
fn sequences_continue_across_commits() {
    let (mut w, d) = with_document();
    assert_eq!(w.commit_updates(&batch(d, vec![vec![1]])).unwrap(), 1);
    assert_eq!(
        w.commit_updates(&batch(d, vec![vec![2], vec![3]])).unwrap(),
        3
    );
    assert_eq!(w.update_count(d).unwrap(), 3);
}

#[test]
fn a_commit_moves_the_position_with_the_journal() {
    // The first atomicity requirement, observed from outside: after a commit,
    // the journal and the recorded version vector agree.
    let (mut w, d) = with_document();
    let mut b = batch(d, vec![vec![1]]);
    b.version_vector = vec![0xAB];
    b.frontiers = vec![0xCD];
    w.commit_updates(&b).unwrap();

    let state = w.document(d).unwrap().unwrap();
    assert_eq!(state.version_vector, vec![0xAB]);
    assert_eq!(state.frontiers, vec![0xCD]);
    assert_eq!(w.update_count(d).unwrap(), 1);
}

#[test]
fn an_empty_batch_writes_nothing_and_is_not_an_error() {
    // A commit timer firing on a quiet window is normal.
    let (mut w, d) = with_document();
    w.commit_updates(&batch(d, vec![vec![1]])).unwrap();
    let before = w.document(d).unwrap().unwrap();

    assert_eq!(w.commit_updates(&batch(d, vec![])).unwrap(), 1);
    assert_eq!(w.document(d).unwrap().unwrap(), before);
    assert_eq!(w.update_count(d).unwrap(), 1);
}

#[test]
fn updates_after_a_sequence_skips_what_is_already_applied() {
    let (mut w, d) = with_document();
    w.commit_updates(&batch(d, vec![vec![1], vec![2], vec![3], vec![4]]))
        .unwrap();
    let rest = w.updates_after(d, Some(2)).unwrap();
    assert_eq!(rest.iter().map(|u| u.seq).collect::<Vec<_>>(), [3, 4]);
}

#[test]
fn the_originating_client_is_recorded_for_diagnosis() {
    let (mut w, d) = with_document();
    let client = ClientId::new();
    let mut b = batch(d, vec![vec![1]]);
    b.origins = vec![koru_store_core::ports::UpdateOrigin::LocalClient { client_id: client }];
    w.commit_updates(&b).unwrap();
    assert_eq!(
        w.updates_after(d, None).unwrap()[0].origin,
        koru_store_core::ports::UpdateOrigin::LocalClient { client_id: client }
    );
}

#[test]
fn every_update_keeps_its_remote_device_provenance() {
    let (mut w, d) = with_document();
    let remote = koru_store_core::ports::UpdateOrigin::RemoteDevice {
        relationship_id: [1; 16],
        device_id: [2; 16],
        endpoint_id: [3; 32],
    };
    let mut b = batch(d, vec![vec![1], vec![2]]);
    b.origins = vec![koru_store_core::ports::UpdateOrigin::Store, remote.clone()];

    w.commit_updates(&b).unwrap();
    let stored = w.updates_after(d, None).unwrap();

    assert_eq!(
        stored[0].origin,
        koru_store_core::ports::UpdateOrigin::Store
    );
    assert_eq!(stored[1].origin, remote);
}

// ------------------------------------------------------------------ checkpoints

#[test]
fn the_first_checkpoint_prunes_nothing() {
    // There is no older snapshot to fall back to, so the journal it covers is
    // the only other copy of that work. Deleting it would make the first
    // checkpoint a single point of failure.
    let (mut w, d) = with_document();
    w.commit_updates(&batch(d, vec![vec![1], vec![2], vec![3], vec![4], vec![5]]))
        .unwrap();

    let pruned = w.commit_checkpoint(d, &checkpoint(3, vec![0xFF])).unwrap();
    assert_eq!(pruned, 0);
    assert_eq!(
        w.updates_after(d, None)
            .unwrap()
            .iter()
            .map(|u| u.seq)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    assert_eq!(
        w.document(d).unwrap().unwrap().latest_checkpoint_seq,
        Some(3)
    );
}

#[test]
fn a_checkpoint_prunes_only_what_the_previous_one_covered() {
    // One generation of overlap. Pruning everything the *new* checkpoint
    // covers would delete exactly the journal that makes the older checkpoint
    // a usable fall-back, so one bad snapshot would cost every edit between
    // them — and keeping several checkpoints would then be pointless.
    let (mut w, d) = with_document();
    w.commit_updates(&batch(
        d,
        vec![vec![1], vec![2], vec![3], vec![4], vec![5], vec![6]],
    ))
    .unwrap();
    w.commit_checkpoint(d, &checkpoint(2, vec![0xAA])).unwrap();

    let pruned = w.commit_checkpoint(d, &checkpoint(5, vec![0xBB])).unwrap();
    assert_eq!(pruned, 2, "only what checkpoint 2 already covered");
    assert_eq!(
        w.updates_after(d, None)
            .unwrap()
            .iter()
            .map(|u| u.seq)
            .collect::<Vec<_>>(),
        [3, 4, 5, 6],
        "the journal that makes checkpoint 2 a usable fall-back must survive"
    );
}

#[test]
fn falling_back_from_a_damaged_checkpoint_has_a_journal_to_replay() {
    // The property the overlap exists for, stated outright rather than left
    // implicit in the pruning arithmetic.
    let (mut w, d) = with_document();
    w.commit_updates(&batch(d, vec![vec![1], vec![2], vec![3], vec![4]]))
        .unwrap();
    w.commit_checkpoint(d, &checkpoint(1, b"older".to_vec()))
        .unwrap();
    w.commit_checkpoint(d, &checkpoint(4, b"newer".to_vec()))
        .unwrap();
    w.connection
        .execute(
            "UPDATE loro_checkpoints SET snapshot = x'00' WHERE seq = 4",
            [],
        )
        .unwrap();

    let sound = w.latest_sound_checkpoint(d).unwrap().expect("fall back");
    assert_eq!(sound.seq, 1);
    assert_eq!(
        w.updates_after(d, Some(sound.seq))
            .unwrap()
            .iter()
            .map(|u| u.seq)
            .collect::<Vec<_>>(),
        [2, 3, 4],
        "nothing left to replay, so the fall-back is lossy"
    );
}

#[test]
fn sequences_continue_past_a_checkpoint_that_pruned_everything() {
    // The trap `head` exists for: once pruning has emptied the journal,
    // `max(seq)` over `loro_updates` is null. The next update must continue the
    // sequence, not restart at 1 — a reused number would collide with the
    // pruned history on any replay.
    let (mut w, d) = with_document();
    w.commit_updates(&batch(d, vec![vec![1], vec![2], vec![3]]))
        .unwrap();
    w.commit_checkpoint(d, &checkpoint(1, vec![0xAA])).unwrap();
    w.commit_checkpoint(d, &checkpoint(3, vec![0xFF])).unwrap();
    assert_eq!(w.update_count(d).unwrap(), 2, "1 pruned, 2 and 3 retained");

    assert_eq!(w.commit_updates(&batch(d, vec![vec![4]])).unwrap(), 4);
    assert_eq!(w.updates_after(d, Some(3)).unwrap()[0].seq, 4);
}

#[test]
fn a_checkpoint_whose_checksum_is_wrong_is_refused_at_write_time() {
    // A caller's bug caught as a bug, rather than surfacing later as a
    // recovery that nobody can explain.
    let (mut w, d) = with_document();
    let mut cp = checkpoint(1, vec![1, 2, 3]);
    cp.checksum = checksum(b"something else");
    assert_eq!(
        w.commit_checkpoint(d, &cp).unwrap_err().code(),
        Code::CorruptState
    );
    assert!(w.checkpoints(d).unwrap().is_empty(), "nothing was written");
}

#[test]
fn the_latest_sound_checkpoint_skips_a_damaged_one() {
    // Why more than one checkpoint is kept: the newest is not always usable.
    let (mut w, d) = with_document();
    w.commit_updates(&batch(d, vec![vec![1], vec![2], vec![3], vec![4]]))
        .unwrap();
    w.commit_checkpoint(d, &checkpoint(2, b"good snapshot".to_vec()))
        .unwrap();
    w.commit_checkpoint(d, &checkpoint(4, b"newer snapshot".to_vec()))
        .unwrap();

    // Rot the newest on disk, leaving its recorded checksum alone.
    w.connection
        .execute(
            "UPDATE loro_checkpoints SET snapshot = x'00' WHERE document_id = ?1 AND seq = 4",
            params![d.to_storage()],
        )
        .unwrap();

    let sound = w.latest_sound_checkpoint(d).unwrap().expect("fall back");
    assert_eq!(sound.seq, 2);
    assert_eq!(sound.snapshot, b"good snapshot");
    // The damaged one is still *there* — skipped, never deleted, because a
    // recovery must not destroy evidence.
    assert!(w.checkpoint(d, 4).unwrap().is_some());
}

#[test]
fn no_sound_checkpoint_is_none_rather_than_an_error() {
    // The journal alone then rebuilds the document, which is a recovery and
    // not a failure.
    let (mut w, d) = with_document();
    w.commit_updates(&batch(d, vec![vec![1]])).unwrap();
    w.commit_checkpoint(d, &checkpoint(1, b"snap".to_vec()))
        .unwrap();
    w.connection
        .execute("UPDATE loro_checkpoints SET snapshot = x'00'", [])
        .unwrap();
    assert_eq!(w.latest_sound_checkpoint(d).unwrap(), None);
}

#[test]
fn checkpoints_are_listed_newest_first_without_their_bytes() {
    let (mut w, d) = with_document();
    w.commit_updates(&batch(d, vec![vec![1], vec![2]])).unwrap();
    w.commit_checkpoint(d, &checkpoint(1, vec![1; 10])).unwrap();
    w.commit_checkpoint(d, &checkpoint(2, vec![2; 20])).unwrap();

    let headers = w.checkpoints(d).unwrap();
    assert_eq!(headers.iter().map(|h| h.seq).collect::<Vec<_>>(), [2, 1]);
    assert_eq!(headers[0].size, 20);
    assert_eq!(headers[1].size, 10);
}

// ------------------------------------------------------------------ blobs

#[test]
fn blob_metadata_round_trips_with_a_unicode_filename() {
    let mut w = repo();
    let blob = Blob {
        hash: BlobHash::from_digest("b3", &"ab".repeat(32)).unwrap(),
        size: 4096,
        mime: "application/pdf".into(),
        filename: Some(UNICODE_FILENAME.into()),
        created_at: at(),
        attributes: Default::default(),
    };
    w.put_blob(&blob).unwrap();
    assert_eq!(w.blob(&blob.hash).unwrap().as_ref(), Some(&blob));
    assert_eq!(w.blobs().unwrap(), vec![blob]);
}

#[test]
fn blob_attributes_round_trip_and_the_first_writer_wins() {
    let mut w = repo();
    let hash = BlobHash::from_digest("b3", &"ef".repeat(32)).unwrap();
    let mut attributes = std::collections::BTreeMap::new();
    attributes.insert(
        koru_store_core::model::blob_attr::ORIGIN_DEVICE.to_string(),
        koru_store_core::model::BlobAttribute::Text("5f9c0d7e-…".into()),
    );
    attributes.insert(
        "pages".into(),
        koru_store_core::model::BlobAttribute::Int(12),
    );
    let blob = Blob {
        hash: hash.clone(),
        size: 10,
        mime: "image/png".into(),
        filename: None,
        created_at: at(),
        attributes: attributes.clone(),
    };
    w.put_blob(&blob).unwrap();
    assert_eq!(w.blob(&hash).unwrap().unwrap().attributes, attributes);

    // A later store of the same bytes — a hydration landing here, say —
    // updates display metadata but keeps the first record's attributes:
    // origin is provenance, not last-touch.
    let mut restated = blob.clone();
    restated.filename = Some(UNICODE_FILENAME.into());
    restated.attributes.clear();
    w.put_blob(&restated).unwrap();
    let back = w.blob(&hash).unwrap().unwrap();
    assert_eq!(back.filename.as_deref(), Some(UNICODE_FILENAME));
    assert_eq!(back.attributes, attributes);
}

#[test]
fn re_putting_a_blob_updates_its_metadata_not_its_identity() {
    let mut w = repo();
    let hash = BlobHash::from_digest("b3", &"cd".repeat(32)).unwrap();
    let mut blob = Blob {
        hash: hash.clone(),
        size: 10,
        mime: "application/octet-stream".into(),
        filename: None,
        created_at: at(),
        attributes: Default::default(),
    };
    w.put_blob(&blob).unwrap();
    blob.mime = "image/png".into();
    blob.filename = Some(BIDI_LINE.into());
    w.put_blob(&blob).unwrap();

    assert_eq!(w.blobs().unwrap().len(), 1);
    let back = w.blob(&hash).unwrap().unwrap();
    assert_eq!(back.mime, "image/png");
    assert_eq!(back.filename.as_deref(), Some(BIDI_LINE));
}

#[test]
fn an_advertised_origin_round_trips_and_the_first_record_wins() {
    let mut w = repo();
    let hash = BlobHash::from_digest("b3", &"12".repeat(32)).unwrap();

    assert_eq!(w.advertised_blob_origin(&hash).unwrap(), None);
    w.record_blob_origin(&hash, "device-a", at()).unwrap();
    assert_eq!(
        w.advertised_blob_origin(&hash).unwrap(),
        Some("device-a".to_string())
    );
    // Provenance is who *first* held the bytes, never last touch.
    w.record_blob_origin(&hash, "device-b", at()).unwrap();
    assert_eq!(
        w.advertised_blob_origin(&hash).unwrap(),
        Some("device-a".to_string())
    );
}

// ---------------------------------------------------------------- history

fn history_capture<'a>(
    document: DocumentId,
    snapshot: &'a [u8],
    frontiers: &'a [u8],
) -> HistoryCapture<'a> {
    HistoryCapture {
        document,
        snapshot,
        version_vector: &[3],
        frontiers,
        checksum: "",
        reason: HistoryReason::Manual,
        recorded_at: at(),
        label: Some("Milestone"),
        message: None,
        boundary: None,
    }
}

#[test]
fn history_deduplicates_state_but_keeps_each_marker() {
    let (mut w, d) = with_document();
    let snapshot = b"opaque loro snapshot";
    let mut first = history_capture(d, snapshot, &[9]);
    let sum = checksum(snapshot);
    first.checksum = &sum;
    let a = w.capture_history(&first).unwrap();
    let b = w.capture_history(&first).unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(a.version, b.version);
    assert_eq!(w.history_entries(d, None, 50).unwrap().len(), 2);
    assert_eq!(
        w.document_version(a.version).unwrap().unwrap().snapshot,
        snapshot
    );
}

#[test]
fn relabeling_a_marker_changes_its_name_and_nothing_else() {
    let (mut w, d) = with_document();
    let snapshot = b"opaque loro snapshot";
    let sum = checksum(snapshot);
    let mut capture = history_capture(d, snapshot, &[9]);
    capture.checksum = &sum;
    let entry = w.capture_history(&capture).unwrap();

    let renamed = w
        .relabel_history_entry(entry.id, Some("Before the rewrite"))
        .unwrap();
    assert_eq!(renamed.id, entry.id);
    assert_eq!(renamed.label.as_deref(), Some("Before the rewrite"));
    assert_eq!(renamed.version, entry.version, "the state is untouched");
    assert_eq!(renamed.recorded_at, entry.recorded_at);

    // Clearing the name falls back to the reason in every listing.
    let cleared = w.relabel_history_entry(entry.id, None).unwrap();
    assert_eq!(cleared.label, None);

    // A marker that does not exist is a structured refusal, not a no-op.
    assert_eq!(
        w.relabel_history_entry(HistoryEntryId::new(), Some("x"))
            .unwrap_err()
            .code(),
        Code::HistoryEntryNotFound
    );
}

#[test]
fn history_is_paginated_with_a_stable_exclusive_cursor() {
    let (mut w, d) = with_document();
    let snapshot = b"same";
    let sum = checksum(snapshot);
    let mut capture = history_capture(d, snapshot, &[4]);
    capture.checksum = &sum;
    for _ in 0..3 {
        w.capture_history(&capture).unwrap();
    }
    let first = w.history_entries(d, None, 2).unwrap();
    let last = first.last().unwrap();
    let second = w
        .history_entries(d, Some((last.recorded_at, last.id)), 2)
        .unwrap();
    assert_eq!(first.len(), 2);
    assert_eq!(second.len(), 1);
    assert!(!first.iter().any(|a| second.iter().any(|b| a.id == b.id)));
}

#[test]
fn a_corrupt_history_version_is_retained_but_refused() {
    let (mut w, d) = with_document();
    let snapshot = b"sound";
    let sum = checksum(snapshot);
    let mut capture = history_capture(d, snapshot, &[5]);
    capture.checksum = &sum;
    let entry = w.capture_history(&capture).unwrap();
    w.connection_mut()
        .execute(
            "UPDATE document_versions SET snapshot = x'00' WHERE id = ?1",
            params![entry.version.to_storage()],
        )
        .unwrap();
    assert_eq!(
        w.document_version(entry.version).unwrap_err().code(),
        Code::CorruptState
    );
    assert!(w.history_entry(entry.id).unwrap().is_some());
}

/// The one mutation on an append-only table, and the round trip that proves it
/// reached storage rather than only the returned value.
#[test]
fn a_marker_can_be_renamed_and_the_new_name_is_what_a_listing_reads() {
    let (mut w, d) = with_document();
    let snapshot = b"named";
    let sum = checksum(snapshot);
    let mut capture = history_capture(d, snapshot, &[7]);
    capture.checksum = &sum;
    let entry = w.capture_history(&capture).unwrap();
    assert_eq!(entry.label.as_deref(), Some("Milestone"));

    let renamed = w
        .relabel_history_entry(entry.id, Some("Before the rewrite"))
        .unwrap();
    assert_eq!(renamed.label.as_deref(), Some("Before the rewrite"));
    // Everything else the marker records is what happened, and none of it moved.
    assert_eq!(renamed.id, entry.id);
    assert_eq!(renamed.version, entry.version);
    assert_eq!(renamed.reason, entry.reason);
    assert_eq!(renamed.recorded_at, entry.recorded_at);
    assert_eq!(renamed.pinned, entry.pinned);

    let listed = w.history_entries(d, None, 50).unwrap();
    assert_eq!(listed.len(), 1, "renaming appends nothing");
    assert_eq!(listed[0].label.as_deref(), Some("Before the rewrite"));
}

/// `None` is a real value: it puts the marker back in the state a capture
/// without a label produces, rather than leaving the old name in place.
#[test]
fn renaming_a_marker_to_nothing_makes_it_unnamed_again() {
    let (mut w, d) = with_document();
    let snapshot = b"unnamed";
    let sum = checksum(snapshot);
    let mut capture = history_capture(d, snapshot, &[8]);
    capture.checksum = &sum;
    let entry = w.capture_history(&capture).unwrap();

    let cleared = w.relabel_history_entry(entry.id, None).unwrap();
    assert_eq!(cleared.label, None);
    assert_eq!(w.history_entries(d, None, 50).unwrap()[0].label, None);
}

/// Not found is the store's own not-found code, not a storage failure.
#[test]
fn renaming_a_marker_that_does_not_exist_finds_nothing() {
    let (mut w, _d) = with_document();
    assert_eq!(
        w.relabel_history_entry(HistoryEntryId::new(), Some("nowhere"))
            .unwrap_err()
            .code(),
        Code::HistoryEntryNotFound
    );
}

/// The newest marker's version vector, and no snapshot with it — the read the
/// staleness comparison is built on.
#[test]
fn the_newest_markers_version_is_readable_without_its_snapshot() {
    let (mut w, d) = with_document();
    assert_eq!(w.newest_history_version(d).unwrap(), None);

    let snapshot = b"first";
    let sum = checksum(snapshot);
    let mut capture = history_capture(d, snapshot, &[1]);
    capture.checksum = &sum;
    capture.version_vector = &[10];
    w.capture_history(&capture).unwrap();
    assert_eq!(w.newest_history_version(d).unwrap(), Some(vec![10]));

    // Newest wins, and "newest" means what the listing means by it.
    let later = b"second";
    let sum = checksum(later);
    let mut capture = history_capture(d, later, &[2]);
    capture.checksum = &sum;
    capture.version_vector = &[20];
    capture.recorded_at = Timestamp::from_millis(at().millis() + 1);
    w.capture_history(&capture).unwrap();
    assert_eq!(w.newest_history_version(d).unwrap(), Some(vec![20]));
    assert_eq!(
        w.newest_history_version(d).unwrap(),
        w.document_version(w.history_entries(d, None, 1).unwrap()[0].version)
            .unwrap()
            .map(|v| v.version_vector),
        "the same marker the listing calls newest"
    );
}

// ------------------------------------------------------------------ text

#[test]
fn a_snapshot_of_every_supported_language_survives_a_round_trip() {
    // Snapshots are opaque bytes, so this is really a BLOB round-trip test —
    // and the case it guards is a text encoding sneaking into a binary column.
    let (mut w, d) = with_document();
    let mut seq = 0;
    for s in SAMPLES {
        seq += 1;
        let snapshot = s.body.as_bytes().to_vec();
        w.commit_updates(&batch(d, vec![snapshot.clone()])).unwrap();
        let cp = checkpoint(seq, snapshot.clone());
        w.commit_checkpoint(d, &cp).unwrap();
        let back = w.latest_sound_checkpoint(d).unwrap().unwrap();
        assert_eq!(back.snapshot, snapshot, "{}", s.tag);
        assert_eq!(
            String::from_utf8(back.snapshot).unwrap(),
            s.body,
            "{}",
            s.tag
        );
    }
}

// ------------------------------------------------------------------ schema

#[test]
fn a_fresh_workspace_is_at_the_current_schema_and_passes_its_check() {
    let w = repo();
    assert_eq!(w.schema_version().unwrap(), WORKSPACE_SCHEMA_VERSION);
    assert!(w.quick_check().unwrap());
}

#[test]
fn a_version_six_workspace_missing_update_origins_is_repaired() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            // **The tables a real version-six database had, not only the one
            // this test asserts on.** Later migrations alter `documents` --
            // version seven adds its `mime` -- so a fixture that omits it
            // fails the migration rather than the assertion, and says
            // "no such table" about a table no real database was missing.
            "CREATE TABLE loro_updates (
               document_id  TEXT NOT NULL,
               seq          INTEGER NOT NULL,
               update_bytes BLOB NOT NULL,
               received_at  INTEGER NOT NULL,
               from_client  TEXT,
               PRIMARY KEY (document_id, seq)
             );
             CREATE TABLE documents (
               doc_id    TEXT PRIMARY KEY,
               parent_id TEXT,
               ordinal   INTEGER NOT NULL,
               title     TEXT    NOT NULL,
               kind      TEXT    NOT NULL,
               position  INTEGER NOT NULL
             );
             -- Present since migration 1; migration 9 alters it.
             CREATE TABLE blobs (hash TEXT PRIMARY KEY, size INTEGER NOT NULL,
               mime TEXT NOT NULL, filename TEXT, created_at INTEGER NOT NULL);
             PRAGMA user_version = 6;",
        )
        .unwrap();

    let workspace = SqliteWorkspace::from_connection(connection).unwrap();
    assert_eq!(
        workspace.schema_version().unwrap(),
        WORKSPACE_SCHEMA_VERSION
    );
    workspace
        .connection()
        .prepare("SELECT origin_json FROM loro_updates")
        .unwrap();
}

#[test]
fn a_workspace_reopened_from_disk_keeps_everything() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.db");
    let d = DocumentId::new();
    {
        let mut w = SqliteWorkspace::open(&path).unwrap();
        w.create_document(d, DocumentKind::Manifest, at()).unwrap();
        w.commit_updates(&batch(d, vec![vec![1], vec![2]])).unwrap();
        w.commit_checkpoint(d, &checkpoint(1, b"snap".to_vec()))
            .unwrap();
    }
    let w = SqliteWorkspace::open(&path).unwrap();
    assert_eq!(
        w.document(d).unwrap().unwrap().latest_checkpoint_seq,
        Some(1)
    );
    assert_eq!(w.updates_after(d, Some(1)).unwrap().len(), 1);
    assert_eq!(
        w.latest_sound_checkpoint(d).unwrap().unwrap().snapshot,
        b"snap"
    );
    assert!(w.size_bytes().unwrap() > 0);
}
