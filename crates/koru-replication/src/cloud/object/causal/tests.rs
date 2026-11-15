// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::cloud::object::tests::*;
use serde_json::Value;

mod membership;
mod references;

pub(super) fn fixture(index: usize) -> Value {
    serde_json::from_str::<Value>(LORO).unwrap()["fixtures"][index].clone()
}
pub(super) fn template(index: usize) -> Metadata {
    metadata(&goldens()[index * 2]["expected"])
}
fn object(
    mut meta: Metadata,
    kind: Kind,
    sequence: u64,
    coverage: u64,
    payload: &[u8],
    predecessor: &[u8],
    version: &[u8],
    frontiers: &[u8],
) -> Vec<u8> {
    meta.kind = kind;
    meta.sequence = sequence;
    meta.covers_through_sequence = coverage;
    meta.object_id = id(&format!("80000000-0000-4000-8000-{sequence:012x}"));
    encode(
        meta,
        Body {
            payload,
            predecessor_version: predecessor,
            result_version: version,
            checkpoint_frontiers: frontiers,
        },
        limits(),
    )
    .unwrap()
}
pub(super) fn cp(
    index: usize,
    sequence: u64,
    coverage: u64,
    payload_name: &str,
    state_name: &str,
) -> Vec<u8> {
    let f = fixture(index);
    object(
        template(index),
        Kind::Checkpoint,
        sequence,
        coverage,
        &bytes(f[payload_name].as_str().unwrap()),
        &[],
        &bytes(f[state_name]["version"].as_str().unwrap()),
        &bytes(f[state_name]["frontiers"].as_str().unwrap()),
    )
}
pub(super) fn segment(index: usize, sequence: u64, number: usize) -> Vec<u8> {
    let f = fixture(index);
    let before = if number == 1 { "base_state" } else { "state1" };
    object(
        template(index),
        Kind::Segment,
        sequence,
        0,
        &bytes(f[format!("segment{number}")].as_str().unwrap()),
        &bytes(f[before]["version"].as_str().unwrap()),
        &bytes(f[format!("state{number}")]["version"].as_str().unwrap()),
        &[],
    )
}
fn snapshot_object(index: usize, doc: &LoroDoc) -> Vec<u8> {
    object(
        template(index),
        Kind::Checkpoint,
        1,
        0,
        &doc.export(ExportMode::snapshot()).unwrap(),
        &[],
        &doc.state_vv().encode(),
        &doc.state_frontiers().encode(),
    )
}
fn verify_first(raw: &[u8]) -> Result<ContentProof, Error> {
    let bound = bind_fixture(raw);
    verify_chain(
        bound,
        CheckpointOrigin::FirstCloudCheckpoint,
        &[],
        head(bound),
        limits(),
    )
}

#[test]
fn f01_f06_f07_f08_all_five_real_unit_chains_and_captured_fallback() {
    for index in 0..5 {
        let base = cp(index, 1, 0, "base", "base_state");
        let first = segment(index, 2, 1);
        let second = segment(index, 4, 2); // abandoned reservation gap
        let tail = [bind_fixture(&first), bind_fixture(&second)];
        let proof = verify_chain(
            bind_fixture(&base),
            CheckpointOrigin::FirstCloudCheckpoint,
            &tail,
            head(tail[1]),
            limits(),
        )
        .unwrap();
        assert_eq!(proof.dataset(), template(index).dataset);
        assert_eq!(proof.unit(), template(index).unit);
        assert_eq!(proof.cloud_unit_id(), template(index).cloud_unit_id);
        assert_eq!(proof.checkpoint_object_sequence(), 1);
        assert_eq!(proof.last_replayed_segment_sequence(), Some(4));
        let checkpoint = cp(index, 3, 2, "checkpoint1", "state1");
        let recovered = verify_chain(
            bind_fixture(&checkpoint),
            CheckpointOrigin::LaterCheckpoint,
            &tail[1..],
            head(tail[1]),
            limits(),
        )
        .unwrap();
        assert_eq!(recovered.head(), proof.head());
        assert_eq!(
            verify_chain(
                bind_fixture(&checkpoint),
                CheckpointOrigin::LaterCheckpoint,
                &[],
                head(tail[1]),
                limits()
            ),
            Err(Error::Chain)
        );
        assert_eq!(
            verify_chain(
                bind_fixture(&base),
                CheckpointOrigin::FirstCloudCheckpoint,
                &tail[1..],
                head(tail[1]),
                limits()
            ),
            Err(Error::Predecessor)
        );
        assert_eq!(
            verify_chain(
                bind_fixture(&base),
                CheckpointOrigin::FirstCloudCheckpoint,
                &[tail[1], tail[0]],
                head(tail[0]),
                limits()
            ),
            Err(Error::Predecessor)
        );
    }
}

#[test]
fn checkpoint_allocation_is_not_coverage_or_sql_head_high_water() {
    // Signed catalog concurrent generation: captured through 4, then segments
    // 6 and 8 commit before checkpoint reservation/publication 10.
    let checkpoint = cp(1, 10, 4, "base", "base_state");
    let first = segment(1, 6, 1);
    let second = segment(1, 8, 2);
    let segments = [bind_fixture(&first), bind_fixture(&second)];
    let proof = verify_chain(
        bind_fixture(&checkpoint),
        CheckpointOrigin::LaterCheckpoint,
        &segments,
        head(segments[1]),
        limits(),
    )
    .unwrap();
    assert_eq!(proof.checkpoint_object_sequence(), 10);
    assert_eq!(proof.last_replayed_segment_sequence(), Some(8));
    assert_eq!(proof.head(), head(segments[1]));
    let at_boundary = segment(1, 4, 1);
    assert_eq!(
        verify_chain(
            bind_fixture(&checkpoint),
            CheckpointOrigin::LaterCheckpoint,
            &[bind_fixture(&at_boundary)],
            head(bind_fixture(&at_boundary)),
            limits()
        ),
        Err(Error::Chain)
    );
    let collision = segment(1, 10, 1);
    assert_eq!(
        verify_chain(
            bind_fixture(&checkpoint),
            CheckpointOrigin::LaterCheckpoint,
            &[bind_fixture(&collision)],
            head(bind_fixture(&collision)),
            limits()
        ),
        Err(Error::Chain)
    );
}

#[test]
fn duplicate_segment_object_ids_cannot_describe_two_receipts() {
    let base = cp(1, 1, 0, "base", "base_state");
    let first = segment(1, 2, 1);
    let second = segment(1, 4, 2);
    let decoded = decode(&second, limits()).unwrap();
    let mut metadata = decoded.metadata();
    metadata.object_id = bind_fixture(&first).metadata().object_id;
    let duplicate = encode(metadata, decoded.body(), limits()).unwrap();
    assert_eq!(
        verify_chain(
            bind_fixture(&base),
            CheckpointOrigin::FirstCloudCheckpoint,
            &[bind_fixture(&first), bind_fixture(&duplicate)],
            head(bind_fixture(&duplicate)),
            limits()
        ),
        Err(Error::Chain)
    );
}

#[test]
fn checkpoints_refuse_real_update_shallow_and_state_only_blobs_with_matching_state() {
    let f = fixture(1);
    let doc = LoroDoc::new();
    doc.import(&bytes(f["checkpoint2"].as_str().unwrap()))
        .unwrap();
    let frontier = doc.state_frontiers();
    for mode in [
        ExportMode::all_updates(),
        ExportMode::shallow_snapshot(&frontier),
        ExportMode::state_only(None),
    ] {
        let payload = doc.export(mode).unwrap();
        let imported = LoroDoc::new();
        imported.import(&payload).unwrap();
        assert_eq!(imported.state_vv(), doc.state_vv());
        assert_eq!(imported.state_frontiers(), doc.state_frontiers());
        assert_ne!(
            LoroDoc::decode_import_blob_meta(&payload, true)
                .unwrap()
                .mode,
            EncodedBlobMode::Snapshot
        );
        let raw = object(
            template(1),
            Kind::Checkpoint,
            1,
            0,
            &payload,
            &[],
            &doc.state_vv().encode(),
            &frontier.encode(),
        );
        assert_eq!(verify_first(&raw), Err(Error::CausalShape));
    }
    assert!(verify_first(&snapshot_object(1, &doc)).is_ok());
}

#[test]
fn f06_pending_import_ok_is_not_applied_and_exact_result_is_required() {
    let f = fixture(1);
    let base = cp(1, 1, 0, "base", "base_state");
    let payload = bytes(f["segment2"].as_str().unwrap());
    let doc = LoroDoc::new();
    doc.import(&bytes(f["base"].as_str().unwrap())).unwrap();
    let before = doc.state_vv();
    assert!(doc.import(&payload).unwrap().pending.is_some());
    assert_eq!(doc.state_vv(), before);
    // Lie about predecessor and result to bypass byte/head correlation; actual
    // missing-ancestor import is still refused, even when import returns Ok.
    let broken = object(
        template(1),
        Kind::Segment,
        2,
        0,
        &payload,
        &bytes(f["base_state"]["version"].as_str().unwrap()),
        &bytes(f["base_state"]["version"].as_str().unwrap()),
        &[],
    );
    assert_eq!(
        verify_chain(
            bind_fixture(&base),
            CheckpointOrigin::FirstCloudCheckpoint,
            &[bind_fixture(&broken)],
            head(bind_fixture(&broken)),
            limits()
        ),
        Err(Error::Ancestors)
    );
    let wrong_result = object(
        template(1),
        Kind::Segment,
        2,
        0,
        &bytes(f["segment1"].as_str().unwrap()),
        &bytes(f["base_state"]["version"].as_str().unwrap()),
        &bytes(f["base_state"]["version"].as_str().unwrap()),
        &[],
    );
    assert_eq!(
        verify_chain(
            bind_fixture(&base),
            CheckpointOrigin::FirstCloudCheckpoint,
            &[bind_fixture(&wrong_result)],
            head(bind_fixture(&wrong_result)),
            limits()
        ),
        Err(Error::Result)
    );
    let wrong_frontier = object(
        template(1),
        Kind::Checkpoint,
        1,
        0,
        &bytes(f["base"].as_str().unwrap()),
        &[],
        &bytes(f["base_state"]["version"].as_str().unwrap()),
        &bytes(f["state1"]["frontiers"].as_str().unwrap()),
    );
    assert_eq!(verify_first(&wrong_frontier), Err(Error::Frontier));
}

#[test]
fn external_store_document_identity_accepts_empty_unmarked_and_legacy_without_mutation() {
    let doc = LoroDoc::new();
    doc.set_peer_id(9001).unwrap();
    for nonempty in [false, true] {
        if nonempty {
            doc.get_text("content")
                .insert(0, "Existing unmarked content")
                .unwrap();
            doc.commit();
        }
        let raw = snapshot_object(1, &doc);
        let original = raw.clone();
        assert!(verify_first(&raw).is_ok());
        assert_eq!(raw, original);
        let model = koru_core::schema::WorkspaceDoc::adopt(doc.fork()).unwrap();
        assert!(model.id().is_none());
        assert_eq!(model.schema_version(), None);
        assert!(model.compatibility().writable());
    }
    // Existing embedded id names a workspace; it is NOT the external document id.
    doc.get_map("meta")
        .insert("id", "10000000-0000-4000-8000-000000000001")
        .unwrap();
    doc.get_map("meta").insert("schema_version", 1_i64).unwrap();
    doc.commit();
    assert!(verify_first(&snapshot_object(1, &doc)).is_ok());
    doc.get_map("meta")
        .insert("schema_version", i64::MAX)
        .unwrap();
    doc.commit();
    assert_eq!(verify_first(&snapshot_object(1, &doc)), Err(Error::Schema));
}

#[test]
fn initial_existing_nonempty_document_and_independent_golden_correlations() {
    let golden = goldens()
        .into_iter()
        .find(|g| g["id"] == "O-INITIAL-NONEMPTY")
        .unwrap();
    let raw = bytes(golden["envelope"].as_str().unwrap());
    let decoded = decode(&raw, limits()).unwrap();
    let bound = decoded.bind(expected(&golden["expected"])).unwrap();
    assert_eq!(bound.metadata().sequence, 1);
    assert_eq!(bound.metadata().covers_through_sequence, 0);
    assert!(
        verify_chain(
            bound,
            CheckpointOrigin::FirstCloudCheckpoint,
            &[],
            head(bound),
            limits()
        )
        .is_ok()
    );
    assert_eq!(
        verify_chain(
            bound,
            CheckpointOrigin::LaterCheckpoint,
            &[],
            head(bound),
            limits()
        ),
        Err(Error::Chain)
    );
    let mut wrong = expected(&golden["expected"]);
    wrong.metadata.unit = Unit::WorkspaceDocument {
        document_id: id("90000000-0000-4000-8000-000000000099"),
    };
    assert_eq!(decoded.bind(wrong).unwrap_err(), Error::Binding);
    let mut wrong = expected(&golden["expected"]);
    wrong.logical_key = "w/foreign/document";
    assert_eq!(decoded.bind(wrong).unwrap_err(), Error::Key);
    let mut wrong = expected(&golden["expected"]);
    wrong.commitments.result_version_digest[0] ^= 1;
    assert_eq!(decoded.bind(wrong).unwrap_err(), Error::Binding);
    assert_eq!(
        bound.body().payload,
        bytes(golden["expected"]["payload"].as_str().unwrap())
    );
    assert_eq!(
        bound.body().result_version,
        bytes(golden["expected"]["result"].as_str().unwrap())
    );
}

#[test]
fn profile_snapshot_segment_checks_scope_before_merge_and_future_schema() {
    for index in 2..5 {
        let f = fixture(index);
        let base = cp(index, 1, 0, "base", "base_state");
        for field in if index == 4 {
            // Personal stores owner_profile_id. profile_id is independently
            // forbidden extra metadata, not the owner-substitution axis.
            vec!["profile_id", "owner_profile_id", "workspace_id"]
        } else {
            vec!["profile_id"]
        } {
            let doc = LoroDoc::new();
            doc.import(&bytes(f["base"].as_str().unwrap())).unwrap();
            doc.set_peer_id(9100).unwrap();
            doc.get_map("meta")
                .insert(field, "90000000-0000-4000-8000-000000000099")
                .unwrap();
            doc.commit();
            let altered = object(
                template(index),
                Kind::Segment,
                2,
                0,
                &doc.export(ExportMode::snapshot()).unwrap(),
                &bytes(f["base_state"]["version"].as_str().unwrap()),
                &doc.state_vv().encode(),
                &[],
            );
            assert_eq!(
                verify_chain(
                    bind_fixture(&base),
                    CheckpointOrigin::FirstCloudCheckpoint,
                    &[bind_fixture(&altered)],
                    head(bind_fixture(&altered)),
                    limits()
                ),
                Err(Error::Scope)
            );
        }
        let doc = LoroDoc::new();
        doc.import(&bytes(f["base"].as_str().unwrap())).unwrap();
        doc.get_map("meta")
            .insert("schema_version", i64::MAX)
            .unwrap();
        doc.commit();
        assert_eq!(
            verify_first(&snapshot_object(index, &doc)),
            Err(Error::Schema)
        );
    }
}

#[test]
fn captured_version_commitment_is_not_a_semantic_reencoding_hash() {
    let original = bytes("03bb1702a31f02c2a9bd9e88e48bff1c0c");
    let reordered = bytes("03bb1702c2a9bd9e88e48bff1c0ca31f02");
    assert_eq!(
        VersionVector::decode(&original).unwrap(),
        VersionVector::decode(&reordered).unwrap()
    );
    assert_ne!(hash(&original), hash(&reordered));
    let f = fixture(2);
    assert_eq!(
        bytes(f["multi_peer_state"]["version"].as_str().unwrap()),
        original
    );
    let raw = cp(2, 1, 0, "multi_peer_checkpoint", "multi_peer_state");
    assert!(verify_first(&raw).is_ok());
    let bound = bind_fixture(&raw);
    let restored = restore_chain(
        bound,
        CheckpointOrigin::FirstCloudCheckpoint,
        &[],
        head(bound),
        limits(),
    )
    .unwrap();
    assert_eq!(restored.result_version(), original);
    let received = LoroDoc::new();
    assert!(
        received
            .import(&restored.snapshot().unwrap())
            .unwrap()
            .pending
            .is_none()
    );
    assert_eq!(
        received.state_vv(),
        VersionVector::decode(&original).unwrap()
    );
    let payload = bytes(f["multi_peer_checkpoint"].as_str().unwrap());
    // Same semantic state, but a different captured predecessor commitment.
    let duplicate = object(
        template(2),
        Kind::Segment,
        2,
        0,
        &payload,
        &reordered,
        &original,
        &[],
    );
    assert_eq!(
        verify_chain(
            bind_fixture(&raw),
            CheckpointOrigin::FirstCloudCheckpoint,
            &[bind_fixture(&duplicate)],
            head(bind_fixture(&duplicate)),
            limits()
        ),
        Err(Error::Predecessor)
    );
}

#[test]
fn restored_exports_converge_for_all_five_units_and_refuse_incomplete_state() {
    for index in 0..5 {
        let base = cp(index, 1, 0, "base", "base_state");
        let first = segment(index, 2, 1);
        let second = segment(index, 4, 2);
        let tail = [bind_fixture(&first), bind_fixture(&second)];
        let restored = restore_chain(
            bind_fixture(&base),
            CheckpointOrigin::FirstCloudCheckpoint,
            &tail,
            head(tail[1]),
            limits(),
        )
        .unwrap();
        let expected = LoroDoc::new();
        expected.import(bind_fixture(&base).body().payload).unwrap();
        for segment in tail {
            expected.import(segment.body().payload).unwrap();
        }
        assert_eq!(
            expected.state_vv(),
            VersionVector::decode(tail[1].body().result_version).unwrap()
        );
        let full = LoroDoc::new();
        assert!(
            full.import(&restored.snapshot().unwrap())
                .unwrap()
                .pending
                .is_none()
        );
        assert_eq!(full.state_vv(), expected.state_vv());
        assert_eq!(full.get_deep_value(), expected.get_deep_value());
        let receiver = LoroDoc::new();
        receiver.import(bind_fixture(&base).body().payload).unwrap();
        let delta = restored
            .updates_since(&receiver.state_vv().encode())
            .unwrap();
        assert!(receiver.import(&delta).unwrap().pending.is_none());
        assert_eq!(receiver.state_vv(), expected.state_vv());
        assert_eq!(receiver.get_deep_value(), expected.get_deep_value());
        // Exact retry and already-current export remain harmless CRDT imports.
        assert!(receiver.import(&delta).unwrap().pending.is_none());
        receiver
            .import(&restored.updates_since(restored.result_version()).unwrap())
            .unwrap();
        assert_eq!(receiver.state_vv(), expected.state_vv());
        assert_eq!(restored.result_version(), tail[1].body().result_version);
        assert!(matches!(
            restore_chain(
                bind_fixture(&base),
                CheckpointOrigin::FirstCloudCheckpoint,
                &tail[1..],
                head(tail[1]),
                limits()
            ),
            Err(Error::Predecessor)
        ));
        assert_eq!(restored.updates_since(&[255]), Err(Error::Loro));
        assert_eq!(
            restored.updates_since(&vec![0; limits().vector_limit() as usize + 1]),
            Err(Error::TooLarge)
        );
    }
}

#[test]
fn restored_large_document_exports_are_bounded_and_keep_content() {
    let doc = LoroDoc::new();
    doc.set_peer_id(9600).unwrap();
    // Deterministic varied content, not a compressed repetition-only fixture.
    let text: String = (0..30_000u32).map(|i| format!("{i:08x} ")).collect();
    assert!(text.len() > 200 * 1024);
    doc.get_text("content").insert(0, &text).unwrap();
    doc.commit();
    let raw = snapshot_object(1, &doc);
    let bound = bind_fixture(&raw);
    let restored = restore_chain(
        bound,
        CheckpointOrigin::FirstCloudCheckpoint,
        &[],
        head(bound),
        limits(),
    )
    .unwrap();
    let receiver = LoroDoc::new();
    assert!(
        receiver
            .import(&restored.snapshot().unwrap())
            .unwrap()
            .pending
            .is_none()
    );
    assert_eq!(receiver.get_text("content").to_string(), text);
    let fresh = LoroDoc::new();
    fresh
        .import(
            &restored
                .updates_since(&VersionVector::default().encode())
                .unwrap(),
        )
        .unwrap();
    assert_eq!(fresh.get_deep_value(), receiver.get_deep_value());
    // Export caps are independently enforced even on previously valid content.
    let restricted = RestoredContent {
        limits: Limits::new(HEADER_MAX, VECTOR_MAX, 1, 1).unwrap(),
        ..restored
    };
    assert_eq!(restricted.snapshot(), Err(Error::TooLarge));
    assert_eq!(
        restricted.updates_since(&VersionVector::default().encode()),
        Err(Error::TooLarge)
    );
}
