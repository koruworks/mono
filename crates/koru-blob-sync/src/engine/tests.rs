// SPDX-License-Identifier: MPL-2.0

use super::*;

use std::sync::atomic::Ordering;

use koru_cloud_blob::BlobCode;

use crate::{
    fakes::{Harness, hash_of},
    ports::TransferHold,
};

fn picture(seed: u8, bytes: usize) -> Vec<u8> {
    (0..bytes)
        .map(|index| (index as u8).wrapping_add(seed))
        .collect()
}

/// A picture pasted on this device reaches Cloud, and a second pass finds it
/// already there rather than sending it again.
#[tokio::test]
async fn a_held_picture_is_uploaded_once() {
    let harness = Harness::new();
    let bytes = picture(1, 9_000);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", Some("shot.png"));
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(pass.uploaded, 1, "{pass:?}");
    assert_eq!(pass.session_refusal, None);

    let again = harness
        .engine
        .reconcile(harness.workspace, Trigger::MappingChanged)
        .await;
    assert_eq!(again.already_durable, 1, "{again:?}");
    assert_eq!(again.uploaded, 0);
}

/// A device that has never seen the picture fetches it back, installs the
/// object, writes the association **last**, and announces identity.
#[tokio::test]
async fn a_fresh_device_hydrates_and_announces() {
    let harness = Harness::new();
    let bytes = picture(2, 7_777);
    let hash = hash_of(&bytes);
    // Cloud holds it; this device does not.
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    harness.downloads.watch(&hash);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(pass.downloaded, 1, "{pass:?}");
    assert_eq!(
        harness.store.object_bytes(&hash).as_deref(),
        Some(&bytes[..])
    );
    assert!(harness.store.associated(harness.workspace, &hash));
    assert_eq!(
        harness.store.installs(),
        vec![(harness.workspace, hash.as_str().to_owned())]
    );
    // **Identity, never bytes.**
    assert_eq!(
        *harness
            .downloads
            .arrived
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
        vec![hash.as_str().to_owned()]
    );
}

/// **S11, at the engine's level.** A download whose bytes do not hash
/// publishes nothing and associates nothing, and the blob is parked with the
/// contract's own code.
#[tokio::test]
async fn bytes_that_do_not_hash_are_never_associated() {
    let harness = Harness::new();
    let bytes = picture(3, 4_096);
    let hash = hash_of(&bytes);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    harness.plane.corrupt(harness.workspace, &hash);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    harness.downloads.watch(&hash);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(pass.downloaded, 0);
    assert_eq!(
        pass.parked,
        vec![(hash.clone(), BlobCode::HashMismatch.as_str())]
    );
    assert!(!harness.store.associated(harness.workspace, &hash));
    assert!(harness.store.object_bytes(&hash).is_none());
    assert!(
        harness
            .downloads
            .arrived
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty(),
        "a view was told about bytes that were never published"
    );
    // The pass did not end: a refusal about one blob defers that blob.
    assert_eq!(pass.session_refusal, None);
}

/// **The test the plan's §10 exists for.** Typing and switching tabs perform
/// no network call and no full-Workspace walk.
#[tokio::test]
async fn typing_performs_no_network_call_and_no_walk() {
    let harness = Harness::new();
    let bytes = picture(4, 3_000);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    let document = koru_store_core::DocumentId::new();

    // The first durable commit that names this picture moves the set.
    assert!(
        harness
            .engine
            .document_changed(harness.workspace, document, &[hash.clone()]),
        "the first sighting of a hash is a change"
    );
    harness
        .engine
        .reconcile(harness.workspace, Trigger::DocumentsChanged)
        .await;
    let walks = harness.engine.walks();
    let calls = harness.calls.total();
    assert!(walks > 0 && calls > 0, "the first pass did nothing");

    // Now a hundred durable commits that changed only text, and a tab switch,
    // which is not a trigger at all.
    for _ in 0..100 {
        assert!(
            !harness
                .engine
                .document_changed(harness.workspace, document, &[hash.clone()]),
            "an unchanged hash set was reported as a change"
        );
    }
    assert_eq!(harness.engine.walks(), walks, "typing walked the Workspace");
    assert_eq!(harness.calls.total(), calls, "typing made a network call");
    assert_eq!(
        harness.calls.walks.load(Ordering::Relaxed),
        walks,
        "the Store was asked for the referenced set while typing"
    );
}

/// A burst of durable commits that *did* change the set is one pass, not one
/// pass per commit.
#[tokio::test]
async fn a_burst_of_changes_is_one_pass() {
    let harness = Harness::new();
    let first = picture(5, 1_000);
    harness
        .store
        .hold(harness.workspace, &first, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash_of(&first), "image/png")]);

    harness
        .engine
        .reconcile(harness.workspace, Trigger::DocumentsChanged)
        .await;
    let walks = harness.engine.walks();
    for _ in 0..5 {
        let pass = harness
            .engine
            .reconcile(harness.workspace, Trigger::DocumentsChanged)
            .await;
        assert!(pass.skipped, "the debounce let a second pass through");
    }
    assert_eq!(harness.engine.walks(), walks);
    assert_eq!(harness.engine.status(harness.workspace).passes_skipped, 5);
}

/// A trigger that says something changed runs at once — the debounce is for
/// document commits and nothing else.
#[tokio::test]
async fn an_explicit_trigger_is_never_debounced() {
    let harness = Harness::new();
    harness
        .engine
        .reconcile(harness.workspace, Trigger::DocumentsChanged)
        .await;
    let walks = harness.engine.walks();
    for trigger in [
        Trigger::Enabled,
        Trigger::MappingChanged,
        Trigger::PolicyPermissive,
        Trigger::Recovery,
    ] {
        let pass = harness.engine.reconcile(harness.workspace, trigger).await;
        assert!(!pass.skipped);
    }
    assert_eq!(harness.engine.walks(), walks + 4);
}

/// **The allowlist, from the engine's side.** A control plane that returned a
/// byte URL this device does not hold sends no bytes, and stops the pass
/// loudly rather than trying the next blob against the same plane.
#[tokio::test]
async fn a_byte_url_outside_the_allowlist_stops_the_pass() {
    let harness = Harness::with_host("https://evil.example.com/");
    let bytes = picture(6, 2_048);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(pass.uploaded, 0);
    assert_eq!(pass.session_refusal, Some("byte_origin_refused"));
    assert_eq!(
        harness.calls.bytes.load(Ordering::Relaxed),
        0,
        "a byte request was made to a host this device does not hold"
    );
}

/// No account, no pass — and no walk wasted on one.
#[tokio::test]
async fn without_cloud_access_nothing_is_attempted() {
    let harness = Harness::new();
    harness.access.revoke();
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(pass.session_refusal, Some("cloud_access_unavailable"));
    assert_eq!(harness.engine.walks(), 0);
    assert_eq!(harness.calls.total(), 0);
    // The stamp is readable by whoever will install the token, and a pass
    // once access is back lifts it.
    assert_eq!(
        harness
            .engine
            .workspaces_pending("cloud_access_unavailable"),
        vec![harness.workspace]
    );
    harness.access.restore();
    harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert!(
        harness
            .engine
            .workspaces_pending("cloud_access_unavailable")
            .is_empty()
    );
}

/// Network policy that refuses is a statement about the session, and the
/// engine waits for the trigger that changes it rather than retrying.
#[tokio::test]
async fn a_refusing_policy_parks_the_session_until_it_becomes_permissive() {
    let harness = Harness::new();
    let bytes = picture(7, 1_500);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    harness.policy.0.store(false, Ordering::Relaxed);
    let refused = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(
        refused.session_refusal,
        Some(BlobCode::ModeRefusesTransport.as_str())
    );
    assert_eq!(harness.calls.total(), 0);

    harness.policy.0.store(true, Ordering::Relaxed);
    let allowed = harness
        .engine
        .reconcile(harness.workspace, Trigger::PolicyPermissive)
        .await;
    assert_eq!(allowed.uploaded, 1, "{allowed:?}");
}

/// **§13's lesson, pinned.** A pause because the connection is metered and a
/// refusal because the Workspace's mode forbids transfer must not arrive at a
/// person as the same sentence — "a silent pause is indistinguishable from
/// being refused" is what cost this project a debugging session.
///
/// So the two report **different stable codes**, and only the pause is a
/// pause: a metered hold leaves `last_failure_code` alone, because a person on
/// a train has had nothing go wrong.
#[tokio::test]
async fn a_metered_pause_is_not_the_same_code_as_a_refusal() {
    let harness = Harness::new();
    let bytes = picture(11, 2_048);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    harness.policy.holding(TransferHold::Metered);
    let paused = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(paused.session_refusal, Some("network_metered"));
    let status = harness.engine.status(harness.workspace);
    assert_eq!(status.pending_reason, Some("network_metered"));
    assert_eq!(
        status.last_failure_code, None,
        "a pause is not a failure and must not be filed as one",
    );

    harness.policy.holding(TransferHold::TransportMode);
    let refused = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(
        refused.session_refusal,
        Some(BlobCode::ModeRefusesTransport.as_str())
    );
    assert_ne!(
        refused.session_refusal, paused.session_refusal,
        "a metered pause and a mode refusal are two different answers",
    );
    let status = harness.engine.status(harness.workspace);
    assert_eq!(
        status.last_failure_code,
        Some(BlobCode::ModeRefusesTransport.as_str()),
        "a refusal is a failure and is filed as one",
    );
    // Neither of them sent a byte.
    assert_eq!(harness.calls.total(), 0);
}

/// **D50, both halves.** On a metered connection an explicit request for one
/// picture goes through — and carries *only* that picture, leaving the rest
/// paused with the hold's own code. The Workspace is still paused afterwards,
/// which is what the status has to keep saying.
#[tokio::test]
async fn a_person_may_ask_for_one_picture_on_a_metered_connection() {
    let harness = Harness::new();
    let wanted = picture(12, 3_000);
    let other = picture(13, 3_100);
    let (wanted_hash, other_hash) = (hash_of(&wanted), hash_of(&other));
    harness
        .plane
        .publish(harness.workspace, &wanted, "image/png");
    harness
        .plane
        .publish(harness.workspace, &other, "image/png");
    harness.store.document(
        harness.workspace,
        &[(&wanted_hash, "image/png"), (&other_hash, "image/png")],
    );

    harness.policy.holding(TransferHold::Metered);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Requested(wanted_hash.clone()))
        .await;
    assert_eq!(pass.session_refusal, None, "{pass:?}");
    assert_eq!(pass.downloaded, 1, "{pass:?}");
    assert!(harness.store.associated(harness.workspace, &wanted_hash));
    assert!(
        !harness.store.associated(harness.workspace, &other_hash),
        "nothing nobody asked for moves on a metered link",
    );
    assert_eq!(
        pass.parked,
        vec![(other_hash, "network_metered")],
        "and what stayed behind says why",
    );
    assert_eq!(
        harness.engine.status(harness.workspace).pending_reason,
        Some("network_metered"),
        "one picture fetched does not un-pause the Workspace",
    );
}

/// The override is D50's and **only** D50's. A Local-only Workspace moves no
/// bytes in either direction (D245), and asking for one picture is not a
/// change to what the Workspace is.
#[tokio::test]
async fn a_request_does_not_override_the_transport_mode() {
    let harness = Harness::new();
    let bytes = picture(14, 900);
    let hash = hash_of(&bytes);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    harness.policy.holding(TransferHold::TransportMode);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Requested(hash.clone()))
        .await;
    assert_eq!(
        pass.session_refusal,
        Some(BlobCode::ModeRefusesTransport.as_str())
    );
    assert_eq!(harness.calls.total(), 0);
    assert!(!harness.store.associated(harness.workspace, &hash));
}

/// Offline is not something a person can ask their way out of either: there is
/// no network to make the request on.
#[tokio::test]
async fn a_request_does_not_override_being_offline() {
    let harness = Harness::new();
    harness.policy.holding(TransferHold::Offline);
    let pass = harness
        .engine
        .reconcile(
            harness.workspace,
            Trigger::Requested(hash_of(&picture(15, 10))),
        )
        .await;
    assert_eq!(pass.session_refusal, Some("network_offline"));
}

/// A blob nobody can upload defers **its own unit**; the picture beside it
/// still reaches Cloud. This is the §13 rule, extended to bytes rather than
/// given a second policy.
#[tokio::test]
async fn one_broken_blob_does_not_stop_its_neighbours() {
    let harness = Harness::new();
    let good = picture(8, 1_200);
    let bad = picture(9, 1_300);
    let (good_hash, bad_hash) = (hash_of(&good), hash_of(&bad));
    harness
        .store
        .hold(harness.workspace, &good, "image/png", None);
    harness
        .store
        .hold(harness.workspace, &bad, "image/png", None);
    harness.store.rot(&bad_hash);
    harness.store.document(
        harness.workspace,
        &[(&good_hash, "image/png"), (&bad_hash, "image/png")],
    );

    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(pass.uploaded, 1, "{pass:?}");
    assert_eq!(pass.session_refusal, None);
    assert_eq!(
        pass.parked,
        vec![(bad_hash.clone(), "blob_checksum_mismatch")]
    );

    // And it stays parked across a document change, because nothing about a
    // commit makes rotted bytes hash again.
    let next = harness
        .engine
        .reconcile(harness.workspace, Trigger::DocumentsChanged)
        .await;
    assert!(next.skipped || next.parked.len() == 1);
}

/// An explicit request for a visible missing blob unparks it and puts it
/// first — which is what somebody staring at a placeholder is asking for.
#[tokio::test]
async fn a_requested_blob_is_unparked_and_goes_first() {
    let harness = Harness::new();
    let wanted = picture(10, 900);
    let other = picture(11, 900);
    let (wanted_hash, other_hash) = (hash_of(&wanted), hash_of(&other));
    harness
        .plane
        .publish(harness.workspace, &wanted, "image/png");
    harness
        .plane
        .publish(harness.workspace, &other, "image/png");
    harness.store.document(
        harness.workspace,
        &[(&wanted_hash, "image/png"), (&other_hash, "image/png")],
    );

    // The read capability is refused once, which parks whichever blob the
    // ordinary order reached first. A refusal about the path rather than the
    // object: a blob the Cloud does not hold yet keeps its wait through a
    // request (`a_missing_object_is_not_asked_for_again_on_every_request`).
    harness
        .plane
        .refuse_once("read_capability", BlobCode::RelayPathRefused);
    // Both are on screen, or neither would be fetched at all (D260).
    harness.downloads.watch(&wanted_hash);
    harness.downloads.watch(&other_hash);
    let first = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(first.parked.len(), 1, "{first:?}");
    let parked = first.parked[0].0.clone();

    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Requested(parked.clone()))
        .await;
    assert!(pass.parked.is_empty(), "{pass:?}");
    assert!(harness.store.associated(harness.workspace, &parked));
}

/// The status surface answers §15's questions, in counts and codes only.
#[tokio::test]
async fn the_status_surface_says_what_the_pass_did() {
    let harness = Harness::new();
    let bytes = picture(12, 6_000);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", Some("shot.png"));
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    let status = harness.engine.status(harness.workspace);
    assert_eq!(status.workspace, Some(harness.workspace));
    assert_eq!(status.referenced, 1);
    assert_eq!(status.present, 1);
    assert_eq!(status.durable, 1);
    assert_eq!(status.missing, 0);
    assert_eq!(status.unreadable_references, 0);
    assert_eq!(status.bytes_complete, bytes.len() as u64);
    assert_eq!(status.direction, Some(crate::status::Direction::Upload));
    assert_eq!(status.path, Some(crate::status::TransferPath::Cloud));
    // **Which service answered.** "The upload is stuck" has two causes now.
    assert_eq!(status.object_host.as_deref(), Some("data.koru.do"));
    assert_eq!(status.pending_reason, None);
    assert!(status.walks > 0);
}

/// Startup recovery sweeps staging against the referenced set, removes only
/// what is abandoned, and interprets nothing it does not recognise.
#[tokio::test]
async fn recovery_sweeps_staging_against_the_referenced_set() {
    let harness = Harness::new();
    let wanted = picture(13, 800);
    let hash = hash_of(&wanted);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    let staging = crate::staging::StagingArea::new(harness.store.staging_root());
    staging.ensure().unwrap();
    staging.append(&hash, 0, &wanted[..100]).unwrap();
    let stale = hash_of(b"no document references this");
    staging.append(&stale, 0, b"partial").unwrap();
    // Abandoned: nothing has written it for longer than the window.
    std::fs::File::options()
        .write(true)
        .open(staging.part_path(&stale))
        .unwrap()
        .set_modified(std::time::SystemTime::now() - crate::staging::ABANDONED_AFTER * 2)
        .unwrap();
    let malformed = format!("b3-not-a-digest{}", crate::staging::DOWNLOAD_SUFFIX);
    std::fs::write(staging.staging_dir().join(&malformed), b"?").unwrap();
    std::fs::write(staging.staging_dir().join("mystery.bin"), b"?").unwrap();

    harness.engine.recover(harness.workspace).await.unwrap();
    assert_eq!(
        staging.synced_length(&hash).unwrap(),
        100,
        "a resume was lost"
    );
    assert_eq!(staging.synced_length(&stale).unwrap(), 0);
    assert!(staging.quarantine_dir().join(&malformed).exists());
    assert!(
        staging.staging_dir().join("mystery.bin").exists(),
        "a name the engine did not write was moved"
    );
}

/// A resume asks for exactly what it does not have — **`resends: 0`**.
#[tokio::test]
async fn a_download_resumes_from_the_synced_length() {
    let harness = Harness::new();
    let bytes = picture(14, 5_000);
    let hash = hash_of(&bytes);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    // A previous process got half of it and died.
    let staging = crate::staging::StagingArea::new(harness.store.staging_root());
    staging.ensure().unwrap();
    staging.append(&hash, 0, &bytes[..2_000]).unwrap();

    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Recovery)
        .await;
    assert_eq!(pass.downloaded, 1, "{pass:?}");
    assert_eq!(
        harness.store.object_bytes(&hash).as_deref(),
        Some(&bytes[..])
    );
    // Only the missing tail was asked for.
    assert_eq!(
        harness.engine.status(harness.workspace).bytes_complete,
        3_000
    );
}

/// **Two passes over one Workspace do not race for one staging file.** Both
/// would open it, both would append at what each believed the length was, and
/// the hole would be blamed on the network at the end.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_passes_over_one_workspace_are_serialised() {
    let harness = Harness::new();
    let bytes = picture(15, 6_000);
    let hash = hash_of(&bytes);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    // Eight callers at once, which is what a document of eight missing
    // pictures looks like from the window.
    let mut running = Vec::new();
    for _ in 0..8 {
        let engine = harness.engine.clone();
        let workspace = harness.workspace;
        let hash = hash.clone();
        running.push(tokio::spawn(async move {
            engine.reconcile(workspace, Trigger::Requested(hash)).await
        }));
    }
    let mut downloaded = 0;
    let mut settled = 0;
    for task in running {
        let pass = task.await.expect("a pass");
        assert!(pass.parked.is_empty(), "{pass:?}");
        assert_eq!(pass.session_refusal, None);
        downloaded += pass.downloaded;
        // Once it has arrived, this device *holds* it, so the pass that comes
        // next asks the upload question and is told Cloud already has it.
        settled += pass.already_durable + pass.already_local;
    }
    assert_eq!(downloaded, 1, "the picture was fetched more than once");
    assert_eq!(settled, 7, "the other callers should have found it settled");
    assert_eq!(
        harness.store.object_bytes(&hash).as_deref(),
        Some(&bytes[..])
    );
}

/// Switching tabs reaches no trigger at all: the engine has no call a tab
/// switch makes, and the one thing a subscription changes — whether a hash is
/// *watched* — is read during a pass and decides order, never whether.
#[tokio::test]
async fn switching_tabs_reaches_nothing() {
    let harness = Harness::new();
    let bytes = picture(16, 1_000);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    let walks = harness.engine.walks();
    let calls = harness.calls.total();

    // A tab opening and closing, twenty times over.
    for _ in 0..20 {
        harness
            .downloads
            .watching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(hash.as_str().to_owned());
        let _ = harness.engine.status(harness.workspace);
        harness
            .downloads
            .watching
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        let _ = harness.engine.status(harness.workspace);
    }
    assert_eq!(harness.engine.walks(), walks, "a tab switch walked");
    assert_eq!(
        harness.calls.total(),
        calls,
        "a tab switch reached a service"
    );
}

// ------------------ §6's fourth point: a Cloud allocate or download, per mode

use crate::fakes::SwitchableModes;

/// One Workspace holding a picture and referencing it, at whichever rung of the
/// ladder is armed.
fn at_mode(modes: std::sync::Arc<SwitchableModes>) -> (crate::fakes::Harness, BlobHash) {
    let harness = crate::fakes::Harness::at_mode(modes);
    let bytes = picture(11, 4_000);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    (harness, hash)
}

/// **Local-only: no allocate, no download, and no walk either.** The gate is in
/// front of the account read and the reference walk, so a closed Workspace
/// makes no network call and does not even read what it would have carried.
#[tokio::test]
async fn a_local_only_workspace_makes_no_cloud_request_at_all() {
    let (harness, _) = at_mode(SwitchableModes::local_only());

    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;

    assert_eq!(
        pass.session_refusal,
        Some(BlobCode::ModeRefusesTransport.as_str())
    );
    assert_eq!(pass.uploaded, 0);
    assert_eq!(harness.calls.total(), 0, "a packet left a closed Workspace");
    assert_eq!(harness.engine.walks(), 0, "a closed Workspace was walked");
}

/// **P2P: the peer path is open and Cloud is not.** The same row of §6's table
/// read from the other end — a Workspace that may talk to a paired device still
/// allocates nothing.
#[tokio::test]
async fn a_p2p_workspace_is_refused_the_cloud_path() {
    let (harness, _) = at_mode(SwitchableModes::p2p());

    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;

    assert_eq!(
        pass.session_refusal,
        Some(BlobCode::ModeRefusesTransport.as_str())
    );
    assert_eq!(harness.calls.total(), 0);
}

/// **Cloud: it moves.** The row that has to keep working, and the one the
/// initialization half exists to reach.
#[tokio::test]
async fn a_cloud_workspace_uploads_as_it_always_did() {
    let (harness, _) = at_mode(SwitchableModes::cloud());

    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;

    assert_eq!(pass.uploaded, 1, "{pass:?}");
    assert_eq!(pass.session_refusal, None);
}

/// **The gate is asked again inside each transfer.** A pass that asked once
/// would carry a large file to completion under a mode somebody turned off a
/// minute into it, which is the whole reason a restrictive choice closes
/// admission before its durable write.
#[tokio::test]
async fn the_gate_is_asked_again_inside_each_transfer() {
    let modes = SwitchableModes::cloud();
    let harness = crate::fakes::Harness::at_mode(modes.clone());
    for seed in 0..3u8 {
        let bytes = picture(seed, 1_200);
        let hash = hash_of(&bytes);
        harness
            .store
            .hold(harness.workspace, &bytes, "image/png", None);
        harness
            .store
            .document(harness.workspace, &[(&hash, "image/png")]);
    }

    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;

    assert_eq!(pass.uploaded, 3, "{pass:?}");
    // One for the session and one for each transfer: a number, not an intent.
    assert_eq!(modes.asked.load(Ordering::Relaxed), 4);
}

/// A Workspace closed between passes stops, and opened again resumes.
#[tokio::test]
async fn closing_the_mode_stops_the_next_pass_and_opening_it_resumes() {
    let modes = SwitchableModes::cloud();
    let (harness, _) = at_mode(modes.clone());
    assert_eq!(
        harness
            .engine
            .reconcile(harness.workspace, Trigger::Enabled)
            .await
            .uploaded,
        1
    );

    modes.close();
    let bytes = picture(42, 2_200);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    let refused = harness
        .engine
        .reconcile(harness.workspace, Trigger::MappingChanged)
        .await;
    assert_eq!(
        refused.session_refusal,
        Some(BlobCode::ModeRefusesTransport.as_str())
    );

    modes.open_cloud();
    let allowed = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(allowed.uploaded, 1, "{allowed:?}");
}

// ---------------------------------------- §8: Cloud mode tries the peer first

/// **Peer-first ordering.** Cloud holds the picture and so does a peer on a
/// direct path; the bytes come from the peer and the object plane is never
/// asked for a range.
#[tokio::test]
async fn cloud_mode_takes_the_bytes_from_the_peer_rather_than_the_object_plane() {
    let harness = crate::fakes::Harness::new();
    let bytes = picture(21, 6_000);
    let hash = hash_of(&bytes);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    harness.peers.hold(&bytes, "image/png");
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    harness.downloads.watch(&hash);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;

    assert_eq!(pass.downloaded, 1, "{pass:?}");
    assert_eq!(
        harness.store.object_bytes(&hash).as_deref(),
        Some(&bytes[..])
    );
    assert!(harness.store.associated(harness.workspace, &hash));
    assert_eq!(
        harness.calls.bytes.load(Ordering::Relaxed),
        0,
        "Cloud egress was paid for a picture a peer was holding"
    );
    assert_eq!(
        harness.engine.status(harness.workspace).path,
        Some(crate::status::TransferPath::PeerDirect)
    );
}

/// **A peer that dies mid-transfer falls through to Cloud without re-sending
/// what it already carried.** The staging file is keyed by hash, so the two
/// arms share one durable progress.
#[tokio::test]
async fn a_peer_that_stops_mid_transfer_is_resumed_by_cloud_from_where_it_got_to() {
    let harness = crate::fakes::Harness::new();
    let bytes = picture(22, 9_000);
    let hash = hash_of(&bytes);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    harness.peers.hold(&bytes, "image/png");
    // It manages the first 4 kB and then leaves the network.
    harness.peers.stops_after(4_096);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    harness.downloads.watch(&hash);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;

    assert_eq!(pass.downloaded, 1, "{pass:?}");
    assert_eq!(
        harness.store.object_bytes(&hash).as_deref(),
        Some(&bytes[..]),
        "the picture is whole whichever arm carried which half"
    );
    let from_peer = harness.peers.served.load(Ordering::Relaxed);
    let from_cloud = harness.plane.bytes_served.load(Ordering::Relaxed);
    assert_eq!(from_peer, 4_096, "{from_peer}");
    assert_eq!(
        from_peer + from_cloud,
        bytes.len() as u64,
        "{from_peer} from the peer and {from_cloud} from Cloud is not one copy of the file"
    );
}

/// No peer holding it is not a failure — it is the fall-through, and Cloud
/// carries the whole file as it did before §8 existed.
#[tokio::test]
async fn no_peer_holding_it_falls_straight_through_to_cloud() {
    let harness = crate::fakes::Harness::new();
    let bytes = picture(23, 3_000);
    let hash = hash_of(&bytes);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    harness.downloads.watch(&hash);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;

    assert_eq!(pass.downloaded, 1, "{pass:?}");
    assert_eq!(harness.peers.served.load(Ordering::Relaxed), 0);
    assert_eq!(
        harness.plane.bytes_served.load(Ordering::Relaxed),
        bytes.len() as u64
    );
}

/// A peer serving bytes that are not the ones at that address quarantines
/// them and lets Cloud start again — never a wrong picture, and never a resume
/// that appends to known-bad bytes.
#[tokio::test]
async fn a_peer_serving_the_wrong_bytes_quarantines_and_cloud_starts_again() {
    let harness = crate::fakes::Harness::new();
    let bytes = picture(24, 2_500);
    let hash = hash_of(&bytes);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    // The peer advertises the real hash and holds something else of the same
    // length. Only the digest can tell, which is the point.
    let mut lie = bytes.clone();
    lie[0] ^= 0xff;
    harness
        .peers
        .hold_as(&hash, &lie, "image/png", bytes.len() as u64);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    harness.downloads.watch(&hash);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;

    assert_eq!(pass.downloaded, 1, "{pass:?}");
    assert_eq!(
        harness.store.object_bytes(&hash).as_deref(),
        Some(&bytes[..]),
        "the bytes that landed are the ones at that address"
    );
}

/// A Workspace whose peer path is closed takes the Cloud arm even when a peer
/// is holding the bytes. The engine asks; it does not infer from the ladder.
#[tokio::test]
async fn a_closed_peer_path_skips_the_peer_even_when_one_holds_it() {
    let modes = SwitchableModes::cloud_only();
    let harness = crate::fakes::Harness::at_mode(modes);
    let bytes = picture(25, 2_000);
    let hash = hash_of(&bytes);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    harness.peers.hold(&bytes, "image/png");
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    harness.downloads.watch(&hash);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;

    assert_eq!(pass.downloaded, 1, "{pass:?}");
    assert_eq!(harness.peers.served.load(Ordering::Relaxed), 0);
    assert_eq!(
        harness.plane.bytes_served.load(Ordering::Relaxed),
        bytes.len() as u64
    );
}

// ----------------------------------------------------------------- coverage

/// The live authorization every coverage test needs: this Workspace, at this
/// incarnation, still admitted.
fn admitted(incarnation: CloudId) -> LiveAuthorization {
    Arc::new(move |_| Some(incarnation))
}

fn an_incarnation() -> CloudId {
    CloudId::from_bytes(*uuid::Uuid::new_v4().as_bytes())
}

/// The whole document half of a request: every document the sample holds,
/// covered at exactly the state it was sampled at.
fn covered(harness: &crate::fakes::Harness) -> crate::coverage::CoveredDocuments {
    harness
        .store
        .sampled(harness.workspace)
        .into_iter()
        .map(|document| (document.document, document.state))
        .collect()
}

/// **Every document synced and one picture missing is not current**, and the
/// two numbers say which of the two is the problem.
///
/// This is the whole of what B4 changes about what a person is told: before
/// it, the same Workspace reported its documents settled and said nothing at
/// all about the picture.
#[tokio::test]
async fn a_workspace_missing_one_picture_is_not_current() {
    let harness = crate::fakes::Harness::new();
    let here = picture(40, 4_000);
    let elsewhere = picture(41, 6_000);
    let held = hash_of(&here);
    let absent = hash_of(&elsewhere);
    // One picture this device holds and has uploaded; one that exists in a
    // document and nowhere else.
    harness
        .store
        .hold(harness.workspace, &here, "image/png", Some("one.png"));
    harness.plane.publish(harness.workspace, &here, "image/png");
    harness
        .store
        .document(harness.workspace, &[(&held, "image/png")]);
    harness
        .store
        .document(harness.workspace, &[(&absent, "image/png")]);

    let incarnation = an_incarnation();
    let report = harness
        .engine
        .coverage(
            CoverageRequest {
                workspace: harness.workspace,
                incarnation,
                documents: covered(&harness),
            },
            &admitted(incarnation),
        )
        .await
        .expect("coverage ran");

    assert_eq!(report.documents_required, 2);
    assert_eq!(report.documents_missing, 0, "a document was blamed");
    assert_eq!(report.blobs_referenced, 2);
    assert_eq!(report.blobs_durable, 1);
    assert_eq!(report.blobs_missing, 1);
    assert!(!report.is_current(), "{report:?}");
    // …and the status surface carries both numbers, not one merged one.
    let status = harness.engine.status(harness.workspace);
    let carried = status.coverage.clone().expect("coverage was committed");
    assert_eq!(carried.documents_missing, 0);
    assert_eq!(carried.blobs_missing, 1);
    assert!(!status.is_current());
}

/// The same Workspace, once its picture is durable.
#[tokio::test]
async fn a_workspace_whose_pictures_all_arrived_is_current() {
    let harness = crate::fakes::Harness::new();
    let bytes = picture(42, 3_000);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    // The ordinary route: a reconciliation pass puts it there.
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(pass.uploaded, 1, "{pass:?}");

    let incarnation = an_incarnation();
    let report = harness
        .engine
        .coverage(
            CoverageRequest {
                workspace: harness.workspace,
                incarnation,
                documents: covered(&harness),
            },
            &admitted(incarnation),
        )
        .await
        .expect("coverage ran");
    assert!(report.is_current(), "{report:?}");
    assert!(harness.engine.status(harness.workspace).is_current());
}

/// **A picture halfway up is not a picture that arrived.** Mid-upload, the
/// Workspace is not current and the uploading count says why.
#[tokio::test]
async fn a_picture_still_uploading_keeps_the_workspace_from_being_current() {
    let harness = crate::fakes::Harness::new();
    let bytes = picture(43, 8_000);
    let hash = hash_of(&bytes);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    harness
        .plane
        .uploading(harness.workspace, &hash, bytes.len() as u64, 2_000);

    let incarnation = an_incarnation();
    let report = harness
        .engine
        .coverage(
            CoverageRequest {
                workspace: harness.workspace,
                incarnation,
                documents: covered(&harness),
            },
            &admitted(incarnation),
        )
        .await
        .expect("coverage ran");
    assert_eq!(report.blobs_uploading, 1, "{report:?}");
    assert_eq!(report.blobs_missing, 0);
    assert!(!report.is_current());
}

/// **Coverage's fourth step, armed rather than assumed.**
///
/// The Workspace moves between the sample coverage was computed from and the
/// boundary read, and nothing is claimed. The race is armed in the fake's
/// sample queue: the first read is the durable set, the second is a document
/// at a state it did not have.
#[tokio::test]
async fn coverage_commits_only_when_the_sample_still_matches() {
    let harness = crate::fakes::Harness::new();
    let bytes = picture(44, 2_500);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    // Unarmed, this exact Workspace is complete — so the assertion below is
    // about the boundary and not about the Workspace.
    let incarnation = an_incarnation();
    let request = CoverageRequest {
        workspace: harness.workspace,
        incarnation,
        documents: covered(&harness),
    };
    let baseline = harness
        .engine
        .coverage(request.clone(), &admitted(incarnation))
        .await
        .expect("coverage ran");
    assert!(baseline.is_current(), "{baseline:?}");

    // Now arm it: the first read is the durable set, the second is a document
    // that moved on while coverage was asking Cloud about its picture.
    let moved: Vec<crate::coverage::SampledDocument> = harness
        .store
        .sampled(harness.workspace)
        .into_iter()
        .map(|document| crate::coverage::SampledDocument {
            state: Some(crate::fakes::state_of(77)),
            ..document
        })
        .collect();
    harness
        .store
        .arm_samples(harness.workspace, vec![None, Some(moved)]);

    let report = harness
        .engine
        .coverage(request, &admitted(incarnation))
        .await
        .expect("coverage ran");
    assert_eq!(report.state, crate::coverage::CoverageState::Superseded);
    assert!(!report.is_current(), "a moved Workspace committed coverage");
    assert!(
        !harness.engine.status(harness.workspace).is_current(),
        "the status kept claiming a coverage that had been superseded"
    );
}

/// The authorization moving supersedes it too, with the same sample.
#[tokio::test]
async fn coverage_commits_only_under_the_incarnation_it_sampled() {
    let harness = crate::fakes::Harness::new();
    let bytes = picture(45, 1_500);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    let incarnation = an_incarnation();
    let replaced = an_incarnation();
    let report = harness
        .engine
        .coverage(
            CoverageRequest {
                workspace: harness.workspace,
                incarnation,
                documents: covered(&harness),
            },
            // The Workspace was dropped and re-provisioned while coverage ran.
            &admitted(replaced),
        )
        .await
        .expect("coverage ran");
    assert_eq!(report.state, crate::coverage::CoverageState::Superseded);
}

/// **A Local-only Workspace has no Cloud coverage to compute**, and asking is
/// refused by the same gate a pass is — before the account, before the sample
/// and before any request.
#[tokio::test]
async fn coverage_asks_the_transport_mode_first() {
    let harness = crate::fakes::Harness::at_mode(SwitchableModes::local_only());
    let incarnation = an_incarnation();
    let walks = harness.engine.walks();
    let refusal = harness
        .engine
        .coverage(
            CoverageRequest {
                workspace: harness.workspace,
                incarnation,
                documents: crate::coverage::CoveredDocuments::new(),
            },
            &admitted(incarnation),
        )
        .await
        .expect_err("a Local-only Workspace answered a Cloud question");
    assert_eq!(refusal.code(), TransferHold::TransportMode.as_str());
    assert_eq!(
        harness.engine.walks(),
        walks,
        "a refused Workspace was walked anyway"
    );
    assert_eq!(harness.calls.total(), 0, "a refused Workspace was asked");
}

// ---------------------------------------- a person's pause and the pre-flight
/// D50's override, extended to the pause a person set themselves: they asked
/// for *this* picture, which is newer than the pause and narrower than it.
/// Everything they did not ask for stays held, with the pause's own code.
#[tokio::test]
async fn a_person_may_ask_for_one_picture_through_their_own_pause() {
    let harness = Harness::new();
    let wanted = picture(48, 1_000);
    let wanted_hash = hash_of(&wanted);
    let other = picture(49, 1_000);
    let other_hash = hash_of(&other);
    harness
        .plane
        .publish(harness.workspace, &wanted, "image/png");
    harness
        .plane
        .publish(harness.workspace, &other, "image/png");
    harness.store.document(
        harness.workspace,
        &[(&wanted_hash, "image/png"), (&other_hash, "image/png")],
    );

    harness.engine.pause(harness.workspace);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Requested(wanted_hash.clone()))
        .await;

    assert_eq!(pass.downloaded, 1, "{pass:?}");
    assert!(harness.store.associated(harness.workspace, &wanted_hash));
    assert!(!harness.store.associated(harness.workspace, &other_hash));
    assert_eq!(
        pass.parked,
        vec![(other_hash, "paused_by_person")],
        "what nobody asked for stays held, and says why",
    );
    assert_eq!(
        harness.engine.status(harness.workspace).pending_reason,
        Some("paused_by_person"),
        "one picture through the pause does not lift it",
    );
}

/// **D256's second answer.** A person's pause holds the blobs and says so in
/// a word of its own — never `network_metered`, which would send somebody
/// looking for better signal, and never `mode_refuses_transport`, which would
/// tell them the Workspace is Local-only when it is not.
///
/// **Documents keep moving**, and that is a fact about the shape of the code
/// rather than a promise: the pause lives in this engine, and the one question
/// that could stop a document — the Store's transport admission — is asked and
/// still answers yes. Nothing on the document path can see this flag.
#[tokio::test]
async fn a_persons_pause_holds_blobs_and_leaves_document_sync_alone() {
    let harness = Harness::new();
    let bytes = picture(46, 4_096);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    assert!(harness.engine.pause(harness.workspace));
    assert!(
        !harness.engine.pause(harness.workspace),
        "pausing twice is not news",
    );
    assert!(harness.engine.paused(harness.workspace));
    // The reason is readable the moment it is pressed, not at the next pass.
    assert_eq!(
        harness.engine.status(harness.workspace).pending_reason,
        Some("paused_by_person"),
    );

    let paused = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(paused.uploaded, 0, "{paused:?}");
    assert_eq!(paused.session_refusal, Some("paused_by_person"));
    assert_eq!(paused.session_hold, Some(TransferHold::Paused));
    assert_eq!(harness.calls.total(), 0, "no byte and no control call");

    let status = harness.engine.status(harness.workspace);
    assert!(status.paused);
    assert_eq!(
        status.last_failure_code, None,
        "a pause a person chose is not something that went wrong",
    );
    // The transport mode — the only thing that could stop a document — was
    // asked, and answered yes. The pause never reached it.
    assert!(harness.modes.asked.load(Ordering::Relaxed) > 0);

    // Resuming does not start anything by itself; the next trigger does.
    assert!(harness.engine.resume(harness.workspace));
    assert!(!harness.engine.paused(harness.workspace));
    assert_eq!(harness.calls.total(), 0);
    assert_eq!(
        harness.engine.status(harness.workspace).pending_reason,
        None,
        "lifting the pause clears the reason it put there",
    );

    let resumed = harness
        .engine
        .reconcile(harness.workspace, Trigger::PolicyPermissive)
        .await;
    assert_eq!(resumed.uploaded, 1, "{resumed:?}");
}

/// A person's pause and a metered pause are two different sentences, and the
/// person's own outranks the network's: it is the one that will still be
/// there when the train arrives, and the only one they can clear.
#[tokio::test]
async fn a_persons_pause_is_not_the_networks_pause() {
    let harness = Harness::new();
    let bytes = picture(47, 512);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    harness.policy.holding(TransferHold::Metered);
    harness.engine.pause(harness.workspace);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(pass.session_refusal, Some("paused_by_person"));
    assert_ne!(
        TransferHold::Paused.as_str(),
        TransferHold::Metered.as_str(),
    );
    assert!(TransferHold::Paused.chosen_by_person());
    assert!(!TransferHold::Metered.chosen_by_person());

    // Lifting the pause hands the line back to the network, which is still
    // holding it — so nothing claims to be moving that is not.
    harness.engine.resume(harness.workspace);
    let still = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(still.session_refusal, Some("network_metered"));
}

/// An ordinary pass does **not** pay for a summary nobody asked for, and does
/// not overwrite the one the enable published.
#[tokio::test]
async fn an_ordinary_pass_keeps_the_enables_summary_and_takes_no_new_one() {
    let harness = Harness::new();
    let bytes = picture(45, 900);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    let first = harness.engine.status(harness.workspace).preflight.unwrap();

    harness
        .engine
        .reconcile(harness.workspace, Trigger::MappingChanged)
        .await;
    let after = harness.engine.status(harness.workspace).preflight.unwrap();
    assert_eq!(first, after, "a later pass is not a second enable");
}

/// **D256's first answer, pinned to the thing that makes it cheap.**
///
/// Enabling Cloud walks the Workspace once and says what is about to move.
/// The number it says comes from *that* walk — `walks` is one after the pass,
/// and the fake Store counted one too, so nothing here is a second sweep of
/// somebody's Workspace wearing a friendlier name.
#[tokio::test]
async fn the_preflight_summary_comes_from_reconciliations_own_walk() {
    let harness = Harness::new();
    let mine = picture(41, 3_000);
    let mine_hash = hash_of(&mine);
    let theirs = picture(42, 5_500);
    let theirs_hash = hash_of(&theirs);
    // One this device holds and will upload; one only Cloud holds and it will
    // fetch. The summary separates them, because a person reading
    // "2 files, 8.5 kB" while half of it is arriving has been told a number
    // about somebody else's upload.
    harness
        .store
        .hold(harness.workspace, &mine, "image/png", Some("mine.png"));
    harness
        .plane
        .publish(harness.workspace, &theirs, "image/png");
    harness.store.document(
        harness.workspace,
        &[(&mine_hash, "image/png"), (&theirs_hash, "image/png")],
    );

    harness.downloads.watch(&theirs_hash);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(pass.uploaded, 1, "{pass:?}");
    assert_eq!(pass.downloaded, 1, "{pass:?}");

    let preflight = harness
        .engine
        .status(harness.workspace)
        .preflight
        .expect("enabling Cloud says what is about to happen");
    assert_eq!(preflight.files, 2);
    assert_eq!(preflight.upload_files, 1);
    assert_eq!(preflight.upload_bytes, mine.len() as u64);
    assert_eq!(preflight.download_files, 1);
    assert_eq!(preflight.unreadable, 0);

    // **One walk.** Both counters, because one is the engine's own claim and
    // the other is what the Store was actually asked to do.
    assert_eq!(harness.engine.walks(), 1);
    assert_eq!(harness.calls.walks.load(Ordering::Relaxed), 1);
}

/// **Before the first upload, not after it.**
///
/// The first blob's allocation is refused in a way that ends the session, so
/// the pass never reaches the second blob at all. The summary still counts
/// both — which it could only do if it had been taken before anything moved.
#[tokio::test]
async fn the_preflight_summary_is_taken_before_the_first_byte_moves() {
    let harness = Harness::new();
    let first = picture(43, 1_200);
    let second = picture(44, 2_400);
    let first_hash = hash_of(&first);
    let second_hash = hash_of(&second);
    harness
        .store
        .hold(harness.workspace, &first, "image/png", None);
    harness
        .store
        .hold(harness.workspace, &second, "image/png", None);
    harness.store.document(
        harness.workspace,
        &[(&first_hash, "image/png"), (&second_hash, "image/png")],
    );
    harness.plane.refuse_once("allocate", BlobCode::WrongRegion);

    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(pass.uploaded, 0, "{pass:?}");
    assert_eq!(pass.session_refusal, Some(BlobCode::WrongRegion.as_str()));

    let preflight = harness
        .engine
        .status(harness.workspace)
        .preflight
        .expect("the summary survives a pass that refused");
    assert_eq!(preflight.files, 2);
    assert_eq!(preflight.upload_files, 2);
    assert_eq!(
        preflight.upload_bytes,
        (first.len() + second.len()) as u64,
        "both files were measured, including the one the pass never reached",
    );
}

// ------------------------------------------- one file's own facts (D259 §4)

/// **A file is a selectable thing with its own facts**, and the engine can
/// answer them for one address without a walk, a probe or a second
/// subsystem.
///
/// The Cloud half is what the last coverage pass found — the control plane
/// already answered it once per referenced blob and the counts were all that
/// was kept. Keeping the answers is the whole of what D259 §4 needed.
#[tokio::test]
async fn one_file_reports_where_its_bytes_are() {
    let harness = crate::fakes::Harness::new();
    let here = picture(70, 4_096);
    let elsewhere = picture(71, 8_192);
    let held = hash_of(&here);
    let absent = hash_of(&elsewhere);
    harness
        .store
        .hold(harness.workspace, &here, "application/zip", Some("a.zip"));
    harness
        .plane
        .publish(harness.workspace, &here, "application/zip");
    harness
        .store
        .document(harness.workspace, &[(&held, "application/zip")]);
    harness
        .store
        .document(harness.workspace, &[(&absent, "image/png")]);

    // Before any coverage pass, nothing has been measured — and that is said
    // as *unmeasured* rather than as *missing*, because no claim has been
    // made about this address at all.
    let cold = harness.engine.file(harness.workspace, held.clone()).await;
    assert_eq!(cold.cloud, None, "nothing has looked at Cloud yet");
    assert!(cold.local.is_some(), "the local record is a local fact");

    let incarnation = an_incarnation();
    harness
        .engine
        .coverage(
            CoverageRequest {
                workspace: harness.workspace,
                incarnation,
                documents: covered(&harness),
            },
            &admitted(incarnation),
        )
        .await
        .expect("coverage ran");

    // **This device and Koru Cloud**, with the size and the media type the
    // local record carries — an `application/zip`, which is a file the
    // surface above used to have no word for.
    let mine = harness.engine.file(harness.workspace, held).await;
    let local = mine.local.expect("this device holds it");
    assert_eq!(local.size, 4_096);
    assert_eq!(local.mime, "application/zip");
    assert_eq!(local.filename.as_deref(), Some("a.zip"));
    assert_eq!(
        mine.cloud,
        Some(crate::coverage::BlobPresence::Durable { size: 4_096 })
    );

    // **And the other half of the sentence**: a file this device does not
    // hold and Cloud has never seen.
    let theirs = harness.engine.file(harness.workspace, absent).await;
    assert!(theirs.local.is_none(), "the bytes are not here");
    assert_eq!(theirs.cloud, Some(crate::coverage::BlobPresence::Missing));
}

/// **The hold travels with the file**, and a person's own pause outranks
/// whatever the newest pass stopped on — the same order every other surface
/// reads them in.
#[tokio::test]
async fn one_file_carries_the_reason_it_is_not_moving() {
    let harness = crate::fakes::Harness::new();
    let bytes = picture(72, 2_048);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    let moving = harness.engine.file(harness.workspace, hash.clone()).await;
    assert_eq!(moving.hold, None, "nothing is holding it");

    harness.engine.pause(harness.workspace);
    let paused = harness.engine.file(harness.workspace, hash).await;
    assert_eq!(paused.hold, Some(TransferHold::Paused.as_str()));
}

/// An address that is no longer referenced leaves the projection with the
/// walk that stopped returning it. A row for a picture nobody references any
/// more is a sentence about a file that is gone.
#[tokio::test]
async fn a_reference_that_goes_away_takes_its_cloud_answer_with_it() {
    let harness = crate::fakes::Harness::new();
    let bytes = picture(73, 1_024);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    let document = harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    let incarnation = an_incarnation();
    let request = |harness: &crate::fakes::Harness| CoverageRequest {
        workspace: harness.workspace,
        incarnation,
        documents: covered(harness),
    };
    harness
        .engine
        .coverage(request(&harness), &admitted(incarnation))
        .await
        .expect("coverage ran");
    assert!(
        harness
            .engine
            .cloud_presence(harness.workspace, &hash)
            .is_some()
    );

    // The document stops naming it.
    harness
        .store
        .document_at(harness.workspace, document, &crate::fakes::state_of(2), &[]);
    harness
        .engine
        .coverage(request(&harness), &admitted(incarnation))
        .await
        .expect("coverage ran");
    assert_eq!(
        harness.engine.cloud_presence(harness.workspace, &hash),
        None,
        "a stale answer would describe a file nothing references",
    );
}

/// **Explaining a file is a lookup, not a pass.** Clicking through a folder of
/// files must cost no walk and no request to a region: a selection is a
/// pointer move, and a pane that probed Cloud per click would be an afternoon
/// of egress on a person scrolling a list.
#[tokio::test]
async fn asking_about_one_file_walks_nothing_and_calls_nobody() {
    let harness = crate::fakes::Harness::new();
    let bytes = picture(74, 3_333);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    let incarnation = an_incarnation();
    harness
        .engine
        .coverage(
            CoverageRequest {
                workspace: harness.workspace,
                incarnation,
                documents: covered(&harness),
            },
            &admitted(incarnation),
        )
        .await
        .expect("coverage ran");

    let walks = harness.engine.walks();
    let calls = harness.calls.total();
    // Forty selections, which is a person going down a list.
    for _ in 0..40 {
        let facts = harness.engine.file(harness.workspace, hash.clone()).await;
        assert!(facts.local.is_some());
    }
    assert_eq!(harness.engine.walks(), walks, "a selection walked");
    assert_eq!(
        harness.calls.total(),
        calls,
        "a selection reached a service"
    );
}

/// **A blob Cloud has never held is a wait, not a failure.**
///
/// Seen on hardware: a Workspace referenced six blobs, Cloud held two, and the
/// three whose only holder was a shut laptop reported `object_missing` — the
/// correct answer, filed under `last_failure_code` and read by a person as
/// damage to their pictures. The code still travels, on `pending_reason`,
/// because what is owed is the reason the wait is happening. This is the same
/// rule `a_metered_pause_is_not_the_same_code_as_a_refusal` pins for a pause,
/// applied to the one unit code that means the same thing.
#[tokio::test]
async fn a_blob_cloud_does_not_hold_yet_is_a_wait_rather_than_a_failure() {
    let harness = Harness::new();
    let bytes = picture(13, 900);
    let hash = hash_of(&bytes);
    // Referenced by the document, present nowhere this device can reach: no
    // local bytes to upload, and no published object to download.
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    harness
        .plane
        .refuse_once("read_capability", BlobCode::ObjectMissing);
    // On screen, so the pass tries at all (D260).
    harness.downloads.watch(&hash);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(pass.parked.len(), 1, "{pass:?}");
    assert_eq!(pass.parked[0].1, BlobCode::ObjectMissing.as_str());

    let status = harness.engine.status(harness.workspace);
    assert_eq!(
        status.pending_reason,
        Some(BlobCode::ObjectMissing.as_str()),
        "the reason for the wait is still owed to whoever is looking",
    );
    assert_eq!(
        status.last_failure_code, None,
        "bytes nobody has uploaded yet are not a failure of this device",
    );
    assert_eq!(status.missing, 1, "it is still counted as not here");
}

/// **A blob the Cloud does not hold is asked for again on a schedule, not
/// on every request for it.** Seen on the server: one workspace asking for
/// one object every seven seconds, because each repaint of the picture was
/// a request, and a request reopened the park. Looking at the picture does
/// not bring the other device online. The wait stands until it runs out;
/// enablement still clears it, because that changes the answer.
#[tokio::test]
async fn a_missing_object_is_not_asked_for_again_on_every_request() {
    let harness = Harness::new();
    let bytes = picture(14, 900);
    let hash = hash_of(&bytes);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    harness.downloads.watch(&hash);

    harness
        .plane
        .refuse_once("read_capability", BlobCode::ObjectMissing);
    harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    let asked_once = harness.calls.total();
    assert!(asked_once > 0);

    // The picture is looked at again, and again. The Cloud is not asked.
    for _ in 0..3 {
        harness
            .plane
            .refuse_once("read_capability", BlobCode::ObjectMissing);
        let pass = harness
            .engine
            .reconcile(harness.workspace, Trigger::Requested(hash.clone()))
            .await;
        assert_eq!(harness.calls.total(), asked_once, "{pass:?}");
    }
    assert_eq!(
        harness.engine.status(harness.workspace).pending_reason,
        Some(BlobCode::ObjectMissing.as_str()),
        "the reason for the wait is still owed",
    );

    // Enablement is a change of answer, and it asks again.
    harness
        .plane
        .refuse_once("read_capability", BlobCode::ObjectMissing);
    harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert!(harness.calls.total() > asked_once);
}

// ------------------------------------------------- clearing local files

/// **A copy goes only when Cloud has it**, and the reference stays.
///
/// The picture is uploaded first, the way a real one is, so the control
/// plane answers `durable` for it from its own records rather than from
/// anything this test arranged; then it is released, and the fake's
/// association row is still there while its object is not.
#[tokio::test]
async fn a_durable_copy_is_released_and_its_reference_kept() {
    let harness = Harness::new();
    let bytes = picture(7, 5_000);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", Some("shot.png"));
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(pass.uploaded, 1, "{pass:?}");

    assert_eq!(harness.engine.status(harness.workspace).present, 1);
    let report = harness
        .engine
        .release_local(harness.workspace)
        .await
        .expect("a report");
    assert_eq!(report.released, 1, "{report:?}");
    assert_eq!(report.bytes_freed, 5_000);
    assert_eq!(report.kept, 0);
    assert_eq!(report.failure_code, None);
    // The count moved with the bytes, without waiting for a pass.
    assert_eq!(harness.engine.status(harness.workspace).present, 0);
    assert!(
        harness.store.associated(harness.workspace, &hash),
        "the document still references the picture"
    );
    assert_eq!(
        harness.store.object_bytes(&hash),
        None,
        "the bytes are no longer on this device"
    );
}

/// **The only copy is never deleted.** Cloud has not confirmed this picture
/// — nothing has uploaded it — so it is counted as kept and untouched, which
/// is the whole rule and the bug this feature must not have.
#[tokio::test]
async fn a_copy_cloud_does_not_hold_is_kept() {
    let harness = Harness::new();
    let bytes = picture(8, 4_000);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    let report = harness
        .engine
        .release_local(harness.workspace)
        .await
        .expect("a report");
    assert_eq!(report.released, 0, "{report:?}");
    assert_eq!(report.kept, 1);
    assert_eq!(report.bytes_freed, 0);
    assert_eq!(harness.store.object_bytes(&hash), Some(bytes));
}

/// A durable answer at the **wrong size** is not the same bytes, and is
/// treated as Cloud not holding these.
#[tokio::test]
async fn a_durable_answer_at_another_size_is_kept() {
    let harness = Harness::new();
    let bytes = picture(9, 6_000);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    // Cloud claims the address at a size this device does not hold.
    harness
        .plane
        .publish(harness.workspace, &picture(9, 6_001), "image/png");

    let report = harness
        .engine
        .release_local(harness.workspace)
        .await
        .expect("a report");
    assert_eq!(report.released, 0, "{report:?}");
    assert_eq!(report.kept, 1);
    assert!(harness.store.object_bytes(&hash).is_some());
}

/// A picture this device never held is nothing to free, and a Workspace with
/// one durable copy and one such reference releases exactly one.
#[tokio::test]
async fn what_is_not_held_here_is_counted_and_nothing_else_moves() {
    let harness = Harness::new();
    let held = picture(10, 3_000);
    let held_hash = hash_of(&held);
    let elsewhere = picture(11, 3_500);
    let elsewhere_hash = hash_of(&elsewhere);
    harness
        .store
        .hold(harness.workspace, &held, "image/png", None);
    harness
        .plane
        .publish(harness.workspace, &elsewhere, "image/png");
    harness.store.document(
        harness.workspace,
        &[(&held_hash, "image/png"), (&elsewhere_hash, "image/png")],
    );
    // The pass uploads the held one and fetches the other, so both are then
    // durable and both are here; release should take both.
    harness.downloads.watch(&elsewhere_hash);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!((pass.uploaded, pass.downloaded), (1, 1), "{pass:?}");
    let report = harness
        .engine
        .release_local(harness.workspace)
        .await
        .expect("a report");
    assert_eq!(report.released, 2, "{report:?}");
    assert_eq!(report.not_held, 0);

    // And asked again, there is nothing left to free — a retry after a
    // partial sweep must not fail on the work already done.
    let again = harness
        .engine
        .release_local(harness.workspace)
        .await
        .expect("a report");
    assert_eq!(again.released, 0, "{again:?}");
    assert_eq!(again.not_held, 2);
    assert_eq!(again.referenced, 2);
}

/// A Local-only Workspace has no Cloud to have copies on, so nothing may go:
/// the gate every pass asks is asked here too.
#[tokio::test]
async fn a_local_only_workspace_releases_nothing() {
    let harness = Harness::new();
    let bytes = picture(12, 2_000);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    harness.modes.close();
    let refused = harness.engine.release_local(harness.workspace).await;
    assert!(refused.is_err(), "{refused:?}");
    assert!(harness.store.object_bytes(&hash).is_some());
}

// ------------------------------------------------- on demand (D260)

/// **A file nobody asked for stays on Cloud.** Cloud holds it, this device
/// does not, no document showing it is open and nobody requested it: an
/// enable pass counts it as deferred and moves nothing. Then a request — a
/// placeholder, a file being opened — fetches exactly that one.
#[tokio::test]
async fn an_absent_file_nobody_asked_for_stays_on_cloud_until_it_is_asked_for() {
    let harness = Harness::new();
    let bytes = picture(60, 4_000);
    let hash = hash_of(&bytes);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);

    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(pass.downloaded, 0, "{pass:?}");
    assert_eq!(pass.deferred, 1);
    assert!(pass.parked.is_empty(), "deferred is not parked: {pass:?}");
    assert_eq!(harness.store.object_bytes(&hash), None);
    let status = harness.engine.status(harness.workspace);
    assert_eq!(status.missing, 0, "nothing is waiting on anything");
    assert_eq!(status.present, 0);

    harness.engine.request(harness.workspace, &hash);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Requested(hash.clone()))
        .await;
    assert_eq!(pass.downloaded, 1, "{pass:?}");
    assert_eq!(harness.store.object_bytes(&hash), Some(bytes));
}

/// A document on screen wants its pictures: `watching` is the second thing
/// that makes an absent file fetched, on the ordinary pass.
#[tokio::test]
async fn a_document_on_screen_fetches_its_pictures_on_the_ordinary_pass() {
    let harness = Harness::new();
    let bytes = picture(61, 4_000);
    let hash = hash_of(&bytes);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    harness.downloads.watch(&hash);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::DocumentsChanged)
        .await;
    assert_eq!(pass.downloaded, 1, "{pass:?}");
    assert_eq!(pass.deferred, 0);
}

// ------------------------------------------- which documents are waiting

/// **Grouped by document, with only the files Cloud does not hold.**
///
/// Three pictures nobody has uploaded, one Cloud holds, and one whose bytes
/// here rotted. A document naming two of the missing ones is one entry with
/// two files; a document sharing one of them is its own entry; a document
/// whose only picture is on Cloud, and one whose only picture failed for a
/// reason that is not *another device has it*, are not waiting on anybody.
#[tokio::test]
async fn waiting_documents_are_the_ones_naming_a_file_cloud_does_not_hold() {
    let harness = Harness::new();
    let (lost_a, lost_b, lost_c) = (picture(70, 500), picture(71, 500), picture(72, 500));
    let (a, b, c) = (hash_of(&lost_a), hash_of(&lost_b), hash_of(&lost_c));
    let held = picture(73, 500);
    let on_cloud = hash_of(&held);
    harness.plane.publish(harness.workspace, &held, "image/png");
    let rotten = picture(74, 500);
    let rotten_hash = hash_of(&rotten);
    harness
        .store
        .hold(harness.workspace, &rotten, "image/png", None);
    harness.store.rot(&rotten_hash);

    let two = harness.store.document(
        harness.workspace,
        &[
            (&a, "image/png"),
            (&b, "image/png"),
            (&on_cloud, "image/png"),
        ],
    );
    let shared = harness
        .store
        .document(harness.workspace, &[(&b, "image/png"), (&c, "image/png")]);
    harness
        .store
        .document(harness.workspace, &[(&on_cloud, "image/png")]);
    harness
        .store
        .document(harness.workspace, &[(&rotten_hash, "image/png")]);
    for hash in [&a, &b, &c, &on_cloud] {
        harness.downloads.watch(hash);
    }
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(pass.parked.len(), 4, "{pass:?}");

    let mut waiting = harness
        .engine
        .waiting(harness.workspace)
        .await
        .expect("an answer");
    waiting.sort_by_key(|entry| entry.blobs.len());
    let mut expected_two = vec![a.clone(), b.clone()];
    expected_two.sort_by(|x, y| x.as_str().cmp(y.as_str()));
    let mut expected_shared = vec![b.clone(), c.clone()];
    expected_shared.sort_by(|x, y| x.as_str().cmp(y.as_str()));
    assert_eq!(waiting.len(), 2, "{waiting:?}");
    let by_document = |document| {
        waiting
            .iter()
            .find(|entry| entry.document == document)
            .map(|entry| entry.blobs.clone())
    };
    assert_eq!(by_document(two), Some(expected_two));
    assert_eq!(by_document(shared), Some(expected_shared));
}

/// **Asking is not a trigger.** The query reads the parks and leaves them:
/// no network call, no park cleared, and the next request for the picture
/// still waits out the park rather than asking the region again.
#[tokio::test]
async fn asking_which_documents_wait_changes_nothing() {
    let harness = Harness::new();
    let bytes = picture(75, 700);
    let hash = hash_of(&bytes);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    harness.downloads.watch(&hash);
    harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    let before = harness.engine.status(harness.workspace);
    let asked = harness.calls.total();

    for _ in 0..3 {
        let waiting = harness
            .engine
            .waiting(harness.workspace)
            .await
            .expect("an answer");
        assert_eq!(waiting.len(), 1, "{waiting:?}");
        assert_eq!(waiting[0].blobs, vec![hash.clone()]);
    }
    assert_eq!(harness.calls.total(), asked, "the query reached the region");
    assert_eq!(harness.engine.status(harness.workspace), before);
    // The park stands: a request inside its wait asks nobody.
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Requested(hash.clone()))
        .await;
    assert_eq!(harness.calls.total(), asked, "{pass:?}");
}

/// A Workspace waiting on nobody answers without a walk.
#[tokio::test]
async fn a_workspace_waiting_on_nobody_is_answered_without_a_walk() {
    let harness = Harness::new();
    let bytes = picture(76, 700);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    let walks = harness.engine.walks();
    let waiting = harness
        .engine
        .waiting(harness.workspace)
        .await
        .expect("an answer");
    assert!(waiting.is_empty(), "{waiting:?}");
    assert_eq!(harness.engine.walks(), walks);
}

// ------------------------------------------------ one document's coverage

/// Coverage over everything the fake store holds, committed under a live
/// incarnation.
async fn run_coverage(harness: &crate::fakes::Harness) {
    let incarnation = an_incarnation();
    harness
        .engine
        .coverage(
            CoverageRequest {
                workspace: harness.workspace,
                incarnation,
                documents: covered(harness),
            },
            &admitted(incarnation),
        )
        .await
        .expect("coverage ran");
}

#[tokio::test]
async fn missing_files_are_listed_before_any_download_is_requested() {
    let harness = Harness::new();
    let bytes = picture(75, 700);
    let hash = hash_of(&bytes);
    let document = harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    // Unmeasured is unknown, not missing.
    assert!(
        harness
            .engine
            .waiting(harness.workspace)
            .await
            .unwrap()
            .is_empty()
    );
    run_coverage(&harness).await;
    let calls = harness.calls.total();
    let waiting = harness.engine.waiting(harness.workspace).await.unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].document, document);
    assert_eq!(waiting[0].blobs, vec![hash]);
    assert_eq!(
        harness.calls.total(),
        calls,
        "reading details must not request a download"
    );
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    run_coverage(&harness).await;
    assert!(
        harness
            .engine
            .waiting(harness.workspace)
            .await
            .unwrap()
            .is_empty()
    );
}

/// **Images and files counted apart, and only this document's.** Two pictures
/// (one durable, one uploading), a PDF Cloud holds and an archive it does
/// not — and a second document whose durable picture must not leak into the
/// first's counts.
#[tokio::test]
async fn a_documents_files_are_split_into_images_and_files_and_counted_by_durability() {
    let harness = Harness::new();
    let (photo, sketch, pdf, archive, elsewhere) = (
        picture(90, 900),
        picture(91, 900),
        picture(92, 900),
        picture(93, 900),
        picture(94, 900),
    );
    let (photo_hash, sketch_hash, pdf_hash, archive_hash, elsewhere_hash) = (
        hash_of(&photo),
        hash_of(&sketch),
        hash_of(&pdf),
        hash_of(&archive),
        hash_of(&elsewhere),
    );
    harness
        .plane
        .publish(harness.workspace, &photo, "image/png");
    harness
        .plane
        .uploading(harness.workspace, &sketch_hash, 900, 300);
    harness
        .plane
        .publish(harness.workspace, &pdf, "application/pdf");
    harness
        .plane
        .publish(harness.workspace, &elsewhere, "image/jpeg");
    let document = harness.store.document(
        harness.workspace,
        &[
            (&photo_hash, "IMAGE/PNG"),
            (&sketch_hash, "image/webp"),
            // Named twice: one file.
            (&photo_hash, "image/png"),
            (&pdf_hash, "application/pdf"),
            (&archive_hash, "application/zip"),
        ],
    );
    harness
        .store
        .document(harness.workspace, &[(&elsewhere_hash, "image/jpeg")]);

    run_coverage(&harness).await;
    let coverage = harness
        .engine
        .document_coverage(harness.workspace, document)
        .await
        .expect("an answer");
    assert_eq!(
        coverage.files,
        Some(DocumentFiles {
            images_total: 2,
            images_synced: 1,
            files_total: 2,
            files_synced: 1,
        })
    );
    assert_eq!(coverage.synced, Some(true), "{coverage:?}");
}

/// **Nothing measured is not "no".** Before any coverage computation the
/// document's state is unknown, while its references — a local fact — are
/// already counted, none of them synced.
#[tokio::test]
async fn before_any_coverage_a_document_is_unmeasured_rather_than_unsynced() {
    let harness = Harness::new();
    let bytes = picture(95, 800);
    let hash = hash_of(&bytes);
    harness
        .plane
        .publish(harness.workspace, &bytes, "image/png");
    let document = harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    let coverage = harness
        .engine
        .document_coverage(harness.workspace, document)
        .await
        .expect("an answer");
    assert_eq!(coverage.synced, None);
    assert_eq!(
        coverage.files,
        Some(DocumentFiles {
            images_total: 1,
            images_synced: 0,
            ..DocumentFiles::default()
        })
    );
}

/// **A document that moved after Cloud got it is not synced**, and one the
/// pass never carried is not either. A document with no files answers zero
/// counts, which is a fact rather than an unknown.
#[tokio::test]
async fn a_document_is_synced_only_at_the_state_cloud_holds() {
    let harness = Harness::new();
    let steady = harness.store.document(harness.workspace, &[]);
    let moving = harness.store.document(harness.workspace, &[]);
    let uncarried = harness.store.document(harness.workspace, &[]);
    let incarnation = an_incarnation();
    let mut documents = covered(&harness);
    documents.insert(uncarried, None);
    harness
        .engine
        .coverage(
            CoverageRequest {
                workspace: harness.workspace,
                incarnation,
                documents,
            },
            &admitted(incarnation),
        )
        .await
        .expect("coverage ran");
    // Somebody typed into one of them after the pass.
    harness
        .store
        .document_at(harness.workspace, moving, &crate::fakes::state_of(9), &[]);

    let ask = |document| {
        harness
            .engine
            .document_coverage(harness.workspace, document)
    };
    let steady = ask(steady).await.expect("an answer");
    assert_eq!(steady.synced, Some(true));
    assert_eq!(steady.files, Some(DocumentFiles::default()));
    assert_eq!(ask(moving).await.expect("an answer").synced, Some(false));
    assert_eq!(ask(uncarried).await.expect("an answer").synced, Some(false));
    // A document the read model does not know at all: neither half is known
    // to be anything, and its files are not "none".
    let unknown = ask(DocumentId::new()).await.expect("an answer");
    assert_eq!(unknown.synced, Some(false));
    assert_eq!(unknown.files, None);
}

/// **A replaced dataset says nothing about a document.** Coverage superseded
/// because the incarnation moved drops every per-document verdict.
#[tokio::test]
async fn a_replaced_incarnation_takes_the_documents_verdicts_with_it() {
    let harness = Harness::new();
    let document = harness.store.document(harness.workspace, &[]);
    run_coverage(&harness).await;
    let ask = || {
        harness
            .engine
            .document_coverage(harness.workspace, document)
    };
    assert_eq!(ask().await.expect("an answer").synced, Some(true));

    let incarnation = an_incarnation();
    harness
        .engine
        .coverage(
            CoverageRequest {
                workspace: harness.workspace,
                incarnation,
                documents: covered(&harness),
            },
            &admitted(an_incarnation()),
        )
        .await
        .expect("coverage ran");
    assert_eq!(ask().await.expect("an answer").synced, Some(false));
}

/// **Asking is not a trigger.** No request reaches the region, no park moves
/// and the status is exactly what it was.
#[tokio::test]
async fn asking_about_one_document_changes_nothing() {
    let harness = Harness::new();
    let bytes = picture(96, 700);
    let hash = hash_of(&bytes);
    let document = harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    harness.downloads.watch(&hash);
    harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    run_coverage(&harness).await;
    let before = harness.engine.status(harness.workspace);
    let asked = harness.calls.total();
    for _ in 0..3 {
        harness
            .engine
            .document_coverage(harness.workspace, document)
            .await
            .expect("an answer");
    }
    assert_eq!(harness.calls.total(), asked, "the query reached the region");
    assert_eq!(harness.engine.status(harness.workspace), before);
    let waiting = harness
        .engine
        .waiting(harness.workspace)
        .await
        .expect("an answer");
    assert_eq!(waiting.len(), 1, "the park moved: {waiting:?}");
}

/// **Free up space stays done.** Released copies are not fetched back by the
/// next pass — a document edit, a recovery, a policy flip — because nothing
/// asked for them. Opening one is what brings it back.
#[tokio::test]
async fn a_released_file_is_not_fetched_back_until_it_is_opened() {
    let harness = Harness::new();
    let bytes = picture(62, 6_000);
    let hash = hash_of(&bytes);
    harness
        .store
        .hold(harness.workspace, &bytes, "image/png", None);
    harness
        .store
        .document(harness.workspace, &[(&hash, "image/png")]);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Enabled)
        .await;
    assert_eq!(pass.uploaded, 1, "{pass:?}");
    let report = harness
        .engine
        .release_local(harness.workspace)
        .await
        .expect("a report");
    assert_eq!(report.released, 1, "{report:?}");

    // Every trigger but the debounced one, which would skip rather than
    // decide and prove nothing either way.
    for trigger in [
        Trigger::Recovery,
        Trigger::PolicyPermissive,
        Trigger::MappingChanged,
    ] {
        let pass = harness.engine.reconcile(harness.workspace, trigger).await;
        assert_eq!(pass.downloaded, 0, "{pass:?}");
        assert_eq!(pass.deferred, 1, "{pass:?}");
        assert_eq!(harness.store.object_bytes(&hash), None);
    }

    harness.engine.request(harness.workspace, &hash);
    let pass = harness
        .engine
        .reconcile(harness.workspace, Trigger::Requested(hash.clone()))
        .await;
    assert_eq!(pass.downloaded, 1, "{pass:?}");
    assert_eq!(harness.store.object_bytes(&hash), Some(bytes));
}
