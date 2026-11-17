// SPDX-License-Identifier: MPL-2.0

//! `GET /v1/workspaces/{id}/cloud-sync/waiting`, through the router against a
//! real store under a temporary directory, with an engine that answers a
//! fixed list rather than moving anything.
//!
//! **What only this layer decides** is the part a person reads: the title
//! the manifest has for each document, and the name — or the honest absence
//! of one — of the device each document is waiting for. The grouping itself
//! is the engine's and is pinned in `koru-blob-sync`.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use koru_device::{DeviceId, RelationshipId, ReplicationEndpointId};
use koru_store_core::ids::{DocumentId, WorkspaceId};
use koru_store_core::model::{Blob, BlobAttribute, BlobHash, Scope, Timestamp, blob_attr};
use koru_store_core::service::Store;
use koru_store_core::text::Locale;
use koru_store_sqlite::{SqliteBackend, SqliteCatalog};
use koru_sync_control::{
    AuthorizationSnapshot, AuthorizedPeer, CONTROL_PROTOCOL_VERSION, ControlHello, ControlRequest,
    ControlSessionId,
};
use tower::ServiceExt as _;

use crate::router;
use crate::state::{AppState, BlobTransferStatusService, KoruStore, WaitingFiles};

fn at() -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000)
}

fn hash(seed: char) -> BlobHash {
    BlobHash::parse(&format!("b3:{}", seed.to_string().repeat(64))).unwrap()
}

/// Answers what it was given, and counts the asks — a read must be a read.
#[derive(Default)]
struct FixedEngine {
    waiting: Mutex<Vec<WaitingFiles>>,
    asked: Mutex<u32>,
}

impl BlobTransferStatusService for FixedEngine {
    fn status(&self, _workspace: WorkspaceId) -> koru_store_wire::internal::BlobTransferReport {
        unreachable!("this route does not read the counts")
    }

    fn set_paused(
        &self,
        _workspace: WorkspaceId,
        _paused: bool,
    ) -> koru_store_wire::internal::BlobTransferReport {
        unreachable!("this route pauses nothing")
    }

    fn enabled(&self, _workspace: WorkspaceId) {
        unreachable!("this route is not a trigger")
    }

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
        unreachable!("this route releases nothing")
    }

    fn waiting(&self, _workspace: WorkspaceId) -> crate::state::WaitingFuture<'_> {
        *self.asked.lock().unwrap() += 1;
        let waiting = self.waiting.lock().unwrap().clone();
        Box::pin(async move { Ok(waiting) })
    }

    fn document_coverage(
        &self,
        _workspace: WorkspaceId,
        _document: DocumentId,
    ) -> crate::state::DocumentCoverageFuture<'_> {
        unreachable!("this route asks about the workspace, not one document")
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    state: Arc<AppState>,
    engine: Arc<FixedEngine>,
    workspace: WorkspaceId,
    token: String,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let catalog = SqliteCatalog::open(&dir.path().join("catalog.db")).unwrap();
    let mut store: KoruStore = Store::open(catalog, SqliteBackend::new(dir.path())).unwrap();
    let profile = store
        .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at())
        .unwrap();
    let workspace = store
        .create_workspace(profile.id, "Photos", at())
        .unwrap()
        .id;
    let state = Arc::new(AppState::new(
        store,
        "<root>".into(),
        "cloud-waiting-route-test".into(),
        at().millis(),
    ));
    let engine = Arc::new(FixedEngine::default());
    assert!(state.set_blob_transfer_status_service(engine.clone()));
    let (token, _) = state.mint_session("test", Scope::ALL.to_vec());
    Fixture {
        _dir: dir,
        state,
        engine,
        workspace,
        token,
    }
}

impl Fixture {
    fn document(&self, title: &str) -> DocumentId {
        self.state
            .store
            .lock()
            .unwrap()
            .create_document(self.workspace, title, "page", None, None, at())
            .unwrap()
            .0
    }

    /// A peer's inventory said `device` first stored these bytes — the
    /// provenance a device that never held them keeps.
    fn originated(&self, hash: &BlobHash, device: DeviceId) {
        let blob = Blob {
            hash: hash.clone(),
            size: 10,
            mime: "image/png".into(),
            filename: None,
            created_at: at(),
            attributes: [(
                blob_attr::ORIGIN_DEVICE.to_owned(),
                BlobAttribute::Text(device.to_storage()),
            )]
            .into(),
        };
        self.state
            .store
            .lock()
            .unwrap()
            .record_blob_origins(self.workspace, &[blob], at())
            .unwrap();
    }

    /// The Agent's snapshot: this device, and the peers it knows by name.
    fn authorize(&self, local: DeviceId, peers: &[(DeviceId, Option<&str>)]) {
        let hello = ControlHello {
            protocol_version: CONTROL_PROTOCOL_VERSION,
            session_id: ControlSessionId::new(),
            local_device: local,
        };
        let mut control = self.state.sync_control.lock().unwrap();
        control.begin_session(&hello).unwrap();
        control.handle(
            hello.session_id,
            ControlRequest::ReplaceAuthorization {
                snapshot: AuthorizationSnapshot {
                    session_id: hello.session_id,
                    sequence: 1,
                    local_device: local,
                    peers: peers
                        .iter()
                        .enumerate()
                        .map(|(index, (device, name))| AuthorizedPeer {
                            relationship_id: RelationshipId::new(),
                            device_id: *device,
                            endpoint_id: ReplicationEndpointId::from_bytes([index as u8 + 1; 32]),
                            all_data: true,
                            authorized_profiles: Vec::new(),
                            workspaces: Vec::new(),
                            via: None,
                            display_name: name.map(str::to_owned),
                            build: None,
                            own_device: false,
                        })
                        .collect(),
                    local_build: None,
                },
            },
        );
    }

    async fn ask(&self) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .method("GET")
            .uri(format!(
                "/v1/workspaces/{}/cloud-sync/waiting",
                self.workspace.to_storage()
            ))
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .body(Body::empty())
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
}

/// **Titles from the manifest, counts from the engine, names from the
/// snapshot** — and a document whose files nobody is recorded as holding is
/// answered with no name rather than a guessed one.
#[tokio::test]
async fn each_waiting_document_is_named_counted_and_told_who_it_waits_for() {
    let fixture = fixture();
    let trip = fixture.document("Trip notes");
    let recipes = fixture.document("Recipes");
    let (local, pixel, laptop, stranger) = (
        DeviceId::new(),
        DeviceId::new(),
        DeviceId::new(),
        DeviceId::new(),
    );
    fixture.authorize(
        local,
        &[(pixel, Some("Pixel 8")), (laptop, Some("Work laptop"))],
    );
    let pictures: Vec<BlobHash> = ['a', 'b', 'c', 'd', 'e', 'f']
        .into_iter()
        .map(hash)
        .collect();
    // Two of the six were first stored on the phone, one on the laptop; the
    // rest have no recorded origin at all.
    fixture.originated(&pictures[0], pixel);
    fixture.originated(&pictures[1], pixel);
    fixture.originated(&pictures[2], laptop);
    // One from a device this one has no name for, and one from this device
    // itself: neither is a device anybody can be told to wait for.
    let unknown = hash('1');
    let mine = hash('2');
    fixture.originated(&unknown, stranger);
    fixture.originated(&mine, local);
    *fixture.engine.waiting.lock().unwrap() = vec![
        WaitingFiles {
            document: recipes,
            blobs: vec![unknown, mine],
        },
        WaitingFiles {
            document: trip,
            blobs: pictures,
        },
    ];

    let (status, body) = fixture.ask().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["workspace"], fixture.workspace.to_storage());
    let documents = body["documents"].as_array().expect("a list");
    assert_eq!(documents.len(), 2, "{body}");
    // Most files first: six pictures in one document is one entry saying six.
    assert_eq!(documents[0]["document"], trip.to_storage());
    assert_eq!(documents[0]["title"], "Trip notes");
    assert_eq!(documents[0]["missing_files"], 6);
    assert_eq!(
        documents[0]["holders"],
        serde_json::json!(["Pixel 8", "Work laptop"])
    );
    assert_eq!(documents[1]["title"], "Recipes");
    assert_eq!(documents[1]["missing_files"], 2);
    assert_eq!(documents[1]["holders"], serde_json::json!([]));
    // No address a person would read as a file's name.
    assert!(!body.to_string().contains("b3:"), "{body}");
    assert_eq!(*fixture.engine.asked.lock().unwrap(), 1);
}

/// Before the Agent has installed a snapshot there are no names to give, and
/// nothing is invented to fill the gap.
#[tokio::test]
async fn without_a_snapshot_nobody_is_named() {
    let fixture = fixture();
    let page = fixture.document("Page");
    let picture = hash('a');
    fixture.originated(&picture, DeviceId::new());
    *fixture.engine.waiting.lock().unwrap() = vec![WaitingFiles {
        document: page,
        blobs: vec![picture],
    }];
    let (status, body) = fixture.ask().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["documents"][0]["holders"], serde_json::json!([]));
    assert_eq!(body["documents"][0]["missing_files"], 1);
}

/// Nothing waiting is an empty list, not an error: a workspace held for
/// another reason — a metered connection, a pause — is waiting on nobody.
#[tokio::test]
async fn a_workspace_waiting_on_nobody_answers_an_empty_list() {
    let fixture = fixture();
    let (status, body) = fixture.ask().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["documents"], serde_json::json!([]));
}

/// The peer runtime's holder directory, as a fixed answer: which devices
/// advertised which blobs, and what it was asked.
#[derive(Default)]
struct FixedDirectory {
    advertised: std::collections::HashMap<BlobHash, Vec<DeviceId>>,
    asked: Mutex<Vec<BlobHash>>,
}

impl crate::state::BlobHolderDirectory for FixedDirectory {
    fn advertised_holders(
        &self,
        _workspace: WorkspaceId,
        hashes: &[BlobHash],
    ) -> std::collections::HashMap<BlobHash, Vec<String>> {
        self.asked.lock().unwrap().extend(hashes.iter().cloned());
        hashes
            .iter()
            .filter_map(|hash| {
                let devices = self.advertised.get(hash)?;
                Some((
                    hash.clone(),
                    devices.iter().map(|device| device.to_storage()).collect(),
                ))
            })
            .collect()
    }
}

/// **A device that holds a copy is named, not only the one that made it.**
/// One blob known only by its origin, one only by a peer's advertisement, and
/// one known to both — the last names each device once. This device's own
/// advertisement names nobody.
#[tokio::test]
async fn a_device_that_advertised_a_copy_is_named_beside_the_origin() {
    let fixture = fixture();
    let origin_only = fixture.document("Origin only");
    let peer_only = fixture.document("Peer only");
    let both = fixture.document("Both");
    let (local, thinkpad, desktop) = (DeviceId::new(), DeviceId::new(), DeviceId::new());
    fixture.authorize(
        local,
        &[(thinkpad, Some("ThinkpadP11")), (desktop, Some("Desktop"))],
    );
    let (a, b, c) = (hash('a'), hash('b'), hash('c'));
    fixture.originated(&a, thinkpad);
    fixture.originated(&c, thinkpad);
    let directory = Arc::new(FixedDirectory {
        advertised: [
            (b.clone(), vec![desktop, local]),
            (c.clone(), vec![thinkpad, desktop]),
        ]
        .into(),
        ..FixedDirectory::default()
    });
    assert!(fixture.state.set_blob_holder_directory(directory.clone()));
    *fixture.engine.waiting.lock().unwrap() = vec![
        WaitingFiles {
            document: origin_only,
            blobs: vec![a.clone()],
        },
        WaitingFiles {
            document: peer_only,
            blobs: vec![b.clone()],
        },
        WaitingFiles {
            document: both,
            blobs: vec![c.clone()],
        },
    ];

    let (status, body) = fixture.ask().await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let holders = |title: &str| {
        body["documents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|document| document["title"] == title)
            .unwrap_or_else(|| panic!("{title} in {body}"))["holders"]
            .clone()
    };
    assert_eq!(holders("Origin only"), serde_json::json!(["ThinkpadP11"]));
    assert_eq!(holders("Peer only"), serde_json::json!(["Desktop"]));
    assert_eq!(
        holders("Both"),
        serde_json::json!(["Desktop", "ThinkpadP11"])
    );
    assert_eq!(*directory.asked.lock().unwrap(), vec![a, b, c]);
    assert!(!body.to_string().contains("b3:"), "{body}");
}
