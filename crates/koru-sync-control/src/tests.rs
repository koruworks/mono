// SPDX-License-Identifier: MPL-2.0

use super::*;

fn hello() -> ControlHello {
    ControlHello {
        protocol_version: CONTROL_PROTOCOL_VERSION,
        session_id: ControlSessionId::new(),
        local_device: DeviceId::new(),
    }
}

fn snapshot(
    hello: &ControlHello,
    sequence: u64,
    peer: DeviceId,
    endpoint: ReplicationEndpointId,
    workspace: WorkspaceId,
) -> AuthorizationSnapshot {
    AuthorizationSnapshot {
        session_id: hello.session_id,
        sequence,
        local_device: hello.local_device,
        peers: vec![AuthorizedPeer {
            relationship_id: RelationshipId::new(),
            device_id: peer,
            endpoint_id: endpoint,
            all_data: false,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: None,
        }],
        local_build: None,
    }
}

#[test]
fn starts_disabled_and_disconnect_clears_everything() {
    let mut host = ControlHost::default();
    assert_eq!(host.status(), ControlStatus::default());
    let hello = hello();
    host.begin_session(&hello).unwrap();
    host.handle(
        hello.session_id,
        ControlRequest::ProvisionEndpoint {
            secret: EndpointSecretBytes::new([7; 32]),
        },
    );
    let peer = DeviceId::new();
    host.handle(
        hello.session_id,
        ControlRequest::ReplaceAuthorization {
            snapshot: snapshot(
                &hello,
                1,
                peer,
                ReplicationEndpointId::from_bytes([8; 32]),
                WorkspaceId::new(),
            ),
        },
    );
    assert!(host.status().replication_ready);
    host.end_session(hello.session_id);
    assert_eq!(host.status(), ControlStatus::default());
}

#[test]
fn sequence_is_strictly_increasing_per_session() {
    let mut host = ControlHost::default();
    let hello = hello();
    host.begin_session(&hello).unwrap();
    let peer = DeviceId::new();
    let endpoint = ReplicationEndpointId::from_bytes([8; 32]);
    let workspace = WorkspaceId::new();
    assert!(matches!(
        host.handle(
            hello.session_id,
            ControlRequest::ReplaceAuthorization {
                snapshot: snapshot(&hello, 2, peer, endpoint, workspace)
            }
        ),
        ControlResponse::AuthorizationApplied { sequence: 2, .. }
    ));
    assert_eq!(
        host.handle(
            hello.session_id,
            ControlRequest::ReplaceAuthorization {
                snapshot: snapshot(&hello, 2, peer, endpoint, workspace)
            }
        ),
        ControlResponse::Rejected {
            code: "out_of_order".into()
        }
    );
    assert_eq!(
        host.handle(
            hello.session_id,
            ControlRequest::ReplaceAuthorization {
                snapshot: snapshot(&hello, 1, peer, endpoint, workspace)
            }
        ),
        ControlResponse::Rejected {
            code: "out_of_order".into()
        }
    );
}

#[test]
fn an_old_session_cannot_mutate_a_new_one() {
    let mut host = ControlHost::default();
    let old = hello();
    let new = hello();
    host.begin_session(&old).unwrap();
    host.begin_session(&new).unwrap();
    assert_eq!(
        host.handle(old.session_id, ControlRequest::Status),
        ControlResponse::Rejected {
            code: "stale_session".into()
        }
    );
}

#[test]
fn authorization_is_exact_and_secret_debug_is_redacted() {
    assert_eq!(
        format!("{:?}", EndpointSecretBytes::new([42; 32])),
        "EndpointSecretBytes([REDACTED])"
    );
    let mut host = ControlHost::default();
    let hello = hello();
    host.begin_session(&hello).unwrap();
    let peer = DeviceId::new();
    let endpoint = ReplicationEndpointId::from_bytes([8; 32]);
    let workspace = WorkspaceId::new();
    host.handle(
        hello.session_id,
        ControlRequest::ReplaceAuthorization {
            snapshot: snapshot(&hello, 1, peer, endpoint, workspace),
        },
    );
    assert!(host.authorize(endpoint, workspace).is_some());
    assert!(host.authorize(endpoint, WorkspaceId::new()).is_none());
}

#[test]
fn urgent_profile_lifecycle_targets_only_a_currently_authorized_profile_peer() {
    let mut host = ControlHost::default();
    let hello = hello();
    host.begin_session(&hello).unwrap();
    let peer = DeviceId::new();
    let profile = ProfileId::new();
    let mut authorization = snapshot(
        &hello,
        1,
        peer,
        ReplicationEndpointId::from_bytes([8; 32]),
        WorkspaceId::new(),
    );
    authorization.peers[0].authorized_profiles = vec![profile];
    host.handle(
        hello.session_id,
        ControlRequest::ReplaceAuthorization {
            snapshot: authorization,
        },
    );
    let key = koru_device::DeviceSigningKey::generate();
    let lifecycle = koru_device::ProfileLifecycleV1::sign(
        koru_device::RecordId::new(),
        hello.local_device,
        profile,
        koru_device::ProfileLifecycleStateV1::Deleted,
        1,
        uuid::Uuid::new_v4(),
        10,
        &key,
    );
    assert!(matches!(
        host.handle(
            hello.session_id,
            ControlRequest::SendTrustRecord {
                peer,
                record: Box::new(koru_device::TrustRecord::ProfileLifecycle(
                    lifecycle.clone()
                )),
            }
        ),
        ControlResponse::Ok
    ));

    let wrong_profile = koru_device::ProfileLifecycleV1::sign(
        koru_device::RecordId::new(),
        hello.local_device,
        ProfileId::new(),
        koru_device::ProfileLifecycleStateV1::Deleted,
        1,
        uuid::Uuid::new_v4(),
        10,
        &key,
    );
    assert!(matches!(
        host.handle(
            hello.session_id,
            ControlRequest::SendTrustRecord {
                peer,
                record: Box::new(koru_device::TrustRecord::ProfileLifecycle(wrong_profile)),
            }
        ),
        ControlResponse::Rejected { .. }
    ));

    let mut forged_issuer = lifecycle;
    forged_issuer.issued_by = peer;
    assert!(matches!(
        host.handle(
            hello.session_id,
            ControlRequest::SendTrustRecord {
                peer,
                record: Box::new(koru_device::TrustRecord::ProfileLifecycle(forged_issuer)),
            }
        ),
        ControlResponse::Rejected { .. }
    ));
}

/// **A gossip batch at the sanity bound fits in one control frame.** Trust
/// gossip reaches the agent inside a `PairingEvents` reply over the store's
/// control socket, and a reply over `koru_ipc::frame::MAX_BODY` fails to
/// encode — which ended the session, replaced the authorization, closed
/// every peer, and had the peer re-send the same batch, four seconds round,
/// for as long as anyone looked. A phone's sixty records were 141 KB against
/// a 64 KB cap. The largest kind at the largest count is what this encodes.
#[test]
fn a_gossip_batch_at_the_sanity_bound_fits_in_one_frame() {
    let key = koru_device::DeviceSigningKey::generate();
    let device = DeviceId::new();
    let payload = koru_device::BuildPayload {
        app_version: "0.0.3-nightly.20260821+8b35a3b".into(),
        channel: "nightly".into(),
        branch: Some("a-branch-name-of-ordinary-length".into()),
        commit: Some("8b35a3b6c41f03".into()),
        os: Some("Windows 11".into()),
        os_version: Some("10.0.26200".into()),
        peer_protocol: Some(PEER_PROTOCOL_VERSION),
    };
    let records: Vec<koru_device::TrustRecord> = (0..MAX_TRUST_GOSSIP_RECORDS)
        .map(|i| {
            koru_device::TrustRecord::DeviceBuild(koru_device::DeviceBuildV1::sign(
                koru_device::RecordId::new(),
                device,
                &payload,
                i as i64,
                &key,
            ))
        })
        .collect();
    let event = |records: Vec<koru_device::TrustRecord>| PairingEvent::TrustRecordsReceived {
        delivery_id: TrustDeliveryId::new(),
        from: device,
        records,
    };
    let reply = |events: Vec<PairingEvent>| ResponseFrame {
        id: 1,
        response: ControlResponse::PairingEvents { events },
    };
    let one = koru_ipc::frame::encode(CONTROL_FRAME_VERSION, &reply(vec![event(records.clone())]))
        .expect("a batch at the bound encodes into one frame");
    assert!(one.len() <= koru_ipc::frame::MAX_BODY + koru_ipc::frame::HEADER_LEN);

    // **It is batches times peers that has to fit, not one batch.**
    // `take_pairing_events` drains the *whole* queue, so a reply carries every
    // batch that arrived since the agent last asked — and an agent that was
    // away, or several peers gossiping at once, is exactly when this reply is
    // biggest and exactly when losing it costs the most. One maximal batch
    // measured 2.6 MB against the 16 MiB cap, so there is room for about six;
    // this pins four, which is the headroom rather than the ceiling. Raising
    // `MAX_TRUST_GOSSIP_RECORDS` is what would fail here, and that is the
    // conversation to have before it ships rather than after.
    const CONCURRENT_BATCHES: usize = 4;
    let many = koru_ipc::frame::encode(
        CONTROL_FRAME_VERSION,
        &reply(
            (0..CONCURRENT_BATCHES)
                .map(|_| event(records.clone()))
                .collect(),
        ),
    )
    .expect("several maximal batches still encode into one frame");
    assert!(
        many.len() <= koru_ipc::frame::MAX_BODY + koru_ipc::frame::HEADER_LEN,
        "{CONCURRENT_BATCHES} maximal batches are {} bytes, over the {} cap",
        many.len(),
        koru_ipc::frame::MAX_BODY
    );
}

// ------------------------------------------------------- grant equality
//
// What `same_grant` answers decides whether a reconnecting agent tears the
// data plane down or leaves it alone, so each tier gets its own test rather
// than one table: a wrong answer in the *cosmetic* direction closes every
// peer on a display-name change, and a wrong answer in the *significant*
// direction leaves a revoked peer connected.

fn build(version: &str, peer_protocol: Option<u32>) -> koru_device::BuildPayload {
    koru_device::BuildPayload {
        app_version: version.to_owned(),
        channel: "nightly".to_owned(),
        branch: None,
        commit: None,
        os: None,
        os_version: None,
        peer_protocol,
    }
}

/// The bookkeeping that changes on *every* reconnect. If any of it counted,
/// nothing would ever be an unchanged grant and the whole idea would be dead.
#[test]
fn session_bookkeeping_is_not_part_of_the_grant() {
    let hello = hello();
    let (peer, endpoint, workspace) = (
        DeviceId::new(),
        ReplicationEndpointId::from_bytes([8; 32]),
        WorkspaceId::new(),
    );
    let first = snapshot(&hello, 1, peer, endpoint, workspace);
    let mut later = first.clone();
    later.session_id = ControlSessionId::new();
    later.sequence = 97;
    assert!(first.same_grant(&later));
}

/// Names and provenance carry no authority — the struct says so — so a trust
/// record arriving with a display name must not close a live connection.
#[test]
fn cosmetics_are_not_part_of_the_grant() {
    let hello = hello();
    let first = snapshot(
        &hello,
        1,
        DeviceId::new(),
        ReplicationEndpointId::from_bytes([8; 32]),
        WorkspaceId::new(),
    );
    let mut renamed = first.clone();
    renamed.peers[0].display_name = Some("Steven's laptop".to_owned());
    renamed.peers[0].via = Some(DeviceId::new());
    assert!(first.same_grant(&renamed));
}

/// Order and duplicates are shape, not substance: `normalize_snapshot` sorts
/// and dedups, and `same_grant` normalizes both sides so an agent that sent an
/// unsorted list does not read as a change.
#[test]
fn workspace_order_and_duplicates_are_not_a_change() {
    let hello = hello();
    let (peer, endpoint) = (DeviceId::new(), ReplicationEndpointId::from_bytes([8; 32]));
    let (a, b) = (WorkspaceId::new(), WorkspaceId::new());
    let mut first = snapshot(&hello, 1, peer, endpoint, a);
    first.peers[0].workspaces = vec![a, b];
    let (profile_a, profile_b) = (ProfileId::new(), ProfileId::new());
    first.peers[0].authorized_profiles = vec![profile_a, profile_b];
    let mut shuffled = first.clone();
    shuffled.peers[0].workspaces = vec![b, a, b];
    shuffled.peers[0].authorized_profiles = vec![profile_b, profile_a, profile_b];
    assert!(first.same_grant(&shuffled));
}

/// Everything the data plane actually authorizes on. Each of these must force
/// the wholesale replace, because each changes what a peer may have.
#[test]
fn what_is_authorized_is_part_of_the_grant() {
    let hello = hello();
    let (peer, endpoint, workspace) = (
        DeviceId::new(),
        ReplicationEndpointId::from_bytes([8; 32]),
        WorkspaceId::new(),
    );
    let first = snapshot(&hello, 1, peer, endpoint, workspace);

    let mut another_workspace = first.clone();
    another_workspace.peers[0]
        .workspaces
        .push(WorkspaceId::new());
    assert!(!first.same_grant(&another_workspace));

    let mut everything = first.clone();
    everything.peers[0].all_data = true;
    assert!(!first.same_grant(&everything));

    let mut another_profile = first.clone();
    another_profile.peers[0]
        .authorized_profiles
        .push(ProfileId::new());
    assert!(!first.same_grant(&another_profile));

    let mut moved = first.clone();
    moved.peers[0].endpoint_id = ReplicationEndpointId::from_bytes([9; 32]);
    assert!(!first.same_grant(&moved));

    let mut repaired = first.clone();
    repaired.peers[0].relationship_id = RelationshipId::new();
    assert!(!first.same_grant(&repaired));

    let mut elsewhere = first.clone();
    elsewhere.local_device = DeviceId::new();
    assert!(!first.same_grant(&elsewhere));
}

/// Adding or revoking a peer is the case the whole safety argument rests on.
#[test]
fn adding_or_revoking_a_peer_is_a_change() {
    let hello = hello();
    let endpoint = ReplicationEndpointId::from_bytes([8; 32]);
    let workspace = WorkspaceId::new();
    let first = snapshot(&hello, 1, DeviceId::new(), endpoint, workspace);

    let mut added = first.clone();
    let mut extra = added.peers[0].clone();
    extra.device_id = DeviceId::new();
    extra.endpoint_id = ReplicationEndpointId::from_bytes([9; 32]);
    added.peers.push(extra);
    assert!(!first.same_grant(&added));

    let mut revoked = first.clone();
    revoked.peers.clear();
    assert!(!first.same_grant(&revoked));
}

/// **Build is significant, and that is the surprising half.** It reads like a
/// label beside `display_name`, but the build-drift refusal is frozen into a
/// peer session when it spawns and re-evaluated only by a reconnect — so a
/// build that becomes known, or drifts, has to force one.
#[test]
fn a_build_change_is_a_change() {
    let hello = hello();
    let first = snapshot(
        &hello,
        1,
        DeviceId::new(),
        ReplicationEndpointId::from_bytes([8; 32]),
        WorkspaceId::new(),
    );

    let mut learned = first.clone();
    learned.peers[0].build = Some(build("0.0.3", Some(4)));
    assert!(!first.same_grant(&learned));

    let mut updated = learned.clone();
    updated.peers[0].build = Some(build("0.0.4", Some(5)));
    assert!(!learned.same_grant(&updated));

    let mut ours = first.clone();
    ours.local_build = Some(build("0.0.3", Some(4)));
    assert!(!first.same_grant(&ours));
}

// ----------------------------------------------- control 7: the Cloud arms
//
// These read the frozen JSON under
// `contracts/vectors/host-control-v1/` rather than values
// built here. The wire shape of these seven operations is the contract the
// Agent and the gateway are written against, and a test that round-trips its
// own construction would not notice a renamed field.

use crate::cloud::{
    CloudPermitV1, CloudStage, CloudWorkspaceDescriptorV1, MAX_CLOUD_CONTROL_FRAME_BYTES,
    MAX_CLOUD_PERMIT_PAGE, MAX_CLOUD_PERMITS,
};
use koru_device::cloud::{CloudId, CloudReplicaPrincipalV1};

const CONTROL_VECTORS: &str =
    include_str!("../../../contracts/vectors/host-control-v1/control.json");
const SEQUENCE_VECTORS: &str =
    include_str!("../../../contracts/vectors/host-control-v1/sequences.json");

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).expect("a host-control vector file parses")
}

fn session_from(value: &serde_json::Value) -> ControlSessionId {
    serde_json::from_value(value.clone()).expect("a canonical control session id")
}

fn host_on(session_id: ControlSessionId) -> ControlHost {
    let mut host = ControlHost::default();
    host.begin_session(&ControlHello {
        protocol_version: CONTROL_PROTOCOL_VERSION,
        session_id,
        local_device: DeviceId::new(),
    })
    .expect("control 7 begins a session");
    host
}

#[test]
fn the_control_protocol_is_seven_and_the_greeting_refuses_every_other_number() {
    assert_eq!(CONTROL_PROTOCOL_VERSION, 7);
    assert_eq!(CONTROL_FRAME_VERSION, 1);
    assert_eq!(PEER_PROTOCOL_VERSION, 5);
    let mut host = ControlHost::default();
    assert_eq!(
        host.begin_session(&ControlHello {
            protocol_version: CONTROL_PROTOCOL_VERSION - 1,
            session_id: ControlSessionId::new(),
            local_device: DeviceId::new(),
        }),
        Err(HostError::Protocol)
    );
}

#[test]
fn frozen_cloud_requests_decode_re_encode_and_refuse_exactly() {
    let vectors = json(CONTROL_VECTORS);
    assert_eq!(vectors["control_protocol_version"], 7);
    let mut seen = BTreeSet::new();
    for case in vectors["requests"].as_array().unwrap() {
        let name = case["case"].as_str().unwrap();
        let body = case["json"].clone();
        if case["valid"].as_bool().unwrap() {
            let request: ControlRequest = serde_json::from_value(body.clone())
                .unwrap_or_else(|error| panic!("{name} should decode: {error}"));
            assert!(
                is_cloud_request(&request),
                "{name} must be one of the seven Cloud variants"
            );
            assert_eq!(
                serde_json::to_value(&request).unwrap(),
                body,
                "{name} must re-encode byte-for-byte"
            );
            seen.insert(case["operation"].as_str().unwrap().to_owned());
        } else {
            assert!(
                serde_json::from_value::<ControlRequest>(body).is_err(),
                "{name} must be refused"
            );
        }
    }
    assert_eq!(
        seen,
        [
            "cloud_status",
            "decide_cloud_transfer",
            "fence_cloud",
            "install_cloud_session",
            "read_cloud_bootstrap",
            "replace_cloud_context",
            "replace_cloud_network_policy",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>(),
        "every one of the seven operations needs a frozen valid case"
    );
}

/// The stateful half. Each sequence gets its own `ControlHost`; every step
/// names either the exact response or the exact refusal code.
#[test]
fn frozen_cloud_sequences_replay_exactly() {
    let vectors = json(SEQUENCE_VECTORS);
    let first = session_from(&vectors["session"]);
    let restarted = session_from(&vectors["restart_session"]);
    for sequence in vectors["sequences"].as_array().unwrap() {
        let case = sequence["case"].as_str().unwrap();
        let mut host = host_on(first);
        let mut session = first;
        for (index, step) in sequence["steps"].as_array().unwrap().iter().enumerate() {
            let at = format!("{case} step {index}");
            if step["restart"].as_bool() == Some(true) {
                host.end_session(session);
                host = host_on(restarted);
                session = restarted;
                continue;
            }
            if let Some(expected) = step["permit_ids"].as_array() {
                let status = host.cloud_status(session);
                assert_eq!(status.version, 1, "{at}: the status version is 1");
                assert_eq!(status.control_session_id, session, "{at}");
                let live: Vec<String> = status
                    .records
                    .iter()
                    .map(|record| record.permit_id.to_string())
                    .collect();
                let want: Vec<String> = expected
                    .iter()
                    .map(|id| id.as_str().unwrap().to_owned())
                    .collect();
                assert_eq!(live, want, "{at}: the live permit set");
                assert!(
                    status.records.len() <= MAX_CLOUD_PERMITS,
                    "{at}: the snapshot is bounded"
                );
                continue;
            }
            let request: ControlRequest = serde_json::from_value(step["request"].clone())
                .unwrap_or_else(|error| panic!("{at} should decode: {error}"));
            let response = host.handle(session, request);
            if let Some(code) = step["expect_code"].as_str() {
                assert_eq!(
                    response,
                    ControlResponse::Rejected {
                        code: code.to_owned()
                    },
                    "{at}"
                );
            } else {
                assert_eq!(
                    serde_json::to_value(&response).unwrap(),
                    step["expect"],
                    "{at}"
                );
            }
        }
    }
}

fn cloud_id(byte: u8) -> CloudId {
    let mut bytes = [byte; 16];
    bytes[6] = 0x40 | (bytes[6] & 0x0f);
    bytes[8] = 0x80 | (bytes[8] & 0x3f);
    CloudId::from_bytes(bytes)
}

fn permit(index: u16, account: CloudId, account_generation: u64) -> CloudPermitV1 {
    let mut bytes = [0u8; 16];
    bytes[..2].copy_from_slice(&index.to_be_bytes());
    bytes[6] = 0x40;
    bytes[8] = 0x80;
    let permit_id = CloudId::from_bytes(bytes).as_uuid();
    CloudPermitV1 {
        version: 1,
        permit_id,
        dataset: CloudReplicaPrincipalV1::Profile {
            profile_id: cloud_id(0x33),
            lineage_id: cloud_id(0xbb),
            logical_replica_id: cloud_id(0xcc),
        },
        account_id: account,
        profile_id: cloud_id(0x33),
        authority_workspace_id: cloud_id(0x88),
        authority_workspace_incarnation_id: cloud_id(0x99),
        account_generation,
        expires_at_seconds: 1_800_000_000,
        allowed_profile_units: Vec::new(),
        workspace_descriptor: None,
    }
}

fn principal(
    permit: &CloudPermitV1,
    session: ControlSessionId,
    session_generation: u64,
) -> CloudPrincipalV1 {
    CloudPrincipalV1 {
        version: koru_device::cloud::CLOUD_PRINCIPAL_VERSION,
        permit_id: permit.permit_id,
        account_id: permit.account_id,
        profile_id: permit.profile_id,
        dataset: permit.dataset,
        client_endpoint_id: ReplicationEndpointId::from_bytes([0x11; 32]),
        worker_endpoint_id: ReplicationEndpointId::from_bytes([0x22; 32]),
        credential_id: cloud_id(0x44),
        authorization_version: 1,
        membership_epoch: 1,
        cloud_access_epoch: 1,
        authority_workspace_id: permit.authority_workspace_id,
        authority_workspace_incarnation_id: permit.authority_workspace_incarnation_id,
        control_session_id: session.as_uuid(),
        account_generation: permit.account_generation,
        session_generation,
        expires_at_seconds: permit.expires_at_seconds,
    }
}

fn context(
    session: ControlSessionId,
    sequence: u64,
    account: CloudId,
    account_generation: u64,
    permits: Vec<CloudPermitV1>,
) -> ControlRequest {
    ControlRequest::ReplaceCloudContext {
        control_session_id: session,
        sequence,
        account_id: account,
        account_generation,
        permits,
    }
}

/// The standing permit bound is independent from the 32-permit wire page. A
/// page that would take the host over the bound is refused whole: nothing is
/// applied in part, so the Agent's next cohort starts from a state it knows.
#[test]
fn the_permit_bound_is_a_host_bound_and_an_over_cap_page_is_refused_whole() {
    let session = ControlSessionId::new();
    let mut host = host_on(session);
    let account = cloud_id(0x22);

    let full: Vec<CloudPermitV1> = (0..MAX_CLOUD_PERMITS as u16)
        .map(|index| permit(index, account, 1))
        .collect();
    for (index, page) in full.chunks(MAX_CLOUD_PERMIT_PAGE).enumerate() {
        let sequence = index as u64 + 1;
        assert_eq!(
            host.handle(
                session,
                context(session, sequence, account, 1, page.to_vec())
            ),
            ControlResponse::CloudContextApplied {
                control_session_id: session,
                sequence
            }
        );
    }
    let next_sequence = full.len().div_ceil(MAX_CLOUD_PERMIT_PAGE) as u64 + 1;
    assert_eq!(host.cloud_status(session).records.len(), MAX_CLOUD_PERMITS);

    // One more account's permit has nowhere to go: the bound is across the
    // host, so a second account cannot quietly double it.
    let other = cloud_id(0x23);
    assert_eq!(
        host.handle(
            session,
            context(
                session,
                next_sequence,
                other,
                1,
                vec![permit(MAX_CLOUD_PERMITS as u16, other, 1)]
            )
        ),
        ControlResponse::Rejected {
            code: "cloud_permit_capacity".into()
        }
    );
    assert_eq!(host.cloud_status(session).records.len(), MAX_CLOUD_PERMITS);

    // And a page over the page bound is refused before any of it is read as
    // authority, whatever the host currently holds.
    let oversized: Vec<CloudPermitV1> = (0..=MAX_CLOUD_PERMIT_PAGE as u16)
        .map(|index| permit(index, account, 1))
        .collect();
    assert_eq!(
        host.handle(
            session,
            context(session, next_sequence, account, 1, oversized)
        ),
        ControlResponse::Rejected {
            code: "cloud_permit_page".into()
        }
    );
}

/// A session generation belongs to the connection that produced it: the worker
/// allocates it from a counter it discards when the connection goes away, so
/// the first session of **every** connection is generation 1. The barrier must
/// therefore refuse a repeat only within one connection, and admit the same
/// number from a new one — or a device that closes its connection when a pass
/// settles, which is what both hosts do, could never open a second session.
#[test]
fn a_session_generation_is_replayed_only_within_its_own_connection() {
    let session = ControlSessionId::new();
    let mut host = host_on(session);
    let account = cloud_id(0x22);
    let held = permit(1, account, 1);
    assert_eq!(
        host.handle(session, context(session, 1, account, 1, vec![held.clone()])),
        ControlResponse::CloudContextApplied {
            control_session_id: session,
            sequence: 1,
        }
    );

    let install = |connection: u64, generation: u64| ControlRequest::InstallCloudSession {
        principal: Box::new(principal(&held, session, generation)),
        connection_id: connection,
        stream_session_id: generation,
    };
    let installed = |generation: u64| ControlResponse::CloudSessionInstalled {
        permit_id: held.permit_id,
        session_generation: generation,
    };

    // The first connection's first session.
    assert_eq!(host.handle(session, install(7, 1)), installed(1));
    // The same number again on the same connection is the replay the barrier
    // exists for.
    assert_eq!(
        host.handle(session, install(7, 1)),
        ControlResponse::Rejected {
            code: "cloud_session_replay".into()
        }
    );
    // The same connection's next session still has to climb.
    assert_eq!(host.handle(session, install(7, 2)), installed(2));
    assert_eq!(
        host.handle(session, install(7, 2)),
        ControlResponse::Rejected {
            code: "cloud_session_replay".into()
        }
    );
    // A new connection starts the worker's counter again, and generation 1 is
    // its first session rather than a replay of the last connection's.
    assert_eq!(host.handle(session, install(8, 1)), installed(1));
    // And the new connection's own repeat is refused, as its own.
    assert_eq!(
        host.handle(session, install(8, 1)),
        ControlResponse::Rejected {
            code: "cloud_session_replay".into()
        }
    );
}

/// A Workspace dataset needs the identity-only descriptor; a Profile dataset
/// must not carry one. Neither shape is silently coerced into the other.
#[test]
fn a_permit_is_refused_without_the_descriptor_its_dataset_requires() {
    let session = ControlSessionId::new();
    let mut host = host_on(session);
    let account = cloud_id(0x22);
    let mut workspace = permit(1, account, 1);
    workspace.dataset = CloudReplicaPrincipalV1::Workspace {
        workspace_id: cloud_id(0x44),
        incarnation_id: cloud_id(0x55),
        logical_replica_id: cloud_id(0x66),
    };
    assert_eq!(
        host.handle(
            session,
            context(session, 1, account, 1, vec![workspace.clone()])
        ),
        ControlResponse::Rejected {
            code: "cloud_permit_descriptor_required".into()
        }
    );

    workspace.workspace_descriptor = Some(CloudWorkspaceDescriptorV1 {
        version: 1,
        profile_id: cloud_id(0x33),
        workspace_id: cloud_id(0x44),
        workspace_incarnation_id: cloud_id(0x55),
        logical_replica_id: cloud_id(0x66),
        lineage_id: cloud_id(0x55),
        manifest_document_id: cloud_id(0xdd),
        authorization_version: 3,
        membership_epoch: 4,
        cloud_access_epoch: 5,
    });
    assert_eq!(
        host.handle(
            session,
            context(session, 1, account, 1, vec![workspace.clone()])
        ),
        ControlResponse::CloudContextApplied {
            control_session_id: session,
            sequence: 1,
        }
    );
    assert_eq!(
        host.cloud_status(session).records[0].stage,
        CloudStage::AwaitingAuthority
    );

    // A Profile dataset carrying a Workspace descriptor is refused, not
    // trimmed: the two contexts are distinct and must stay so.
    let mut profile = permit(2, account, 1);
    profile.workspace_descriptor = workspace.workspace_descriptor;
    assert_eq!(
        host.handle(session, context(session, 2, account, 1, vec![profile])),
        ControlResponse::Rejected {
            code: "cloud_permit_descriptor_forbidden".into()
        }
    );
}

/// **The extra 64 KiB cap is a cap the biggest legal body fits under.** A
/// maximal page of thirty-two permits, each with its Workspace descriptor, is
/// the largest of the seven bodies; if the structural bounds ever let it past
/// 64 KiB the cap would start refusing correct messages.
#[test]
fn a_maximal_cloud_page_fits_under_the_sixty_four_kibibyte_cap() {
    let session = ControlSessionId::new();
    let account = cloud_id(0x22);
    let permits: Vec<CloudPermitV1> = (0..MAX_CLOUD_PERMIT_PAGE as u16)
        .map(|index| {
            let mut held = permit(index, account, 1);
            held.dataset = CloudReplicaPrincipalV1::Workspace {
                workspace_id: cloud_id(0x44),
                incarnation_id: cloud_id(0x55),
                logical_replica_id: cloud_id(0x66),
            };
            held.workspace_descriptor = Some(CloudWorkspaceDescriptorV1 {
                version: 1,
                profile_id: cloud_id(0x33),
                workspace_id: cloud_id(0x44),
                workspace_incarnation_id: cloud_id(0x55),
                logical_replica_id: cloud_id(0x66),
                lineage_id: cloud_id(0x55),
                manifest_document_id: cloud_id(0xdd),
                authorization_version: i64::MAX,
                membership_epoch: i64::MAX,
                cloud_access_epoch: i64::MAX,
            });
            held
        })
        .collect();
    let frame = RequestFrame {
        id: u64::MAX,
        request: context(session, u64::MAX, account, u64::MAX, permits),
    };
    let encoded = serde_json::to_vec(&frame).expect("a maximal Cloud page encodes");
    assert!(
        encoded.len() <= MAX_CLOUD_CONTROL_FRAME_BYTES,
        "a maximal page is {} bytes, over the {MAX_CLOUD_CONTROL_FRAME_BYTES} Cloud cap",
        encoded.len()
    );
    assert!(encoded.len() <= koru_ipc::frame::MAX_BODY);
}

/// The status snapshot carries no credential, name, title or content, and
/// `Settled` is never durable coverage — only a stage label.
#[test]
fn a_status_snapshot_names_identity_and_stage_and_nothing_else() {
    let session = ControlSessionId::new();
    let mut host = host_on(session);
    let account = cloud_id(0x22);
    host.handle(
        session,
        context(session, 1, account, 1, vec![permit(1, account, 1)]),
    );
    let status = host.cloud_status(session);
    let text = serde_json::to_value(&status).unwrap().to_string();
    for forbidden in ["secret", "token", "credential", "title"] {
        assert!(
            !text.contains(forbidden),
            "a status snapshot must not carry {forbidden}"
        );
    }
    assert_eq!(status.records.len(), 1);
    assert_eq!(status.records[0].bytes_held, 0);
    assert_eq!(status.records[0].queued_jobs, 0);
    assert!(status.records[0].pending_consent_ids.is_empty());
    assert!(status.records[0].error_code.is_none());
}

// ------------------------------------------------- the blob control plane

fn blob_access(expires_at_millis: i64) -> crate::cloud::CloudBlobAccessV1 {
    crate::cloud::CloudBlobAccessV1 {
        version: crate::cloud::CLOUD_BLOB_ACCESS_VERSION,
        api_origin: "https://us.api.koru.do/".into(),
        region: "us".into(),
        access_token: "an-account-bearer-token".into(),
        expires_at_millis,
    }
}

fn set_blob_access(
    session: ControlSessionId,
    sequence: u64,
    account: CloudId,
    account_generation: u64,
    access: crate::cloud::CloudBlobAccessV1,
) -> ControlRequest {
    ControlRequest::ReplaceCloudBlobAccess {
        control_session_id: session,
        sequence,
        account_id: account,
        account_generation,
        access: Box::new(access),
    }
}

/// The account's reach into the blob control plane is state like every other
/// Cloud command: sequenced, replayable, and gone with the session.
#[test]
fn blob_access_is_sequenced_state_that_does_not_survive_a_session() {
    let session = ControlSessionId::new();
    let mut host = host_on(session);
    let account = cloud_id(0x31);
    assert!(
        host.cloud_blob_access(1).is_none(),
        "a fresh host holds no account token"
    );

    let applied = host.handle(
        session,
        set_blob_access(session, 1, account, 1, blob_access(i64::MAX)),
    );
    assert!(
        matches!(
            applied,
            ControlResponse::CloudBlobAccessApplied { sequence: 1, .. }
        ),
        "{applied:?}"
    );
    assert_eq!(
        host.cloud_blob_access(1)
            .map(|access| access.region.clone()),
        Some("us".to_string())
    );

    // The same message again is the same answer, not a conflict.
    let repeat = host.handle(
        session,
        set_blob_access(session, 1, account, 1, blob_access(i64::MAX)),
    );
    assert!(
        matches!(
            repeat,
            ControlResponse::CloudBlobAccessApplied { sequence: 1, .. }
        ),
        "{repeat:?}"
    );
    // A different body under the same number is.
    let conflict = host.handle(
        session,
        set_blob_access(session, 1, account, 1, blob_access(7)),
    );
    assert!(
        matches!(conflict, ControlResponse::Rejected { .. }),
        "{conflict:?}"
    );

    // Nothing here survives a control session.
    host.end_session(session);
    assert!(host.cloud_blob_access(1).is_none());
}

/// **Expiry is the host's answer, not a discovery from a `401`.** The value
/// carries how long the gateway may use the token without being told again.
#[test]
fn an_expired_blob_token_is_no_access_at_all() {
    let session = ControlSessionId::new();
    let mut host = host_on(session);
    let account = cloud_id(0x32);
    host.handle(
        session,
        set_blob_access(session, 1, account, 1, blob_access(1_000)),
    );
    assert!(host.cloud_blob_access(999).is_some());
    assert!(
        host.cloud_blob_access(1_000).is_none(),
        "a token was handed out at the instant it stopped being one"
    );
}

/// **An authority that has been replaced takes its token with it**, which is
/// the same rule every other Cloud command follows.
#[test]
fn a_replaced_account_generation_withdraws_the_token() {
    let session = ControlSessionId::new();
    let mut host = host_on(session);
    let account = cloud_id(0x33);
    host.handle(
        session,
        set_blob_access(session, 1, account, 1, blob_access(i64::MAX)),
    );
    assert!(host.cloud_blob_access(1).is_some());
    // A context at a newer generation replaces the authority.
    host.handle(
        session,
        context(session, 2, account, 2, vec![permit(1, account, 2)]),
    );
    assert!(
        host.cloud_blob_access(1).is_none(),
        "a token outlived the authority that issued it"
    );
    // And an older generation cannot write one back.
    let stale = host.handle(
        session,
        set_blob_access(session, 3, account, 1, blob_access(i64::MAX)),
    );
    assert!(
        matches!(stale, ControlResponse::Rejected { .. }),
        "{stale:?}"
    );
}

/// A value whose shape could never work is refused here rather than carried
/// to a service that would refuse it — and a cleartext origin that is not
/// loopback is one of them.
#[test]
fn a_blob_access_value_that_could_never_work_is_refused() {
    let session = ControlSessionId::new();
    let mut host = host_on(session);
    let account = cloud_id(0x34);
    let mut sequence = 0;
    for mutate in [
        (|value: &mut crate::cloud::CloudBlobAccessV1| value.version = 2)
            as fn(&mut crate::cloud::CloudBlobAccessV1),
        |value| value.api_origin = "http://api.koru.do/".into(),
        |value| value.api_origin = "us.api.koru.do".into(),
        |value| value.api_origin = "https://us.api.koru.do/?t=1".into(),
        |value| value.region = "mars".into(),
        |value| value.access_token = String::new(),
        |value| value.access_token = "a token with spaces".into(),
        |value| value.expires_at_millis = 0,
    ] {
        sequence += 1;
        let mut value = blob_access(i64::MAX);
        mutate(&mut value);
        let response = host.handle(
            session,
            set_blob_access(session, sequence, account, 1, value.clone()),
        );
        assert!(
            matches!(response, ControlResponse::Rejected { .. }),
            "{value:?} was admitted: {response:?}"
        );
    }
    // A loopback origin over cleartext is the one exception, because a local
    // stack is where a person tests two devices.
    sequence += 1;
    let mut local = blob_access(i64::MAX);
    local.api_origin = "http://127.0.0.1:8080/".into();
    let response = host.handle(
        session,
        set_blob_access(session, sequence, account, 1, local),
    );
    assert!(
        matches!(response, ControlResponse::CloudBlobAccessApplied { .. }),
        "{response:?}"
    );
}

/// **The token never reaches a log.** The one place a value like this escapes
/// is a `{:?}` somebody added while debugging something else.
#[test]
fn a_blob_access_value_redacts_its_token() {
    let rendered = format!("{:?}", blob_access(7));
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(!rendered.contains("an-account-bearer-token"), "{rendered}");
}

// ------------------------------------------------- root-catalog sync status

/// **An older daemon says nothing about organization sync**, and a newer
/// Agent reads that as "no reading" rather than refusing the frame.
#[test]
fn a_status_without_root_sync_reads_as_empty() {
    let old = r#"{"replication_ready":false,"endpoint_id":null,"authorization_sequence":0,"authorized_peers":0,"authorized_workspaces":0,"desired_connections":0}"#;
    let status: ControlStatus = serde_json::from_str(old).unwrap();
    assert!(status.root_sync.is_empty());
    let peer = DeviceId::new();
    let subject = RootSyncSubject {
        peer,
        profile: Some(ProfileId::new()),
        workspace: None,
        state: RootSyncState::PausedBySettings,
    };
    let rendered = serde_json::to_string(&subject).unwrap();
    assert!(rendered.contains("\"paused_by_settings\""), "{rendered}");
    assert_eq!(
        serde_json::from_str::<RootSyncSubject>(&rendered).unwrap(),
        subject
    );
}

/// A stop outranks a wait and a wait outranks done, whichever side it is on.
#[test]
fn the_worse_root_sync_reading_wins() {
    use RootSyncState::*;
    assert_eq!(UpToDate.worse(Pending), Pending);
    assert_eq!(Pending.worse(UpToDate), Pending);
    assert_eq!(Pending.worse(Held), Held);
    assert_eq!(Held.worse(PausedBySettings), PausedBySettings);
    assert_eq!(PausedBySettings.worse(BlockedByMismatch), BlockedByMismatch);
    assert_eq!(BlockedByMismatch.worse(UpToDate), BlockedByMismatch);
}
