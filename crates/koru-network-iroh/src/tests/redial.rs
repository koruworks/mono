// SPDX-License-Identifier: MPL-2.0

//! The runtime's redial behavior against a peer that closes every connection
//! on arrival — the older build in the maintainers' 2026-10-10 log. The schedule's
//! numbers and report rules are unit-tested in `reconnect/tests.rs`; these
//! hold the runtime to them over real endpoints.

use super::*;

/// An older peer as the maintainers' Mac behaved: it completes every handshake on
/// the replication ALPN and closes the connection on arrival. Counts the
/// connections it accepted — every one is a dial of ours.
struct ClosingPeer {
    endpoint: Endpoint,
    secret: ReplicationEndpointSecret,
    accepted: Arc<AtomicUsize>,
}

impl ClosingPeer {
    async fn bind() -> Self {
        let secret = ReplicationEndpointSecret::generate();
        let endpoint = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&secret.secret_bytes()))
            .alpns(vec![REPLICATION_ALPN_V1.to_vec()])
            .bind()
            .await
            .unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepting = endpoint.clone();
        let counted = Arc::clone(&accepted);
        tokio::spawn(async move {
            while let Some(incoming) = accepting.accept().await {
                let counted = Arc::clone(&counted);
                tokio::spawn(async move {
                    let Ok(accepting) = incoming.accept() else {
                        return;
                    };
                    let Ok(connection) = accepting.await else {
                        return;
                    };
                    counted.fetch_add(1, Ordering::SeqCst);
                    // Long enough for the dialer to install it as current,
                    // far inside `EARLY_DEATH`.
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    connection.close(0u32.into(), b"older build refuses");
                });
            }
        });
        Self {
            endpoint,
            secret,
            accepted,
        }
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    /// Dial `runtime` the way the older peer's own retry loop did, and close
    /// the connection once the runtime has had time to install it.
    async fn dial_and_close(&self, runtime: &RuntimeHandle) {
        let connection = self
            .endpoint
            .connect(runtime.endpoint_addr().unwrap(), REPLICATION_ALPN_V1)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        connection.close(0u32.into(), b"older build refuses");
    }
}

/// `local` on our protocol, linked to `peer` advertising `peer_protocol`.
fn snapshot_with_peer_on(
    local: DeviceId,
    peer: DeviceId,
    endpoint_id: ReplicationEndpointId,
    peer_protocol: u32,
) -> AuthorizationSnapshot {
    AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: local,
        peers: vec![AuthorizedPeer {
            relationship_id: RelationshipId::new(),
            device_id: peer,
            endpoint_id,
            all_data: true,
            authorized_profiles: Vec::new(),
            workspaces: vec![WorkspaceId::new()],
            via: None,
            display_name: Some("Older".into()),
            build: Some(build_on_protocol(peer_protocol)),
            own_device: false,
        }],
        local_build: Some(build_on_protocol(PEER_PROTOCOL_VERSION)),
    }
}

async fn runtime_linked_to(
    closing: &ClosingPeer,
    peer_protocol: u32,
) -> (RuntimeHandle, DeviceId, DeviceId) {
    let local_secret = ReplicationEndpointSecret::generate();
    let runtime = RuntimeHandle::spawn(IrohNetworkConfig::isolated());
    runtime.provision_endpoint(local_secret.secret_bytes());
    wait_until(|| runtime.endpoint_addr().is_some()).await;
    let local = DeviceId::new();
    let peer = DeviceId::new();
    runtime.replace_authorization(snapshot_with_peer_on(
        local,
        peer,
        closing.secret.endpoint_id(),
        peer_protocol,
    ));
    (runtime, local, peer)
}

async fn wait_for_accepted(closing: &ClosingPeer, count: usize, within: Duration) {
    tokio::time::timeout(within, async {
        while closing.accepted() < count {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "expected {count} dials within {within:?}, saw {}",
            closing.accepted()
        )
    });
}

async fn wait_for_backing_off(runtime: &RuntimeHandle, peer: DeviceId) {
    wait_until(|| runtime.status().peers.get(&peer) == Some(&PeerConnectionState::BackingOff))
        .await;
}

/// The maintainers' log: an older peer that closes every connection was redialed
/// within milliseconds, for hours. Its connection dies on first use and the
/// next dial waits the incompatible schedule — thirty seconds, not the
/// quarter second a network blip gets — while the peer's own dials are
/// still accepted, so an updated peer is never kept waiting by us.
#[tokio::test]
async fn a_refused_peer_that_closes_on_arrival_is_not_redialed_at_once() {
    let closing = ClosingPeer::bind().await;
    let (runtime, _, peer) = runtime_linked_to(&closing, 4).await;
    runtime.connect_peer_at(peer, closing.endpoint.addr());
    wait_for_accepted(&closing, 1, Duration::from_secs(2)).await;
    wait_for_backing_off(&runtime, peer).await;

    // The transient schedule would have dialed three more times by now.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(closing.accepted(), 1, "a refused peer was redialed at once");
    assert_eq!(runtime.status().backoff_failures.get(&peer), Some(&1));

    // Inbound is never held back: the peer's own dial lands during backoff.
    let inbound = closing
        .endpoint
        .connect(runtime.endpoint_addr().unwrap(), REPLICATION_ALPN_V1)
        .await
        .unwrap();
    wait_until(|| {
        matches!(
            runtime.status().peers.get(&peer),
            Some(PeerConnectionState::Connected { .. })
        )
    })
    .await;
    inbound.close(0u32.into(), b"done");
    runtime.clear();
    closing.endpoint.close().await;
}

/// The backoff is for the incompatible case only: a compatible peer whose
/// connection dies on first use is redialed on the quarter-second schedule
/// it always had.
#[tokio::test]
async fn a_compatible_peer_that_closes_on_arrival_is_still_redialed_quickly() {
    let closing = ClosingPeer::bind().await;
    let (runtime, _, peer) = runtime_linked_to(&closing, PEER_PROTOCOL_VERSION).await;
    runtime.connect_peer_at(peer, closing.endpoint.addr());
    // 250 ms, then 500 ms, each plus at most a quarter.
    wait_for_accepted(&closing, 3, Duration::from_secs(4)).await;
    runtime.clear();
    closing.endpoint.close().await;
}

/// Reset rule (a): the peer's advertised build changing to one we sync with
/// ends the incompatible backoff at once — that grant is the update arriving.
#[tokio::test]
async fn a_refused_peer_that_updates_is_dialed_without_waiting_out_the_backoff() {
    let closing = ClosingPeer::bind().await;
    let (runtime, local, peer) = runtime_linked_to(&closing, 4).await;
    runtime.connect_peer_at(peer, closing.endpoint.addr());
    wait_for_accepted(&closing, 1, Duration::from_secs(2)).await;
    wait_for_backing_off(&runtime, peer).await;

    let mut updated = snapshot_with_peer_on(
        local,
        peer,
        closing.secret.endpoint_id(),
        PEER_PROTOCOL_VERSION,
    );
    updated.sequence = 2;
    runtime.replace_authorization(updated);
    wait_for_accepted(&closing, 2, Duration::from_secs(1)).await;
    runtime.clear();
    closing.endpoint.close().await;
}

/// The mechanism behind the storm. Every connection that died — including
/// each one the *peer* dialed — used to spawn another retry timer beside the
/// ones already waiting, and every timer dialed when it fired. A peer that
/// kills each connection on arrival multiplied them until dozens fired in one
/// burst. Now each close replaces the one pending retry: after the peer's
/// burst of dials, the next dial of ours waits the latest backoff.
#[tokio::test]
async fn each_closed_connection_replaces_the_pending_retry_rather_than_adding_one() {
    let closing = ClosingPeer::bind().await;
    let (runtime, _, peer) = runtime_linked_to(&closing, PEER_PROTOCOL_VERSION).await;
    runtime.connect_peer_at(peer, closing.endpoint.addr());
    wait_for_accepted(&closing, 1, Duration::from_secs(2)).await;

    // Six inbound connections, each dying on first use: six more early
    // deaths, each of which used to leave a timer behind.
    for _ in 0..6 {
        closing.dial_and_close(&runtime).await;
    }
    wait_until(|| {
        runtime
            .status()
            .backoff_failures
            .get(&peer)
            .is_some_and(|failures| *failures >= 6)
    })
    .await;
    let dialed = closing.accepted();
    // The latest retry waits at least 250 ms × 2^5 = 8 s. The stacked timers
    // of the old code — 0.5, 1, 2 and 4 s among them — would each have
    // dialed inside this window.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        closing.accepted() <= dialed + 1,
        "stacked retries dialed {} times in three seconds",
        closing.accepted() - dialed
    );
    runtime.clear();
    closing.endpoint.close().await;
}
