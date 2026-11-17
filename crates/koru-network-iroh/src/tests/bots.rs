// SPDX-License-Identifier: MPL-2.0

//! The Bot client carrier (M8, piece N1): the `koru/bots/1` ALPN, who is
//! admitted to it, what closes it, and that it leaves replication alone.
//!
//! Every case runs over isolated loopback endpoints: no relay, no address
//! lookup, no public socket. The engine behind the runtime is a recorder
//! that keeps the sessions it was handed and counts connection events.
//!
//! The cohorts are `m8_alpn_admission`, `m8_revocation_closes_bots` and
//! `m8_no_connection_event`. The last case of `m8_alpn_admission` is the one
//! that matters most: **a Koru from before M8 keeps replicating with this
//! one**.

use super::*;

/// The engine side of a runtime: every session admitted, and every
/// connection event.
#[derive(Default)]
struct BotRecorder {
    sessions: Mutex<Vec<TransportSession>>,
    admitted: AtomicUsize,
    connected: Mutex<Vec<u64>>,
    disconnected: AtomicUsize,
}

impl BotRecorder {
    fn admitted(&self) -> usize {
        self.admitted.load(Ordering::SeqCst)
    }

    fn connected(&self) -> Vec<u64> {
        self.connected.lock().unwrap().clone()
    }

    fn disconnected(&self) -> usize {
        self.disconnected.load(Ordering::SeqCst)
    }

    async fn next_session(&self) -> TransportSession {
        for _ in 0..500 {
            if let Some(session) = self.sessions.lock().unwrap().pop() {
                return session;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no session was admitted");
    }
}

impl NetworkIngress for BotRecorder {
    fn admit(&self, session: TransportSession) -> Result<(), koru_network_edge::EdgeError> {
        self.admitted.fetch_add(1, Ordering::SeqCst);
        self.sessions.lock().unwrap().push(session);
        Ok(())
    }

    fn connected(
        &self,
        connection: koru_network_edge::ConnectionMetadata,
    ) -> Result<(), koru_network_edge::EdgeError> {
        self.connected
            .lock()
            .unwrap()
            .push(connection.connection_id);
        Ok(())
    }

    fn disconnected(&self, _connection_id: u64) {
        self.disconnected.fetch_add(1, Ordering::SeqCst);
    }
}

/// One device: its identity, its runtime and the engine behind it.
struct Device {
    id: DeviceId,
    secret: ReplicationEndpointSecret,
    runtime: RuntimeHandle,
    engine: Arc<BotRecorder>,
}

impl Device {
    async fn start(config: IrohNetworkConfig) -> Self {
        let secret = ReplicationEndpointSecret::generate();
        let engine = Arc::new(BotRecorder::default());
        let runtime = RuntimeHandle::spawn_with_opaque_ingress(config, engine.clone());
        runtime.provision_endpoint(secret.secret_bytes());
        wait_until(|| runtime.endpoint_addr().is_some()).await;
        Self {
            id: DeviceId::new(),
            secret,
            runtime,
            engine,
        }
    }

    fn addr(&self) -> EndpointAddr {
        self.runtime.endpoint_addr().unwrap()
    }

    /// Install a snapshot naming `peers`.
    fn authorize(&self, sequence: u64, peers: Vec<AuthorizedPeer>) {
        self.runtime.replace_authorization(AuthorizationSnapshot {
            session_id: ControlSessionId::new(),
            sequence,
            local_device: self.id,
            peers,
            local_build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        });
    }

    async fn open_bots(
        &self,
        owner: DeviceId,
    ) -> Result<TransportSession, koru_network_edge::EdgeError> {
        tokio::time::timeout(
            Duration::from_secs(10),
            self.runtime.open_product_session(
                koru_network_edge::NetworkTarget::Device { device_id: owner },
                bots_protocol(),
            ),
        )
        .await
        .expect("the Bot stream was answered")
    }

    fn connected_to(&self, peer: DeviceId) -> bool {
        matches!(
            self.runtime.status().peers.get(&peer),
            Some(PeerConnectionState::Connected { .. })
        )
    }
}

/// What a device's snapshot says of another device.
#[derive(Clone, Copy)]
enum Grant {
    /// Paired directly with all data: an own device.
    Own,
    /// As [`Grant::Own`], after a Profile was deleted on the local device:
    /// `all_data` is cleared and the Profiles are named, `own_device` stays.
    OwnAfterProfileDeletion,
    /// An own device that no replication scope names at all.
    OwnWithNothingToReplicate,
    /// Profile grants only.
    ProfileOnly,
    /// On the all-data graph, reached through another device.
    Introduced,
    /// What an Agent from before M8 sends for its own device: no bit.
    OwnBeforeM8,
}

fn peer(
    relationship: RelationshipId,
    device: DeviceId,
    endpoint: ReplicationEndpointId,
    grant: Grant,
) -> AuthorizedPeer {
    let workspace = WorkspaceId::new();
    let (all_data, authorized_profiles, workspaces, via, own_device) = match grant {
        Grant::Own => (true, Vec::new(), vec![workspace], None, true),
        Grant::OwnAfterProfileDeletion => {
            (false, vec![ProfileId::new()], vec![workspace], None, true)
        }
        Grant::OwnWithNothingToReplicate => (false, Vec::new(), Vec::new(), None, true),
        Grant::ProfileOnly => (false, vec![ProfileId::new()], Vec::new(), None, false),
        Grant::Introduced => (
            true,
            Vec::new(),
            vec![workspace],
            Some(DeviceId::new()),
            false,
        ),
        Grant::OwnBeforeM8 => (true, Vec::new(), vec![workspace], None, false),
    };
    AuthorizedPeer {
        relationship_id: relationship,
        device_id: device,
        endpoint_id: endpoint,
        all_data,
        authorized_profiles,
        workspaces,
        via,
        display_name: None,
        build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
        own_device,
    }
}

/// Link two devices: each names the other as `a_sees_b` and `b_sees_a`.
fn link(a: &Device, b: &Device, a_sees_b: Grant, b_sees_a: Grant) -> RelationshipId {
    let relationship = RelationshipId::new();
    a.authorize(
        1,
        vec![peer(relationship, b.id, b.secret.endpoint_id(), a_sees_b)],
    );
    b.authorize(
        1,
        vec![peer(relationship, a.id, a.secret.endpoint_id(), b_sees_a)],
    );
    relationship
}

/// Bring the replication connection up, both ways aware of it.
async fn replicate(a: &Device, b: &Device) {
    a.runtime.connect_peer_at(b.id, b.addr());
    wait_within(Duration::from_secs(10), || {
        a.connected_to(b.id) && b.connected_to(a.id)
    })
    .await;
    wait_within(Duration::from_secs(10), || {
        a.engine.connected().len() == 1 && b.engine.connected().len() == 1
    })
    .await;
}

async fn read(session: &mut TransportSession, bytes: usize) -> Vec<u8> {
    let mut received = Vec::new();
    while received.len() < bytes {
        let chunk = tokio::time::timeout(Duration::from_secs(10), session.receive.recv())
            .await
            .expect("bytes arrived")
            .expect("the stream is open")
            .expect("the stream has not ended");
        received.extend(chunk);
    }
    received
}

/// Wait for a stream to end, by a reset, a close or a clean finish, and
/// return every byte it carried first.
async fn read_to_end(session: &mut TransportSession) -> Vec<u8> {
    let mut received = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(10), session.receive.recv())
            .await
            .expect("the stream ended")
        {
            Ok(Some(chunk)) => received.extend(chunk),
            Ok(None) | Err(_) => return received,
        }
    }
}

/// A dialer that is nothing but an endpoint: what a device looks like to a
/// runtime before the runtime decides anything about it.
async fn raw_endpoint(secret: &ReplicationEndpointSecret) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&secret.secret_bytes()))
        .alpns(Vec::new())
        .bind()
        .await
        .unwrap()
}

/// Dial `owner` on the Bots ALPN from a bare endpoint and report how the
/// connection ended and whether a single byte came back.
async fn dial_and_watch(dialer: &Endpoint, owner: &Device) -> (String, usize) {
    let connection = dialer
        .connect(owner.addr(), BOTS_ALPN_V1)
        .await
        .expect("the handshake completes: the ALPN is offered");
    // A client would open its first stream and write its hello.
    let mut received = 0;
    if let Ok((mut send, mut recv)) = connection.open_bi().await {
        let _ = send.write_all(b"\x00\x00\x00\x02{}").await;
        let _ = send.finish();
        if let Ok(Ok(bytes)) =
            tokio::time::timeout(Duration::from_secs(10), recv.read_to_end(1024)).await
        {
            received = bytes.len();
        }
    }
    let reason = tokio::time::timeout(Duration::from_secs(10), connection.closed())
        .await
        .expect("the owner closed the connection");
    (reason.to_string(), received)
}

#[test]
fn m8_alpn_admission_the_token_is_the_alpn_and_the_default_list_holds_it() {
    assert_eq!(BOTS_ALPN_V1, b"koru/bots/1");
    assert_eq!(bots_protocol().as_str(), "koru/bots/1");
    assert_ne!(bots_protocol(), replication_protocol());
    // Today's list, plus the new ALPN, in every configuration a host builds.
    let expected = vec![
        REPLICATION_ALPN_V1.to_vec(),
        PAIRING_ALPN_V1.to_vec(),
        ENROLLMENT_ALPN_V1.to_vec(),
        BOTS_ALPN_V1.to_vec(),
    ];
    assert_eq!(default_alpns(), expected);
    assert_eq!(IrohNetworkConfig::default().alpns, expected);
    assert_eq!(IrohNetworkConfig::isolated().alpns, expected);
    for relay_enabled in [true, false] {
        for kind in [RelayChannelKind::Internal, RelayChannelKind::Production] {
            for isolated in [true, false] {
                assert_eq!(
                    resolve_network_config(relay_enabled, kind, isolated, None).alpns,
                    expected
                );
            }
        }
    }
    // The list a build from before M8 offered.
    assert_eq!(
        IrohNetworkConfig::isolated()
            .without_alpn(BOTS_ALPN_V1)
            .alpns,
        expected[..3]
    );
}

#[tokio::test]
async fn m8_alpn_admission_a_directly_paired_all_data_device_is_admitted() {
    let client = Device::start(IrohNetworkConfig::isolated()).await;
    let owner = Device::start(IrohNetworkConfig::isolated()).await;
    let relationship = link(&client, &owner, Grant::Own, Grant::Own);
    replicate(&client, &owner).await;
    let replication_sessions = owner.engine.admitted();

    let mut stream = client.open_bots(owner.id).await.expect("admitted");
    assert_eq!(stream.protocol, bots_protocol());
    assert!(stream.path.outbound);
    stream.send.send(b"client hello".to_vec()).await.unwrap();

    // The owner's engine is handed one opaque session, labelled with the
    // Bots token and carrying the authenticated device.
    let mut served = owner.engine.next_session().await;
    assert_eq!(served.protocol, bots_protocol());
    assert!(!served.path.outbound);
    let principal = served.principal.device().expect("a device principal");
    assert_eq!(principal.device_id, client.id);
    assert_eq!(principal.relationship_id, relationship);
    assert_eq!(principal.endpoint_id, client.secret.endpoint_id());
    assert_eq!(principal.authorization_revision, 1);
    assert_eq!(owner.engine.admitted(), replication_sessions + 1);

    // The runtime wrote no frame: the first bytes each side reads are the
    // other engine's.
    assert_eq!(read(&mut served, 12).await, b"client hello");
    served.send.send(b"owner hello".to_vec()).await.unwrap();
    assert_eq!(read(&mut stream, 11).await, b"owner hello");

    // A second stream reuses the live Bot connection.
    let second = client.open_bots(owner.id).await.expect("admitted");
    second.send.send(b"again".to_vec()).await.unwrap();
    let mut served_second = owner.engine.next_session().await;
    assert_eq!(read(&mut served_second, 5).await, b"again");
    assert_eq!(second.connection_id, stream.connection_id);
    assert_eq!(served_second.connection_id, served.connection_id);
    // It is not the replication connection.
    assert_ne!(served.connection_id, owner.engine.connected()[0]);
    assert_ne!(stream.connection_id, client.engine.connected()[0]);

    // An own device stays one after a Profile was deleted here: `all_data`
    // is cleared and the bit is not.
    owner.authorize(
        2,
        vec![peer(
            relationship,
            client.id,
            client.secret.endpoint_id(),
            Grant::OwnAfterProfileDeletion,
        )],
    );
    // The changed grant closed the first connection; the next stream dials
    // a new one and is admitted under the new snapshot.
    assert!(read_to_end(&mut stream).await.is_empty());
    let after = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(stream) = client.open_bots(owner.id).await {
                if stream.send.send(b"after".to_vec()).await.is_ok() {
                    return stream;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("admitted after the Profile deletion");
    let mut served_after = owner.engine.next_session().await;
    while served_after.protocol != bots_protocol() {
        served_after = owner.engine.next_session().await;
    }
    assert_eq!(read(&mut served_after, 5).await, b"after");
    assert_eq!(
        served_after
            .principal
            .device()
            .unwrap()
            .authorization_revision,
        2
    );
    drop(after);

    client.runtime.clear();
    owner.runtime.clear();
}

#[tokio::test]
async fn m8_alpn_admission_needs_no_replication_connection() {
    // The owner names the client an own device and gives it nothing to
    // replicate, so its replication connection is refused. Its Bot
    // connection is admitted all the same: the two do not depend on each
    // other.
    let client = Device::start(IrohNetworkConfig::isolated()).await;
    let owner = Device::start(IrohNetworkConfig::isolated()).await;
    link(
        &client,
        &owner,
        Grant::Own,
        Grant::OwnWithNothingToReplicate,
    );
    // The host names the owner's address. The replication dial this starts
    // is closed by the owner as unauthorized.
    client.runtime.connect_peer_at(owner.id, owner.addr());

    let stream = client.open_bots(owner.id).await.expect("admitted");
    stream.send.send(b"hello".to_vec()).await.unwrap();
    let mut served = owner.engine.next_session().await;
    assert_eq!(served.protocol, bots_protocol());
    assert_eq!(read(&mut served, 5).await, b"hello");

    assert!(!owner.connected_to(client.id));
    assert!(owner.engine.connected().is_empty());
    client.runtime.clear();
    owner.runtime.clear();
}

#[tokio::test]
async fn m8_alpn_admission_other_peers_are_closed_with_no_frame() {
    let owner = Device::start(IrohNetworkConfig::isolated()).await;
    let profile_only = ReplicationEndpointSecret::generate();
    let introduced = ReplicationEndpointSecret::generate();
    let before_m8 = ReplicationEndpointSecret::generate();
    let unlinked = ReplicationEndpointSecret::generate();
    owner.authorize(
        1,
        vec![
            peer(
                RelationshipId::new(),
                DeviceId::new(),
                profile_only.endpoint_id(),
                Grant::ProfileOnly,
            ),
            peer(
                RelationshipId::new(),
                DeviceId::new(),
                introduced.endpoint_id(),
                Grant::Introduced,
            ),
            peer(
                RelationshipId::new(),
                DeviceId::new(),
                before_m8.endpoint_id(),
                Grant::OwnBeforeM8,
            ),
        ],
    );

    for (what, secret) in [
        ("a Profile-only peer", &profile_only),
        ("an introduced peer", &introduced),
        ("a peer whose snapshot carries no bit", &before_m8),
        ("an unlinked endpoint", &unlinked),
    ] {
        let dialer = raw_endpoint(secret).await;
        let (reason, received) = dial_and_watch(&dialer, &owner).await;
        assert!(
            reason.contains("not an own device"),
            "{what} was closed with: {reason}"
        );
        assert_eq!(received, 0, "{what} was sent bytes");
        dialer.close().await;
    }
    // Nothing reached the engine: no session and no connection event.
    assert_eq!(owner.engine.admitted(), 0);
    assert!(owner.engine.connected().is_empty());
    assert_eq!(owner.engine.disconnected(), 0);

    // And this device does not dial an owner that is not its own device:
    // the answer is `Closed`, at once, with nothing sent.
    let client = Device::start(IrohNetworkConfig::isolated()).await;
    for grant in [Grant::ProfileOnly, Grant::Introduced, Grant::OwnBeforeM8] {
        client.authorize(
            1,
            vec![peer(
                RelationshipId::new(),
                owner.id,
                owner.secret.endpoint_id(),
                grant,
            )],
        );
        assert_eq!(
            client.open_bots(owner.id).await.unwrap_err(),
            koru_network_edge::EdgeError::Closed
        );
    }
    assert_eq!(
        client.open_bots(DeviceId::new()).await.unwrap_err(),
        koru_network_edge::EdgeError::Closed
    );
    assert_eq!(owner.engine.admitted(), 0);
    client.runtime.clear();
    owner.runtime.clear();
}

/// **The compatibility case.** `older` is a Koru from before M8: its
/// endpoint offers replication, pairing and enrollment and not
/// `koru/bots/1`, and its snapshot carries no `own_device`. `newer` is this
/// build and believes `older` is an own device.
///
/// What is shown, with real replication between two stores:
///
/// 1. the two replicate, in both directions, before any Bot dial;
/// 2. `newer`'s Bot dial fails as `Closed`, because the handshake finds no
///    common ALPN, and it fails every time it is repeated;
/// 3. the two are still connected afterwards and still replicate in both
///    directions.
#[tokio::test]
async fn m8_alpn_admission_an_endpoint_without_the_alpn_fails_the_dial_and_replication_stays_up() {
    let workspace = WorkspaceId::new();
    let manifest = DocumentId::new();
    let relationship = RelationshipId::new();
    let newer_secret = ReplicationEndpointSecret::generate();
    let older_secret = ReplicationEndpointSecret::generate();
    let newer_id = DeviceId::new();
    let older_id = DeviceId::new();
    let newer_store = Arc::new(InventoryStore::new(workspace, manifest));
    let older_store = Arc::new(InventoryStore::new(workspace, manifest));
    let newer = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), newer_store.clone());
    let older = RuntimeHandle::spawn_with_store(
        IrohNetworkConfig::isolated().without_alpn(BOTS_ALPN_V1),
        older_store.clone(),
    );
    newer.provision_endpoint(newer_secret.secret_bytes());
    older.provision_endpoint(older_secret.secret_bytes());
    wait_until(|| newer.endpoint_addr().is_some() && older.endpoint_addr().is_some()).await;
    let snapshot = |local, device, endpoint, own_device| AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: local,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: device,
            endpoint_id: endpoint,
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
            own_device,
        }],
        local_build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
    };
    newer.replace_authorization(snapshot(
        newer_id,
        older_id,
        older_secret.endpoint_id(),
        true,
    ));
    older.replace_authorization(snapshot(
        older_id,
        newer_id,
        newer_secret.endpoint_id(),
        false,
    ));
    let connected = |runtime: &RuntimeHandle, peer| {
        matches!(
            runtime.status().peers.get(&peer),
            Some(PeerConnectionState::Connected { .. })
        )
    };
    newer.connect_peer_at(older_id, older.endpoint_addr().unwrap());
    older.connect_peer_at(newer_id, newer.endpoint_addr().unwrap());
    wait_within(Duration::from_secs(10), || {
        connected(&newer, older_id) && connected(&older, newer_id)
    })
    .await;

    let replicates = |from: Arc<InventoryStore>,
                      from_runtime: RuntimeHandle,
                      to: Arc<InventoryStore>,
                      what: &'static str| async move {
        let document = DocumentId::new();
        from.add(document);
        from_runtime.notify_local_change();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !to.has(document) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{what}"));
    };

    // 1. Both directions replicate.
    replicates(
        newer_store.clone(),
        newer.clone(),
        older_store.clone(),
        "the newer build's document did not reach the older one",
    )
    .await;
    replicates(
        older_store.clone(),
        older.clone(),
        newer_store.clone(),
        "the older build's document did not reach the newer one",
    )
    .await;
    // 2. The Bot dial fails, and keeps failing, as `Closed`.
    for _ in 0..3 {
        let answer = tokio::time::timeout(
            Duration::from_secs(10),
            newer.open_product_session(
                koru_network_edge::NetworkTarget::Device {
                    device_id: older_id,
                },
                bots_protocol(),
            ),
        )
        .await
        .expect("the Bot dial was answered");
        assert_eq!(answer.unwrap_err(), koru_network_edge::EdgeError::Closed);
    }

    // 3. Replication is untouched: still connected on both sides through a
    // window of polling, and documents still travel both ways. (That it is
    // the same connection, never reported closed, is the next case.)
    for _ in 0..25 {
        assert!(connected(&newer, older_id), "the newer side dropped");
        assert!(connected(&older, newer_id), "the older side dropped");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    replicates(
        newer_store.clone(),
        newer.clone(),
        older_store.clone(),
        "after the refused Bot dial the newer build's document did not reach the older one",
    )
    .await;
    replicates(
        older_store.clone(),
        older.clone(),
        newer_store.clone(),
        "after the refused Bot dial the older build's document did not reach the newer one",
    )
    .await;
    newer.clear();
    older.clear();
}

/// The other direction of the compatibility case: **the older Koru dials
/// this build**, and this build never dials it. The connection it opens on
/// the replication ALPN is accepted by an endpoint that now also offers
/// `koru/bots/1`, and the two replicate in both directions.
#[tokio::test]
async fn m8_alpn_admission_an_older_peer_that_dials_this_build_replicates() {
    let workspace = WorkspaceId::new();
    let manifest = DocumentId::new();
    let relationship = RelationshipId::new();
    let newer_secret = ReplicationEndpointSecret::generate();
    let older_secret = ReplicationEndpointSecret::generate();
    let newer_id = DeviceId::new();
    let older_id = DeviceId::new();
    let newer_store = Arc::new(InventoryStore::new(workspace, manifest));
    let older_store = Arc::new(InventoryStore::new(workspace, manifest));
    let newer = RuntimeHandle::spawn_with_store(IrohNetworkConfig::isolated(), newer_store.clone());
    let older = RuntimeHandle::spawn_with_store(
        IrohNetworkConfig::isolated().without_alpn(BOTS_ALPN_V1),
        older_store.clone(),
    );
    newer.provision_endpoint(newer_secret.secret_bytes());
    older.provision_endpoint(older_secret.secret_bytes());
    wait_until(|| newer.endpoint_addr().is_some() && older.endpoint_addr().is_some()).await;
    let snapshot = |local, device, endpoint, own_device| AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: local,
        peers: vec![AuthorizedPeer {
            relationship_id: relationship,
            device_id: device,
            endpoint_id: endpoint,
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![workspace],
            via: None,
            display_name: None,
            build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
            own_device,
        }],
        local_build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
    };
    newer.replace_authorization(snapshot(
        newer_id,
        older_id,
        older_secret.endpoint_id(),
        true,
    ));
    older.replace_authorization(snapshot(
        older_id,
        newer_id,
        newer_secret.endpoint_id(),
        false,
    ));
    // Only the older one dials.
    older.connect_peer_at(newer_id, newer.endpoint_addr().unwrap());
    let connected = |runtime: &RuntimeHandle, peer| {
        matches!(
            runtime.status().peers.get(&peer),
            Some(PeerConnectionState::Connected { .. })
        )
    };
    wait_within(Duration::from_secs(10), || {
        connected(&newer, older_id) && connected(&older, newer_id)
    })
    .await;

    for (from, from_runtime, to, what) in [
        (
            older_store.clone(),
            older.clone(),
            newer_store.clone(),
            "the older build's document did not reach the newer one",
        ),
        (
            newer_store.clone(),
            newer.clone(),
            older_store.clone(),
            "the newer build's document did not reach the older one",
        ),
    ] {
        let document = DocumentId::new();
        from.add(document);
        from_runtime.notify_local_change();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !to.has(document) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{what}"));
    }
    assert!(connected(&newer, older_id) && connected(&older, newer_id));
    newer.clear();
    older.clear();
}

/// The same compatibility case seen at the connection: with the engine
/// recorder on both sides, the replication connection of an older peer is
/// the same connection, with no `disconnected`, before and after refused
/// Bot dials, and a replication stream still opens on it.
#[tokio::test]
async fn m8_alpn_admission_a_refused_bot_dial_does_not_touch_the_replication_connection() {
    let newer = Device::start(IrohNetworkConfig::isolated()).await;
    let older = Device::start(IrohNetworkConfig::isolated().without_alpn(BOTS_ALPN_V1)).await;
    link(&newer, &older, Grant::Own, Grant::OwnBeforeM8);
    replicate(&newer, &older).await;
    let newer_connection = newer.engine.connected();
    let older_connection = older.engine.connected();
    let older_sessions = older.engine.admitted();

    for _ in 0..3 {
        assert_eq!(
            newer.open_bots(older.id).await.unwrap_err(),
            koru_network_edge::EdgeError::Closed
        );
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(newer.engine.connected(), newer_connection);
    assert_eq!(older.engine.connected(), older_connection);
    assert_eq!(newer.engine.disconnected(), 0);
    assert_eq!(older.engine.disconnected(), 0);
    assert!(newer.connected_to(older.id) && older.connected_to(newer.id));
    // The older peer was handed nothing new: no Bot stream reached it.
    assert_eq!(older.engine.admitted(), older_sessions);

    // A replication stream still opens on the same connection.
    let stream = newer
        .runtime
        .open_product_session(
            koru_network_edge::NetworkTarget::Device {
                device_id: older.id,
            },
            replication_protocol(),
        )
        .await
        .expect("the replication connection is live");
    assert_eq!(stream.connection_id, newer_connection[0]);
    stream.send.send(b"frame".to_vec()).await.unwrap();
    let mut served = older.engine.next_session().await;
    assert_eq!(served.protocol, replication_protocol());
    assert_eq!(served.connection_id, older_connection[0]);
    assert_eq!(read(&mut served, 5).await, b"frame");

    newer.runtime.clear();
    older.runtime.clear();
}

#[tokio::test]
async fn m8_revocation_closes_bots_when_the_owner_grant_changes() {
    let client = Device::start(IrohNetworkConfig::isolated()).await;
    let owner = Device::start(IrohNetworkConfig::isolated()).await;
    let relationship = link(&client, &owner, Grant::Own, Grant::Own);
    replicate(&client, &owner).await;

    let mut stream = client.open_bots(owner.id).await.expect("admitted");
    stream.send.send(b"hello".to_vec()).await.unwrap();
    let mut served = owner.engine.next_session().await;
    assert_eq!(read(&mut served, 5).await, b"hello");
    let admitted = owner.engine.admitted();

    // The owner's trust records change: the client is no longer an own
    // device (still linked, by a Profile).
    owner.authorize(
        2,
        vec![peer(
            relationship,
            client.id,
            client.secret.endpoint_id(),
            Grant::ProfileOnly,
        )],
    );

    // Both ends of the Bot stream end, with nothing more read.
    assert!(read_to_end(&mut stream).await.is_empty());
    assert!(read_to_end(&mut served).await.is_empty());

    // The client still believes the owner is its own device and dials
    // again. The owner closes that connection: no stream reaches its
    // engine, and whatever the client was handed carries nothing.
    for _ in 0..3 {
        if let Ok(mut retried) = client.open_bots(owner.id).await {
            let _ = retried.send.send(b"again".to_vec()).await;
            assert!(read_to_end(&mut retried).await.is_empty());
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let bot_sessions = owner
        .engine
        .sessions
        .lock()
        .unwrap()
        .iter()
        .filter(|session| session.protocol == bots_protocol())
        .count();
    assert_eq!(bot_sessions, 0, "a revoked device reached the engine");
    assert!(owner.engine.admitted() >= admitted);

    client.runtime.clear();
    owner.runtime.clear();
}

#[tokio::test]
async fn m8_revocation_closes_bots_on_any_change_of_the_grant_and_readmits_after() {
    let client = Device::start(IrohNetworkConfig::isolated()).await;
    let owner = Device::start(IrohNetworkConfig::isolated()).await;
    let relationship = RelationshipId::new();
    let granted = peer(
        relationship,
        client.id,
        client.secret.endpoint_id(),
        Grant::Own,
    );
    client.authorize(
        1,
        vec![peer(
            relationship,
            owner.id,
            owner.secret.endpoint_id(),
            Grant::Own,
        )],
    );
    owner.authorize(1, vec![granted.clone()]);
    replicate(&client, &owner).await;

    let mut stream = client.open_bots(owner.id).await.expect("admitted");
    stream.send.send(b"hello".to_vec()).await.unwrap();
    let mut served = owner.engine.next_session().await;
    assert_eq!(read(&mut served, 5).await, b"hello");

    // The same grant sent again (a new control session) closes nothing.
    let same = granted.clone();
    owner.authorize(2, vec![same.clone()]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    stream.send.send(b"still".to_vec()).await.unwrap();
    assert_eq!(read(&mut served, 5).await, b"still");

    // A change that keeps the device an own device (one more workspace)
    // replaces the grant. Every connection admitted under the old one is
    // closed, the Bot connection with them.
    let mut wider = same.clone();
    wider.workspaces.push(WorkspaceId::new());
    owner.authorize(3, vec![wider]);
    assert!(read_to_end(&mut stream).await.is_empty());
    assert!(read_to_end(&mut served).await.is_empty());

    // The device is still an own device, so a new stream is admitted, under
    // the new revision.
    let reopened = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(stream) = client.open_bots(owner.id).await {
                if stream.send.send(b"back".to_vec()).await.is_ok() {
                    return stream;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("readmitted");
    let mut served = owner.engine.next_session().await;
    while served.protocol != bots_protocol() {
        served = owner.engine.next_session().await;
    }
    assert_eq!(read(&mut served, 4).await, b"back");
    assert_eq!(served.principal.device().unwrap().authorization_revision, 3);
    drop(reopened);
    client.runtime.clear();
    owner.runtime.clear();
}

#[tokio::test]
async fn m8_revocation_closes_bots_when_the_client_grant_changes() {
    let client = Device::start(IrohNetworkConfig::isolated()).await;
    let owner = Device::start(IrohNetworkConfig::isolated()).await;
    let relationship = link(&client, &owner, Grant::Own, Grant::Own);
    replicate(&client, &owner).await;

    let mut stream = client.open_bots(owner.id).await.expect("admitted");
    stream.send.send(b"hello".to_vec()).await.unwrap();
    let mut served = owner.engine.next_session().await;
    assert_eq!(read(&mut served, 5).await, b"hello");

    // On the client the owner stops being an own device. The connection
    // the client opened is closed from its side, and it does not dial
    // again.
    client.authorize(
        2,
        vec![peer(
            relationship,
            owner.id,
            owner.secret.endpoint_id(),
            Grant::Introduced,
        )],
    );
    assert!(read_to_end(&mut stream).await.is_empty());
    assert!(read_to_end(&mut served).await.is_empty());
    assert_eq!(
        client.open_bots(owner.id).await.unwrap_err(),
        koru_network_edge::EdgeError::Closed
    );

    client.runtime.clear();
    owner.runtime.clear();
}

#[tokio::test]
async fn m8_no_connection_event_for_a_bot_connection() {
    let client = Device::start(IrohNetworkConfig::isolated()).await;
    let owner = Device::start(IrohNetworkConfig::isolated()).await;
    link(&client, &owner, Grant::Own, Grant::Own);
    replicate(&client, &owner).await;
    // The replication connection is the one event on each side.
    let client_events = client.engine.connected();
    let owner_events = owner.engine.connected();
    assert_eq!((client_events.len(), owner_events.len()), (1, 1));

    let stream = client.open_bots(owner.id).await.expect("admitted");
    stream.send.send(b"hello".to_vec()).await.unwrap();
    let mut served = owner.engine.next_session().await;
    assert_eq!(read(&mut served, 5).await, b"hello");
    assert_ne!(served.connection_id, owner_events[0]);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Opening it sent no `connected`, on either side.
    assert_eq!(client.engine.connected(), client_events);
    assert_eq!(owner.engine.connected(), owner_events);
    assert_eq!(client.engine.disconnected(), 0);
    assert_eq!(owner.engine.disconnected(), 0);
    // The peer's replication state did not move either.
    assert!(client.connected_to(owner.id) && owner.connected_to(client.id));

    client.runtime.clear();
    owner.runtime.clear();
}

#[tokio::test]
async fn m8_no_connection_event_when_a_bot_connection_is_the_only_one() {
    // No replication connection exists here, so any event at all would be
    // the Bot connection's.
    let client = Device::start(IrohNetworkConfig::isolated()).await;
    let owner = Device::start(IrohNetworkConfig::isolated()).await;
    link(
        &client,
        &owner,
        Grant::Own,
        Grant::OwnWithNothingToReplicate,
    );
    client.runtime.connect_peer_at(owner.id, owner.addr());

    let mut stream = client.open_bots(owner.id).await.expect("admitted");
    stream.send.send(b"hello".to_vec()).await.unwrap();
    let mut served = owner.engine.next_session().await;
    assert_eq!(read(&mut served, 5).await, b"hello");
    assert!(owner.engine.connected().is_empty());

    // It closes, from the client's end, and still no event is sent.
    client.runtime.clear();
    assert!(read_to_end(&mut served).await.is_empty());
    assert!(read_to_end(&mut stream).await.is_empty());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(owner.engine.connected().is_empty());
    assert_eq!(owner.engine.disconnected(), 0);
    assert_eq!(owner.engine.admitted(), 1);
    owner.runtime.clear();
}
