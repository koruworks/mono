// SPDX-License-Identifier: MPL-2.0

//! `PUT /v1/profiles/{id}/transport-mode` and
//! `POST /v1/workspaces/{id}/blobs/pause`, through the router against a real
//! store under a temporary directory.
//!
//! **The seam this file exists for is the last one in the chain.** The group
//! commit is pinned in `koru-store-sqlite` and the pre-flight in
//! `koru-blob-sync`; what is only decidable here is that answering the prompt
//! actually tells the engine *"Cloud is on for this Workspace now"* — §10's
//! first trigger, which until D258 had no caller in a running Koru at all, and
//! without which *tell, then start* never happens.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use koru_store_core::ids::{DocumentId, ProfileId, WorkspaceId};
use koru_store_core::model::{Scope, Timestamp};
use koru_store_core::service::Store;
use koru_store_core::text::Locale;
use koru_store_core::transport::{SetTransportMode, TransportMode, TransportSubject};
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};
use tower::ServiceExt as _;

use crate::router;
use crate::state::{AppState, BlobTransferStatusService, KoruStore};

fn at() -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000)
}

/// An engine that records what it was told rather than moving anything.
///
/// *"Which Workspaces were enabled"* is a claim about a list, so the list is
/// what the test reads — not an assertion about intent.
#[derive(Default)]
struct RecordingEngine {
    enabled: Mutex<Vec<WorkspaceId>>,
    paused: Mutex<Vec<(WorkspaceId, bool)>>,
}

impl RecordingEngine {
    fn enabled(&self) -> Vec<WorkspaceId> {
        self.enabled.lock().unwrap().clone()
    }
}

impl BlobTransferStatusService for RecordingEngine {
    fn status(&self, _workspace: WorkspaceId) -> koru_store_wire::internal::BlobTransferReport {
        report()
    }

    fn set_paused(
        &self,
        workspace: WorkspaceId,
        paused: bool,
    ) -> koru_store_wire::internal::BlobTransferReport {
        self.paused.lock().unwrap().push((workspace, paused));
        koru_store_wire::internal::BlobTransferReport { paused, ..report() }
    }

    fn enabled(&self, workspace: WorkspaceId) {
        self.enabled.lock().unwrap().push(workspace);
    }

    /// This fixture is about the transport-mode group and holds no blobs, so
    /// it says so rather than inventing a file.
    fn file(
        &self,
        _workspace: WorkspaceId,
        _file: DocumentId,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Option<koru_store_wire::internal::BlobFileReport>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async { None })
    }
    fn release_local(
        &self,
        _workspace: WorkspaceId,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<koru_store_wire::internal::BlobReleaseReport, &'static str>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async { Err("blob_transfer_unavailable") })
    }

    fn waiting(&self, _workspace: WorkspaceId) -> crate::state::WaitingFuture<'_> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn document_coverage(
        &self,
        _workspace: WorkspaceId,
        _document: DocumentId,
    ) -> crate::state::DocumentCoverageFuture<'_> {
        Box::pin(async { Ok(Default::default()) })
    }
}

fn report() -> koru_store_wire::internal::BlobTransferReport {
    koru_store_wire::internal::BlobTransferReport {
        referenced: 0,
        present: 0,
        durable: 0,
        uploading: 0,
        downloading: 0,
        missing: 0,
        bytes_complete: 0,
        bytes_total: 0,
        pending_reason: None,
        last_failure_code: None,
        direction: None,
        path: None,
        object_host: None,
        oldest_retry_at_millis: None,
        unreadable_references: 0,
        walks: 0,
        passes: 0,
        passes_skipped: 0,
        coverage: None,
        paused: false,
        preflight: None,
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    engine: Arc<RecordingEngine>,
    profile: ProfileId,
    workspaces: [WorkspaceId; 3],
    token: String,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let catalog = SqliteCatalog::open(&dir.path().join("catalog.db")).unwrap();
    let mut store: KoruStore = Store::open(catalog, SqliteBackend::new(dir.path())).unwrap();
    let profile = store
        .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at())
        .unwrap();
    let workspaces = [
        store.create_workspace(profile.id, "one", at()).unwrap().id,
        store.create_workspace(profile.id, "two", at()).unwrap().id,
        store
            .create_workspace(profile.id, "three", at())
            .unwrap()
            .id,
    ];
    let state = Arc::new(AppState::new(
        store,
        "<root>".into(),
        "transport-mode-route-test".into(),
        at().millis(),
    ));
    let engine = Arc::new(RecordingEngine::default());
    assert!(state.set_blob_transfer_status_service(engine.clone()));
    let (token, _) = state.mint_session("test", Scope::ALL.to_vec());
    Fixture {
        _dir: dir,
        state,
        engine,
        profile: profile.id,
        workspaces,
        token,
    }
}

impl Fixture {
    async fn choose(
        &self,
        mode: &str,
        include_workspaces: bool,
    ) -> (StatusCode, serde_json::Value) {
        let body = serde_json::json!({
            "mode": mode,
            "include_workspaces": include_workspaces,
        });
        let request = Request::builder()
            .method("PUT")
            .uri(format!(
                "/v1/profiles/{}/transport-mode",
                self.profile.to_storage()
            ))
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let response = router(self.state.clone())
            .oneshot(request)
            .await
            .expect("the router answers");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    async fn pause(&self, workspace: WorkspaceId, paused: bool) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("POST")
            .uri(format!(
                "/v1/workspaces/{}/blobs/pause",
                workspace.to_storage()
            ))
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({ "paused": paused })).unwrap(),
            ))
            .unwrap();
        let response = router(self.state.clone())
            .oneshot(request)
            .await
            .expect("the router answers");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    fn mode(&self, subject: TransportSubject) -> TransportMode {
        self.state
            .store
            .lock()
            .unwrap()
            .transport_mode(subject)
            .unwrap()
            .mode
    }
}

/// **Tell, then start** (D256) begins here: answering *move everything* moves
/// the group **and** raises §10's first trigger for every Workspace that may
/// now move bytes. Without the second half the pre-flight summary has nowhere
/// to be published from and nothing begins until something unrelated happens.
#[tokio::test]
async fn answering_yes_moves_the_group_and_enables_every_workspace() {
    let fixture = fixture();
    let (status, body) = fixture.choose("cloud", true).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mode"], "cloud");
    assert_eq!(body["profile_changed"], true);
    assert_eq!(body["explicit_choice"], true);
    assert_eq!(body["workspaces_moved"], 3);
    assert_eq!(body["workspaces_left_alone"], 0);

    // The durable rows, read through the Store rather than from the answer.
    assert_eq!(
        fixture.mode(TransportSubject::Profile(fixture.profile)),
        TransportMode::Cloud
    );
    for workspace in fixture.workspaces {
        assert_eq!(
            fixture.mode(TransportSubject::Workspace(workspace)),
            TransportMode::Cloud
        );
    }

    let mut enabled = fixture.engine.enabled();
    enabled.sort();
    let mut expected = fixture.workspaces.to_vec();
    expected.sort();
    assert_eq!(enabled, expected, "every Workspace was told Cloud is on");
}

/// **The other answer.** The Profile moves alone, its Workspaces stay capped
/// beneath it, and **nothing is enabled** — an enable pass over a Workspace
/// that still refuses transport would walk it to find out it cannot move a
/// byte.
#[tokio::test]
async fn answering_no_moves_the_profile_alone_and_enables_nothing() {
    let fixture = fixture();
    let (status, body) = fixture.choose("cloud", false).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["workspaces_submitted"], 0);
    assert_eq!(body["workspaces_moved"], 0);
    assert_eq!(
        fixture.mode(TransportSubject::Profile(fixture.profile)),
        TransportMode::Cloud
    );
    for workspace in fixture.workspaces {
        assert_eq!(
            fixture.mode(TransportSubject::Workspace(workspace)),
            TransportMode::LocalOnly
        );
    }
    assert!(fixture.engine.enabled().is_empty());
}

/// **An explicit prior choice survives the prompt**, and is reported as left
/// alone rather than as a failure — and it is not enabled either, because it
/// is still Local-only and a pass over it would move nothing.
#[tokio::test]
async fn an_explicit_local_only_workspace_is_left_alone_and_not_enabled() {
    let fixture = fixture();
    let refused = fixture.workspaces[1];
    {
        let mut store = fixture.state.store.lock().unwrap();
        let row = store
            .transport_mode(TransportSubject::Workspace(refused))
            .unwrap();
        store
            .set_transport_mode(SetTransportMode {
                subject: TransportSubject::Workspace(refused),
                expected_revision: row.revision,
                mode: TransportMode::LocalOnly,
            })
            .unwrap();
    }

    let (status, body) = fixture.choose("cloud", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["workspaces_moved"], 2);
    assert_eq!(body["workspaces_left_alone"], 1);
    assert_eq!(
        fixture.mode(TransportSubject::Workspace(refused)),
        TransportMode::LocalOnly
    );

    let enabled = fixture.engine.enabled();
    assert_eq!(enabled.len(), 2);
    assert!(
        !enabled.contains(&refused),
        "a Workspace somebody kept off was told Cloud is on",
    );
}

/// An argument that is not understood is not run: a mode token this build
/// cannot decode is refused rather than read as Local-only, and the rows are
/// untouched.
#[tokio::test]
async fn an_unknown_mode_token_is_refused_and_changes_nothing() {
    let fixture = fixture();
    let (status, _) = fixture.choose("everything", true).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        fixture.mode(TransportSubject::Profile(fixture.profile)),
        TransportMode::LocalOnly
    );
    assert!(fixture.engine.enabled().is_empty());
}

/// **Turning it on, then off, leaves the durable state the person last
/// chose.** The Cloud sync switch's *off* is D227's immediate stop and writes
/// no transport mode; this route is the only thing that does, and answering
/// it twice with the same value writes nothing at all.
#[tokio::test]
async fn answering_twice_writes_nothing_the_second_time() {
    let fixture = fixture();
    fixture.choose("cloud", true).await;
    let revision = fixture
        .state
        .store
        .lock()
        .unwrap()
        .transport_mode(TransportSubject::Profile(fixture.profile))
        .unwrap()
        .revision;
    let children_before = fixture
        .state
        .store
        .lock()
        .unwrap()
        .transport_mode_group(fixture.profile)
        .unwrap()
        .children;

    let (status, body) = fixture.choose("cloud", true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["profile_changed"], false);
    assert_eq!(body["mode"], "cloud");
    assert_eq!(
        body["workspaces_left_alone"], 0,
        "inherited Workspaces remain defaults rather than explicit choices",
    );
    assert_eq!(body["workspaces_moved"], 0);
    let children_after = fixture
        .state
        .store
        .lock()
        .unwrap()
        .transport_mode_group(fixture.profile)
        .unwrap()
        .children;
    assert_eq!(
        children_before, children_after,
        "a retry writes no child row"
    );
    assert!(children_after.iter().all(|row| !row.explicit_choice));
    assert_eq!(
        fixture
            .state
            .store
            .lock()
            .unwrap()
            .transport_mode(TransportSubject::Profile(fixture.profile))
            .unwrap()
            .revision,
        revision,
        "a retry costs bytes, never correctness",
    );
}

/// **The pause reaches the engine and answers what it left behind**, so the
/// surface that pressed it does not have to ask again to draw the result.
#[tokio::test]
async fn pausing_reaches_the_engine_and_answers_the_report() {
    let fixture = fixture();
    let workspace = fixture.workspaces[0];

    let (status, body) = fixture.pause(workspace, true).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["paused"], true);

    let (status, body) = fixture.pause(workspace, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["paused"], false);

    assert_eq!(
        *fixture.engine.paused.lock().unwrap(),
        vec![(workspace, true), (workspace, false)],
    );
}
