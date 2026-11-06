// SPDX-License-Identifier: MPL-2.0

//! **The field names are the contract**, and one of them is a contract with a
//! second body: the six filters `TaskQuery` shares with `AttributeQuery` are
//! two declarations of one shape, so the test that holds them equal is here
//! rather than in a comment.

use super::*;
use crate::attributes::{AttributeQuery, DateWindow, InstantWindow};

fn keys(value: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .expect("an object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// The six filter names, spelled once here so a rename on either body has to
/// come past this list.
const SHARED: [&str; 6] = ["has", "missing", "tags", "priority", "min_priority", "due"];

fn a_due_filter() -> DueFilter {
    DueFilter {
        dates: Some(DateWindow {
            after: Some("2026-09-01".into()),
            before: Some("2026-09-08".into()),
        }),
        instants: Some(InstantWindow {
            after_ms: Some(1_772_000_000_000),
            before_ms: None,
        }),
    }
}

/// **ADR-274's mirror test.** `AttributeQuery`'s wire shape is not touched and
/// `TaskQuery` re-declares its six filters; this is what stops the two
/// declarations drifting — a type changed on one side, a `#[serde(rename)]`
/// added to the other, a bound quietly made optional.
#[test]
fn the_shared_filters_serialize_identically_on_both_queries() {
    let attributes = serde_json::to_value(AttributeQuery {
        has: Some("koru.due".into()),
        missing: Some("koru.plan".into()),
        tags: vec!["tag-a".into(), "tag-b".into()],
        priority: Some("high".into()),
        min_priority: Some("medium".into()),
        due: Some(a_due_filter()),
        ..AttributeQuery::default()
    })
    .unwrap();
    let tasks = serde_json::to_value(TaskQuery {
        has: Some("koru.due".into()),
        missing: Some("koru.plan".into()),
        tags: vec!["tag-a".into(), "tag-b".into()],
        priority: Some("high".into()),
        min_priority: Some("medium".into()),
        due: Some(a_due_filter()),
        ..TaskQuery::default()
    })
    .unwrap();

    for key in SHARED {
        assert_eq!(
            attributes.get(key),
            tasks.get(key),
            "the two queries disagree about `{key}`"
        );
        assert!(attributes.get(key).is_some(), "`{key}` was not serialized");
    }
}

/// The whole body, so a field added to one of the twenty has to be named
/// here.
#[test]
fn a_query_carries_its_filters_its_scope_and_its_page() {
    let body = serde_json::to_value(TaskQuery::default()).unwrap();
    assert_eq!(
        keys(&body),
        [
            "after",
            "bucket",
            "bucket_windows",
            "completed_on_or_after",
            "covering",
            "due",
            "group_by",
            "has",
            "include_trashed",
            "lifecycle",
            "limit",
            "list",
            "min_priority",
            "missing",
            "overdue",
            "priority",
            "recently_completed_since",
            "sort",
            "tags",
            "workspaces",
        ]
    );
}

/// **Absence is `open`, `due` and a full page** — the three defaults a Tasks
/// view relies on when it sends the emptiest body it can.
#[test]
fn an_empty_query_asks_for_open_tasks_by_due_date() {
    let body: TaskQuery = serde_json::from_str("{}").unwrap();
    assert_eq!(body.lifecycle, LifecycleFilter::Open);
    assert_eq!(body.sort, TaskSort::Due);
    assert_eq!(body.limit, DEFAULT_LIMIT);
    assert!(!body.include_trashed);
    assert_eq!(body.after, None);
    assert!(body.workspaces.is_empty());
}

/// The tokens, pinned. A Rust rename moves both sides together; the JSON is
/// what a shipped client already sends.
#[test]
fn the_lifecycle_and_sort_tokens_are_snake_case() {
    for (value, token) in [
        (LifecycleFilter::Open, "\"open\""),
        (LifecycleFilter::Completed, "\"completed\""),
        (LifecycleFilter::All, "\"all\""),
    ] {
        assert_eq!(serde_json::to_string(&value).unwrap(), token);
    }
    for (value, token) in [
        (TaskSort::Due, "\"due\""),
        (TaskSort::Priority, "\"priority\""),
        (TaskSort::Modified, "\"modified\""),
        (TaskSort::Manual, "\"manual\""),
    ] {
        assert_eq!(serde_json::to_string(&value).unwrap(), token);
    }
}

/// A field a client has not heard of is ignored rather than fatal — the crate's
/// standing serde rule, asserted on the newest body.
#[test]
fn a_field_this_build_does_not_know_is_ignored() {
    // `bucket` used to be the stand-in here and is a real field now, which is
    // the hazard: an unknown-field test whose unknown field ships stops testing
    // anything and says nothing about it.
    let body: TaskQuery =
        serde_json::from_str(r#"{ "starred": true, "lifecycle": "all" }"#).unwrap();
    assert_eq!(body.lifecycle, LifecycleFilter::All);
}

/// **A bucket travels with the zone it was resolved in, or not at all.** SPEC
/// §7 is a function of the reader's zone and this store carries no timezone
/// database, so the six edges and the priority floor are the caller's numbers —
/// the same reason `overdue` is a caller's window rather than a server's clock.
#[test]
fn a_bucket_query_carries_the_readers_windows() {
    let windows = BucketWindows {
        today: "2026-06-15".into(),
        horizon: "2026-06-22".into(),
        upcoming_horizon: "2026-07-15".into(),
        today_ends_ms: 1_781_215_200_000,
        horizon_ends_ms: 1_781_820_000_000,
        upcoming_ends_ms: 1_783_807_200_000,
        now_ms: 1_781_164_800_000,
        min_next_priority: "high".into(),
    };
    let body = serde_json::to_value(TaskQuery {
        bucket: Some("today".into()),
        bucket_windows: Some(windows.clone()),
        group_by: Some("bucket".into()),
        ..TaskQuery::default()
    })
    .unwrap();
    assert_eq!(body["bucket"], "today");
    assert_eq!(body["group_by"], "bucket");
    assert_eq!(
        keys(&body["bucket_windows"]),
        [
            "horizon",
            "horizon_ends_ms",
            "min_next_priority",
            "now_ms",
            "today",
            "today_ends_ms",
            "upcoming_ends_ms",
            "upcoming_horizon",
        ],
        "two horizons, each stated as a day and as an instant, plus now and the floor"
    );

    let back: TaskQuery = serde_json::from_str(&serde_json::to_string(&body).unwrap()).unwrap();
    assert_eq!(back.bucket_windows, Some(windows));

    let empty: TaskQuery = serde_json::from_str("{}").unwrap();
    assert_eq!(empty.bucket, None, "absence is every bucket");
    assert!(empty.bucket_windows.is_none(), "and no zone claimed");
}

/// The five tokens, pinned — the vocabulary a client sends and the one the
/// group rows come back in.
#[test]
fn the_bucket_tokens_are_the_agenda_in_order() {
    assert_eq!(
        BUCKETS,
        ["today", "next", "upcoming", "later", "completed"],
        "agenda order, because a rail draws them in it"
    );
}

fn a_row() -> TaskRow {
    TaskRow {
        workspace: WorkspaceId::new(),
        document: DocumentId::new(),
        node: "node-1".into(),
        task: "task:w:d:n".into(),
        node_kind: "paragraph".into(),
        lifecycle: "open".into(),
        document_trashed: false,
        in_trash: true,
        breadcrumb: "Acme / 四半期計画 / Ship it".into(),
        title: "Ship it".into(),
        attributes: Vec::new(),
    }
}

/// **Two trash flags, two spellings.** One field for both facts is the
/// collision this row was amended to avoid: a binned node in a live document
/// and a live node in a binned document are different situations and a restore
/// surface acts differently on each.
#[test]
fn a_row_carries_both_trash_answers_under_their_own_names() {
    let json = serde_json::to_value(a_row()).unwrap();
    assert_eq!(json["in_trash"], serde_json::json!(true));
    assert_eq!(json["document_trashed"], serde_json::json!(false));
    assert_eq!(
        keys(&json),
        [
            "attributes",
            "breadcrumb",
            "document",
            "document_trashed",
            "in_trash",
            "lifecycle",
            "node",
            "node_kind",
            "task",
            "title",
            "workspace",
        ]
    );
}

/// **A list is a filter and its counts are a second question** (SPEC §8): the
/// membership rides in `attributes` like any other value, the filter is an id,
/// and the rail's numbers come back as groups over the corpus rather than as
/// something a client counts off the page.
#[test]
fn a_list_query_filters_by_id_and_asks_for_its_counts_separately() {
    let body = serde_json::to_value(TaskQuery {
        list: Some("00000000-0000-4000-8000-00000000cafe".into()),
        group_by: Some("list".into()),
        overdue: Some(a_due_filter()),
        ..TaskQuery::default()
    })
    .unwrap();
    assert_eq!(body["list"], "00000000-0000-4000-8000-00000000cafe");
    assert_eq!(body["group_by"], "list");
    assert!(body["overdue"].is_object(), "the windows travel whole");

    let empty: TaskQuery = serde_json::from_str("{}").unwrap();
    assert_eq!(empty.list, None, "absence is every list");
    assert_eq!(empty.group_by, None, "and no counts asked for");
    assert!(empty.overdue.is_none());
}

#[test]
fn a_group_carries_an_id_a_count_and_the_overdue_share() {
    let json = serde_json::to_value(TaskGroup {
        workspace: WorkspaceId::new(),
        list: "list-1".into(),
        count: 14,
        overdue: 1,
        trashed: 0,
    })
    .unwrap();
    assert_eq!(
        keys(&json),
        ["count", "list", "overdue", "trashed", "workspace"],
        "no name and no color: the list table is where an id becomes a word"
    );

    // **The same shape carries a bucket's numbers**, with the token in the key
    // column — the field's name is the list grouping's history rather than a
    // second meaning, and the type's doc says so. One row shape means a client
    // reads `groups` the same way whichever question it asked.
    let rail = serde_json::to_value(TaskGroup {
        workspace: WorkspaceId::new(),
        list: "today".into(),
        count: 5,
        overdue: 3,
        trashed: 0,
    })
    .unwrap();
    assert_eq!(keys(&rail), keys(&json));
    assert!(BUCKETS.contains(&rail["list"].as_str().unwrap()));
}

#[test]
fn a_response_round_trips_with_its_cursor_and_its_freshness() {
    let response = TaskQueryResponse {
        rows: vec![a_row()],
        definitions: Vec::new(),
        tags: Vec::new(),
        groups: vec![TaskGroup {
            workspace: WorkspaceId::new(),
            list: "list-1".into(),
            count: 14,
            overdue: 1,
            trashed: 0,
        }],
        projected: false,
        updating_workspaces: vec![WorkspaceId::new()],
        limit: 100,
        next_after: Some("task:w:d:n".into()),
        restarted: true,
        covered: None,
    };
    let json = serde_json::to_string(&response).unwrap();
    let back: TaskQueryResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(back.rows.len(), 1);
    assert_eq!(back.rows[0].breadcrumb, "Acme / 四半期計画 / Ship it");
    assert_eq!(back.next_after.as_deref(), Some("task:w:d:n"));
    assert!(back.restarted);
    assert_eq!(back.groups.len(), 1);
    assert_eq!((back.groups[0].count, back.groups[0].overdue), (14, 1));
    assert!(!back.projected);
    assert_eq!(back.updating_workspaces.len(), 1);
}

// ------------------------------------------------------------ covering

fn a_requirement() -> RevisionRequirement {
    RevisionRequirement {
        workspace: WorkspaceId::new(),
        document: DocumentId::new(),
        peer: "12345678901234567890".to_owned(),
        counter: 7,
    }
}

/// Pagination and requirements ride together: the cursor is `after`, the
/// requirement is `covering`, and neither field is the other.
#[test]
fn a_query_carries_its_cursor_and_its_requirements_together() {
    let query = TaskQuery {
        after: Some("task:w:d:n".into()),
        covering: vec![a_requirement()],
        ..TaskQuery::default()
    };
    let json = serde_json::to_value(&query).unwrap();
    assert_eq!(json["after"], "task:w:d:n");
    // The peer travels as a string: it is a `u64` and a JSON number is an
    // `f64`, so an id from the top of the range would lose its low bits.
    assert_eq!(json["covering"][0]["peer"], "12345678901234567890");
    assert_eq!(json["covering"][0]["counter"], 7);
    let back: TaskQuery = serde_json::from_value(json).unwrap();
    assert_eq!(back.after.as_deref(), Some("task:w:d:n"));
    assert_eq!(back.covering, query.covering);
}

/// An old client sends no `covering`; a new server reads an empty one.
#[test]
fn a_request_without_requirements_is_an_empty_requirement_list() {
    let back: TaskQuery = serde_json::from_str(r#"{"workspaces":[]}"#).unwrap();
    assert!(back.covering.is_empty());
}

/// An old server sends no `covered`; a new client reads *unknown*, not
/// coverage. This is the whole compatibility story, so it is pinned.
#[test]
fn an_answer_without_covered_is_unknown_never_covered() {
    let json = r#"{"rows":[],"definitions":[],"tags":[],"projected":true,"limit":100}"#;
    let back: TaskQueryResponse = serde_json::from_str(json).unwrap();
    assert_eq!(back.covered, None);
}

#[test]
fn covered_round_trips_both_ways() {
    for value in [Some(true), Some(false), None] {
        let response = TaskQueryResponse {
            rows: Vec::new(),
            definitions: Vec::new(),
            tags: Vec::new(),
            groups: Vec::new(),
            projected: true,
            updating_workspaces: Vec::new(),
            limit: 100,
            next_after: None,
            restarted: false,
            covered: value,
        };
        let json = serde_json::to_string(&response).unwrap();
        let back: TaskQueryResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.covered, value);
    }
}

#[test]
fn a_revision_is_spelled_hex_and_read_back_exactly() {
    let bytes: Vec<u8> = (0..=255).collect();
    let hex = revision_hex(&bytes);
    assert_eq!(hex.len(), 512);
    assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(revision_bytes(&hex).unwrap(), bytes);
    assert_eq!(revision_bytes("").unwrap(), Vec::<u8>::new());
}

#[test]
fn a_revision_that_is_not_hex_is_refused_rather_than_guessed() {
    assert_eq!(revision_bytes("abc"), None, "odd length");
    assert_eq!(revision_bytes("zz"), None, "not hex");
    assert_eq!(revision_bytes("é1"), None, "not ascii");
}
