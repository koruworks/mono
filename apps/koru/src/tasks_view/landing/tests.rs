// SPDX-License-Identifier: MPL-2.0

//! Where a task from search lands, and that a landing nobody is waiting for
//! any more changes nothing.

use std::time::{Duration, Instant};

use koru_store_wire::attributes::AttributeRowValue;

use super::*;

fn attr(role: &str, value: &str) -> AttributeRowValue {
    AttributeRowValue {
        id: format!("{role}-1"),
        role: role.to_owned(),
        kind: "text".to_owned(),
        value: value.to_owned(),
        zone: None,
        label: None,
    }
}

fn row(lifecycle: &str, bucket: Option<&str>, attributes: Vec<AttributeRowValue>) -> TaskRow {
    TaskRow {
        workspace: koru_store_core::ids::WorkspaceId::new(),
        document: koru_store_core::ids::DocumentId::new(),
        node: NodeId::new().to_storage(),
        task: String::new(),
        node_kind: "paragraph".to_owned(),
        lifecycle: lifecycle.to_owned(),
        document_trashed: false,
        in_trash: false,
        breadcrumb: String::new(),
        title: "Ship it".to_owned(),
        attributes,
        bucket: bucket.map(str::to_owned),
    }
}

fn placed(row: &TaskRow, live: &[ListId]) -> Option<(RailTarget, Rule)> {
    task_destination(row, |list| live.contains(&list)).map(|p| (p.target, p.rule))
}

fn queue(bucket: Bucket) -> RailTarget {
    RailTarget::Bucket(bucket)
}

/// S9, rule by rule, in the order the rules are tried.
#[test]
fn a_task_lands_where_it_is_now() {
    let list = ListId::new();
    let in_list = || attr(koru_core::attributes::KEY_LIST, &list.to_storage());
    let pinned = || attr(koru_core::attributes::KEY_TODAY, "true");

    // 1. Completed wins over everything: overdue, pinned, in a list.
    assert_eq!(
        placed(
            &row("completed", Some("completed"), vec![in_list(), pinned()]),
            &[list]
        ),
        Some((queue(Bucket::Completed), Rule::Completed))
    );
    // 2. Naturally on Today — overdue included — or pinned there by hand.
    assert_eq!(
        placed(&row("open", Some("today"), vec![in_list()]), &[list]),
        Some((queue(Bucket::Today), Rule::Today))
    );
    assert_eq!(
        placed(&row("open", Some("upcoming"), vec![pinned(), in_list()]), &[list]),
        Some((queue(Bucket::Today), Rule::Today)),
        "pinned with a later due: its natural bucket is not Today, and it is"
    );
    // 3. Otherwise the list that owns it, while that list is live.
    assert_eq!(
        placed(&row("open", Some("later"), vec![in_list()]), &[list]),
        Some((RailTarget::List(list), Rule::List))
    );
    // 4. Otherwise its queue: no list, or a list deleted since.
    assert_eq!(
        placed(&row("open", Some("next"), Vec::new()), &[list]),
        Some((queue(Bucket::Next), Rule::Queue)),
        "undated, High: Next"
    );
    assert_eq!(
        placed(&row("open", Some("later"), vec![in_list()]), &[]),
        Some((queue(Bucket::Later), Rule::Queue)),
        "its list was deleted: routed by its bucket"
    );
    assert_eq!(
        placed(&row("open", Some("upcoming"), Vec::new()), &[]),
        Some((queue(Bucket::Upcoming), Rule::Queue))
    );
}

/// No bucket rule is written in the UI: a row the store did not place, and
/// that no earlier rule places, has no destination.
#[test]
fn a_row_with_no_bucket_is_not_guessed_at() {
    assert_eq!(placed(&row("open", None, Vec::new()), &[]), None);
    assert_eq!(placed(&row("open", Some("someday"), Vec::new()), &[]), None);
    // The first three rules need no bucket.
    let list = ListId::new();
    assert!(placed(&row("completed", None, Vec::new()), &[]).is_some());
    assert!(
        placed(
            &row(
                "open",
                None,
                vec![attr(koru_core::attributes::KEY_LIST, &list.to_storage())]
            ),
            &[list]
        )
        .is_some()
    );
}

fn ids() -> (DocumentId, NodeId) {
    (DocumentId::new(), NodeId::new())
}

#[test]
fn the_answer_lands_once() {
    let mut landing = SearchLanding::default();
    let (document, node) = ids();
    let now = Instant::now();
    let (generation, send) = landing.begin_with_for_test(document, node, now);
    assert!(landing.pending());
    assert!(landing.poll(now).is_none(), "nothing yet");

    send.send((generation, Some(row("open", Some("later"), Vec::new()))))
        .unwrap();
    assert!(matches!(landing.poll(now), Some(Landed::Task { .. })));
    assert!(!landing.pending());
    assert!(landing.poll(now).is_none(), "and only once");
}

/// A task deleted or unmarked since it was indexed, and a request that
/// failed: the task's own document opens instead.
#[test]
fn a_task_that_is_gone_opens_its_document() {
    let mut landing = SearchLanding::default();
    let (document, node) = ids();
    let now = Instant::now();
    let (generation, send) = landing.begin_with_for_test(document, node, now);
    send.send((generation, None)).unwrap();
    match landing.poll(now) {
        Some(Landed::Document {
            document: d,
            node: n,
            ..
        }) => assert_eq!((d, n), (document, node)),
        other => panic!("the document, not {other:?}"),
    }

    // The worker went away without answering at all.
    let (_, send) = landing.begin_with_for_test(document, node, now);
    drop(send);
    assert!(matches!(landing.poll(now), Some(Landed::Document { .. })));
}

/// Past the ceiling the landing is retired and the document opens — and the
/// answer that arrives afterwards changes nothing.
#[test]
fn an_answer_after_the_ceiling_changes_nothing() {
    let mut landing = SearchLanding::default();
    let (document, node) = ids();
    let now = Instant::now();
    let (generation, send) = landing.begin_with_for_test(document, node, now);
    assert!(landing.poll(now + wait_ceiling() - Duration::from_millis(1)).is_none());
    assert!(matches!(
        landing.poll(now + wait_ceiling()),
        Some(Landed::Document { .. })
    ));
    // Late. Nobody is listening, and nothing lands.
    let _ = send.send((generation, Some(row("open", Some("later"), Vec::new()))));
    assert!(landing.poll(now + wait_ceiling() * 2).is_none());
    assert!(!landing.pending());
}

/// Two results opened in quick succession: only the second lands.
#[test]
fn a_newer_landing_discards_the_older_one() {
    let mut landing = SearchLanding::default();
    let now = Instant::now();
    let (first_doc, first_node) = ids();
    let (first, first_send) = landing.begin_with_for_test(first_doc, first_node, now);
    let (second_doc, second_node) = ids();
    let (second, second_send) = landing.begin_with_for_test(second_doc, second_node, now);
    assert_ne!(first, second);

    // The first one's answer arrives; its channel is no longer read.
    let _ = first_send.send((first, Some(row("open", Some("today"), Vec::new()))));
    assert!(landing.poll(now).is_none());
    second_send
        .send((second, Some(row("open", Some("later"), Vec::new()))))
        .unwrap();
    match landing.poll(now) {
        Some(Landed::Task { document, node, .. }) => {
            assert_eq!((document, node), (second_doc, second_node))
        }
        other => panic!("the second, not {other:?}"),
    }
}

/// Any navigation retires a landing in flight.
#[test]
fn a_retired_landing_never_lands() {
    let mut landing = SearchLanding::default();
    let (document, node) = ids();
    let now = Instant::now();
    let (generation, send) = landing.begin_with_for_test(document, node, now);
    assert!(landing.retire());
    let _ = send.send((generation, Some(row("open", Some("later"), Vec::new()))));
    assert!(landing.poll(now).is_none());
    assert!(!landing.retire(), "nothing left to retire");
}

/// The landing's question is the exclusive selector with this device's
/// windows, and nothing else — a filter beside it would be refused.
#[test]
fn the_landing_asks_for_exactly_one_task_with_windows() {
    let (_, _, windows) = windows_now();
    let node = NodeId::new();
    let query = only_query(
        koru_store_core::ids::WorkspaceId::new(),
        koru_store_core::ids::DocumentId::new(),
        node,
        windows,
    );
    assert_eq!(query.workspaces.len(), 1);
    let only = query.only.as_ref().unwrap();
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].node, node.to_storage());
    assert!(query.bucket_windows.is_some());
    assert!(query.bucket.is_none() && query.list.is_none() && query.after.is_none());
    assert!(query.group_by.is_none() && query.tags.is_empty());
}
