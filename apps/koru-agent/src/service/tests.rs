// SPDX-License-Identifier: MPL-2.0

//! **The supervision decision, without a process.**
//!
//! [`super::ensure_voice_with`] takes the two effects -- asking, and launching
//! -- as closures precisely so this suite can drive the *decision* while
//! spawning nothing. That is not squeamishness: `ensure_voice` starts
//! `koru-voice-worker` beside this binary on whatever root `Paths` carries,
//! and a unit test that reached it would launch real infrastructure on a
//! made-up root and leave it holding an executable the next build needs to
//! overwrite.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::gateway_host::GatewayServiceError;

#[test]
fn inspection_control_keeps_a_reserved_cancel_lane_after_lost_start_ack() {
    use koru_store_wire::cloud_document::{CloudDocumentAction, CloudDocumentSelection};
    use koru_sync_control::inspection::{InspectionOwner, InspectionRequest};
    use koru_sync_control::{ControlRequest, ControlSessionId};
    let (commands, queue) = std::sync::mpsc::channel();
    let mut control = SyncControl::for_test(koru_sync_control::ControlStatus::default());
    control.commands = commands;
    let session = ControlSessionId::new();
    let token = uuid::Uuid::new_v4();
    let owner = InspectionOwner {
        owner_scope: 1,
        account_generation: 2,
        account_id: koru_device::cloud::CloudId::from_bytes(uuid::Uuid::new_v4().into_bytes()),
    };
    let selection = CloudDocumentSelection {
        account_id: owner.account_id.to_string(),
        workspace_id: uuid::Uuid::new_v4().to_string(),
        incarnation_id: uuid::Uuid::new_v4().to_string(),
        cloud_unit_id: uuid::Uuid::new_v4().to_string(),
        document_id: uuid::Uuid::new_v4().to_string(),
        source_head_digest: "ab".repeat(32),
    };
    let start = || ControlRequest::StartCloudInspection {
        control_session_id: session,
        owner,
        token,
        request: Box::new(InspectionRequest {
            selection: selection.clone(),
            action: CloudDocumentAction::Inspect,
            can_write: false,
        }),
    };
    drop(control.inspection_request_pending(start()).unwrap());
    assert!(
        control.inspection_request_pending(start()).is_err(),
        "caller timeout cannot release queued ownership"
    );
    let cancel = || ControlRequest::CancelCloudInspection {
        control_session_id: session,
        owner,
        token,
    };
    drop(control.inspection_request_pending(cancel()).unwrap());
    assert!(
        control.inspection_request_pending(cancel()).is_err(),
        "cancellation backlog is bounded too"
    );
    let first = queue.recv().unwrap();
    let second = queue.recv().unwrap();
    assert!(
        matches!(&first, PairingCommand::Cloud { request, .. } if matches!(request.as_ref(), ControlRequest::StartCloudInspection { token: got, .. } if *got == token))
    );
    assert!(
        matches!(&second, PairingCommand::Cloud { request, .. } if matches!(request.as_ref(), ControlRequest::CancelCloudInspection { token: got, .. } if *got == token))
    );
    assert!(queue.try_recv().is_err());
    drop((first, second));
    assert!(control.inspection_request_pending(start()).is_ok());
}

#[test]
fn every_voice_host_uses_the_store_service_root() {
    let paths = Paths::for_root(std::path::PathBuf::from("C:/isolated-koru"));
    let root = voice_service_root(&paths);

    assert_eq!(root, std::path::PathBuf::from("C:/isolated-koru/store"));
    assert_ne!(root, paths.explicit_root().unwrap());
}

fn ready(launch_id: Option<&str>) -> VoiceReady {
    VoiceReady {
        worker_version: "0.0.0-test".into(),
        process_id: 4242,
        supervisor_launch_id: launch_id.map(str::to_string),
        launched: false,
    }
}

/// The worker's endpoint belongs under Store, but its model cache does not.
/// Losing this child-only override made a successful rooted install invisible
/// at runtime and every press failed with `model_missing`.
#[test]
fn the_voice_child_carries_the_installers_model_root() {
    let service_root = std::path::Path::new("C:/isolated-koru/store");
    let model_root = std::path::Path::new("C:/isolated-koru/voice/models");
    let command = voice_command(
        std::path::Path::new("C:/bin/koru-voice-worker.exe"),
        service_root,
        model_root,
    );

    assert!(
        command
            .get_args()
            .any(|arg| arg == service_root.as_os_str())
    );
    assert!(command.get_envs().any(|(name, value)| {
        name == koru_env::name::KORU_VOICE_MODELS && value == Some(model_root.as_os_str())
    }));
    assert_eq!(
        command.get_program(),
        std::path::Path::new("C:/bin/koru-voice-worker.exe").as_os_str()
    );
}

/// **Connect first.** A worker that is already there is adopted, and nothing is
/// launched -- which is the ordinary case, because a worker outlives the
/// dictation that started it by five minutes of idle model and half an hour of
/// idle process.
#[test]
fn a_worker_that_is_already_running_is_adopted_rather_than_launched() {
    let launches = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&launches);
    let found = ensure_voice_with(
        None,
        || Ok(Some(ready(None))),
        move |_| {
            counted.fetch_add(1, Ordering::Release);
            Ok(())
        },
    )
    .expect("an already-running worker should be adopted");

    assert_eq!(launches.load(Ordering::Acquire), 0);
    assert!(!found.launched);
    assert_eq!(found.process_id, 4242);
}

/// **Two concurrent asks launch one process.**
///
/// Both threads reach the decision; the lock serialises them; the second one's
/// connect-first check finds what the first one started. Without the lock both
/// mint a launch id and spawn, the endpoint admits one, and the loser reports
/// `voice_incompatible` for a worker that is perfectly good.
#[test]
fn two_concurrent_ensure_calls_launch_one_worker() {
    // The fictional installation: `None` until something launches, then a
    // worker echoing the launch id it was given.
    let running: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
    let launches = Arc::new(AtomicU64::new(0));

    let ensure = || {
        let running = Arc::clone(&running);
        let launches = Arc::clone(&launches);
        move || {
            let asking = Arc::clone(&running);
            let launching = Arc::clone(&running);
            let counted = Arc::clone(&launches);
            ensure_voice_with(
                None,
                move || {
                    Ok(asking
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone()
                        .map(|id| ready(Some(&id))))
                },
                move |launch_id| {
                    counted.fetch_add(1, Ordering::Release);
                    *launching.lock().unwrap_or_else(|error| error.into_inner()) =
                        Some(launch_id.to_string());
                    Ok(())
                },
            )
        }
    };

    let first = std::thread::spawn(ensure());
    let second = std::thread::spawn(ensure());
    let one = first.join().expect("the first ensure panicked");
    let two = second.join().expect("the second ensure panicked");

    one.expect("the first ensure failed");
    two.expect("the second ensure failed");
    assert_eq!(
        launches.load(Ordering::Acquire),
        1,
        "two concurrent asks started two workers"
    );
}

/// **Verification, not decoration.** A worker on this endpoint that was started
/// for somebody else's launch is refused rather than adopted: this Agent does
/// not know what it is talking to, and saying so is the only honest answer.
#[test]
fn a_worker_from_another_launch_is_refused_rather_than_adopted() {
    let launched = Arc::new(std::sync::Mutex::new(false));
    let flag = Arc::clone(&launched);
    let refused = ensure_voice_with(
        None,
        move || {
            if *flag.lock().unwrap_or_else(|error| error.into_inner()) {
                // Somebody else's worker won the bind.
                Ok(Some(ready(Some("a-different-launch"))))
            } else {
                Ok(None)
            }
        },
        {
            let flag = Arc::clone(&launched);
            move |_| {
                *flag.lock().unwrap_or_else(|error| error.into_inner()) = true;
                Ok(())
            }
        },
    )
    .expect_err("a foreign launch should not be reported as a successful one");

    assert_eq!(refused.code(), "voice_incompatible");
}

/// A worker that never binds is a bounded wait and a stable code, not a hang.
#[test]
fn a_worker_that_never_binds_is_reported_rather_than_waited_on_forever() {
    let began = std::time::Instant::now();
    let failed = ensure_voice_with(None, || Ok(None), |_| Ok(()))
        .expect_err("a worker that never appeared should not be a success");

    assert_eq!(failed.code(), "voice_unreachable");
    assert!(
        began.elapsed() < VOICE_START_DEADLINE * 3,
        "the readiness poll ran long past its deadline"
    );
}

/// A binary that is not beside this one is `voice_not_found`, and it says so
/// before anything is spawned.
#[test]
fn a_launch_failure_keeps_its_own_code() {
    let failed = ensure_voice_with(
        None,
        || Ok(None),
        |_| {
            Err(VoiceServiceError::NotFound(
                "no worker beside this build".into(),
            ))
        },
    )
    .expect_err("a missing binary is not a success");
    assert_eq!(failed.code(), "voice_not_found");
}

/// **`KORU_VOICE_DISABLED` refuses before anything is dialled.** Q-V44 names
/// four surfaces that have to agree about the kill switch, and this is
/// supervision's: a switched-off installation starts no worker, and it says
/// *switched off* rather than *not answering* -- which is a different sentence
/// and a different remedy.
///
/// The flag is a parameter rather than the environment, deliberately: the
/// variable is process-global and this suite runs beside thirty others.
/// `koru_voice::availability`'s own tests own the reading of it.
#[test]
fn a_switched_off_installation_supervises_nothing() {
    let launches = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&launches);
    let refused = ensure_voice_with(
        Some(koru_voice::Off::Switch),
        || panic!("a switched-off installation must not even dial"),
        move |_| {
            counted.fetch_add(1, Ordering::Release);
            Ok(())
        },
    )
    .expect_err("a switched-off installation should not report a worker");

    assert_eq!(refused.code(), "voice_disabled");
    assert!(
        refused.to_string().contains(koru_voice::DISABLE_VAR),
        "the refusal should name the variable a person has to unset: {refused}"
    );
    assert_eq!(launches.load(Ordering::Acquire), 0);
}

/// The five codes are distinct from the gateway's, so a log line and a shell
/// branch can tell a dictation that could not start from a store that could
/// not. The gateway's vocabulary moved to `gateway_host` with its supervision
/// in Plan 4; this stays here, beside the codes it is protecting.
#[test]
fn the_voice_codes_are_not_the_gateways() {
    let voice = [
        VoiceServiceError::Disabled(koru_voice::Off::Switch, String::new()).code(),
        VoiceServiceError::NotFound(String::new()).code(),
        VoiceServiceError::Launch(String::new()).code(),
        VoiceServiceError::Unavailable(String::new()).code(),
        VoiceServiceError::Incompatible(String::new()).code(),
    ];
    let gateway = [
        GatewayServiceError::NotFound(String::new()).code(),
        GatewayServiceError::Launch(String::new()).code(),
        GatewayServiceError::Unavailable(String::new()).code(),
        GatewayServiceError::Incompatible(String::new()).code(),
    ];
    for code in voice {
        assert!(code.starts_with("voice_"), "{code} is not a voice code");
        assert!(!gateway.contains(&code), "{code} is spelled twice");
    }
}

/// **The worker fingerprints its own image with its own copy of the digest,
/// and this process fingerprints the same bytes with the bridge's.** A
/// bootstrap carrying a different spelling would be refused at every launch,
/// so both sides assert this same literal -- the BLAKE3 reference vector for
/// "abc" -- rather than each comparing a function with itself.
#[test]
fn the_launch_fingerprint_is_the_one_the_worker_computes() {
    assert_eq!(
        koru_network_bridge::fingerprint(b"abc"),
        "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
    );
}

/// **A platform with no voice runtime refuses the same way, and blames no
/// switch.** Nothing is dialled and nothing is launched; the code is the one a
/// shell already branches on, and the text does not tell a person to unset a
/// variable they never set.
#[test]
fn a_platform_without_voice_supervises_nothing() {
    let refused = ensure_voice_with(
        Some(koru_voice::Off::Platform),
        || panic!("a platform without voice must not even dial"),
        |_| panic!("a platform without voice must not launch a worker"),
    )
    .expect_err("a platform without voice should not report a worker");

    assert_eq!(refused.code(), "voice_disabled");
    assert!(
        !refused.to_string().contains(koru_voice::DISABLE_VAR),
        "a platform refusal must not name the switch: {refused}"
    );
}

// ----------------------------------------------------------------------
// M8, cohort `m8_own_device_bit`: the bit as the trust records give it.
//
// The snapshot's peers are built by the same function the running Agent
// calls, from an **in-memory trust repository** filled with signed records.
// No Store, no control channel and no file is opened.
// ----------------------------------------------------------------------

mod own_device {
    use std::collections::{BTreeMap, BTreeSet};

    use koru_device::{
        AllDataGrantV1, DeviceDescriptorV1, DeviceId, DeviceSigningKey, ProfileGrantV1,
        ProfileLifecycleStateV1, ProfileLifecycleV1, RecordId, RelationshipId,
        ReplicationEndpointSecret, TrustRecord, TrustRelationshipV1, TrustRepository,
        transcript_hash, verify_gossip_record,
    };
    use koru_store_core::{ProfileId, WorkspaceId};
    use koru_sync_control::AuthorizedPeer;

    use crate::identity::SqliteTrustRepository;
    use crate::service::authorized_peers;

    struct Party {
        key: DeviceSigningKey,
        descriptor: DeviceDescriptorV1,
    }

    fn party(name: &str) -> Party {
        let key = DeviceSigningKey::generate();
        let descriptor = DeviceDescriptorV1::new(
            DeviceId::new(),
            name.to_owned(),
            &key,
            ReplicationEndpointSecret::generate().endpoint_id(),
            1,
        );
        Party { key, descriptor }
    }

    fn relationship(a: &Party, b: &Party) -> TrustRelationshipV1 {
        let id = RelationshipId::new();
        TrustRelationshipV1::sign(
            id,
            a.descriptor.clone(),
            b.descriptor.clone(),
            transcript_hash(id.to_storage().as_bytes()),
            3,
            &a.key,
            &b.key,
        )
    }

    /// Both directional all-data grants of a QR pairing.
    fn all_data(relationship: &TrustRelationshipV1, a: &Party, b: &Party) -> Vec<TrustRecord> {
        let grant = |from: &Party, to: &Party| {
            TrustRecord::AllDataGrant(AllDataGrantV1::sign(
                relationship.relationship_id,
                to.descriptor.device_id,
                from.descriptor.device_id,
                RecordId::new(),
                4,
                &from.key,
            ))
        };
        vec![grant(a, b), grant(b, a)]
    }

    /// Both directional grants of one Profile: what account enrollment
    /// gives.
    fn profile(
        relationship: &TrustRelationshipV1,
        profile: ProfileId,
        a: &Party,
        b: &Party,
    ) -> Vec<TrustRecord> {
        let grant = |from: &Party, to: &Party| {
            TrustRecord::ProfileGrant(ProfileGrantV1::sign(
                relationship.relationship_id,
                RecordId::new(),
                profile,
                to.descriptor.device_id,
                from.descriptor.device_id,
                4,
                &from.key,
            ))
        };
        vec![grant(a, b), grant(b, a)]
    }

    fn repository(local: &Party) -> SqliteTrustRepository {
        let mut repository = SqliteTrustRepository::in_memory().unwrap();
        repository.initialize(&local.descriptor).unwrap();
        repository
    }

    fn apply(repository: &mut SqliteTrustRepository, records: Vec<TrustRecord>) {
        for record in records {
            let view = repository.trust_view().unwrap();
            let verified = verify_gossip_record(&view, record).expect("the record verifies");
            repository.apply(verified).unwrap();
        }
    }

    fn peers(
        repository: &SqliteTrustRepository,
        local_profiles: &BTreeSet<ProfileId>,
    ) -> BTreeMap<DeviceId, AuthorizedPeer> {
        let view = repository.trust_view().unwrap();
        let exportable: BTreeMap<WorkspaceId, ProfileId> = local_profiles
            .iter()
            .map(|profile| (WorkspaceId::new(), *profile))
            .collect();
        authorized_peers(&view, &exportable, local_profiles)
            .into_iter()
            .map(|peer| (peer.device_id, peer))
            .collect()
    }

    #[test]
    fn m8_own_device_bit_is_true_only_for_a_directly_paired_all_data_device() {
        let local = party("This computer");
        let paired = party("Paired by QR");
        let introduced = party("Paired only with the paired one");
        let enrolled = party("Enrolled, one Profile");
        let shared_profile = ProfileId::new();
        let mut repository = repository(&local);

        // This computer and `paired`: a QR pairing with all data.
        let direct = relationship(&local, &paired);
        apply(
            &mut repository,
            vec![TrustRecord::Relationship(direct.clone())],
        );
        apply(&mut repository, all_data(&direct, &local, &paired));
        // `paired` and `introduced`: their own QR pairing. This computer
        // has no pairing with `introduced`.
        let onward = relationship(&paired, &introduced);
        apply(
            &mut repository,
            vec![TrustRecord::Relationship(onward.clone())],
        );
        apply(&mut repository, all_data(&onward, &paired, &introduced));
        // This computer and `enrolled`: Profile grants only.
        let enrollment = relationship(&local, &enrolled);
        apply(
            &mut repository,
            vec![TrustRecord::Relationship(enrollment.clone())],
        );
        apply(
            &mut repository,
            profile(&enrollment, shared_profile, &local, &enrolled),
        );

        let peers = peers(&repository, &BTreeSet::from([shared_profile]));
        assert_eq!(peers.len(), 3);

        let paired = &peers[&paired.descriptor.device_id];
        assert!(paired.all_data);
        assert_eq!(paired.via, None);
        assert!(paired.own_device, "paired directly, with all data");

        // On the all-data graph, and reached only through another device.
        let introduced = &peers[&introduced.descriptor.device_id];
        assert!(introduced.all_data);
        assert_eq!(introduced.via, Some(paired.device_id));
        assert!(!introduced.own_device, "not paired with this computer");

        // Paired directly, with a Profile and not all data.
        let enrolled = &peers[&enrolled.descriptor.device_id];
        assert!(!enrolled.all_data);
        assert_eq!(enrolled.via, None);
        assert!(!enrolled.authorized_profiles.is_empty());
        assert!(!enrolled.own_device, "Profile grants only");
    }

    #[test]
    fn m8_own_device_bit_stays_true_when_a_profile_was_deleted() {
        let local = party("This computer");
        let paired = party("Paired by QR");
        let kept = ProfileId::new();
        let deleted = ProfileId::new();
        let mut repository = repository(&local);
        let direct = relationship(&local, &paired);
        apply(
            &mut repository,
            vec![TrustRecord::Relationship(direct.clone())],
        );
        apply(&mut repository, all_data(&direct, &local, &paired));
        let profiles = BTreeSet::from([kept, deleted]);

        let before = peers(&repository, &profiles);
        let before = &before[&paired.descriptor.device_id];
        assert!(before.all_data);
        assert!(before.own_device);

        // A Profile is deleted on this computer.
        let lifecycle = ProfileLifecycleV1::sign(
            RecordId::new(),
            local.descriptor.device_id,
            deleted,
            ProfileLifecycleStateV1::Deleted,
            1,
            uuid::Uuid::new_v4(),
            50,
            &local.key,
        );
        let verified = lifecycle.verify(local.key.public_key()).unwrap();
        repository.apply(verified).unwrap();

        let after = peers(&repository, &profiles);
        let after = &after[&paired.descriptor.device_id];
        // The replication scope narrows to the Profiles that remain...
        assert!(!after.all_data, "all_data is cleared by the deletion");
        assert_eq!(after.authorized_profiles, vec![kept]);
        // ...and the device is still the person's own.
        assert!(after.own_device, "own_device follows the pairing");
        assert_eq!(after.via, None);

        // So the grant changed, and the bit is not what changed.
        assert_ne!(before, after);
        assert_eq!(before.own_device, after.own_device);
    }
}
