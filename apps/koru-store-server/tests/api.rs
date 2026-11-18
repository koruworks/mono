// SPDX-License-Identifier: MPL-2.0

//! **The API, against a real daemon.**
//!
//! `api_catalog::ROUTES` claims a set of endpoints and the OpenAPI document
//! describes them. These tests check the third thing neither can: that they
//! actually answer. A route in the table with no handler behind it would pass
//! every unit test in `koru-store-api` and 404 for every integrator.

mod common;

use std::net::Shutdown;
use std::process::{Child, Command};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use koru_store_api::api_catalog::{Audience, ROUTES};
use koru_store_client::{CatalogWatch, Client as StoreClient, Discovery};

struct Daemon {
    child: Child,
    port: u16,
    token: String,
}

impl Daemon {
    fn start() -> (tempfile::TempDir, Self) {
        let dir = tempfile::tempdir().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_koru-store-server"));
        command
            .arg("serve")
            // A suite starting daemons on throwaway roots must never raise a
            // modal dialog: nothing dismisses it, so the process would never
            // exit and would go on holding its databases and this binary.
            .env("KORU_STORE_NO_DIALOG", "1")
            .arg("--console")
            .arg("--root")
            .arg(dir.path());
        common::configure(&mut command, dir.path());
        let child = command.spawn().expect("spawn koru-store-server");

        // The handshake is what hands out both the port and the credential
        // now. There is no `store.json` to poll and no `store.secret` to read —
        // and the loop below is bounded only because a store that never binds
        // has to fail the test rather than hang it.
        let discovery = common::wait_for_discovery(dir.path());
        let daemon = Self {
            child,
            port: discovery.port,
            token: discovery.token,
        };
        (dir, daemon)
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn get(&self, path: &str) -> reqwest::blocking::Response {
        client()
            .get(self.url(path))
            .bearer_auth(&self.token)
            .send()
            .expect("send")
    }

    fn post<B: serde::Serialize>(&self, path: &str, body: &B) -> reqwest::blocking::Response {
        client()
            .post(self.url(path))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .expect("send")
    }

    fn delete(&self, path: &str) -> reqwest::blocking::Response {
        client()
            .delete(self.url(path))
            .bearer_auth(&self.token)
            .send()
            .expect("send")
    }

    fn json(&self, path: &str) -> serde_json::Value {
        let response = self.get(path);
        assert!(
            response.status().is_success(),
            "{path}: {}",
            response.status()
        );
        response.json().expect("json")
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap()
}

fn blocked_generation(
    mut watch: CatalogWatch,
) -> (
    mpsc::Receiver<()>,
    thread::JoinHandle<(CatalogWatch, koru_store_core::Result<u64>)>,
) {
    let (finished, completion) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let result = watch.next_generation();
        finished.send(()).unwrap();
        (watch, result)
    });
    (completion, reader)
}

/// Substitute real ids into a templated path, so a table row can be called.
///
/// `{id}` is not one thing: it is a profile under `/v1/profiles` and a
/// workspace everywhere else. Substituting the same value into both looks like
/// a missing route when it is only a missing *row* — which is the difference
/// this whole test is trying to detect.
fn concrete(path: &str, profile: &str, workspace: &str, document: &str) -> String {
    let id = if path.starts_with("/v1/profiles") {
        profile
    } else {
        workspace
    };
    path.replace("{id}", id).replace("{document}", document)
}

// ------------------------------------------------------------------ the table

#[test]
fn catalog_watch_blocks_and_wakes_for_workspace_topology() {
    let (_dir, daemon) = Daemon::start();
    let discovery = Discovery {
        port: daemon.port,
        token: daemon.token.clone(),
        expires_at_millis: i64::MAX,
    };
    let refused = Discovery {
        token: "not-a-session-this-store-minted".into(),
        ..discovery.clone()
    };
    assert!(
        CatalogWatch::open(&refused).is_err(),
        "an unrecognised credential opened the internal watch"
    );

    let store = StoreClient::new(discovery.clone());
    let profile = store
        .create_profile("Owner", None, None, Some("en"))
        .unwrap();
    let mut watch = CatalogWatch::open(&discovery).unwrap();
    assert_eq!(
        watch.next_generation().unwrap(),
        0,
        "the immediate generation closes the subscribe race"
    );

    let (completion, reader) = blocked_generation(watch);
    assert_eq!(
        completion.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "healthy idle produced work"
    );
    let first = store.create_workspace(profile.id, "First").unwrap();
    completion
        .recv_timeout(Duration::from_secs(2))
        .expect("the first workspace wakes the catalog watch");
    let (watch, generation) = reader.join().unwrap();
    assert_eq!(generation.unwrap(), 1);

    let (completion, reader) = blocked_generation(watch);
    let _second = store.create_workspace(profile.id, "Second").unwrap();
    completion
        .recv_timeout(Duration::from_secs(2))
        .expect("a new workspace wakes the catalog watch");
    let (watch, generation) = reader.join().unwrap();
    assert_eq!(generation.unwrap(), 2);

    let (completion, reader) = blocked_generation(watch);
    store.delete_workspace(first.id).unwrap();
    completion
        .recv_timeout(Duration::from_secs(2))
        .expect("deleting a workspace wakes the catalog watch");
    let (watch, generation) = reader.join().unwrap();
    assert_eq!(generation.unwrap(), 3);

    let (completion, reader) = blocked_generation(watch);
    store.restore_workspace(first.id).unwrap();
    completion
        .recv_timeout(Duration::from_secs(2))
        .expect("restoring a workspace wakes the catalog watch");
    let (watch, generation) = reader.join().unwrap();
    assert_eq!(generation.unwrap(), 4);

    let exported = store.export_workspace(first.id).unwrap();
    let bundle = koru_store_wire::bundle::decode(&exported).unwrap();
    let (completion, reader) = blocked_generation(watch);
    store
        .import_workspace(&bundle, koru_store_core::ImportMode::Duplicate, profile.id)
        .unwrap();
    completion
        .recv_timeout(Duration::from_secs(2))
        .expect("importing a workspace wakes the catalog watch");
    let (watch, generation) = reader.join().unwrap();
    assert_eq!(generation.unwrap(), 5);

    let abort = watch.abort_handle().unwrap();
    let (completion, reader) = blocked_generation(watch);
    abort.shutdown(Shutdown::Both).unwrap();
    completion
        .recv_timeout(Duration::from_secs(2))
        .expect("closing the watch unblocks its reader");
    let (_watch, result) = reader.join().unwrap();
    assert!(result.is_err(), "an aborted watch reported a generation");
}

#[test]
fn a_live_store_writes_an_authenticated_whole_store_backup() {
    let (_store_dir, daemon) = Daemon::start();
    let backups = tempfile::tempdir().unwrap();
    let destination = backups.path().join("live.koru-store");
    let client = StoreClient::new(Discovery {
        port: daemon.port,
        token: daemon.token.clone(),
        expires_at_millis: i64::MAX,
    });
    assert_eq!(client.backup_store(&destination).unwrap(), destination);
    assert!(destination.is_file(), "the Store reported no backup file");
}

/// **The claim the one-shot 401 retry rests on.**
///
/// `koru-store-client` re-rendezvouses on an unexpected 401 and sends the
/// request a second time, including for routes that mutate. That is only safe
/// because `auth::require_token` is a layer *wrapping* the router, so a
/// rejected request never reaches a handler and cannot have half-applied
/// anything worth not repeating. Proved on a delete rather than argued: a
/// rename would leave a name to inspect, but a delete that ran anyway would be
/// the expensive way to find this out.
#[test]
fn a_rejected_mutation_does_not_reach_its_handler() {
    let (_dir, daemon) = Daemon::start();
    let profile = daemon
        .post(
            "/v1/profiles",
            &serde_json::json!({ "name": "Owner", "locale": "en" }),
        )
        .json::<serde_json::Value>()
        .unwrap();
    let workspace = daemon
        .post(
            "/v1/workspaces",
            &serde_json::json!({ "owner": profile["id"].as_str().unwrap(), "name": "W" }),
        )
        .json::<serde_json::Value>()
        .unwrap();
    let id = workspace["id"].as_str().unwrap().to_string();

    let refused = client()
        .delete(daemon.url(&format!("/v1/workspaces/{id}?force=true")))
        .bearer_auth("a-credential-this-store-never-minted")
        .send()
        .expect("send");
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Still there, and still listed: the retry the client would now send is a
    // first attempt, not a second one.
    let listed = daemon.json("/v1/workspaces");
    assert!(
        listed
            .as_array()
            .expect("a list of workspaces")
            .iter()
            .any(|w| w["id"].as_str() == Some(id.as_str())),
        "an unauthenticated delete removed the workspace anyway: {listed}"
    );
}

/// **A marker can be renamed, and a listing says whether the document moved
/// on** — over HTTP, through the typed client, against a real daemon.
///
/// Both halves of one request cycle, because that is how the Ctrl+S modal uses
/// them: it lists to decide which modal to open, then patches the marker the
/// listing named.
#[test]
fn a_history_marker_renames_and_a_listing_reports_staleness() {
    let (_dir, daemon) = Daemon::start();
    let client = StoreClient::new(Discovery {
        port: daemon.port,
        token: daemon.token.clone(),
        expires_at_millis: i64::MAX,
    });
    let profile = daemon
        .post(
            "/v1/profiles",
            &serde_json::json!({ "name": "Owner", "locale": "en" }),
        )
        .json::<serde_json::Value>()
        .unwrap();
    let workspace = daemon
        .post(
            "/v1/workspaces",
            &serde_json::json!({ "owner": profile["id"].as_str().unwrap(), "name": "W" }),
        )
        .json::<serde_json::Value>()
        .unwrap();
    let workspace: koru_store_core::ids::WorkspaceId =
        serde_json::from_value(workspace["id"].clone()).unwrap();
    let document = client.create_document(workspace, "Notes", "page").unwrap();

    let query = koru_store_client::http::HistoryQuery {
        limit: Some(50),
        ..Default::default()
    };
    // Nothing captured yet: there is no marker to be unchanged relative to.
    let page = client.history(workspace, document, &query).unwrap();
    assert!(page.entries.is_empty());
    assert_eq!(page.stale, Some(true));

    let marker = client
        .capture_history(
            workspace,
            document,
            &koru_store_client::http::CaptureHistory {
                reason: "manual".into(),
                frontiers: None,
                label: None,
                message: None,
                boundary: None,
            },
        )
        .unwrap();
    let page = client.history(workspace, document, &query).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].label, None);
    assert_eq!(
        page.stale,
        Some(false),
        "a marker just taken is the current state"
    );

    let renamed = client
        .relabel_history(
            workspace,
            marker.id,
            &koru_store_wire::RelabelHistory {
                label: Some("Before the rewrite".into()),
            },
        )
        .unwrap();
    assert_eq!(renamed.id, marker.id);
    assert_eq!(renamed.label.as_deref(), Some("Before the rewrite"));

    // The name is on the marker, so a listing reads it back — and renaming
    // appended nothing.
    let page = client.history(workspace, document, &query).unwrap();
    assert_eq!(page.entries.len(), 1, "a rename is not a capture");
    assert_eq!(page.entries[0].label.as_deref(), Some("Before the rewrite"));
    assert_eq!(page.stale, Some(false), "a rename is not a change");

    // `null` puts it back to unnamed, which is the state a capture with no
    // label produces.
    client
        .relabel_history(
            workspace,
            marker.id,
            &koru_store_wire::RelabelHistory { label: None },
        )
        .unwrap();
    assert_eq!(
        client.history(workspace, document, &query).unwrap().entries[0].label,
        None
    );

    // An entry nobody has ever seen is the store's own not-found, not a
    // storage failure and not a silent success.
    let missing = client
        .relabel_history(
            workspace,
            koru_store_core::ids::HistoryEntryId::new(),
            &koru_store_wire::RelabelHistory {
                label: Some("nowhere".into()),
            },
        )
        .unwrap_err();
    assert_eq!(
        missing.code(),
        koru_store_core::errors::Code::HistoryEntryNotFound,
        "{missing}"
    );
}

#[test]
fn every_route_in_the_table_answers() {
    // A row with no handler behind it passes every unit test and 404s for
    // every integrator.
    let (_dir, daemon) = Daemon::start();
    let profile = daemon
        .post(
            "/v1/profiles",
            &serde_json::json!({ "name": "Owner", "locale": "en" }),
        )
        .json::<serde_json::Value>()
        .unwrap();
    let profile_id = profile["id"].as_str().unwrap().to_string();

    let workspace = daemon
        .post(
            "/v1/workspaces",
            &serde_json::json!({ "owner": profile_id, "name": "W" }),
        )
        .json::<serde_json::Value>()
        .unwrap();
    let workspace_id = workspace["id"].as_str().unwrap().to_string();
    let manifest = daemon.json(&format!("/v1/workspaces/{workspace_id}/manifest"));
    let document_id = manifest["document"].as_str().unwrap().to_string();
    let blob_hash = client()
        .put(daemon.url(&format!("/v1/workspaces/{workspace_id}/blobs")))
        .bearer_auth(&daemon.token)
        .body(b"route probe".to_vec())
        .send()
        .expect("put route-probe blob")
        .json::<serde_json::Value>()
        .unwrap()["hash"]
        .as_str()
        .unwrap()
        .to_owned();

    for route in ROUTES {
        // Stopping the store would end the test; it is exercised in
        // `lifecycle.rs` instead. `/v1/import` needs a real bundle body, which
        // the round-trip test supplies.
        if route.path == "/v1/shutdown" || route.path == "/v1/import" {
            continue;
        }
        let path = concrete(route.path, &profile_id, &workspace_id, &document_id)
            .replace("{hash}", &blob_hash);
        let url = daemon.url(&path);
        let request = match route.method {
            "GET" => client().get(&url),
            "PUT" => client().put(&url).body(b"bytes".to_vec()),
            "POST" => client()
                .post(&url)
                // Enough of a body for the two POSTs that need one; the
                // others ignore it.
                .json(&serde_json::json!({
                    "name": "X", "title": "X", "kind": "page", "owner": profile_id
                })),
            // One body serves the PATCH routes: renames read `name`, while
            // appearance bodies default their optional fields.
            "PATCH" => client()
                .patch(&url)
                .json(&serde_json::json!({ "name": "X" })),
            "DELETE" => client().delete(&url),
            other => panic!("no driver for {other}"),
        };
        let response = request.bearer_auth(&daemon.token).send().expect("send");
        let status = response.status().as_u16();
        let body = response.text().unwrap_or_default();
        assert_ne!(
            status, 405,
            "{} {} is in the table with the wrong method",
            route.method, route.path
        );
        // **A router 404 and a handler 404 are different answers**, and until
        // now this could not tell them apart — so a row was only provable when
        // its ids happened to resolve. Axum's fallback answers 404 with an
        // *empty* body; a handler answers with `{ code, message }`. So a 404
        // carrying a code is a route that exists and did not like what it was
        // given, which is all this test needs to know.
        //
        // `/v1/debug/documents/{id}` is handed a workspace id by `concrete`;
        // that real handler is therefore expected to answer a coded 404.
        assert!(
            status != 404 || body.contains("\"code\""),
            "{} {} is in the table but does not exist ({status}, body {body:?})",
            route.method,
            route.path
        );
    }
}

#[test]
fn every_route_in_the_table_refuses_a_request_with_no_token() {
    // There is no unauthenticated endpoint. Not even one that reports a
    // version — that is how anything on the machine learns a store is here.
    let (_dir, daemon) = Daemon::start();
    for route in ROUTES {
        let nil = "00000000-0000-4000-8000-000000000000";
        let path = concrete(route.path, nil, nil, nil)
            .replace("{hash}", &format!("b3:{}", "00".repeat(32)));
        let url = daemon.url(&path);
        let request = match route.method {
            "GET" => client().get(&url),
            "PUT" => client().put(&url).body(b"bytes".to_vec()),
            "PATCH" => client().patch(&url).json(&serde_json::json!({})),
            _ => client().post(&url).json(&serde_json::json!({})),
        };
        let response = request.send().expect("send");
        assert_eq!(
            response.status().as_u16(),
            401,
            "{} {} answered without a token",
            route.method,
            route.path
        );
    }
}

/// **What a customer is shown, from a running store.**
///
/// The unit tests prove the two documents are built correctly; this proves the
/// store actually serves them that way — the split is only real if it survives
/// the round trip.
#[test]
fn the_served_documents_match_the_routes_that_exist() {
    let (_dir, daemon) = Daemon::start();
    let public = daemon.json("/v1/openapi.json");
    let internal = daemon.json("/v1/debug/openapi.json");
    let public_paths = public["paths"].as_object().expect("paths");
    let internal_paths = internal["paths"].as_object().expect("paths");

    for route in ROUTES {
        let method = route.method.to_lowercase();
        assert!(
            internal_paths
                .get(route.path)
                .and_then(|p| p.get(&method))
                .is_some(),
            "{} {} is missing from the internal document",
            route.method,
            route.path
        );
        let published = public_paths
            .get(route.path)
            .and_then(|p| p.get(&method))
            .is_some();
        match route.audience {
            Audience::Public => assert!(
                published,
                "{} {} is missing from the public document",
                route.method, route.path
            ),
            // A diagnostic counter published to a customer is a dependency
            // nobody offered them.
            Audience::Internal => assert!(
                !published,
                "{} {} is internal and was published anyway",
                route.method, route.path
            ),
        }
    }
}

// ------------------------------------------------------------------ the flow

#[test]
fn a_profile_a_workspace_and_a_document_can_be_created_and_read_back() {
    let (_dir, daemon) = Daemon::start();

    let profile: serde_json::Value = daemon
        .post(
            "/v1/profiles",
            &serde_json::json!({ "name": "Steven — Work", "locale": "pt-BR" }),
        )
        .json()
        .unwrap();
    assert_eq!(profile["locale"], "pt-BR");
    // Creating a profile requires no account and no network.
    assert!(profile.get("account").is_none(), "{profile}");
    let profile_id = profile["id"].as_str().unwrap().to_string();

    let workspace: serde_json::Value = daemon
        .post(
            "/v1/workspaces",
            &serde_json::json!({ "owner": profile_id, "name": "Planejamento" }),
        )
        .json()
        .unwrap();
    let workspace_id = workspace["id"].as_str().unwrap().to_string();

    let created: serde_json::Value = daemon
        .post(
            &format!("/v1/workspaces/{workspace_id}/documents"),
            &serde_json::json!({ "title": "四半期計画", "kind": "page" }),
        )
        .json()
        .unwrap();
    assert!(created["id"].is_string());

    let documents = daemon.json(&format!("/v1/workspaces/{workspace_id}/documents"));
    let listed = documents.as_array().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["title"], "四半期計画");
}

#[test]
fn profile_name_icon_and_colour_can_be_changed() {
    let (_dir, daemon) = Daemon::start();
    let profile: serde_json::Value = daemon
        .post("/v1/profiles", &serde_json::json!({ "name": "Owner" }))
        .json()
        .unwrap();
    let id = profile["id"].as_str().unwrap();

    let renamed: serde_json::Value = client()
        .patch(daemon.url(&format!("/v1/profiles/{id}")))
        .bearer_auth(&daemon.token)
        .json(&serde_json::json!({ "name": "Koru" }))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(renamed["name"], "Koru");

    let appearance: serde_json::Value = client()
        .patch(daemon.url(&format!("/v1/profiles/{id}/appearance")))
        .bearer_auth(&daemon.token)
        .json(&serde_json::json!({ "icon": "🧑‍💻", "color": "purple" }))
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(appearance["icon"], "🧑‍💻");
    assert_eq!(appearance["color"], "purple");

    let listed = daemon.json("/v1/profiles");
    assert_eq!(listed[0]["name"], "Koru");
    assert_eq!(listed[0]["icon"], "🧑‍💻");
    assert_eq!(listed[0]["color"], "purple");
}

#[test]
fn deleting_hides_every_lookup_and_restore_brings_the_document_back() {
    let (_dir, daemon) = Daemon::start();
    let profile: serde_json::Value = daemon
        .post("/v1/profiles", &serde_json::json!({ "name": "Owner" }))
        .json()
        .unwrap();
    let workspace: serde_json::Value = daemon
        .post(
            "/v1/workspaces",
            &serde_json::json!({ "owner": profile["id"], "name": "Recoverable" }),
        )
        .json()
        .unwrap();
    let id = workspace["id"].as_str().unwrap();
    daemon.post(
        &format!("/v1/workspaces/{id}/documents"),
        &serde_json::json!({ "title": "Still here", "kind": "page" }),
    );

    assert!(
        daemon
            .delete(&format!("/v1/workspaces/{id}"))
            .status()
            .is_success()
    );
    assert_eq!(daemon.json("/v1/workspaces").as_array().unwrap().len(), 0);
    assert_eq!(
        daemon
            .json("/v1/workspaces?include_deleted=true")
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        daemon.get(&format!("/v1/workspaces/{id}")).status(),
        reqwest::StatusCode::NOT_FOUND
    );
    assert_eq!(
        daemon
            .get(&format!("/v1/workspaces/{id}/documents"))
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );

    assert!(
        daemon
            .post(
                &format!("/v1/workspaces/{id}/restore"),
                &serde_json::Value::Null,
            )
            .status()
            .is_success()
    );
    let documents = daemon.json(&format!("/v1/workspaces/{id}/documents"));
    assert_eq!(documents[0]["title"], "Still here");
}

#[test]
fn text_in_every_supported_language_survives_the_json_boundary() {
    let (_dir, daemon) = Daemon::start();
    let profile: serde_json::Value = daemon
        .post("/v1/profiles", &serde_json::json!({ "name": "Owner" }))
        .json()
        .unwrap();
    let owner = profile["id"].as_str().unwrap().to_string();

    let workspace: serde_json::Value = daemon
        .post(
            "/v1/workspaces",
            &serde_json::json!({ "owner": owner, "name": "W" }),
        )
        .json()
        .unwrap();
    let id = workspace["id"].as_str().unwrap().to_string();

    let mut expected = Vec::new();
    for sample in koru_store_core::fixtures::SAMPLES {
        daemon.post(
            &format!("/v1/workspaces/{id}/documents"),
            &serde_json::json!({ "title": sample.title, "kind": "page" }),
        );
        expected.push(sample.title.to_string());
    }
    daemon.post(
        &format!("/v1/workspaces/{id}/documents"),
        &serde_json::json!({ "title": koru_store_core::fixtures::BIDI_LINE, "kind": "page" }),
    );
    expected.push(koru_store_core::fixtures::BIDI_LINE.to_string());

    let documents = daemon.json(&format!("/v1/workspaces/{id}/documents"));
    let got: Vec<String> = documents
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(got, expected, "a title changed crossing the API");
}

#[test]
fn a_snapshot_comes_back_as_bytes_a_client_can_import() {
    let (_dir, daemon) = Daemon::start();
    let profile: serde_json::Value = daemon
        .post("/v1/profiles", &serde_json::json!({ "name": "Owner" }))
        .json()
        .unwrap();
    let workspace: serde_json::Value = daemon
        .post(
            "/v1/workspaces",
            &serde_json::json!({ "owner": profile["id"], "name": "W" }),
        )
        .json()
        .unwrap();
    let id = workspace["id"].as_str().unwrap().to_string();
    daemon.post(
        &format!("/v1/workspaces/{id}/documents"),
        &serde_json::json!({ "title": "الخطة الفصلية", "kind": "page" }),
    );

    let manifest = daemon.json(&format!("/v1/workspaces/{id}/manifest"));
    let document = manifest["document"].as_str().unwrap();
    let response = client()
        .post(daemon.url(&format!("/v1/workspaces/{id}/snapshot/{document}")))
        .bearer_auth(&daemon.token)
        .send()
        .expect("send");
    assert!(response.status().is_success());
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream",
        "a snapshot is bytes, not base64 in JSON"
    );

    // The real assertion: it imports, and it says what it should.
    let bytes = response.bytes().unwrap();
    let loaded = koru_store_core::manifest::Manifest::from_snapshot(&bytes).expect("import");
    assert_eq!(
        loaded
            .entries()
            .into_iter()
            .map(|e| e.title)
            .collect::<Vec<_>>(),
        ["الخطة الفصلية"]
    );
}

// ------------------------------------------------------------------ refusals

#[test]
fn a_malformed_id_is_rejected_with_a_stable_code() {
    let (_dir, daemon) = Daemon::start();
    let response = daemon.get("/v1/workspaces/not-a-uuid");
    assert_eq!(response.status().as_u16(), 400);
    let body: serde_json::Value = response.json().unwrap();
    assert_eq!(body["code"], "invalid_id");
}

#[test]
fn an_unknown_workspace_is_a_404_with_a_stable_code() {
    let (_dir, daemon) = Daemon::start();
    let response = daemon.get("/v1/workspaces/00000000-0000-4000-8000-000000000000");
    assert_eq!(response.status().as_u16(), 404);
    let body: serde_json::Value = response.json().unwrap();
    assert_eq!(body["code"], "workspace_not_found");
}

#[test]
fn a_bad_locale_is_refused_rather_than_silently_defaulted() {
    // Quietly substituting a default would give someone a profile in a
    // language they did not ask for, and no way to tell.
    let (_dir, daemon) = Daemon::start();
    let response = daemon.post(
        "/v1/profiles",
        &serde_json::json!({ "name": "Owner", "locale": "Portugues" }),
    );
    assert_eq!(response.status().as_u16(), 400);
    let body: serde_json::Value = response.json().unwrap();
    assert_eq!(body["code"], "invalid_locale");
}

#[test]
fn an_absent_locale_is_undetermined_not_english() {
    // The absence of an answer is not an answer.
    let (_dir, daemon) = Daemon::start();
    let profile: serde_json::Value = daemon
        .post("/v1/profiles", &serde_json::json!({ "name": "Owner" }))
        .json()
        .unwrap();
    assert_eq!(profile["locale"], "und");
}

#[test]
fn a_workspace_for_a_missing_profile_is_refused() {
    let (_dir, daemon) = Daemon::start();
    let response = daemon.post(
        "/v1/workspaces",
        &serde_json::json!({
            "owner": "00000000-0000-4000-8000-000000000000",
            "name": "Orphan"
        }),
    );
    assert_eq!(response.status().as_u16(), 404);
    let body: serde_json::Value = response.json().unwrap();
    assert_eq!(body["code"], "profile_not_found");
}

#[test]
fn no_response_leaks_a_path_or_a_table_name() {
    // A third party never needs to learn that SQLite is underneath, and
    // telling them makes it a compatibility surface we would have to keep.
    let (_dir, daemon) = Daemon::start();
    let mut seen = String::new();
    seen.push_str(&daemon.json("/v1/status").to_string());
    seen.push_str(&daemon.json("/v1/openapi.json").to_string());
    seen.push_str(&daemon.get("/v1/workspaces/not-a-uuid").text().unwrap());
    seen.push_str(
        &daemon
            .get("/v1/workspaces/00000000-0000-4000-8000-000000000000")
            .text()
            .unwrap(),
    );

    let seen = seen.to_lowercase();
    for leak in [
        "sqlite",
        "catalog.db",
        "workspace.db",
        "loro_updates",
        "loro_checkpoints",
        "appdata",
        "\\users\\",
        "/home/",
    ] {
        assert!(!seen.contains(leak), "a response mentions {leak}");
    }
}

// ------------------------------------------------------------------ blobs

#[test]
fn bytes_go_in_and_come_back_out_addressed_by_their_content() {
    let (_dir, daemon) = Daemon::start();
    let profile: serde_json::Value = daemon
        .post("/v1/profiles", &serde_json::json!({ "name": "Owner" }))
        .json()
        .unwrap();
    let workspace: serde_json::Value = daemon
        .post(
            "/v1/workspaces",
            &serde_json::json!({ "owner": profile["id"], "name": "W" }),
        )
        .json()
        .unwrap();
    let id = workspace["id"].as_str().unwrap().to_string();

    let stored: serde_json::Value = client()
        .put(daemon.url(&format!("/v1/workspaces/{id}/blobs")))
        .bearer_auth(&daemon.token)
        .header("content-type", "application/pdf")
        // A Japanese filename with a space, percent-encoded — a raw one would
        // not survive an HTTP header grammar at all.
        .header(
            "x-koru-filename",
            "%E5%9B%9B%E5%8D%8A%E6%9C%9F%20%E8%A8%88%E7%94%BB.pdf",
        )
        .body("四半期計画の中身".as_bytes().to_vec())
        .send()
        .expect("put")
        .json()
        .expect("json");

    let hash = stored["hash"].as_str().unwrap().to_string();
    assert!(hash.starts_with("b3:"), "{hash}");
    assert_eq!(stored["mime"], "application/pdf");
    assert_eq!(stored["filename"], "四半期 計画.pdf");

    let response = daemon.get(&format!("/v1/workspaces/{id}/blobs/{hash}"));
    assert!(response.status().is_success());
    assert_eq!(response.headers()["content-type"], "application/pdf");
    assert_eq!(response.bytes().unwrap(), "四半期計画の中身".as_bytes());

    let listed = daemon.json(&format!("/v1/workspaces/{id}/blobs"));
    assert_eq!(listed.as_array().unwrap().len(), 1);
}

#[test]
fn the_same_bytes_stored_twice_are_one_blob() {
    let (_dir, daemon) = Daemon::start();
    let profile: serde_json::Value = daemon
        .post("/v1/profiles", &serde_json::json!({ "name": "Owner" }))
        .json()
        .unwrap();
    let workspace: serde_json::Value = daemon
        .post(
            "/v1/workspaces",
            &serde_json::json!({ "owner": profile["id"], "name": "W" }),
        )
        .json()
        .unwrap();
    let id = workspace["id"].as_str().unwrap().to_string();

    let put = |body: &'static str| -> String {
        client()
            .put(daemon.url(&format!("/v1/workspaces/{id}/blobs")))
            .bearer_auth(&daemon.token)
            .body(body.as_bytes().to_vec())
            .send()
            .expect("put")
            .json::<serde_json::Value>()
            .unwrap()["hash"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(put("same"), put("same"));
    assert_ne!(put("same"), put("different"));
    assert_eq!(
        daemon
            .json(&format!("/v1/workspaces/{id}/blobs"))
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn a_missing_blob_says_why_instead_of_a_false_404_or_a_hang() {
    let (_dir, daemon) = Daemon::start();
    let profile: serde_json::Value = daemon
        .post("/v1/profiles", &serde_json::json!({ "name": "Owner" }))
        .json()
        .unwrap();
    let workspace: serde_json::Value = daemon
        .post(
            "/v1/workspaces",
            &serde_json::json!({ "owner": profile["id"], "name": "W" }),
        )
        .json()
        .unwrap();
    let id = workspace["id"].as_str().unwrap().to_string();

    // This used to assert the request *waited* (a client-side timeout). That
    // was half right: hydration must keep waiting, but the request must not —
    // a blob whose holder is away answers with the reason, so the app can say
    // "it will arrive" instead of hanging until the transport gives up. The
    // original point stands unchanged: never a false 404 — `blob_not_found`
    // would tell a caller the bytes do not exist anywhere, and they do.
    let absent = format!("b3:{}", "ab".repeat(32));
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
        .get(daemon.url(&format!("/v1/workspaces/{id}/blobs/{absent}")))
        .bearer_auth(&daemon.token)
        .send()
        .expect("a parked hydration held the request open");
    assert_eq!(response.status(), 503, "unavailable, not a false 404");
    let body: serde_json::Value = response.json().unwrap();
    // This daemon has no linked devices, so "waiting for one" would be a lie.
    assert_eq!(body["code"], "no_linked_devices", "{body}");
}

#[test]
fn a_malformed_hash_is_rejected_rather_than_treated_as_a_filename() {
    let (_dir, daemon) = Daemon::start();
    let profile: serde_json::Value = daemon
        .post("/v1/profiles", &serde_json::json!({ "name": "Owner" }))
        .json()
        .unwrap();
    let workspace: serde_json::Value = daemon
        .post(
            "/v1/workspaces",
            &serde_json::json!({ "owner": profile["id"], "name": "W" }),
        )
        .json()
        .unwrap();
    let id = workspace["id"].as_str().unwrap().to_string();

    for bad in ["not-a-hash", "b3:nothex", "..%2F..%2Fetc%2Fpasswd"] {
        let response = daemon.get(&format!("/v1/workspaces/{id}/blobs/{bad}"));
        assert!(
            !response.status().is_success(),
            "{bad} was accepted as a hash"
        );
    }
}

// ------------------------------------------------------------------ diagnosis

#[test]
fn the_debug_routes_report_metadata_and_never_content() {
    // These end up in bug reports, support threads and log aggregators. The one
    // thing that must never travel that way is what somebody wrote.
    let (_dir, daemon) = Daemon::start();
    let profile: serde_json::Value = daemon
        .post("/v1/profiles", &serde_json::json!({ "name": "Owner" }))
        .json()
        .unwrap();
    let workspace: serde_json::Value = daemon
        .post(
            "/v1/workspaces",
            &serde_json::json!({ "owner": profile["id"], "name": "W" }),
        )
        .json()
        .unwrap();
    let id = workspace["id"].as_str().unwrap().to_string();

    // Something distinctive in every place text can hide.
    let secret_title = "季度SECRETTITLE";
    daemon.post(
        &format!("/v1/workspaces/{id}/documents"),
        &serde_json::json!({ "title": secret_title, "kind": "page" }),
    );
    client()
        .put(daemon.url(&format!("/v1/workspaces/{id}/blobs")))
        .bearer_auth(&daemon.token)
        .header("x-koru-filename", "SECRETFILENAME.pdf")
        .body(b"SECRETBYTES".to_vec())
        .send()
        .expect("put");

    let manifest = daemon.json(&format!("/v1/workspaces/{id}/manifest"));
    let document = manifest["document"].as_str().unwrap();

    let mut seen = String::new();
    seen.push_str(&daemon.json("/v1/debug/store").to_string());
    seen.push_str(
        &daemon
            .json(&format!("/v1/debug/workspaces/{id}"))
            .to_string(),
    );
    seen.push_str(
        &daemon
            .json(&format!("/v1/debug/documents/{document}"))
            .to_string(),
    );
    seen.push_str(&daemon.json("/v1/metrics").to_string());

    for leak in ["SECRETTITLE", "SECRETFILENAME", "SECRETBYTES", "季度"] {
        assert!(!seen.contains(leak), "a debug route leaked {leak}");
    }
}

#[test]
fn the_store_report_says_what_it_is_and_what_it_is_doing() {
    let (_dir, daemon) = Daemon::start();
    let report = daemon.json("/v1/debug/store");

    assert!(report["channel"].as_str().is_some_and(|c| !c.is_empty()));
    assert!(report["pid"].as_u64().is_some_and(|p| p > 0));
    assert!(report["uptime_ms"].as_i64().is_some());
    assert_eq!(report["api_version"], 1);
    // All five versions, so a bug report carries them without being asked.
    for key in [
        "catalog_schema_version",
        "workspace_schema_version",
        "crdt_schema_version",
        "bundle_format_version",
    ] {
        assert!(report[key].is_number(), "{key} missing");
    }
    // A peer id is a string: u64 loses precision in JSON's number type, and one
    // that silently rounds is worse than one that is awkward to read.
    assert!(report["peer_id"].is_string());
    assert_eq!(report["workspaces_open"], 0, "nothing is opened eagerly");
    assert!(report["recoveries"].as_array().unwrap().is_empty());
}

#[test]
fn the_document_report_shows_where_a_document_has_got_to() {
    let (_dir, daemon) = Daemon::start();
    let profile: serde_json::Value = daemon
        .post("/v1/profiles", &serde_json::json!({ "name": "Owner" }))
        .json()
        .unwrap();
    let workspace: serde_json::Value = daemon
        .post(
            "/v1/workspaces",
            &serde_json::json!({ "owner": profile["id"], "name": "W" }),
        )
        .json()
        .unwrap();
    let id = workspace["id"].as_str().unwrap().to_string();
    let manifest = daemon.json(&format!("/v1/workspaces/{id}/manifest"));
    let document = manifest["document"].as_str().unwrap();

    let report = daemon.json(&format!("/v1/debug/documents/{document}"));
    assert_eq!(
        report["workspace"], id,
        "it resolved through the location index"
    );
    assert_eq!(report["kind"], "manifest");
    // Hex, because a version vector is bytes and JSON has no type for them.
    let vv = report["version_vector"].as_str().unwrap();
    assert!(vv.chars().all(|c| c.is_ascii_hexdigit()), "{vv}");
    assert!(report["pending_updates"].as_u64().is_some());
    assert_eq!(report["subscribers"], 0);
}

#[test]
fn a_debug_route_for_something_absent_is_a_404() {
    let (_dir, daemon) = Daemon::start();
    let nil = "00000000-0000-4000-8000-000000000000";
    for path in [
        format!("/v1/debug/workspaces/{nil}"),
        format!("/v1/debug/documents/{nil}"),
    ] {
        assert_eq!(daemon.get(&path).status().as_u16(), 404, "{path}");
    }
}

#[test]
fn metrics_count_what_is_open() {
    let (_dir, daemon) = Daemon::start();
    let before = daemon.json("/v1/metrics");
    assert_eq!(before["workspaces_known"], 0);
    assert_eq!(before["rooms_occupied"], 0);

    let profile: serde_json::Value = daemon
        .post("/v1/profiles", &serde_json::json!({ "name": "Owner" }))
        .json()
        .unwrap();
    daemon.post(
        "/v1/workspaces",
        &serde_json::json!({ "owner": profile["id"], "name": "W" }),
    );

    let after = daemon.json("/v1/metrics");
    assert_eq!(after["workspaces_known"], 1);
    assert!(after["uptime_ms"].as_i64().unwrap() >= 0);
}

// ------------------------------------------------------------------ bundles

/// A profile and a workspace with one document, over the API.
fn a_workspace_with(daemon: &Daemon, title: &str) -> (String, String) {
    let profile: serde_json::Value = daemon
        .post("/v1/profiles", &serde_json::json!({ "name": "Owner" }))
        .json()
        .unwrap();
    let owner = profile["id"].as_str().unwrap().to_string();
    let workspace: serde_json::Value = daemon
        .post(
            "/v1/workspaces",
            &serde_json::json!({ "owner": owner, "name": "W" }),
        )
        .json()
        .unwrap();
    let id = workspace["id"].as_str().unwrap().to_string();
    daemon.post(
        &format!("/v1/workspaces/{id}/documents"),
        &serde_json::json!({ "title": title, "kind": "page" }),
    );
    (owner, id)
}

fn export(daemon: &Daemon, workspace: &str) -> Vec<u8> {
    let response = client()
        .post(daemon.url(&format!("/v1/workspaces/{workspace}/export")))
        .bearer_auth(&daemon.token)
        .send()
        .expect("export");
    assert!(response.status().is_success(), "{}", response.status());
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream"
    );
    response.bytes().unwrap().to_vec()
}

#[test]
fn a_workspace_can_be_exported_and_imported_over_the_api() {
    // The service methods were tested long before this route existed, which is
    // exactly how a feature ends up working and unreachable.
    let (_dir, daemon) = Daemon::start();
    let (owner, workspace) = a_workspace_with(&daemon, "四半期計画");
    let bundle = export(&daemon, &workspace);
    assert!(!bundle.is_empty());

    let imported: serde_json::Value = client()
        .post(daemon.url(&format!("/v1/import?mode=duplicate&owner={owner}")))
        .bearer_auth(&daemon.token)
        .body(bundle)
        .send()
        .expect("import")
        .json()
        .expect("json");

    let copy = imported["id"].as_str().unwrap();
    assert_ne!(copy, workspace, "a duplicate kept its origin's id");

    let documents = daemon.json(&format!("/v1/workspaces/{copy}/documents"));
    assert_eq!(documents.as_array().unwrap()[0]["title"], "四半期計画");
}

#[test]
fn a_store_export_larger_than_axums_default_can_be_imported() {
    // Export and import are two halves of one public format. Axum's Bytes
    // extractor defaults to 2 MiB, so two individually ordinary blobs are
    // enough to prove the aggregate bundle is not accidentally capped there.
    let (_dir, daemon) = Daemon::start();
    let (owner, workspace) = a_workspace_with(&daemon, "Large workspace");
    let first = vec![0x31; 1_200_000];
    let second = vec![0x72; 1_200_000];
    let mut originals = Vec::new();
    for bytes in [&first, &second] {
        let response = client()
            .put(daemon.url(&format!("/v1/workspaces/{workspace}/blobs")))
            .bearer_auth(&daemon.token)
            .header("content-type", "application/octet-stream")
            .body(bytes.clone())
            .send()
            .expect("put blob");
        assert!(response.status().is_success(), "{}", response.status());
        let blob: serde_json::Value = response.json().expect("blob json");
        originals.push((blob["hash"].as_str().unwrap().to_string(), bytes.as_slice()));
    }

    let bundle = export(&daemon, &workspace);
    assert!(
        bundle.len() > 2 * 1024 * 1024,
        "bundle was {} bytes",
        bundle.len()
    );

    let response = client()
        .post(daemon.url(&format!("/v1/import?mode=duplicate&owner={owner}")))
        .bearer_auth(&daemon.token)
        .header("content-type", "application/octet-stream")
        .body(bundle)
        .send()
        .expect("import");
    assert!(response.status().is_success(), "{}", response.status());
    let imported: serde_json::Value = response.json().expect("workspace json");
    let copy = imported["id"].as_str().unwrap();
    assert_ne!(copy, workspace, "a duplicate kept its origin's id");

    let blobs = daemon.json(&format!("/v1/workspaces/{copy}/blobs"));
    let mut copied_hashes: Vec<_> = blobs
        .as_array()
        .unwrap()
        .iter()
        .map(|blob| blob["hash"].as_str().unwrap().to_string())
        .collect();
    originals.sort_by(|left, right| left.0.cmp(&right.0));
    copied_hashes.sort();
    assert_eq!(
        copied_hashes,
        originals
            .iter()
            .map(|(hash, _)| hash.clone())
            .collect::<Vec<_>>()
    );
    for (hash, (_, expected)) in copied_hashes.iter().zip(&originals) {
        let response = daemon.get(&format!("/v1/workspaces/{copy}/blobs/{hash}"));
        assert!(response.status().is_success(), "{}", response.status());
        assert_eq!(response.bytes().unwrap().as_ref(), *expected);
    }
}

#[test]
fn importing_without_a_mode_is_refused_rather_than_guessed() {
    // The two modes differ in whether the result can ever converge with its
    // origin. Guessing is quiet and slow to notice, so there is no default.
    let (_dir, daemon) = Daemon::start();
    let (owner, workspace) = a_workspace_with(&daemon, "One");
    let bundle = export(&daemon, &workspace);

    let response = client()
        .post(daemon.url(&format!("/v1/import?owner={owner}")))
        .bearer_auth(&daemon.token)
        .body(bundle.clone())
        .send()
        .expect("send");
    assert!(!response.status().is_success(), "an import guessed a mode");

    // And a mode that is not one of the two is refused by name.
    let response = client()
        .post(daemon.url(&format!("/v1/import?mode=copy&owner={owner}")))
        .bearer_auth(&daemon.token)
        .body(bundle)
        .send()
        .expect("send");
    assert_eq!(response.status().as_u16(), 400);
    let body: serde_json::Value = response.json().unwrap();
    assert_eq!(body["code"], "invalid_request");
    assert!(
        body["message"].as_str().unwrap().contains("no default"),
        "{body}"
    );
}

#[test]
fn restoring_a_workspace_that_is_already_here_is_refused() {
    let (_dir, daemon) = Daemon::start();
    let (owner, workspace) = a_workspace_with(&daemon, "One");
    let bundle = export(&daemon, &workspace);

    let response = client()
        .post(daemon.url(&format!("/v1/import?mode=restore&owner={owner}")))
        .bearer_auth(&daemon.token)
        .body(bundle)
        .send()
        .expect("send");
    assert_eq!(response.status().as_u16(), 409);
    let body: serde_json::Value = response.json().unwrap();
    assert_eq!(body["code"], "workspace_exists");
}

#[test]
fn a_bundle_that_is_not_a_bundle_is_refused() {
    let (_dir, daemon) = Daemon::start();
    let (owner, _) = a_workspace_with(&daemon, "One");
    for body in [b"".to_vec(), b"nonsense".to_vec()] {
        let response = client()
            .post(daemon.url(&format!("/v1/import?mode=duplicate&owner={owner}")))
            .bearer_auth(&daemon.token)
            .body(body)
            .send()
            .expect("send");
        assert_eq!(response.status().as_u16(), 400);
        let body: serde_json::Value = response.json().expect("failure json");
        assert_eq!(body["code"], "invalid_request");
    }
}

/// **`/v1/readings/reader…` and `/v1/readings/{workspace}/{hash}` do not
/// shadow each other** (OCR Plans 1.8, 2.4 and 3.5 meet here): each request
/// reaches its own handler, told apart by what answers.
#[test]
fn the_reader_routes_and_one_reading_reach_their_own_handlers() {
    let (_dir, daemon) = Daemon::start();
    let profile = daemon
        .post(
            "/v1/profiles",
            &serde_json::json!({ "name": "Owner", "locale": "en" }),
        )
        .json::<serde_json::Value>()
        .unwrap();
    let workspace = daemon
        .post(
            "/v1/workspaces",
            &serde_json::json!({ "owner": profile["id"], "name": "W" }),
        )
        .json::<serde_json::Value>()
        .unwrap();
    let ws = workspace["id"].as_str().unwrap().to_owned();
    let hash = format!("b3:{}", "ab".repeat(32));

    // The reader's state: a `ReaderState`, not a reading.
    let state = daemon.json("/v1/readings/reader");
    assert!(
        state.get("catalog").is_some() && state.get("counts").is_some(),
        "{state}"
    );

    // One reading: the reading port's own answer, `null` for nothing read.
    let one = daemon.get(&format!("/v1/readings/{ws}/{hash}"));
    assert_eq!(one.status().as_u16(), 200);
    assert_eq!(
        one.json::<serde_json::Value>().unwrap(),
        serde_json::Value::Null
    );

    // A reader verb reaches the reader, never the two-segment reading route.
    let declined = daemon.post("/v1/readings/reader/decline", &serde_json::json!({}));
    assert_eq!(declined.status().as_u16(), 200);
    let declined: serde_json::Value = declined.json().unwrap();
    assert_eq!(declined["offer_declined"], true);
    let stopped = daemon.post("/v1/readings/reader/stop", &serde_json::json!({}));
    assert_eq!(stopped.status().as_u16(), 200);
    assert!(
        stopped
            .json::<serde_json::Value>()
            .unwrap()
            .get("counts")
            .is_some()
    );

    // Off and on (OCR Plan 3.5) reach the reader too — never the reading
    // route taking `reader` for a workspace and `off` for a hash. With nothing
    // installed there is nothing to turn off, and the answer says so.
    for verb in ["off", "on"] {
        let answered = daemon.post(
            &format!("/v1/readings/reader/{verb}"),
            &serde_json::json!({}),
        );
        assert_eq!(answered.status().as_u16(), 200, "{verb}");
        let answered: serde_json::Value = answered.json().unwrap();
        assert!(answered.get("counts").is_some(), "{verb}: {answered}");
        assert_eq!(answered["turned_off"], false, "{verb}: {answered}");
        let got = daemon.get(&format!("/v1/readings/reader/{verb}"));
        assert_eq!(got.status().as_u16(), 405, "{verb}");
    }

    // The reader's static paths own their segments: a GET on a reader verb
    // is that verb's route refusing the method, never the reading route
    // taking `reader` for a workspace. And a POST on a reading's path is the
    // reading route refusing it, never a reader verb.
    let crossed = daemon.get("/v1/readings/reader/install");
    assert_eq!(crossed.status().as_u16(), 405);
    let wrong = daemon.post(&format!("/v1/readings/{ws}/{hash}"), &serde_json::json!({}));
    assert_eq!(wrong.status().as_u16(), 405);

    // A person's two requests (OCR Plan 3.2) reach the reading port, which
    // answers for nothing read here with its own refusal — never the reader's
    // routes, never a 404 for an unknown path. A GET on them is refused.
    for verb in ["read-all", "read-again"] {
        let asked = daemon.post(
            &format!("/v1/readings/{ws}/{hash}/{verb}"),
            &serde_json::json!({}),
        );
        assert_eq!(asked.status().as_u16(), 400, "{verb}");
        let body: serde_json::Value = asked.json().unwrap();
        assert_eq!(body["code"], "invalid_request", "{verb}: {body}");
        let got = daemon.get(&format!("/v1/readings/{ws}/{hash}/{verb}"));
        assert_eq!(got.status().as_u16(), 405, "{verb}");
    }
    // Still a reading's answer, with nothing offered: nothing was read.
    let one = daemon.get(&format!("/v1/readings/{ws}/{hash}"));
    assert_eq!(
        one.json::<serde_json::Value>().unwrap(),
        serde_json::Value::Null
    );
    // An unknown workspace is the catalog's 404, as for the reading itself.
    let nowhere = daemon.post(
        &format!(
            "/v1/readings/{}/{hash}/read-all",
            koru_store_core::ids::WorkspaceId::new()
        ),
        &serde_json::json!({}),
    );
    assert_eq!(nowhere.status().as_u16(), 404);
}
