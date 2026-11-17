// SPDX-License-Identifier: MPL-2.0

//! `m8_route_by_token` and the gateway half of `m8_bot_peers_port`.
//!
//! Every stream here is one half of `TransportSession::pair()`: no bridge,
//! no device network and no Store is opened. The admission port is the real
//! one, fed the authorization snapshot the Agent would send.

use std::sync::Mutex;

use koru_bot_client_wire::{
    Ack, Limits, OwnerHello, Role, Subscribe, client_capabilities, decode_body,
};
use koru_device::{DeviceId, RelationshipId, ReplicationEndpointId};
use koru_network_edge::{NetworkPrincipal, ProtocolToken};
use koru_sync_control::{AuthorizationSnapshot, AuthorizedPeer, ControlSessionId};

use super::*;
use crate::composition::route_accepted_session;

fn bots_token() -> ProtocolToken {
    ProtocolToken::parse(koru_bot_client_wire::PROTOCOL_TOKEN).unwrap()
}

fn device_principal(device: DeviceId) -> NetworkPrincipal {
    NetworkPrincipal::Device {
        device: DevicePrincipal {
            device_id: device,
            relationship_id: RelationshipId::new(),
            endpoint_id: ReplicationEndpointId::from_bytes([5; 32]),
            authorization_revision: 1,
        },
    }
}

/// One stream between a client and this gateway: the client's half, and the
/// half the bridge would hand the gateway, carrying the client's principal.
fn stream(client: DeviceId, token: ProtocolToken) -> (TransportSession, TransportSession) {
    let owner = DeviceId::new();
    TransportSession::pair(device_principal(owner), device_principal(client), token, 8)
}

fn authorized(device: DeviceId, own_device: bool) -> AuthorizedPeer {
    AuthorizedPeer {
        relationship_id: RelationshipId::new(),
        device_id: device,
        endpoint_id: ReplicationEndpointId::from_bytes([5; 32]),
        all_data: true,
        authorized_profiles: Vec::new(),
        workspaces: Vec::new(),
        via: None,
        display_name: None,
        build: None,
        own_device,
    }
}

/// The admission port, told what the Agent's snapshot says.
fn port(peers: Vec<AuthorizedPeer>) -> BotPeers {
    let port = BotPeers::new();
    port.replace_authorization(&AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 1,
        local_device: DeviceId::new(),
        peers,
        local_build: None,
    });
    port
}

fn hello(client: DeviceId, role: Role) -> ClientHello {
    ClientHello {
        min_version: 1,
        max_version: 1,
        capabilities: client_capabilities(),
        role,
        client_device: client,
        client_build: "test".to_owned(),
    }
}

async fn send(session: &TransportSession, message: WireMessage) {
    session.send.send(encode(&message).unwrap()).await.unwrap();
}

/// Everything the gateway wrote until it ended the stream, within a bound:
/// a stream that is left open fails the case.
async fn answers(session: &mut TransportSession) -> Vec<WireMessage> {
    let mut decoder = FrameDecoder::new();
    let mut messages = Vec::new();
    loop {
        let received = tokio::time::timeout(Duration::from_secs(5), session.receive.recv())
            .await
            .expect("the gateway answered and ended the stream; it did not hang");
        match received {
            Ok(Some(bytes)) => {
                decoder.push(&bytes).unwrap();
                while let Some(message) = decoder.next().unwrap() {
                    messages.push(message);
                }
            }
            Ok(None) | Err(_) => return messages,
        }
    }
}

/// The one refusal a stream was answered with.
async fn refusal_of(session: &mut TransportSession) -> Refused {
    let mut messages = answers(session).await;
    assert_eq!(messages.len(), 1, "one frame, then the end of the stream");
    match messages.remove(0) {
        WireMessage::Refused(refused) => refused,
        other => panic!("answered {}", other.type_name()),
    }
}

/// An owner server that records what it was handed and answers the hello
/// the way the real one will: with the scopes the adapter gave it.
#[derive(Default)]
struct RecordingOwner {
    served: Mutex<Vec<AdmittedBotStream>>,
}

impl BotOwnerServer for RecordingOwner {
    fn serve(&self, stream: AdmittedBotStream) {
        let supported: Vec<&str> = BUILT_CAPABILITIES
            .iter()
            .map(|capability| capability.name)
            .collect();
        let answer = match Negotiated::owner_side(
            &stream.hello,
            &stream.device.device_id,
            &stream.scopes,
            &supported,
        ) {
            Ok(negotiated) => WireMessage::OwnerHello(negotiated.owner_hello(
                DeviceId::new(),
                "0123456789abcdef0123456789abcdef".to_owned(),
                "fedcba9876543210fedcba9876543210".to_owned(),
                7,
                0,
            )),
            Err(refused) => WireMessage::Refused(refused),
        };
        stream
            .session
            .send
            .try_send(encode(&answer).unwrap())
            .unwrap();
        self.served.lock().unwrap().push(stream);
    }
}

impl RecordingOwner {
    async fn next(&self) -> AdmittedBotStream {
        for _ in 0..500 {
            if let Some(stream) = self.served.lock().unwrap().pop() {
                return stream;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the owner server was handed no stream");
    }
}

async fn owner_hello_of(session: &mut TransportSession) -> OwnerHello {
    let mut decoder = FrameDecoder::new();
    loop {
        let bytes = tokio::time::timeout(Duration::from_secs(5), session.receive.recv())
            .await
            .expect("answered")
            .expect("open")
            .expect("not ended");
        decoder.push(&bytes).unwrap();
        if let Some(message) = decoder.next().unwrap() {
            match message {
                WireMessage::OwnerHello(hello) => return hello,
                other => panic!("answered {}", other.type_name()),
            }
        }
    }
}

// ----------------------------------------------------------------------
// m8_route_by_token
// ----------------------------------------------------------------------

#[test]
fn m8_route_by_token_the_token_is_the_one_the_device_network_labels_a_bot_stream_with() {
    assert_eq!(koru_bot_client_wire::PROTOCOL_TOKEN, "koru/bots/1");
    assert_eq!(
        koru_network_iroh::bots_protocol().as_str(),
        koru_bot_client_wire::PROTOCOL_TOKEN
    );
    assert_eq!(
        koru_network_iroh::BOTS_ALPN_V1,
        koru_bot_client_wire::PROTOCOL_TOKEN.as_bytes()
    );
    // And the port's scope names are the protocol's.
    assert_eq!(koru_store_server::bot_peers::SCOPE_READ, scope::READ);
    assert_eq!(koru_store_server::bot_peers::SCOPE_CONTROL, scope::CONTROL);
    assert_eq!(
        koru_store_server::bot_peers::SCOPE_CONFIGURE,
        scope::CONFIGURE
    );
}

#[tokio::test]
async fn m8_route_by_token_a_bot_session_reaches_the_adapter_and_never_the_store_port() {
    let client = DeviceId::new();
    let bots = BotIngress::new(port(vec![authorized(client, true)]));
    let owner = Arc::new(RecordingOwner::default());
    bots.set_owner_server(owner.clone());
    let (store_tx, mut store_rx) = tokio::sync::mpsc::channel(8);

    let (mut client_half, gateway_half) = stream(client, bots_token());
    send(
        &client_half,
        WireMessage::ClientHello(hello(client, Role::Feed)),
    )
    .await;
    assert!(route_accepted_session(gateway_half, &store_tx, &bots).await);

    // The adapter took it: its owner server is handed the stream and the
    // client reads a hello.
    let served = owner.next().await;
    assert_eq!(served.session.protocol, bots_token());
    owner_hello_of(&mut client_half).await;
    // The Store worker's port received nothing.
    assert!(store_rx.try_recv().is_err());

    // The same when nothing serves Bots here: the stream is answered by the
    // adapter and is still not the Store worker's to reset.
    let unserved = BotIngress::new(port(vec![authorized(client, true)]));
    let (mut client_half, gateway_half) = stream(client, bots_token());
    send(
        &client_half,
        WireMessage::ClientHello(hello(client, Role::Feed)),
    )
    .await;
    assert!(route_accepted_session(gateway_half, &store_tx, &unserved).await);
    assert_eq!(
        refusal_of(&mut client_half).await.reason,
        refusal::BOTS_UNAVAILABLE
    );
    assert!(store_rx.try_recv().is_err());
}

#[tokio::test]
async fn m8_route_by_token_a_replication_session_is_unchanged() {
    let client = DeviceId::new();
    let bots = BotIngress::new(port(vec![authorized(client, true)]));
    let owner = Arc::new(RecordingOwner::default());
    bots.set_owner_server(owner.clone());
    let (store_tx, mut store_rx) = tokio::sync::mpsc::channel(8);

    // Replication, and every other token there is today, goes to the Store
    // worker as it did before M8: the same session, unread.
    for token in [
        koru_network_iroh::replication_protocol(),
        koru_network_iroh::cloud_content_protocol(),
        ProtocolToken::parse("koru/bots/2").unwrap(),
        ProtocolToken::parse("koru/bots/1/extra").unwrap(),
        ProtocolToken::parse("a-token-nobody-knows").unwrap(),
    ] {
        let (client_half, mut gateway_half) = stream(client, token.clone());
        gateway_half.connection_id = 91;
        gateway_half.session_id = 17;
        client_half.send.send(b"untouched".to_vec()).await.unwrap();
        assert!(route_accepted_session(gateway_half, &store_tx, &bots).await);

        let mut forwarded = store_rx.try_recv().expect("forwarded to the Store worker");
        assert_eq!(forwarded.protocol, token);
        assert_eq!(forwarded.connection_id, 91);
        assert_eq!(forwarded.session_id, 17);
        assert_eq!(forwarded.principal.device().unwrap().device_id, client);
        // Not a byte was read on the way.
        assert_eq!(
            forwarded.receive.recv().await.unwrap().unwrap(),
            b"untouched"
        );
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(owner.served.lock().unwrap().is_empty());

    // With the Store worker gone the forwarder is told to stop, as before.
    drop(store_rx);
    let (_client_half, gateway_half) = stream(client, koru_network_iroh::replication_protocol());
    assert!(!route_accepted_session(gateway_half, &store_tx, &bots).await);
    // A Bot session is still taken: it never depended on the Store port.
    let (_client_half, gateway_half) = stream(client, bots_token());
    assert!(route_accepted_session(gateway_half, &store_tx, &bots).await);
}

// ----------------------------------------------------------------------
// m8_bot_peers_port, at the adapter
// ----------------------------------------------------------------------

#[tokio::test]
async fn m8_bot_peers_port_a_principal_the_port_does_not_name_is_refused_not_own_device() {
    let own = DeviceId::new();
    let profile_only = DeviceId::new();
    let unlinked = DeviceId::new();
    let peers = port(vec![authorized(own, true), authorized(profile_only, false)]);
    let bots = BotIngress::new(peers.clone());
    let owner = Arc::new(RecordingOwner::default());
    bots.set_owner_server(owner.clone());

    for device in [profile_only, unlinked] {
        let (mut client_half, gateway_half) = stream(device, bots_token());
        send(
            &client_half,
            WireMessage::ClientHello(hello(device, Role::Feed)),
        )
        .await;
        bots.admit(gateway_half);
        let refused = refusal_of(&mut client_half).await;
        assert_eq!(refused.reason, refusal::NOT_OWN_DEVICE);
        assert_eq!((refused.min_version, refused.max_version), (None, None));
    }
    // The owner server never saw them.
    assert!(owner.served.lock().unwrap().is_empty());

    // Before the port was told anything it names nobody, so even the own
    // device is refused: the hello is never answered on a guess.
    let untold = BotIngress::new(BotPeers::new());
    untold.set_owner_server(owner.clone());
    let (mut client_half, gateway_half) = stream(own, bots_token());
    send(
        &client_half,
        WireMessage::ClientHello(hello(own, Role::Feed)),
    )
    .await;
    untold.admit(gateway_half);
    assert_eq!(
        refusal_of(&mut client_half).await.reason,
        refusal::NOT_OWN_DEVICE
    );

    // The port is read for each stream. Once the pairing is revoked, the
    // next stream of the same device is refused.
    let (mut client_half, gateway_half) = stream(own, bots_token());
    send(
        &client_half,
        WireMessage::ClientHello(hello(own, Role::Feed)),
    )
    .await;
    bots.admit(gateway_half);
    owner_hello_of(&mut client_half).await;
    peers.replace_authorization(&AuthorizationSnapshot {
        session_id: ControlSessionId::new(),
        sequence: 2,
        local_device: DeviceId::new(),
        peers: vec![authorized(own, false)],
        local_build: None,
    });
    let (mut client_half, gateway_half) = stream(own, bots_token());
    send(
        &client_half,
        WireMessage::ClientHello(hello(own, Role::Feed)),
    )
    .await;
    bots.admit(gateway_half);
    assert_eq!(
        refusal_of(&mut client_half).await.reason,
        refusal::NOT_OWN_DEVICE
    );
}

#[tokio::test]
async fn m8_bot_peers_port_the_hello_carries_the_ports_scopes() {
    let client = DeviceId::new();
    let peers = port(vec![authorized(client, true)]);
    let port_scopes = peers.current().client_scopes(&client).cloned().unwrap();
    let bots = BotIngress::new(peers);
    let owner = Arc::new(RecordingOwner::default());
    bots.set_owner_server(owner.clone());

    let (mut client_half, gateway_half) = stream(client, bots_token());
    let sent = hello(client, Role::Command);
    send(&client_half, WireMessage::ClientHello(sent.clone())).await;
    // A client may send its first message with the hello; it must reach the
    // owner server too.
    send(
        &client_half,
        WireMessage::Subscribe(Subscribe::fresh(DeviceId::new())),
    )
    .await;
    bots.admit(gateway_half);

    // The owner server is handed the hello, the authenticated device, the
    // port's scopes and a linked-device principal with exactly the gateway
    // scopes those stand for.
    let mut served = owner.next().await;
    assert_eq!(served.hello, sent);
    assert_eq!(served.device.device_id, client);
    assert_eq!(served.scopes, port_scopes);
    assert_eq!(
        served.principal.scopes,
        vec!["bots.read", "bots.run", "bots.configure"]
    );
    assert_eq!(served.principal.label, "linked-device");
    // What followed the hello was not lost.
    let mut pipelined = served.decoder.next().unwrap();
    while pipelined.is_none() {
        let bytes = served.session.receive.recv().await.unwrap().unwrap();
        served.decoder.push(&bytes).unwrap();
        pipelined = served.decoder.next().unwrap();
    }
    assert!(matches!(pipelined, Some(WireMessage::Subscribe(_))));

    // And the hello the client reads carries the port's scopes, in the
    // documented order, with the capabilities those scopes allow.
    let answer = owner_hello_of(&mut client_half).await;
    assert_eq!(answer.scopes, ["read", "control", "configure"]);
    assert_eq!(
        answer.scopes.iter().cloned().collect::<BTreeSet<_>>(),
        port_scopes
    );
    assert_eq!(answer.capabilities, client_capabilities());
    assert_eq!(answer.limits, Limits::this_build());
    let negotiated = Negotiated::client_side(&sent, &answer).unwrap();
    assert_eq!(negotiated.scopes(), &port_scopes);
}

#[test]
fn m8_bot_peers_port_the_principal_holds_only_what_the_ports_scopes_stand_for() {
    let scopes = |values: &[&str]| -> BTreeSet<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    };
    assert_eq!(
        linked_device_principal(&scopes(&["read", "control", "configure"])).scopes,
        vec!["bots.read", "bots.run", "bots.configure"]
    );
    assert_eq!(
        linked_device_principal(&scopes(&["read"])).scopes,
        vec!["bots.read"]
    );
    assert_eq!(
        linked_device_principal(&scopes(&["read", "control"])).scopes,
        vec!["bots.read", "bots.run"]
    );
    // A scope this build does not know grants nothing, and `bots.manage` is
    // never held.
    assert_eq!(
        linked_device_principal(&scopes(&["server.admin", "read"])).scopes,
        vec!["bots.read"]
    );
    assert!(
        linked_device_principal(&scopes(&["server.admin"]))
            .scopes
            .is_empty()
    );
}

// ----------------------------------------------------------------------
// With no owner server: a clear refusal, never a hang
// ----------------------------------------------------------------------

#[tokio::test]
async fn m8_bot_ingress_without_an_owner_server_answers_one_refusal_and_ends_the_stream() {
    let client = DeviceId::new();
    let bots = BotIngress::new(port(vec![authorized(client, true)]));

    // Every role of an own device: not ready, retry later.
    for role in [Role::Feed, Role::Command, Role::Live] {
        let (mut client_half, gateway_half) = stream(client, bots_token());
        send(&client_half, WireMessage::ClientHello(hello(client, role))).await;
        bots.admit(gateway_half);
        let refused = refusal_of(&mut client_half).await;
        assert_eq!(refused.reason, refusal::BOTS_UNAVAILABLE);
        assert_eq!((refused.min_version, refused.max_version), (None, None));
        // The stream is over: nothing more can be sent on it.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            client_half
                .send
                .send(encode(&WireMessage::Ack(Ack { cursor: 1 })).unwrap())
                .await
                .is_err()
        );
    }

    // The hello's own refusal comes before "not ready": a client that needs
    // an update, or that names another device, is told that.
    let (mut client_half, gateway_half) = stream(client, bots_token());
    let mut newer = hello(client, Role::Feed);
    (newer.min_version, newer.max_version) = (2, 3);
    send(&client_half, WireMessage::ClientHello(newer)).await;
    bots.admit(gateway_half);
    let refused = refusal_of(&mut client_half).await;
    assert_eq!(refused.reason, refusal::UNSUPPORTED_VERSION);
    assert_eq!(
        (refused.min_version, refused.max_version),
        (Some(1), Some(1))
    );

    let (mut client_half, gateway_half) = stream(client, bots_token());
    send(
        &client_half,
        WireMessage::ClientHello(hello(DeviceId::new(), Role::Feed)),
    )
    .await;
    bots.admit(gateway_half);
    assert_eq!(
        refusal_of(&mut client_half).await.reason,
        refusal::PRINCIPAL_MISMATCH
    );
}

#[tokio::test]
async fn m8_bot_ingress_a_stream_that_does_not_start_with_a_hello_is_a_protocol_error() {
    let client = DeviceId::new();
    let bots = BotIngress::new(port(vec![authorized(client, true)]));
    let owner = Arc::new(RecordingOwner::default());
    bots.set_owner_server(owner.clone());

    // Another message first.
    let (mut client_half, gateway_half) = stream(client, bots_token());
    send(
        &client_half,
        WireMessage::Subscribe(Subscribe::fresh(DeviceId::new())),
    )
    .await;
    bots.admit(gateway_half);
    assert_eq!(
        refusal_of(&mut client_half).await.reason,
        refusal::PROTOCOL_ERROR
    );

    // Bytes that are no frame of this protocol: a replication frame, say.
    let (mut client_half, gateway_half) = stream(client, bots_token());
    client_half
        .send
        .send(b"\x00\x00\x00\x0f{\"kind\":\"sync\"}".to_vec())
        .await
        .unwrap();
    bots.admit(gateway_half);
    assert_eq!(
        refusal_of(&mut client_half).await.reason,
        refusal::PROTOCOL_ERROR
    );

    // A frame announced as longer than the limit: refused at its length,
    // without waiting for a megabyte that may never come.
    let (mut client_half, gateway_half) = stream(client, bots_token());
    client_half
        .send
        .send(
            (koru_bot_client_wire::MAX_FRAME_BYTES as u32 + 1)
                .to_be_bytes()
                .to_vec(),
        )
        .await
        .unwrap();
    bots.admit(gateway_half);
    assert_eq!(
        refusal_of(&mut client_half).await.reason,
        refusal::PROTOCOL_ERROR
    );
    // A refused frame is itself a frame of the protocol.
    assert!(decode_body(br#"{"type":"refused","reason":"protocol_error"}"#).is_ok());
    assert!(owner.served.lock().unwrap().is_empty());
}

#[tokio::test]
async fn m8_bot_ingress_a_stream_that_says_nothing_is_closed() {
    let client = DeviceId::new();
    let bots = BotIngress::with_hello_timeout(
        port(vec![authorized(client, true)]),
        Duration::from_millis(100),
    );
    // The default is the contract's ten seconds.
    assert_eq!(limits::HELLO_TIMEOUT_MS, 10_000);

    // Nothing at all, and half a frame: both are closed at the deadline
    // with no frame written.
    for prefix in [&b""[..], &b"\x00\x00\x00\x40{\"type\":\"client_"[..]] {
        let (mut client_half, gateway_half) = stream(client, bots_token());
        if !prefix.is_empty() {
            client_half.send.send(prefix.to_vec()).await.unwrap();
        }
        bots.admit(gateway_half);
        assert!(answers(&mut client_half).await.is_empty());
    }

    // A client that hangs up first costs nothing.
    let (client_half, gateway_half) = stream(client, bots_token());
    drop(client_half);
    bots.admit(gateway_half);
    tokio::time::sleep(Duration::from_millis(50)).await;
}

// ----------------------------------------------------------------------
// m8_owner_wiring: the adapter in front of the AI worker's owner server
// ----------------------------------------------------------------------

/// An AI worker as the composition builds one for these cases: the mock
/// provider, an `ai.db` in a temporary folder, and the admission port.
fn ai_worker(
    root: &std::path::Path,
    peers: BotPeers,
    authority: Option<koru_ai_server::BotAuthorityAdapter>,
) -> koru_ai_server::WorkerHandle {
    let config =
        koru_ai_server::WorkerConfig::deterministic_mock(root.to_owned(), "internal-debug")
            .with_bot_peers(peers);
    let config = match authority {
        Some(authority) => config.with_bot_authority(Arc::new(authority)),
        None => config,
    };
    koru_ai_server::AiWorker::new(config).spawn().unwrap()
}

/// The wiring the composition does: the AI worker's owner server behind the
/// adapter. A stream the adapter admits is answered by the engine, under
/// this computer's own device and the port's scopes, and its feed is served;
/// a device the port does not name never reaches the engine.
#[tokio::test]
async fn m8_owner_wiring_an_admitted_stream_is_served_by_the_ai_workers_owner_server() {
    let memory = koru_store_server::memory::MemoryStore::open("bot-ingress-owner-server").unwrap();
    let this_computer = DeviceId::new();
    let authority = koru_ai_server::BotAuthorityAdapter::from_device(
        memory.client(),
        &this_computer.to_string(),
    )
    .expect("a device id is a trusted device");
    let (client, stranger) = (DeviceId::new(), DeviceId::new());
    let peers = port(vec![authorized(client, true), authorized(stranger, false)]);
    let root = tempfile::tempdir().unwrap();
    let worker = ai_worker(root.path(), peers.clone(), Some(authority));

    let bots = BotIngress::new(peers);
    let server = worker
        .bot_owner_server()
        .expect("a worker with a Bot authority and the port serves Bots");
    bots.set_owner_server(Arc::new(AiOwnerServer(server)));

    let (mut client_half, gateway_half) = stream(client, bots_token());
    send(
        &client_half,
        WireMessage::ClientHello(hello(client, Role::Feed)),
    )
    .await;
    bots.admit(gateway_half);
    let answer = owner_hello_of(&mut client_half).await;
    assert_eq!(answer.owner_device, this_computer);
    assert_eq!(answer.version, 1);
    assert_eq!(answer.scopes, ["read", "control", "configure"]);
    assert_eq!(answer.limits, Limits::this_build());
    assert_eq!(answer.head, 0);

    // The feed of a computer with no Bot: one gap, then one page that says
    // the copy is up to date and holds nothing.
    send(
        &client_half,
        WireMessage::Subscribe(Subscribe::fresh(this_computer)),
    )
    .await;
    let mut decoder = FrameDecoder::new();
    let mut read = Vec::new();
    while read.len() < 2 {
        let bytes = tokio::time::timeout(Duration::from_secs(10), client_half.receive.recv())
            .await
            .expect("the owner server answers the subscribe")
            .expect("the stream is open")
            .expect("the stream has not ended");
        decoder.push(&bytes).unwrap();
        while let Some(message) = decoder.next().unwrap() {
            read.push(message);
        }
    }
    assert!(matches!(&read[0], WireMessage::Gap(gap) if gap.owner_device == this_computer));
    match &read[1] {
        WireMessage::Changes(page) => {
            assert!(page.reset && page.caught_up && page.entries.is_empty());
            assert_eq!(page.owner_device, this_computer);
            assert_eq!(page.withheld_bots, Some(0));
        }
        other => panic!("answered {}", other.type_name()),
    }
    send(&client_half, WireMessage::Ack(Ack { cursor: 0 })).await;

    // A device the port does not name a client is refused by the adapter.
    let (mut other_half, gateway_half) = stream(stranger, bots_token());
    send(
        &other_half,
        WireMessage::ClientHello(hello(stranger, Role::Feed)),
    )
    .await;
    bots.admit(gateway_half);
    assert_eq!(
        refusal_of(&mut other_half).await.reason,
        refusal::NOT_OWN_DEVICE
    );

    drop(client_half);
    worker.shutdown();
    let _ = worker.join();
}

/// A worker that runs no Bots has no owner server, so the composition
/// installs none and the adapter keeps answering `bots_unavailable`.
#[tokio::test]
async fn m8_owner_wiring_a_worker_with_no_bot_authority_serves_no_linked_device() {
    let client = DeviceId::new();
    let peers = port(vec![authorized(client, true)]);
    let root = tempfile::tempdir().unwrap();
    let worker = ai_worker(root.path(), peers.clone(), None);
    assert!(worker.bot_owner_server().is_none());

    let bots = BotIngress::new(peers);
    let (mut client_half, gateway_half) = stream(client, bots_token());
    send(
        &client_half,
        WireMessage::ClientHello(hello(client, Role::Command)),
    )
    .await;
    bots.admit(gateway_half);
    assert_eq!(
        refusal_of(&mut client_half).await.reason,
        refusal::BOTS_UNAVAILABLE
    );
    worker.shutdown();
    let _ = worker.join();
}

// ----------------------------------------------------------------------
// m8_linked_switch: both directions of Bots on linked computers are closed
// ----------------------------------------------------------------------

/// The bridge link as the Bot client would use it: it counts every open
/// and refuses it.
#[derive(Default)]
struct CountingEgress {
    opened: std::sync::atomic::AtomicUsize,
}

impl koru_network_edge::NetworkEgress for CountingEgress {
    fn open(
        &self,
        _target: koru_network_edge::NetworkTarget,
        _protocol: ProtocolToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<TransportSession, koru_network_edge::EdgeError>>
                + Send
                + '_,
        >,
    > {
        self.opened
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Err(koru_network_edge::EdgeError::Closed) })
    }
}

/// The switch is `false`, and with it `false` the composition's own step
/// opens neither direction: with an AI worker that could do both (a Bot
/// authority, and an admission port that names an own device a client and
/// an owner), **nothing is dialed**, no mirror is started (so nothing ever
/// writes a copy table and the gateway's AI client is the worker's own, not
/// the routing port), and no owner server is installed, so a linked
/// device's stream is still answered `bots_unavailable`.
///
/// Flipping the constant fails this case, on purpose: it is flipped only
/// after the exposure review with a person.
#[tokio::test]
async fn m8_linked_switch_is_closed_and_nothing_serves_or_dials() {
    assert!(
        !crate::composition::LINKED_BOT_CONTROL_AVAILABLE,
        "Bots on linked computers stay closed until the exposure review"
    );
    let memory = koru_store_server::memory::MemoryStore::open("bot-linked-switch").unwrap();
    let this_computer = DeviceId::new();
    let authority = koru_ai_server::BotAuthorityAdapter::from_device(
        memory.client(),
        &this_computer.to_string(),
    )
    .expect("a device id is a trusted device");
    let linked = DeviceId::new();
    let peers = port(vec![authorized(linked, true)]);
    assert!(peers.current().sequence > 0 && peers.current().is_owner(&linked));
    let root = tempfile::tempdir().unwrap();
    let worker = ai_worker(root.path(), peers.clone(), Some(authority));
    let bots = BotIngress::new(peers);
    let egress = Arc::new(CountingEgress::default());

    crate::composition::connect_linked_bots(&worker, Some(&bots), Some(egress.clone()));

    // Long enough for a mirror, had one been started, to read the port and
    // dial the owner it names: its first dial has no pause before it.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        egress.opened.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "nothing dials a linked device while the switch is closed"
    );
    assert!(
        worker.linked_bots().is_none(),
        "no mirror was started, so no copy table is ever written"
    );
    // The serving direction: no owner server behind the adapter.
    let (mut client_half, gateway_half) = stream(linked, bots_token());
    send(
        &client_half,
        WireMessage::ClientHello(hello(linked, Role::Feed)),
    )
    .await;
    bots.admit(gateway_half);
    assert_eq!(
        refusal_of(&mut client_half).await.reason,
        refusal::BOTS_UNAVAILABLE
    );
    worker.shutdown();
    let _ = worker.join();
}
