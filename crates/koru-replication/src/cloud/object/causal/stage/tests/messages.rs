// SPDX-License-Identifier: MPL-2.0

//! The producer's half of "commit messages survive a Cloud checkpoint" (Cloud
//! MCP §16): the checkpoint the worker writes from a unit's recovered content
//! keeps every change's commit message, which is where an app connection's
//! `actor/1` attribution, its creation request hash included, lives.

use super::*;

/// Every change of the document, with its commit message, in a stable order.
fn messages(doc: &LoroDoc) -> Vec<(loro::ID, Option<String>)> {
    let mut changes = Vec::new();
    for (&peer, &end) in doc.oplog_vv().iter() {
        let mut counter = 0;
        while counter < end {
            let change = doc.get_change(loro::ID::new(peer, counter)).unwrap();
            changes.push((change.id, change.message.as_deref().map(str::to_owned)));
            counter = change.id.counter + change.len as i32;
        }
    }
    changes.sort();
    changes
}

fn commit(doc: &LoroDoc, text: &str, message: &str) {
    let body = doc.get_text("body");
    body.insert(body.len_unicode(), text).unwrap();
    doc.set_next_commit_message(message);
    doc.commit();
}

#[test]
fn a_cloud_checkpoint_keeps_each_changes_commit_message() {
    let index = 1;
    assert!(matches!(
        template(index).unit,
        Unit::WorkspaceDocument { .. }
    ));
    let cp_meta = |sequence, coverage| Metadata {
        kind: Kind::Checkpoint,
        sequence,
        covers_through_sequence: coverage,
        ..bind_fixture(&cp(index, sequence, coverage, "base", "base_state")).metadata()
    };
    let doc = LoroDoc::new();
    doc.set_peer_id(7201).unwrap();
    commit(
        &doc,
        "seed",
        r#"{"koru":"actor/1","kind":"assistant","op":"1","create":{"request_hash":"aa"}}"#,
    );
    let (first, _) = stage_initial_checkpoint(
        cp_meta(1, 0),
        &doc.export(ExportMode::snapshot()).unwrap(),
        limits(),
    )
    .unwrap()
    .into_parts();
    let first = bind_fixture(&first);
    let restored = restore_chain(
        first,
        CheckpointOrigin::FirstCloudCheckpoint,
        &[],
        head(first),
        limits(),
    )
    .unwrap();
    // Two later edits, one of them by a second peer, each with its message.
    let before = doc.oplog_vv();
    commit(&doc, " one", r#"{"koru":"actor/1","op":"2"}"#);
    doc.set_peer_id(7202).unwrap();
    commit(&doc, " two", r#"{"koru":"actor/1","op":"3"}"#);
    let payload = doc.export(ExportMode::updates(&before)).unwrap();
    let segment_meta = Metadata {
        kind: Kind::Segment,
        ..cp_meta(2, 0)
    };
    let (segment, _) = restored
        .stage_segment(segment_meta, &payload)
        .unwrap()
        .unwrap()
        .into_parts();
    let segment = bind_fixture(&segment);
    let restored = restore_chain(
        first,
        CheckpointOrigin::FirstCloudCheckpoint,
        &[segment],
        head(segment),
        limits(),
    )
    .unwrap();
    // The worker's checkpoint: a fresh export of the recovered content.
    let (checkpoint, proof) = restored
        .stage_checkpoint(cp_meta(3, 2))
        .unwrap()
        .into_parts();
    let checkpoint = bind_fixture(&checkpoint);
    assert_eq!(
        verify_chain(
            checkpoint,
            CheckpointOrigin::LaterCheckpoint,
            &[],
            head(checkpoint),
            limits()
        )
        .unwrap(),
        proof
    );
    let read = LoroDoc::new();
    assert!(
        read.import(checkpoint.body().payload)
            .unwrap()
            .pending
            .is_none()
    );
    let expected = messages(&doc);
    assert!(expected.len() >= 3, "{expected:?}");
    assert!(expected.iter().all(|(_, message)| message.is_some()));
    assert_eq!(messages(&read), expected);
    // The seed's creation hash is still where recovery reads it.
    let seed = read.get_change(loro::ID::new(7201, 0)).unwrap();
    assert!(seed.message.unwrap().contains(r#""request_hash":"aa""#));
}
