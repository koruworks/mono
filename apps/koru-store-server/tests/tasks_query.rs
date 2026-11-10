// SPDX-License-Identifier: MPL-2.0

//! **The tasks query, asked the way a Tasks view asks it** — R-1 (roadmap rev6
//! §5): freshness, page boundaries, and the deterministic performance
//! assertions §6 puts store-side.
//!
//! `tasks.rs` is the vertical slice: one task, every semantic. This one is
//! about the *reading* of a list — what a client is told when a workspace
//! cannot be answered for, what happens to a page boundary when the list moves
//! under it, and what a query costs when nothing has changed. All three are
//! asserted through the public route, because all three are promises to a
//! client rather than facts about a table; the same properties over a large
//! corpus are pinned in `koru-projection`, against `crate::fixtures`, where a
//! corpus can be built without publishing fifty-five thousand rows through a
//! sync session.
//!
//! **Structure, never a wall clock.** The counters say how many documents the
//! projector walked and how many rows the port answered; a millisecond would
//! say what this machine was doing at the time.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use koru_core::attributes::KEY_PRIORITY;
use koru_core::node::{AttrValue, Node};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::tasks::{self, LifecycleReason};
use koru_core::time::{DateOnly, Temporal};
use koru_store_client::{Client, LifecycleFilter, TaskQuery};
use koru_store_core::ids::WorkspaceId;
use koru_workspace::store::StoreSession;

fn segment() -> &'static str {
    koru_channel::segment()
}

struct Store {
    child: Option<Child>,
    root: PathBuf,
}

impl Store {
    fn start(root: &Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_koru-store-server"));
        command
            .arg("serve")
            // A suite starting daemons on throwaway roots must never raise a
            // modal dialog: nothing dismisses it, so the process would never
            // exit and would go on holding its databases and this binary.
            .env("KORU_STORE_NO_DIALOG", "1")
            .arg("--console")
            .arg("--root")
            .arg(root);
        common::configure(&mut command, root);
        let child = command.spawn().expect("spawn koru-store-server");
        let deadline = Instant::now() + Duration::from_secs(30);
        while common::discovery(root).map(|d| Client::new(d).status().is_ok()) != Some(true) {
            assert!(
                Instant::now() < deadline,
                "the store never became reachable"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Self {
            child: Some(child),
            root: root.to_path_buf(),
        }
    }

    fn client(&self) -> Client {
        Client::new(common::discovery(&self.root).expect("discover"))
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn root_for(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("koru-store-tasksq-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp root");
    dir
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn counter(client: &Client, route: &str, name: &str) -> u64 {
    client
        .debug(route)
        .unwrap_or_default()
        .get(name)
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
}

/// A workspace, a document, and `count` tasks in it — one publish, so the
/// projector walks the document once rather than once per task.
///
/// The dues are consecutive days from a fixed date, so the default sort is
/// total and a page boundary is somewhere a test can name. Nothing here reads a
/// clock: a fixture whose order depended on today would page differently every
/// morning.
struct Fixture {
    session: StoreSession,
    doc: WorkspaceDoc,
    workspace: koru_store_core::ids::WorkspaceId,
    nodes: Vec<koru_core::NodeId>,
}

impl Fixture {
    fn build(store: &Store, root: &Path, count: usize) -> Self {
        let client = store.client();
        let profile = client
            .create_profile("Tester", None, None, None)
            .expect("profile");
        let workspace = client
            .create_workspace(profile.id, "Acme")
            .expect("workspace");
        let document = client
            .create_document(workspace.id, "Tasks", "page")
            .expect("document");

        let (session, snapshot) =
            StoreSession::join(segment(), Some(root), workspace.id, document).expect("join");
        // The document carries its own workspace id, and every command reads it
        // in preflight — the same reason `tasks.rs` builds it this way.
        let mut doc = match WorkspaceDoc::from_snapshot(&snapshot) {
            Ok(doc) if doc.id().is_some() => doc,
            _ => WorkspaceDoc::create(
                koru_core::WorkspaceId::from_uuid(workspace.id.as_uuid()),
                "Acme",
            )
            .expect("create"),
        };
        let page = doc
            .insert(Parent::Root, Node::document("Tasks").unwrap())
            .unwrap();
        let mut nodes = Vec::with_capacity(count);
        for i in 0..count {
            let node = doc
                .insert(
                    Parent::Node(page),
                    Node::task(&format!("Task {:02}", i + 1)).unwrap(),
                )
                .unwrap();
            // Every fourth task is important, so the priority floor has
            // something to keep and something to drop.
            if i % 4 == 0 {
                doc.set_attribute(node, KEY_PRIORITY, AttrValue::Text("urgent".into()))
                    .expect("priority");
            }
            tasks::set_due(
                &mut doc,
                node,
                Temporal::Date(DateOnly::parse(&format!("2026-09-{:02}", i + 1)).unwrap()),
            )
            .expect("due");
            nodes.push(node);
        }
        let mut fixture = Self {
            session,
            doc,
            workspace: workspace.id,
            nodes,
        };
        fixture.publish();
        wait_for("the tasks to reach the read model", || {
            let page = store
                .client()
                .query_tasks(&TaskQuery::default())
                .expect("q");
            page.projected && page.rows.len() == count
        });
        fixture
    }

    /// Send everything written since the last publish and wait for the store to
    /// call it durable — acknowledgement, never what was sent.
    fn publish(&mut self) {
        let Some(batch) = self.session.publish(&self.doc).expect("publish") else {
            return;
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.session.is_durable(batch) {
            let _ = self.session.drain(&mut self.doc);
            assert!(Instant::now() < deadline, "the write never became durable");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// **A workspace the read model cannot answer for is named, never omitted.**
///
/// The rule ADR-274 states as *sweep first, never wait*: the answer comes back
/// with the rows it has, `projected` false, and the workspace it may be behind
/// on listed by name. The difference that matters to a Tasks view is between
/// "you have nothing to do" and "still counting", and a silently missing
/// workspace reads as the first.
#[test]
fn a_workspace_the_read_model_cannot_answer_for_is_named_rather_than_omitted() {
    let root = root_for("updating");
    let store = Store::start(&root);
    let client = store.client();
    let fixture = Fixture::build(&store, &root, 4);

    // A workspace this store has never held. It cannot converge — which is what
    // makes it a *deterministic* stand-in for one whose sweep has not landed,
    // where a real cold workspace converges while the assertion is being made.
    let unreachable = WorkspaceId::new();
    let answer = client
        .query_tasks(&TaskQuery {
            workspaces: vec![fixture.workspace, unreachable],
            ..TaskQuery::default()
        })
        .expect("query");

    assert_eq!(
        answer.updating_workspaces,
        vec![unreachable],
        "the one it cannot answer for is named"
    );
    assert!(
        !answer.projected,
        "and `projected` is that list being empty"
    );
    assert_eq!(
        answer.rows.len(),
        4,
        "while the workspace it can answer for still answers"
    );

    // The port asked for a sweep of both — that is what makes a cold workspace
    // converge because somebody looked rather than because somebody typed.
    assert!(counter(&client, "tasks", "sweeps_requested") >= 2);

    // And a scope of only the workspace that is ready is clean, so `projected`
    // is a fact about the scope rather than about the store.
    let ready = client
        .query_tasks(&TaskQuery {
            workspaces: vec![fixture.workspace],
            ..TaskQuery::default()
        })
        .expect("query");
    assert!(ready.projected && ready.updating_workspaces.is_empty());
}

/// **A page boundary holds over the wire, and a boundary that stopped meaning
/// anything says so.**
///
/// The keyset promise, end to end: a list walked a page at a time returns every
/// row once, `next_after` stops at the end rather than one page past it, and a
/// cursor naming a row the filter no longer matches restarts the list with
/// `restarted` set instead of quietly drawing page one twice.
#[test]
fn a_page_boundary_holds_over_the_wire_and_a_stale_cursor_says_so() {
    let root = root_for("paging");
    let store = Store::start(&root);
    let client = store.client();
    let mut fixture = Fixture::build(&store, &root, 7);

    let page_of = |after: Option<String>| {
        client
            .query_tasks(&TaskQuery {
                limit: 3,
                after,
                ..TaskQuery::default()
            })
            .expect("query")
    };

    let first = page_of(None);
    assert_eq!(first.rows.len(), 3);
    assert_eq!(first.limit, 3, "the page size comes back with the page");
    let cursor = first.next_after.clone().expect("a full page continues");
    assert_eq!(
        cursor,
        first.rows.last().unwrap().task,
        "the cursor is the last row's own identity"
    );

    let mut seen: Vec<String> = first.rows.iter().map(|r| r.task.clone()).collect();
    let second = page_of(Some(cursor.clone()));
    assert!(!second.restarted);
    seen.extend(second.rows.iter().map(|r| r.task.clone()));
    let third = page_of(second.next_after.clone());
    assert!(!third.restarted);
    seen.extend(third.rows.iter().map(|r| r.task.clone()));

    assert_eq!(third.rows.len(), 1, "seven rows, three pages");
    assert_eq!(
        third.next_after, None,
        "a short page is the end of the list, not a fourth request"
    );
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), 7, "every row once: {seen:?}");

    // The row the first page ended on is completed, so the open filter no
    // longer matches the cursor.
    let ended_on = fixture.nodes[2];
    tasks::complete_task(
        &mut fixture.doc,
        ended_on,
        LifecycleReason::ExplicitComplete,
        koru_core::time::DateOnly::parse("2026-09-06").unwrap(),
    )
    .expect("complete");
    fixture.publish();
    wait_for("the completion to reach the read model", || {
        let open = client.query_tasks(&TaskQuery::default()).expect("query");
        open.projected && open.rows.len() == 6
    });

    let restarted = page_of(Some(cursor));
    assert!(
        restarted.restarted,
        "a cursor the filter no longer matches restarts the list"
    );
    assert_eq!(restarted.rows.len(), 3, "from the top, not from nowhere");
    assert_eq!(
        restarted.rows[0].task, seen[0],
        "and the top is the top of the list as it is now"
    );
    assert_eq!(
        counter(&client, "tasks", "restarts"),
        1,
        "the port counts what it had to restart"
    );
}

/// **A warm query opens no document, and asking again is not more work**
/// (roadmap §6).
///
/// Two counters, and the shape of the claim is in which two. `projected` on the
/// projection's debug surface counts documents the projector *walked*: a query
/// that had to open one to build a row would move it, and no number of queries
/// may. `rows_answered` on the tasks surface grows by exactly the rows each
/// answer carried — so a query that had begun accumulating state, caching a
/// corpus or re-reading a page it already had would show as work that does not
/// belong to any one answer.
///
/// Structural, not timed: a millisecond budget here would be a promise about
/// this machine, and the promise the product needs is that a Tasks view
/// re-querying at every local midnight and on every resume costs one query
/// each time.
#[test]
fn repeated_warm_queries_open_no_document_and_accumulate_no_work() {
    let root = root_for("warm");
    let store = Store::start(&root);
    let client = store.client();
    let fixture = Fixture::build(&store, &root, 8);

    // A sweep can publish the task rows while the same commit still waits in
    // the projector's debounce queue. Measure queries only after fixture work
    // drains, as the performance fixtures do; row visibility is not idleness.
    wait_for("the fixture's projection work to drain", || {
        let projection = client.debug("projection").expect("projection state");
        let pending = projection["pending"].as_u64().expect("pending count");
        let sweeping = projection["sweeping"].as_bool().expect("sweep state");
        let failed = projection["failed"].as_u64().expect("failure count");
        assert_eq!(failed, 0, "fixture projection failed: {projection}");
        pending == 0 && !sweeping
    });

    let walked = counter(&client, "projection", "projected");
    let queries = counter(&client, "tasks", "queries");
    let answered = counter(&client, "tasks", "rows_answered");

    let asked = 10;
    for _ in 0..asked {
        let page = client
            .query_tasks(&TaskQuery {
                workspaces: vec![fixture.workspace],
                limit: 5,
                ..TaskQuery::default()
            })
            .expect("query");
        assert_eq!(page.rows.len(), 5, "a page is its limit");
        assert!(page.projected);
    }

    assert_eq!(
        counter(&client, "projection", "projected"),
        walked,
        "ten queries walked no document at all"
    );
    assert_eq!(
        counter(&client, "tasks", "queries"),
        queries + asked,
        "one query is one query"
    );
    assert_eq!(
        counter(&client, "tasks", "rows_answered"),
        answered + asked * 5,
        "and each one answered its page and nothing else"
    );

    // The Priority >= High chip, over the same warm query: the floor arrives
    // through the shared attribute builder, so this is also what says the two
    // routes cannot come to disagree about what "at least high" means.
    let important = client
        .query_tasks(&TaskQuery {
            workspaces: vec![fixture.workspace],
            min_priority: Some("high".to_owned()),
            limit: 50,
            ..TaskQuery::default()
        })
        .expect("query");
    assert_eq!(important.rows.len(), 2, "every fourth task is urgent");
    assert!(
        important
            .rows
            .iter()
            .all(|r| r.attributes.iter().any(|a| a.value == "urgent")),
        "and nothing below the floor came with them"
    );
    assert_eq!(
        counter(&client, "projection", "projected"),
        walked,
        "a filtered query opens no document either"
    );

    // Completed and All are the other two chips on that dimension, and together
    // they partition the list.
    let done = client
        .query_tasks(&TaskQuery {
            workspaces: vec![fixture.workspace],
            lifecycle: LifecycleFilter::Completed,
            limit: 50,
            ..TaskQuery::default()
        })
        .expect("query");
    let all = client
        .query_tasks(&TaskQuery {
            workspaces: vec![fixture.workspace],
            lifecycle: LifecycleFilter::All,
            limit: 50,
            ..TaskQuery::default()
        })
        .expect("query");
    assert_eq!(done.rows.len(), 0);
    assert_eq!(all.rows.len(), 8);
}
