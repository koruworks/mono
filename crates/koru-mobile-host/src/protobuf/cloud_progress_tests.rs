// SPDX-License-Identifier: MPL-2.0

//! `store.cloud_sync_status` and `store.cloud_waiting`: the phone's two reads
//! behind the Cloud sync progress screen, against a real host on a private
//! root. The host's Store worker composes the same blob engine the desktop
//! runs, so these answer from it rather than from a double.

use super::*;
use koru_store_core::model::{Blob, BlobAttribute, BlobHash, blob_attr};
use koru_store_core::{DocumentId, Locale, Timestamp, WorkspaceId};
use std::collections::HashMap;

fn at() -> Timestamp {
    Timestamp::from_millis(1_770_000_000_000)
}

fn context(root: &std::path::Path) -> wire::HostContext {
    wire::HostContext {
        root: root.display().to_string(),
        secrets: vec![91; 64],
        device_name: "Test".into(),
        relay_enabled: None,
    }
}

/// A Profile with one Workspace, straight into the host's own Store.
fn a_workspace(host: &MobileHost) -> WorkspaceId {
    host.store
        .with_store(|store| {
            let profile = store
                .create_profile("Owner", None, None, Locale::parse("en").unwrap(), at())
                .unwrap();
            store
                .create_workspace(profile.id, "Photos", at())
                .unwrap()
                .id
        })
        .unwrap()
}

fn hash(seed: char) -> BlobHash {
    BlobHash::parse(&format!("b3:{}", seed.to_string().repeat(64))).unwrap()
}

/// **Asked through the generated messages and the gateway's catalog**, a
/// Workspace answers about its manifest, with the engine's counts and no
/// Cloud permit — a phone with no account has nothing on Cloud, which is an
/// absent `sync`, never an error.
#[test]
fn a_workspace_answers_its_manifest_counts_and_no_permit() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("cloud-status");
    let host = get_host(root.clone(), [91; 64], "Test", true).unwrap();
    let workspace = a_workspace(&host);
    let manifest = host
        .store
        .with_store(move |store| store.manifest_id(workspace))
        .unwrap()
        .unwrap();

    let answer = cloud_sync_status(wire::CloudSyncStatusRequest {
        host: Some(context(&root)),
        workspace_id: workspace.to_string(),
    });
    assert_eq!(answer.error_code, None);
    assert_eq!(answer.workspace_id, workspace.to_string());
    assert_eq!(
        answer.manifest_document_id,
        manifest.to_string(),
        "asked about the manifest, as the desktop asks",
    );
    assert_eq!(answer.sync, None, "no account, no live permit");
    let files = answer.files.expect("the phone composes the engine");
    assert_eq!(files.referenced, 0);
    assert_eq!(files.pending_reason, "");
    assert!(!files.paused);
    assert_eq!(
        files.coverage, None,
        "no claim computed is absent, never a zero claim",
    );
}

/// A Workspace nobody can parse, and a call with no host context, both
/// answer a stable code in the reply's own shape.
#[test]
fn a_refusal_is_a_code_in_the_reply_shape() {
    let refused = cloud_sync_status(wire::CloudSyncStatusRequest {
        host: None,
        workspace_id: "whatever".into(),
    });
    assert_eq!(refused.error_code.as_deref(), Some("missing_host_context"));
    assert_eq!(refused.workspace_id, "whatever");
    assert_eq!(refused.files, None);

    let waiting = cloud_waiting(wire::CloudWaitingRequest {
        host: None,
        workspace_id: "whatever".into(),
    });
    assert_eq!(waiting.error_code.as_deref(), Some("missing_host_context"));
    assert!(waiting.documents.is_empty());

    let directory = tempfile::tempdir().unwrap();
    let host = MobileHost::open(directory.path().to_path_buf(), [92; 64], "Test", false).unwrap();
    assert_eq!(
        host.cloud_sync_status("not-a-workspace").unwrap_err(),
        "invalid_workspace_id"
    );
    assert_eq!(
        host.cloud_waiting("not-a-workspace").unwrap_err(),
        "invalid_workspace_id"
    );
}

/// A Workspace with no `object_missing` park is waiting on nobody — an empty
/// list, not an error — and asking again answers the same. (That the engine's
/// read leaves every park where it was is pinned beside the engine, in
/// `koru-blob-sync`; a fresh phone has no park to leave.)
#[test]
fn a_workspace_waiting_on_nobody_answers_an_empty_list() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("cloud-waiting");
    let host = get_host(root.clone(), [93; 64], "Test", true).unwrap();
    let workspace = a_workspace(&host);

    for _ in 0..2 {
        let answer = cloud_waiting(wire::CloudWaitingRequest {
            host: Some(wire::HostContext {
                secrets: vec![93; 64],
                ..context(&root)
            }),
            workspace_id: workspace.to_string(),
        });
        assert_eq!(answer.error_code, None);
        assert_eq!(answer.workspace_id, workspace.to_string());
        assert!(answer.documents.is_empty());
    }
}

/// **Names come from the snapshot the host installed**, through the one
/// mapping the desktop's route also calls: the host keeps what it handed the
/// network runtime, and a waiting document is told the name of the peer
/// that first held its files — and never this device's own.
#[test]
fn a_waiting_document_is_named_from_the_hosts_own_authorization() {
    let directory = tempfile::tempdir().unwrap();
    let host = MobileHost::open(directory.path().to_path_buf(), [94; 64], "Test", false).unwrap();
    let workspace = a_workspace(&host);
    let local = koru_device::DeviceId::new();
    let pixel = koru_device::DeviceId::new();
    host.install_authorization(koru_sync_control::AuthorizationSnapshot {
        session_id: koru_sync_control::ControlSessionId::new(),
        sequence: 1,
        local_device: local,
        peers: vec![koru_sync_control::AuthorizedPeer {
            relationship_id: koru_device::RelationshipId::new(),
            device_id: pixel,
            endpoint_id: koru_device::ReplicationEndpointId::from_bytes([7; 32]),
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: Vec::new(),
            via: None,
            display_name: Some("Pixel 8".into()),
            build: None,
            own_device: false,
        }],
        local_build: None,
    });
    let (theirs, mine) = (hash('a'), hash('b'));
    let origin = |hash: &BlobHash, device: koru_device::DeviceId| Blob {
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
    let blobs = vec![origin(&theirs, pixel), origin(&mine, local)];
    let document: DocumentId = host
        .store
        .with_store(move |store| {
            store.record_blob_origins(workspace, &blobs, at()).unwrap();
            store
                .create_document(workspace, "Trip notes", "page", None, None, at())
                .unwrap()
                .0
        })
        .unwrap();

    let authorization = host.authorization.lock().unwrap().clone();
    assert_eq!(
        authorization.as_ref().map(|snapshot| snapshot.local_device),
        Some(local),
        "the host reads back what it installed",
    );
    let waiting = vec![koru_store_api::state::WaitingFiles {
        document,
        blobs: vec![theirs, mine],
    }];
    let documents = host
        .name_waiting(workspace, waiting, HashMap::new())
        .unwrap();
    let messages: Vec<_> = documents.into_iter().map(cloud_waiting_message).collect();
    assert_eq!(
        messages,
        vec![wire::CloudWaitingDocument {
            document_id: document.to_string(),
            title: "Trip notes".into(),
            missing_files: 2,
            holders: vec!["Pixel 8".into()],
        }]
    );
}

/// **A device that holds a copy is named, not only the one that made it**:
/// what the phone's network runtime heard advertised is merged with each
/// blob's origin — one blob known only by origin, one only by advertisement,
/// one by both, which names each device once. A fresh runtime has heard
/// nothing, and says so with no entry rather than an empty guess.
#[test]
fn a_device_that_advertised_a_copy_is_named_beside_the_origin() {
    let directory = tempfile::tempdir().unwrap();
    let host = MobileHost::open(directory.path().to_path_buf(), [95; 64], "Test", false).unwrap();
    let workspace = a_workspace(&host);
    let (local, thinkpad, desktop) = (
        koru_device::DeviceId::new(),
        koru_device::DeviceId::new(),
        koru_device::DeviceId::new(),
    );
    let peer = |device, name: &str, seed| koru_sync_control::AuthorizedPeer {
        relationship_id: koru_device::RelationshipId::new(),
        device_id: device,
        endpoint_id: koru_device::ReplicationEndpointId::from_bytes([seed; 32]),
        all_data: true,
        authorized_profiles: Vec::new(),
        workspaces: Vec::new(),
        via: None,
        display_name: Some(name.into()),
        build: None,
        own_device: false,
    };
    host.install_authorization(koru_sync_control::AuthorizationSnapshot {
        session_id: koru_sync_control::ControlSessionId::new(),
        sequence: 1,
        local_device: local,
        peers: vec![
            peer(thinkpad, "ThinkpadP11", 7),
            peer(desktop, "Desktop", 8),
        ],
        local_build: None,
    });
    let (a, b, c) = (hash('a'), hash('b'), hash('c'));
    let origin = |hash: &BlobHash| Blob {
        hash: hash.clone(),
        size: 10,
        mime: "image/png".into(),
        filename: None,
        created_at: at(),
        attributes: [(
            blob_attr::ORIGIN_DEVICE.to_owned(),
            BlobAttribute::Text(thinkpad.to_storage()),
        )]
        .into(),
    };
    let blobs = vec![origin(&a), origin(&c)];
    let [origin_only, peer_only, both]: [DocumentId; 3] = host
        .store
        .with_store(move |store| {
            store.record_blob_origins(workspace, &blobs, at()).unwrap();
            ["Origin only", "Peer only", "Both"].map(|title| {
                store
                    .create_document(workspace, title, "page", None, None, at())
                    .unwrap()
                    .0
            })
        })
        .unwrap();
    let waiting = vec![
        koru_store_api::state::WaitingFiles {
            document: origin_only,
            blobs: vec![a.clone()],
        },
        koru_store_api::state::WaitingFiles {
            document: peer_only,
            blobs: vec![b.clone()],
        },
        koru_store_api::state::WaitingFiles {
            document: both,
            blobs: vec![c.clone()],
        },
    ];
    assert!(
        host.advertised_holders(workspace, &waiting).is_empty(),
        "a runtime that has exchanged no inventory has heard nobody",
    );
    let advertised: HashMap<BlobHash, Vec<String>> = [
        (b, vec![desktop.to_storage(), local.to_storage()]),
        (c, vec![thinkpad.to_storage(), desktop.to_storage()]),
    ]
    .into();

    let documents = host.name_waiting(workspace, waiting, advertised).unwrap();
    let holders = |document: DocumentId| {
        documents
            .iter()
            .find(|entry| entry.document == document)
            .unwrap()
            .holders
            .clone()
    };
    assert_eq!(holders(origin_only), ["ThinkpadP11"]);
    assert_eq!(holders(peer_only), ["Desktop"]);
    assert_eq!(holders(both), ["Desktop", "ThinkpadP11"]);
}

/// The report reaches the message field for field, including the ones a
/// progress row turns into words: the pause, the direction and path of a
/// transfer, and a coverage claim — superseded included, which the screen
/// then refuses to read a number from.
#[test]
fn the_store_report_reaches_the_message_field_for_field() {
    use koru_store_wire::internal::{
        BlobCoverageReport, BlobTransferReport, CloudSyncState, CloudSyncStatus,
    };
    let status = CloudSyncStatus {
        sync: Some(CloudSyncState {
            enabled: true,
            stage: Some("unavailable".into()),
            error_code: Some("cloud_unit_unavailable".into()),
        }),
        blobs: Some(BlobTransferReport {
            referenced: 72,
            present: 70,
            durable: 33,
            uploading: 2,
            downloading: 0,
            missing: 4,
            bytes_complete: 0,
            bytes_total: 0,
            pending_reason: Some("object_missing".into()),
            last_failure_code: None,
            direction: Some("upload".into()),
            path: Some("cloud".into()),
            object_host: None,
            oldest_retry_at_millis: None,
            unreadable_references: 1,
            walks: 0,
            passes: 0,
            passes_skipped: 0,
            paused: true,
            coverage: Some(BlobCoverageReport {
                state: "superseded".into(),
                current: false,
                documents_required: 88,
                documents_missing: 40,
                blobs_referenced: 72,
                blobs_durable: 33,
                blobs_uploading: 2,
                blobs_missing: 37,
                unreadable_references: 1,
                sample: String::new(),
            }),
            preflight: None,
        }),
        ..CloudSyncStatus::default()
    };
    let message = cloud_sync_message("ws", &status);
    assert_eq!(message.workspace_id, "ws");
    assert_eq!(message.manifest_document_id, "");
    let sync = message.sync.unwrap();
    assert!(sync.enabled);
    assert_eq!(sync.stage, "unavailable");
    assert_eq!(sync.error_code, "cloud_unit_unavailable");
    let files = message.files.unwrap();
    assert_eq!(
        (files.referenced, files.present, files.durable),
        (72, 70, 33)
    );
    assert_eq!((files.uploading, files.missing), (2, 4));
    assert_eq!(files.pending_reason, "object_missing");
    assert_eq!(files.direction, "upload");
    assert_eq!(files.path, "cloud");
    assert_eq!(files.unreadable_references, 1);
    assert!(files.paused);
    let coverage = files.coverage.unwrap();
    assert_eq!(coverage.state, "superseded");
    assert_eq!(
        (coverage.documents_required, coverage.documents_missing),
        (88, 40)
    );
}
