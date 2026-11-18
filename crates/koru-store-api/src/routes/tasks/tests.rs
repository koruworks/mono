// SPDX-License-Identifier: MPL-2.0

//! What the route promises before any port exists behind it: the boundary it
//! checks, the ceiling it applies, and the honest answer it gives when nothing
//! is installed.
//!
//! The handler is driven directly rather than through the router: B-Gate owns
//! the catalog row that mounts it, and a test that needed the mount would be
//! asserting on somebody else's wiring instead of on this file.

use std::sync::{Arc, Mutex};

use koru_store_core::ids::WorkspaceId;
use koru_store_core::model::Scope;
use koru_store_core::service::Store;
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};
use koru_store_wire::tasks::{DEFAULT_LIMIT, MAX_LIMIT, TaskRow};

use super::*;
use crate::state::KoruStore;

fn state(dir: &std::path::Path) -> Arc<AppState> {
    let catalog = SqliteCatalog::open(&dir.join("catalog.db")).unwrap();
    let store: KoruStore = Store::open(catalog, SqliteBackend::new(dir)).unwrap();
    Arc::new(AppState::new(
        store,
        "<root>".into(),
        "internal-debug".into(),
        1_770_000_000_000,
    ))
}

fn caller(scopes: Vec<Scope>) -> Caller {
    Caller {
        scopes,
        label: "a-test".into(),
    }
}

/// A port that records what it was handed and answers whatever it was told to.
struct Recording {
    seen: Mutex<Vec<TaskQuery>>,
    answer: Mutex<Option<TaskQueryResponse>>,
}

impl Recording {
    fn answering(response: TaskQueryResponse) -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            answer: Mutex::new(Some(response)),
        })
    }
}

impl TaskService for Recording {
    fn query(&self, query: &TaskQuery) -> Result<TaskQueryResponse, TaskServiceError> {
        self.seen.lock().unwrap().push(query.clone());
        match self.answer.lock().unwrap().clone() {
            Some(response) => Ok(response),
            None => Err(TaskServiceError::Failed("no answer was armed".into())),
        }
    }
}

struct Refuses(TaskServiceError);

impl TaskService for Refuses {
    fn query(&self, _query: &TaskQuery) -> Result<TaskQueryResponse, TaskServiceError> {
        Err(match &self.0 {
            TaskServiceError::Invalid(why) => TaskServiceError::Invalid(why.clone()),
            TaskServiceError::Failed(why) => TaskServiceError::Failed(why.clone()),
        })
    }
}

fn a_response() -> TaskQueryResponse {
    TaskQueryResponse {
        rows: vec![TaskRow {
            workspace: WorkspaceId::new(),
            document: koru_store_core::ids::DocumentId::new(),
            node: "n1".into(),
            task: "task:w:d:n1".into(),
            node_kind: "paragraph".into(),
            lifecycle: "open".into(),
            document_trashed: false,
            in_trash: false,
            breadcrumb: "Acme / Plans".into(),
            title: "Ship it".into(),
            attributes: Vec::new(),
            bucket: None,
        }],
        definitions: Vec::new(),
        tags: Vec::new(),
        groups: Vec::new(),
        projected: false,
        updating_workspaces: vec![WorkspaceId::new()],
        limit: 100,
        next_after: Some("task:w:d:n1".into()),
        restarted: true,
        covered: None,
    }
}

/// **A store with no projector still serves documents.** The honest answer is
/// no rows, `projected: false` and the requested workspaces named as updating —
/// not a failure a Tasks view has to draw a banner for.
#[tokio::test]
async fn a_store_with_no_task_service_answers_empty_rather_than_failing() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let workspace = WorkspaceId::new();

    let Json(response) = query(
        State(state),
        Extension(caller(vec![Scope::ReadDocuments])),
        Json(TaskQuery {
            workspaces: vec![workspace],
            ..TaskQuery::default()
        }),
    )
    .await
    .expect("a missing service is not an error");

    assert!(response.rows.is_empty());
    assert!(!response.projected);
    assert_eq!(response.updating_workspaces, vec![workspace]);
    assert_eq!(response.limit, 0);
    assert!(!response.restarted);
}

/// The boundary is `read_documents`, the same one the attribute query and
/// search ask for.
#[tokio::test]
async fn a_credential_that_cannot_read_documents_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let refused = query(
        State(state),
        Extension(caller(vec![Scope::EditDocuments])),
        Json(TaskQuery::default()),
    )
    .await;
    assert!(
        refused.is_err(),
        "a write-only credential cannot read tasks"
    );
}

/// **`Scope::ReadTasks` is not consulted**, and this is what says so: a caller
/// holding `read_documents` and *not* `read_tasks` is served. The value stays
/// shipped and unenforced; a future that enforces it has to come past this
/// test, which is the point of writing it down as an assertion.
#[tokio::test]
async fn read_tasks_is_shipped_and_not_required() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let service = Recording::answering(a_response());
    assert!(state.set_task_service(service.clone()));

    let answered = query(
        State(state),
        Extension(caller(vec![Scope::ReadDocuments])),
        Json(TaskQuery::default()),
    )
    .await;
    assert!(answered.is_ok(), "reading a task is reading a node");
}

/// A hopeful page size is clamped rather than refused, and a zero is lifted to
/// one — the port is never handed a limit that would answer nothing.
#[tokio::test]
async fn a_page_size_is_clamped_at_both_ends() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let service = Recording::answering(a_response());
    assert!(state.set_task_service(service.clone()));

    for (asked, applied) in [(0, 1), (10, 10), (MAX_LIMIT + 5_000, MAX_LIMIT)] {
        let Json(_) = query(
            State(state.clone()),
            Extension(caller(vec![Scope::ReadDocuments])),
            Json(TaskQuery {
                limit: asked,
                ..TaskQuery::default()
            }),
        )
        .await
        .unwrap();
        assert_eq!(service.seen.lock().unwrap().last().unwrap().limit, applied);
    }
    // And the default a body with no limit at all carries through unchanged.
    let body: TaskQuery = serde_json::from_str("{}").unwrap();
    let Json(_) = query(
        State(state),
        Extension(caller(vec![Scope::ReadDocuments])),
        Json(body),
    )
    .await
    .unwrap();
    assert_eq!(
        service.seen.lock().unwrap().last().unwrap().limit,
        DEFAULT_LIMIT
    );
}

/// The three facts a paginating, still-indexing surface needs come back
/// untouched: what it may be behind on, where the next page starts, and whether
/// this page *is* the next page.
#[tokio::test]
async fn freshness_and_the_cursor_reach_the_caller() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path());
    let expected = a_response();
    let service = Recording::answering(expected.clone());
    assert!(state.set_task_service(service));

    let Json(response) = query(
        State(state),
        Extension(caller(vec![Scope::ReadDocuments])),
        Json(TaskQuery::default()),
    )
    .await
    .unwrap();

    assert_eq!(response.rows.len(), 1);
    assert_eq!(response.rows[0].breadcrumb, "Acme / Plans");
    assert_eq!(
        response.updating_workspaces, expected.updating_workspaces,
        "named rather than waited for"
    );
    assert_eq!(response.next_after, expected.next_after);
    assert!(response.restarted);
    assert!(!response.projected);
}

/// **A caller's mistake and a broken read model are different answers.**
/// Collapsing them made every typo look like storage trouble on the attribute
/// route; the split is inherited here rather than rediscovered.
#[tokio::test]
async fn a_malformed_query_and_a_broken_read_model_are_told_apart() {
    let dir = tempfile::tempdir().unwrap();
    let invalid = state(dir.path());
    assert!(
        invalid.set_task_service(Arc::new(Refuses(TaskServiceError::Invalid(
            "not a priority token".into()
        ))))
    );
    let error = query(
        State(invalid),
        Extension(caller(vec![Scope::ReadDocuments])),
        Json(TaskQuery::default()),
    )
    .await
    .expect_err("an invalid query is refused");
    assert_eq!(error.0.code(), Code::InvalidRequest);

    let other = tempfile::tempdir().unwrap();
    let failed = state(other.path());
    assert!(
        failed.set_task_service(Arc::new(Refuses(TaskServiceError::Failed(
            "the projection lock is poisoned".into()
        ))))
    );
    let error = query(
        State(failed),
        Extension(caller(vec![Scope::ReadDocuments])),
        Json(TaskQuery::default()),
    )
    .await
    .expect_err("a broken read model is a failure");
    assert_eq!(error.0.code(), Code::StorageFailed);
}
