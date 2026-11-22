// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_projection::model::ProjectedNode;

fn node(blob: Option<&str>, in_trash: bool) -> ProjectedNode {
    ProjectedNode {
        id: "n".into(),
        parent: None,
        ordinal: 0,
        depth: 0,
        kind: "image".into(),
        text: String::new(),
        heading: None,
        list_style: None,
        done: false,
        level_name: None,
        name: None,
        icon: None,
        colour: None,
        language: None,
        blob: blob.map(str::to_owned),
        mime: None,
        embed: None,
        in_trash,
        runs: Vec::new(),
        attrs: Vec::new(),
    }
}

/// Both owe their first full pass, and a fresh inbox says so.
#[test]
fn a_new_inbox_owes_the_catch_up_and_the_queue() {
    let inbox = Inbox::new();
    assert!(inbox.take_mirror().catch_up_all);
    assert!(inbox.take_coordinator().dirty);
    assert!(inbox.take_mirror().is_empty());
    assert!(inbox.take_coordinator().is_empty());
}

/// **Only a projected document holding a live blob is a trigger**; a listing
/// change owes both a catch-up and a recompute; landed bytes coalesce.
#[test]
fn the_feed_turns_projector_news_into_triggers() {
    let inbox = Inbox::new();
    let _ = (inbox.take_mirror(), inbox.take_coordinator());
    let mut feed = ReadingsFeed::new(Arc::clone(&inbox));
    let (ws, with, without, binned) = (
        WorkspaceId::new(),
        DocumentId::new(),
        DocumentId::new(),
        DocumentId::new(),
    );
    for (document, nodes) in [
        (with, vec![node(Some("b3:aa"), false)]),
        (without, vec![node(None, false)]),
        (binned, vec![node(Some("b3:bb"), true)]),
    ] {
        feed.projected(&DocumentProjection {
            workspace: ws,
            document,
            source_revision: vec![1],
            nodes,
        });
    }
    let work = inbox.take_coordinator();
    assert_eq!(work.documents, vec![(ws, with)]);
    assert!(!work.dirty);

    let hash = koru_store_core::model::BlobHash::parse(&format!("b3:{}", "a".repeat(64))).unwrap();
    let landed = BlobStored {
        workspace: ws,
        hash: hash.clone(),
    };
    Tee(Arc::clone(&inbox)).blobs_stored(&[landed.clone(), landed]);
    let work = inbox.take_coordinator();
    assert_eq!(work.hashes, vec![(ws, vec![hash.as_str().to_owned()])]);

    feed.listing_changed(ws);
    assert_eq!(inbox.take_mirror().catch_up, vec![ws]);
    assert!(inbox.take_coordinator().dirty);
    assert_eq!(inbox.mirror_backlog(), (0, 0));
}

/// A projection rebuild projects every document at once; past the ceiling
/// the coordinator recomputes its queue instead of resolving each hint.
#[test]
fn too_many_hints_become_one_recompute() {
    let inbox = Inbox::new();
    let _ = inbox.take_coordinator();
    let ws = WorkspaceId::new();
    for _ in 0..=HINT_CEILING {
        inbox.document_projected(ws, DocumentId::new());
    }
    let work = inbox.take_coordinator();
    assert!(work.dirty);
    assert!(work.documents.len() < HINT_CEILING);
}

/// A stop wakes a waiting thread, and a wait with nothing to do times out.
#[test]
fn a_stop_ends_the_waits() {
    let inbox = Inbox::new();
    let _ = (inbox.take_mirror(), inbox.take_coordinator());
    assert!(!inbox.wait_for_coordinator(Duration::from_millis(1)));
    let waiting = Arc::clone(&inbox);
    let thread = std::thread::spawn(move || waiting.wait_for_mirror(Duration::from_secs(30)));
    inbox.stop();
    assert!(!thread.join().unwrap(), "the wait saw the stop");
    assert!(inbox.is_stopping());
}
