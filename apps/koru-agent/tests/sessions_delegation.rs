// SPDX-License-Identifier: MPL-2.0

//! **The delegation family over real IPC to a real host** — protocol v14's
//! non-creating lookup, key retirement, controller handover, cancel reason,
//! and the MCP provisioning a delegated Codex launch carries.
//!
//! The registry-level half of §7.6's recovery scenarios — the ones that pause
//! a creation between spawn and insert — lives beside the registry in
//! `koru-session-runtime`, where the window can be held open. What this file
//! proves is the wire: that each answer crosses as itself, that a peer without
//! the capability is refused with the stable code and nothing else changes,
//! and that the provider process is actually handed the rendering.

mod sessions_harness;

use std::str::FromStr;
use std::time::{Duration, Instant};

use koru_session_client::{ClientError, SessionClient};
use koru_session_protocol::{
    AgentEvent, BuiltInProfileKey, CancelReason, CapabilityName, CapabilitySet,
    ControlRequestOutcome, CreationLookup, DelegationLaunch, ErrorCode, GridSize, HandoverState,
    IdempotencyKey, McpToolAllowlist, PendingInput, ProtocolOffer, ProtocolRange, ProviderKind,
    RawCommand, SessionCommand, SessionEvent, SessionFailure, SessionId, SessionMode,
    SessionProfileKey, TakeoverUrgency, ViewportGeneration,
};
use koru_session_wire::SubscriptionUpdate;
use sessions_harness::{Host, fixture_codex, wait_until};

fn shell() -> SessionProfileKey {
    SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("platform-shell").unwrap())
}

fn size() -> GridSize {
    GridSize::new(80, 24).unwrap()
}

fn is_code(error: &ClientError, code: ErrorCode) -> bool {
    matches!(error, ClientError::Session(SessionFailure { code: seen, .. }) if *seen == code)
}

fn offer(max: u32, names: &[&str]) -> ProtocolOffer {
    ProtocolOffer {
        versions: ProtocolRange { min: 1, max },
        capabilities: CapabilitySet::new(
            names
                .iter()
                .map(|name| CapabilityName::from_str(name).unwrap()),
        ),
    }
}

fn terminate(client: &SessionClient, session_id: SessionId) {
    let (lease, _render) = client
        .attach_live(session_id, ViewportGeneration::new(1).unwrap(), 80, 1)
        .unwrap();
    client
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id,
            controller_lease: lease.token,
            action: RawCommand::Terminate,
        })
        .unwrap();
    // One lease per connection: forget this one so the next terminate on
    // the same client can attach again.
    let _ = client.detach(session_id, lease.token);
}

/// **The lookup never creates.** A miss is `NoEvidence` paired with the
/// host's incarnation and leaves the catalogue empty however often it is
/// asked; a hit is `Known` with the session the create returned; after the
/// session ends and is reaped it is `KnownEnded`, still from the same
/// incarnation, still with nothing spawned. And a retired key refuses the
/// create that arrives late, with the stable code.
#[test]
fn the_lookup_answers_from_evidence_and_never_creates() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let client = host.client("koru-delegate-test");
    let instance_id = client.host().instance_id.clone();
    let health = client.health().unwrap();
    assert_eq!(health.instance_id, instance_id);

    let key = IdempotencyKey::mint();
    for _ in 0..3 {
        let (seen, lookup) = client.lookup_creation(key).unwrap();
        assert_eq!(seen, instance_id);
        assert_eq!(lookup, CreationLookup::NoEvidence);
    }
    assert!(
        client.list().unwrap().is_empty(),
        "a miss spawned something"
    );

    let created = client
        .create_profile(key, shell(), None, root.path().to_path_buf(), size())
        .unwrap();
    let (seen, lookup) = client.lookup_creation(key).unwrap();
    assert_eq!(seen, instance_id);
    match lookup {
        CreationLookup::Known { session } => assert_eq!(session.id, created.id),
        other => panic!("a live creation answered {other:?}"),
    }
    assert_eq!(client.list().unwrap().len(), 1);

    terminate(&client, created.id);
    wait_until(|| client.list().unwrap().is_empty());
    let (seen, lookup) = client.lookup_creation(key).unwrap();
    assert_eq!(seen, instance_id);
    match lookup {
        CreationLookup::KnownEnded { session } => assert_eq!(session.id, created.id),
        other => panic!("an ended creation answered {other:?}"),
    }
    assert!(client.list().unwrap().is_empty(), "the lookup respawned");
    // The replay ledger still answers the ended creation too, without
    // spawning: the honest second answer to a retried create.
    let replayed = client
        .create_profile(key, shell(), None, root.path().to_path_buf(), size())
        .unwrap();
    assert_eq!(replayed.id, created.id);
    assert!(client.list().unwrap().is_empty());

    // Retirement fences a key nothing ever ran under: the late create is
    // refused, the lookup still says nothing ran, and nothing was spawned.
    let fresh = IdempotencyKey::mint();
    let (seen, lookup) = client.retire_creation(fresh).unwrap();
    assert_eq!(seen, instance_id);
    assert_eq!(lookup, CreationLookup::NoEvidence);
    let late = client
        .create_profile(fresh, shell(), None, root.path().to_path_buf(), size())
        .unwrap_err();
    assert!(is_code(&late, ErrorCode::CreationRetired), "{late}");
    assert_eq!(
        client.lookup_creation(fresh).unwrap().1,
        CreationLookup::NoEvidence
    );
    assert!(client.list().unwrap().is_empty());

    // Retiring a live key reports what is running so the caller reattaches
    // instead of spawning, and still fences a later create under it.
    let live = IdempotencyKey::mint();
    let running = client
        .create_profile(live, shell(), None, root.path().to_path_buf(), size())
        .unwrap();
    match client.retire_creation(live).unwrap().1 {
        CreationLookup::Known { session } => assert_eq!(session.id, running.id),
        other => panic!("retiring a live key answered {other:?}"),
    }
    let late = client
        .create_profile(live, shell(), None, root.path().to_path_buf(), size())
        .unwrap_err();
    assert!(is_code(&late, ErrorCode::CreationRetired), "{late}");
    assert_eq!(client.list().unwrap().len(), 1);
    terminate(&client, running.id);
    host.stop();
}

/// **Old client against new host, and the version-without-capability
/// half.** A v13 peer, and a v14 peer that did not ask for `delegation`,
/// are each answered `CapabilityNotNegotiated` for every verb of the
/// family — never a dropped connection — while every manual verb they
/// already had keeps working, and a v13 client's own summaries carry
/// nothing it cannot read.
#[test]
fn an_older_or_uncapable_peer_is_refused_the_family_and_keeps_manual_mode() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let manual = ["controller-leases", "profile-launch", "render-deltas"];
    let peers = [
        ("koru-workspace-v13", offer(13, &manual)),
        ("v14-without-delegation", offer(14, &manual)),
    ];
    for (product, peer_offer) in peers {
        let peer = SessionClient::connect(&host.endpoint, product, peer_offer).unwrap();
        assert!(!peer.supports_delegation(), "{product}");
        let key = IdempotencyKey::mint();
        let session_id = SessionId::mint();
        let lease = koru_session_protocol::ControllerLeaseToken::mint();
        let refusals = [
            peer.lookup_creation(key).map(|_| ()),
            peer.retire_creation(key).map(|_| ()),
            peer.request_control(session_id, TakeoverUrgency::AtTurnBoundary)
                .map(|_| ()),
            peer.release_control(session_id, lease),
            peer.return_control(session_id, lease),
        ];
        for refusal in refusals {
            let error = refusal.expect_err(product);
            assert!(
                is_code(&error, ErrorCode::CapabilityNotNegotiated),
                "{product}: {error}"
            );
        }
        // The connection survived every refusal, and manual mode is intact.
        let created = peer
            .create_profile(key, shell(), None, root.path().to_path_buf(), size())
            .unwrap();
        assert_eq!(created.handover, HandoverState::Idle);
        let (lease, _render) = peer
            .attach_live(created.id, ViewportGeneration::new(1).unwrap(), 80, 1)
            .unwrap();
        peer.detach(created.id, lease.token).unwrap();
        terminate(&peer, created.id);
        wait_until(|| peer.list().unwrap().is_empty());
    }
    host.stop();
}

/// **New client against a host without the capability.** The client
/// refuses to send a delegated create on a connection whose agreement lacks
/// the family — locally, before a frame is written — because the failure
/// mode on the other side is a provider spawned without its tools. Proved
/// with a connection that did not negotiate it, which is what an older host
/// yields. And the host, for its part, refuses a delegated create from a
/// peer that did not negotiate the capability.
#[test]
fn a_delegated_create_is_refused_without_the_capability_on_either_side() {
    let root = tempfile::tempdir().unwrap();
    let provider_path = fixture_codex(root.path());
    let host = Host::start_with_provider_path(root.path(), Some(&provider_path));
    let launch = DelegationLaunch {
        model: None,
        mcp_endpoint: "http://127.0.0.1:1/mcp".parse().unwrap(),
        credential: None,
        tools: McpToolAllowlist::default(),
        resume: None,
    };
    let provider = ProviderKind::from_str("codex").unwrap();
    let codex = SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("codex").unwrap());

    let uncapable = SessionClient::connect(
        &host.endpoint,
        "new-client-old-host",
        offer(
            koru_session_protocol::MAX_PROTOCOL_VERSION,
            &["profile-launch", "managed-sessions", "managed-events"],
        ),
    )
    .unwrap();
    let error = uncapable
        .create_delegated(
            IdempotencyKey::mint(),
            codex.clone(),
            provider.clone(),
            None,
            launch.clone(),
            root.path().to_path_buf(),
            size(),
        )
        .unwrap_err();
    assert!(
        is_code(&error, ErrorCode::CapabilityNotNegotiated),
        "{error}"
    );
    assert!(
        uncapable.list().unwrap().is_empty(),
        "the client sent it anyway"
    );

    // The host's half: the same frame, sent raw by a peer that holds every
    // managed capability but not `delegation`.
    let error = SessionClient::connect(
        &host.endpoint,
        "raw-sender",
        offer(
            koru_session_protocol::MAX_PROTOCOL_VERSION,
            &["profile-launch", "managed-sessions", "managed-events"],
        ),
    )
    .unwrap()
    .create_profile_delegated_unchecked(
        IdempotencyKey::mint(),
        codex,
        provider,
        launch,
        root.path().to_path_buf(),
        size(),
    )
    .unwrap_err();
    assert!(
        is_code(&error, ErrorCode::CapabilityNotNegotiated),
        "{error}"
    );
    assert!(uncapable.list().unwrap().is_empty());
    host.stop();
}

/// **A delegated Codex launch reaches the provider process with its MCP
/// rendering, and the manual launch is untouched.** The fixture shim traces
/// its argv; the delegated create carries the endpoint and the allowlist as
/// `-c` overrides and turns multi-agent off, while a manual managed create
/// on the same host carries none of them. Raw PTY is refused a delegation.
/// A bearer no real token could be mistaken for, so a leak into argv or a
/// trace is unmistakable.
const BEARER_SENTINEL: &str = "koru_run_BEARER-SENTINEL-92741";

#[test]
fn a_delegated_codex_launch_carries_its_mcp_rendering_to_the_process() {
    let root = tempfile::tempdir().unwrap();
    let provider_path = fixture_codex(root.path());
    let trace = root.path().join("codex-fixture-trace.txt");
    let host = Host::start_with(
        root.path(),
        Some(&provider_path),
        &[("CODEX_FIXTURE_TRACE", trace.as_os_str())],
    );
    let client = host.client("koru-delegate-test");
    assert!(client.supports_delegation());
    let provider = ProviderKind::from_str("codex").unwrap();
    let codex = SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("codex").unwrap());

    let delegated = client
        .create_delegated(
            IdempotencyKey::mint(),
            codex.clone(),
            provider.clone(),
            None,
            DelegationLaunch {
                model: None,
                mcp_endpoint: "http://127.0.0.1:4321/v1/runs/fixture/mcp".parse().unwrap(),
                credential: Some(BEARER_SENTINEL.parse().unwrap()),
                tools: McpToolAllowlist::new(
                    [
                        "attach_to_run",
                        "create",
                        "create_version",
                        "get",
                        "read",
                        "search",
                    ]
                    .iter()
                    .map(|name| name.parse().unwrap()),
                )
                .unwrap(),
                resume: None,
            },
            root.path().to_path_buf(),
            size(),
        )
        .unwrap();
    assert_eq!(
        delegated.mode,
        SessionMode::Managed {
            provider: provider.clone()
        }
    );
    assert_eq!(delegated.handover, HandoverState::Idle);
    let model = delegated.model.expect("a managed session names its model");
    assert_eq!(
        model.requested.as_ref().map(|model| model.as_str()),
        Some("fixture-model")
    );
    assert_eq!(model.reported, None);

    let manual = client
        .create_profile_mode(
            IdempotencyKey::mint(),
            codex,
            None,
            SessionMode::Managed {
                provider: provider.clone(),
            },
            root.path().to_path_buf(),
            size(),
        )
        .unwrap();

    let traced = std::fs::read_to_string(&trace).unwrap();
    let launches: Vec<&str> = traced
        .lines()
        .filter(|line| line.starts_with("args:app-server"))
        .collect();
    assert_eq!(launches.len(), 2, "{traced}");
    let delegated_launch = launches
        .iter()
        .find(|line| line.contains("mcp_servers.koru_run.url"))
        .expect("the delegated launch reached the process");
    // The essentials, quoting aside: the fixture shim is PowerShell behind a
    // `.cmd`, and what PowerShell does to quotes is not what node does. The
    // rendering itself is pinned in `launch.rs`; what this proves is that it
    // reached the process.
    assert!(delegated_launch.contains("mcp_servers.koru_run.url="));
    assert!(delegated_launch.contains("http://127.0.0.1:4321/v1/runs/fixture/mcp"));
    assert!(delegated_launch.contains("mcp_servers.koru_run.enabled_tools=["));
    for tool in [
        "create",
        "create_version",
        "get",
        "read",
        "search",
        "attach_to_run",
    ] {
        assert!(
            delegated_launch.contains(tool),
            "{tool}: {delegated_launch}"
        );
    }
    assert!(delegated_launch.contains("mcp_servers.koru_run.bearer_token_env_var="));
    assert!(delegated_launch.contains("features.multi_agent=false"));

    // **The bearer is not on argv** (ADR-359). The launch names the
    // variable; the value reaches the child's environment and nowhere a
    // second process could read it. The fixture traces every argument it
    // was given and whether the variable arrived.
    assert!(
        !traced.contains(BEARER_SENTINEL),
        "the bearer was rendered where another process can read it"
    );
    // Exactly one process got it: the delegated app-server. Not the manual
    // one, and not either version probe — the variable is set on the child
    // Koru starts, never on this process, so nothing inherits it.
    assert_eq!(
        traced
            .lines()
            .filter(|line| *line == "bearer_env:set")
            .count(),
        1,
        "{traced}"
    );
    let manual_launch = launches
        .iter()
        .find(|line| !line.contains("mcp_servers"))
        .expect("the manual launch reached the process");
    assert!(!manual_launch.contains("-c"), "{manual_launch}");

    // Raw PTY is never delegatable (D4).
    let Err(error) = client.create_profile_delegated_unchecked(
        IdempotencyKey::mint(),
        shell(),
        provider,
        DelegationLaunch {
            model: None,
            mcp_endpoint: "http://127.0.0.1:1/mcp".parse().unwrap(),
            credential: None,
            tools: McpToolAllowlist::default(),
            resume: None,
        },
        root.path().to_path_buf(),
        size(),
    ) else {
        panic!("a raw PTY accepted a delegation");
    };
    assert!(
        matches!(&error, ClientError::Session(SessionFailure { code, .. }) if *code != ErrorCode::CapabilityNotNegotiated),
        "{error}"
    );

    terminate(&client, delegated.id);
    terminate(&client, manual.id);
    host.stop();
}

/// **UC6 on the wire: request, release, take, return, reclaim.** Two
/// connections — one holding the lease the way the orchestrator will, one
/// asking the way a window will. Each step is an answer the requester
/// reads and a `HandoverChanged` the holder's subscription receives; a
/// third, older connection subscribed to the same session is never sent the
/// tag it cannot read and keeps receiving everything else.
#[test]
fn control_is_handed_over_by_release_and_returned_by_the_person() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let holder = host.client("koru-orchestrator-test");
    let window = host.client("koru-window-test");
    let legacy = SessionClient::connect(
        &host.endpoint,
        "koru-workspace-v13",
        offer(13, &["controller-leases", "observation-cursors"]),
    )
    .unwrap();

    let session = holder
        .create_profile(
            IdempotencyKey::mint(),
            shell(),
            None,
            root.path().to_path_buf(),
            size(),
        )
        .unwrap();
    let holder_events = holder.subscribe_events(session.id).unwrap();
    let legacy_events = legacy.subscribe_events(session.id).unwrap();
    let (held, _render) = holder
        .attach_live(session.id, ViewportGeneration::new(1).unwrap(), 80, 1)
        .unwrap();

    // Free session: the answer is Available and nothing moves.
    let spare = host.client("koru-window-spare");
    let free = spare
        .create_profile(
            IdempotencyKey::mint(),
            shell(),
            None,
            root.path().to_path_buf(),
            size(),
        )
        .unwrap();
    assert_eq!(
        spare
            .request_control(free.id, TakeoverUrgency::AtTurnBoundary)
            .unwrap(),
        ControlRequestOutcome::Available
    );
    terminate(&spare, free.id);

    // Held session: the requester waits and the holder is told.
    assert_eq!(
        window
            .request_control(session.id, TakeoverUrgency::AtTurnBoundary)
            .unwrap(),
        ControlRequestOutcome::Waiting
    );
    assert_eq!(
        next_handover(&holder_events),
        HandoverState::Requested,
        "the holder was not told"
    );
    match window.attach_live(session.id, ViewportGeneration::new(1).unwrap(), 80, 1) {
        Err(error) => assert!(is_code(&error, ErrorCode::ControllerConflict), "{error}"),
        Ok(_) => panic!("the lease was taken from its holder"),
    }
    assert_eq!(
        summary(&holder, session.id).handover,
        HandoverState::Requested
    );

    // The holder releases at its boundary; the person takes the lease.
    holder.release_control(session.id, held.token).unwrap();
    assert_eq!(next_handover(&holder_events), HandoverState::Released);
    assert_eq!(
        summary(&window, session.id).handover,
        HandoverState::Released
    );
    let (taken, _render) = window
        .attach_live(session.id, ViewportGeneration::new(2).unwrap(), 80, 1)
        .unwrap();
    assert_eq!(next_handover(&holder_events), HandoverState::Taken);

    // Returning the lease, and the holder reclaiming it.
    window.return_control(session.id, taken.token).unwrap();
    assert_eq!(next_handover(&holder_events), HandoverState::Returned);
    let (reclaimed, _render) = holder
        .attach_live(session.id, ViewportGeneration::new(3).unwrap(), 80, 1)
        .unwrap();
    assert_eq!(next_handover(&holder_events), HandoverState::Idle);
    assert_eq!(summary(&holder, session.id).handover, HandoverState::Idle);

    // The v13 subscriber saw the session start and none of the handover.
    holder
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: session.id,
            controller_lease: reclaimed.token,
            action: RawCommand::Terminate,
        })
        .unwrap();
    let mut saw_exit = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match legacy_events.recv_timeout(Duration::from_millis(250)) {
            Ok(SubscriptionUpdate::Event { event }) => match event.event {
                SessionEvent::HandoverChanged { .. } => {
                    panic!("a v13 subscriber was sent a v14 tag")
                }
                SessionEvent::Exited { .. } => {
                    saw_exit = true;
                    break;
                }
                _ => {}
            },
            Ok(SubscriptionUpdate::Closed { .. }) | Err(ClientError::Disconnected) => break,
            _ => {}
        }
    }
    assert!(
        saw_exit,
        "the older subscriber stopped receiving what it can read"
    );
    assert_eq!(
        legacy.state(),
        koru_session_client::ConnectionState::Connected
    );
    host.stop();
}

/// **A window that vanishes has returned; a holder that vanishes has
/// released.** The two crash shortcuts of §7.4, produced by the connection
/// loss the host already handles: dropping the client detaches its lease.
#[test]
fn losing_a_connection_advances_the_handover_rather_than_wedging_it() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let holder = host.client("koru-orchestrator-test");
    let session = holder
        .create_profile(
            IdempotencyKey::mint(),
            shell(),
            None,
            root.path().to_path_buf(),
            size(),
        )
        .unwrap();
    let (held, _render) = holder
        .attach_live(session.id, ViewportGeneration::new(1).unwrap(), 80, 1)
        .unwrap();

    let window = host.client("koru-window-test");
    assert_eq!(
        window
            .request_control(session.id, TakeoverUrgency::AtTurnBoundary)
            .unwrap(),
        ControlRequestOutcome::Waiting
    );
    holder.release_control(session.id, held.token).unwrap();
    let (_taken, _render) = window
        .attach_live(session.id, ViewportGeneration::new(2).unwrap(), 80, 1)
        .unwrap();
    assert_eq!(summary(&holder, session.id).handover, HandoverState::Taken);
    // The window dies.
    drop(window);
    wait_until(|| summary(&holder, session.id).handover == HandoverState::Returned);
    let (reclaimed, _render) = holder
        .attach_live(session.id, ViewportGeneration::new(3).unwrap(), 80, 1)
        .unwrap();
    assert_eq!(summary(&holder, session.id).handover, HandoverState::Idle);

    // Now the holder dies while a request is pending.
    let window = host.client("koru-window-test");
    assert_eq!(
        window
            .request_control(session.id, TakeoverUrgency::AtTurnBoundary)
            .unwrap(),
        ControlRequestOutcome::Waiting
    );
    drop(reclaimed);
    drop(holder);
    wait_until(|| summary(&window, session.id).handover == HandoverState::Released);
    let (_taken, _render) = window
        .attach_live(session.id, ViewportGeneration::new(4).unwrap(), 80, 1)
        .unwrap();
    assert_eq!(summary(&window, session.id).handover, HandoverState::Taken);
    terminate_with(&window, session.id);
    host.stop();
}

/// **A takeover cancellation ends the step, not the job (D36).** Against
/// the managed fixture: a `Now` request from the window interrupts the
/// active turn, the `TurnCancelled` that comes back carries
/// `TakeoverBoundary`, and a cancel the holder sends itself with `Run`
/// reads `Run` — the reason travels, and a peer that omits it gets `Run`.
#[test]
fn a_takeover_cancellation_is_stamped_on_the_turn_it_ended() {
    let root = tempfile::tempdir().unwrap();
    let provider_path = fixture_codex(root.path());
    let host = Host::start_with_provider_path(root.path(), Some(&provider_path));
    let holder = host.client("koru-orchestrator-test");
    let window = host.client("koru-window-test");
    let provider = ProviderKind::from_str("codex").unwrap();
    let session = holder
        .create_profile_mode(
            IdempotencyKey::mint(),
            SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("codex").unwrap()),
            None,
            SessionMode::Managed { provider },
            root.path().to_path_buf(),
            size(),
        )
        .unwrap();
    let (held, _render) = holder
        .attach_live(session.id, ViewportGeneration::new(1).unwrap(), 80, 1)
        .unwrap();
    let events = holder
        .subscribe_agent_events_from(session.id, None)
        .unwrap();

    let submitted = holder
        .send_pending_input(
            session.id,
            held.token,
            PendingInput {
                model: None,
                idempotency_key: IdempotencyKey::mint(),
                text: "fixture turn".into(),
                attachment_ids: Vec::new(),
                mode: None,
                fast: None,
                reasoning: None,
                skills: Vec::new(),
            },
        )
        .unwrap();
    wait_until(|| summary(&holder, session.id).active_turn_id == Some(submitted.turn_id));

    assert_eq!(
        window
            .request_control(session.id, TakeoverUrgency::Now)
            .unwrap(),
        ControlRequestOutcome::Waiting
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stamped = None;
    while Instant::now() < deadline && stamped.is_none() {
        if let Ok(SubscriptionUpdate::AgentEvent { event }) =
            events.recv_timeout(Duration::from_millis(250))
            && let AgentEvent::TurnCancelled { turn_id, reason } = event.event
            && turn_id == submitted.turn_id
        {
            stamped = Some(reason);
        }
    }
    assert_eq!(stamped, Some(CancelReason::TakeoverBoundary));
    assert_eq!(
        summary(&holder, session.id).handover,
        HandoverState::Requested
    );

    // The holder's own cancel of a later turn, with the reason it means.
    let second = holder
        .send_pending_input(
            session.id,
            held.token,
            PendingInput {
                model: None,
                idempotency_key: IdempotencyKey::mint(),
                text: "second fixture turn".into(),
                attachment_ids: Vec::new(),
                mode: None,
                fast: None,
                reasoning: None,
                skills: Vec::new(),
            },
        )
        .unwrap();
    wait_until(|| summary(&holder, session.id).active_turn_id == Some(second.turn_id));
    holder
        .cancel_turn_for(
            session.id,
            held.token,
            IdempotencyKey::mint(),
            second.turn_id,
            CancelReason::Run,
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stamped = None;
    while Instant::now() < deadline && stamped.is_none() {
        if let Ok(SubscriptionUpdate::AgentEvent { event }) =
            events.recv_timeout(Duration::from_millis(250))
            && let AgentEvent::TurnCancelled { turn_id, reason } = event.event
            && turn_id == second.turn_id
        {
            stamped = Some(reason);
        }
    }
    assert_eq!(stamped, Some(CancelReason::Run));

    holder
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: session.id,
            controller_lease: held.token,
            action: RawCommand::Terminate,
        })
        .unwrap();
    host.stop();
}

fn summary(client: &SessionClient, session_id: SessionId) -> koru_session_protocol::SessionSummary {
    client
        .list()
        .unwrap()
        .into_iter()
        .find(|session| session.id == session_id)
        .expect("the session is listed")
}

fn next_handover(events: &koru_session_client::Subscription) -> HandoverState {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(SubscriptionUpdate::Event { event }) =
            events.recv_timeout(Duration::from_millis(250))
            && let SessionEvent::HandoverChanged { handover } = event.event
        {
            return handover;
        }
    }
    panic!("no handover event arrived");
}

fn terminate_with(client: &SessionClient, session_id: SessionId) {
    let (_, lease) = client.controller().expect("this client holds the lease");
    client
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id,
            controller_lease: lease.token,
            action: RawCommand::Terminate,
        })
        .unwrap();
}

/// **A run's end reaches the session-owned process tree while a person
/// holds control** (§7.6).
///
/// The orchestrator creates a delegated session and hands the lease to a
/// window the way §7.4 says. From then on it holds no lease, so it cannot
/// attach and cannot send the ordinary terminate — and a run a person
/// walked away from mid-takeover must not be a provider nobody can stop.
/// The delegated end takes no lease, because the lease belongs to whoever
/// borrowed a step and the session belongs to the run that created it. A
/// session no run created keeps the lease rule exactly as it has it.
#[test]
fn a_delegated_session_ends_with_its_run_even_while_a_person_holds_control() {
    let root = tempfile::tempdir().unwrap();
    let provider_path = fixture_codex(root.path());
    let host = Host::start_with_provider_path(root.path(), Some(&provider_path));
    let orchestrator = host.client("koru-delegate-test");
    let window = host.client("koru-window-test");
    let provider = ProviderKind::from_str("codex").unwrap();
    let codex = SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("codex").unwrap());

    let delegated = orchestrator
        .create_delegated(
            IdempotencyKey::mint(),
            codex,
            provider,
            None,
            DelegationLaunch {
                model: None,
                mcp_endpoint: "http://127.0.0.1:4321/runs/fixture/mcp".parse().unwrap(),
                credential: None,
                tools: McpToolAllowlist::default(),
                resume: None,
            },
            root.path().to_path_buf(),
            size(),
        )
        .unwrap();
    let manual = orchestrator
        .create_profile(
            IdempotencyKey::mint(),
            shell(),
            None,
            root.path().to_path_buf(),
            size(),
        )
        .unwrap();

    // §7.4: the orchestrator holds the lease, a person asks, the
    // orchestrator releases at its boundary, the person takes it.
    let (held, _render) = orchestrator
        .attach_live(delegated.id, ViewportGeneration::new(1).unwrap(), 80, 1)
        .unwrap();
    assert_eq!(
        window
            .request_control(delegated.id, TakeoverUrgency::AtTurnBoundary)
            .unwrap(),
        ControlRequestOutcome::Waiting
    );
    orchestrator
        .release_control(delegated.id, held.token)
        .unwrap();
    let (person, _person_render) = window
        .attach_live(delegated.id, ViewportGeneration::new(1).unwrap(), 80, 1)
        .unwrap();
    assert_eq!(
        summary(&orchestrator, delegated.id).handover,
        HandoverState::Taken
    );
    // The orchestrator cannot take the lease back, so the ordinary end is
    // shut to it. Nothing preempts a holder (§7.4).
    match orchestrator.attach_live(delegated.id, ViewportGeneration::new(1).unwrap(), 80, 1) {
        Err(error) => assert!(is_code(&error, ErrorCode::ControllerConflict), "{error}"),
        Ok(_) => panic!("the lease was taken from its holder"),
    }

    // A session no run created is refused: the lease rule is unchanged for
    // a manual managed profile and a raw terminal.
    match orchestrator.terminate_delegated(manual.id) {
        Err(error) => assert!(is_code(&error, ErrorCode::Forbidden), "{error}"),
        Ok(()) => panic!("an undelegated session ended without a lease"),
    }
    assert!(
        client_holds(&orchestrator, manual.id),
        "the refused end changed nothing"
    );

    // The run ends, and its session ends with it — the person's lease and
    // all.
    orchestrator.terminate_delegated(delegated.id).unwrap();
    wait_until(|| !client_holds(&orchestrator, delegated.id));
    // Once the registry has reaped it there is no session to end, and the
    // host says so rather than pretending: `session_not_found`, the same
    // answer every other verb gives for a session that is gone.
    match orchestrator.terminate_delegated(delegated.id) {
        Err(error) => assert!(is_code(&error, ErrorCode::SessionNotFound), "{error}"),
        Ok(()) => panic!("a reaped session answered as if it were live"),
    }
    let _ = person;

    terminate(&orchestrator, manual.id);
    host.stop();
}

/// Whether the host still lists the session.
fn client_holds(client: &SessionClient, session_id: SessionId) -> bool {
    client
        .list()
        .unwrap()
        .into_iter()
        .any(|summary| summary.id == session_id)
}
