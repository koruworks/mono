// SPDX-License-Identifier: MPL-2.0

use super::*;
use std::collections::BTreeSet;
use std::process::Command;

use koru_session_protocol::{BuiltInProfileKey, ProfileKind, RawCommand, SessionProfileKey};
use koru_session_wire::LaunchBootstrap;

#[test]
fn retired_render_reader_cannot_regress_viewport_or_terminal_generation() {
    let mut terminal = Some(TerminalGeneration::new(3).unwrap());
    let mut viewport = ViewportGeneration::new(5).unwrap();
    assert!(!advance_render_generations(
        &mut terminal,
        &mut viewport,
        TerminalGeneration::new(3).unwrap(),
        ViewportGeneration::new(4).unwrap(),
    ));
    assert_eq!(terminal, Some(TerminalGeneration::new(3).unwrap()));
    assert_eq!(viewport, ViewportGeneration::new(5).unwrap());

    assert!(advance_render_generations(
        &mut terminal,
        &mut viewport,
        TerminalGeneration::new(4).unwrap(),
        ViewportGeneration::new(5).unwrap(),
    ));
    assert_eq!(terminal, Some(TerminalGeneration::new(4).unwrap()));
    assert_eq!(viewport, ViewportGeneration::new(5).unwrap());
}

#[test]
fn rejected_viewport_replacement_keeps_the_live_stream_generation() {
    let mut viewport = ViewportGeneration::new(4).unwrap();
    let error = subscribe_next_viewport(&mut viewport, |candidate| {
        assert_eq!(candidate, ViewportGeneration::new(5).unwrap());
        Err::<(), _>(ClientError::Session(failure(ErrorCode::StaleGeneration)))
    })
    .unwrap_err();
    assert!(matches!(
        error,
        ClientError::Session(SessionFailure {
            code: ErrorCode::StaleGeneration,
            ..
        })
    ));
    assert_eq!(
        viewport,
        ViewportGeneration::new(4).unwrap(),
        "the still-live subscription must continue accepting generation four"
    );

    assert_eq!(
        subscribe_next_viewport(&mut viewport, Ok).unwrap(),
        ViewportGeneration::new(5).unwrap()
    );
    assert_eq!(viewport, ViewportGeneration::new(5).unwrap());
}

struct IsolatedHost {
    endpoint: koru_ipc::Endpoint,
    authority: String,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl IsolatedHost {
    fn start(root: &Path) -> Self {
        std::fs::create_dir_all(root.join("agent/sessions")).unwrap();
        std::fs::create_dir_all(root.join("store")).unwrap();
        let authority = uuid::Uuid::new_v4().to_string();
        let bootstrap = LaunchBootstrap::new(
            uuid::Uuid::new_v4().to_string(),
            koru_channel::segment().into(),
            root.to_path_buf(),
            root.join("agent/sessions"),
            root.join("store"),
            true,
            authority.clone(),
        );
        let endpoint = direct_endpoint(Some(root)).unwrap();
        let owned_root = root.to_path_buf();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let local = tokio::task::LocalSet::new();
            local
                .block_on(
                    &runtime,
                    koru_session_host::serve(bootstrap, Some(&owned_root), None),
                )
                .unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut backoff = Duration::from_millis(1);
        loop {
            if SessionClient::connect(&endpoint, "koru-session-backend-test", protocol_offer())
                .is_ok()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "isolated Session Host did not start"
            );
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_millis(20));
        }
        Self {
            endpoint,
            authority,
            thread: Some(thread),
        }
    }

    fn client(&self) -> SessionClient {
        SessionClient::connect(
            &self.endpoint,
            "koru-session-backend-test",
            protocol_offer(),
        )
        .unwrap()
    }

    fn stop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.client().stop_host(self.authority.clone());
            thread.join().unwrap();
        }
    }
}

impl Drop for IsolatedHost {
    fn drop(&mut self) {
        self.stop();
    }
}

fn interactive_shell(root: &Path) -> CreateSession {
    #[cfg(windows)]
    let (profile_key, argv) = (
        "powershell",
        vec![
            "powershell.exe".into(),
            "-NoLogo".into(),
            "-NoProfile".into(),
        ],
    );
    #[cfg(unix)]
    let (profile_key, argv) = ("platform-shell", vec!["/bin/sh".into()]);
    CreateSession {
        profile: SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str(profile_key).unwrap()),
        parent_session_id: None,
        profile_kind: ProfileKind::Shell,
        display_name: "Terminal raw fixture".into(),
        working_directory: root.to_path_buf(),
        mode: SessionMode::RawPty,
        attachment_delivery: koru_session_protocol::AttachmentDeliveryPolicy::Unsupported,
        capabilities: protocol_offer().capabilities,
        project_id: None,
        argv,
        environment: EnvironmentSpec::Inherit,
        size: GridSize::new(80, 24).unwrap(),
    }
}

fn render_contains(subscription: &Subscription, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(SubscriptionUpdate::Render { update }) =
            subscription.recv_timeout(Duration::from_millis(250))
        {
            let text = match update {
                RenderUpdate::Snapshot { snapshot } => snapshot.rows,
                RenderUpdate::Delta { delta } => delta.rows,
                RenderUpdate::Gap { .. } => Vec::new(),
            }
            .into_iter()
            .flat_map(|row| row.cells)
            .map(|cell| cell.text)
            .collect::<String>();
            if text.contains(marker) {
                return;
            }
        }
    }
    panic!("raw terminal render did not contain its fixture marker")
}

fn render_reaches_size(subscription: &Subscription, expected: GridSize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Ok(SubscriptionUpdate::Render {
            update: RenderUpdate::Snapshot { snapshot },
        }) = subscription.recv_timeout(Duration::from_millis(250))
            && snapshot.size == expected
        {
            return;
        }
    }
    panic!("raw terminal render did not acknowledge its resized grid")
}

fn wait_until_absent(client: &SessionClient, session_id: SessionId) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while client
        .list()
        .unwrap()
        .iter()
        .any(|current| current.id == session_id)
    {
        assert!(
            Instant::now() < deadline,
            "terminated session stayed in catalog"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn dispatch_raw_text(
    sender: &mut InteractionSender,
    receive: &mpsc::Receiver<BackendCommand>,
    client: &SessionClient,
    session_id: SessionId,
    lease: koru_session_protocol::ControllerLeaseToken,
    text: &str,
) {
    SessionSurfaceInteraction::dispatch(
        sender,
        SessionSurfaceIntent::Terminal(TerminalIntent::Raw(RawCommand::WriteText {
            text: text.into(),
        })),
    );
    let BackendCommand::Intent(TerminalIntent::Raw(action)) = receive.recv().unwrap() else {
        panic!("surface input did not stay a typed raw command")
    };
    send_raw(client, session_id, lease, action).unwrap();
}

#[test]
fn rooted_endpoints_are_isolated_and_no_gateway_is_constructed() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let a = direct_endpoint(Some(a.path())).unwrap();
    let b = direct_endpoint(Some(b.path())).unwrap();
    assert_ne!(a.name(), b.name());
    assert_eq!(a.service(), koru_ipc::SESSION_HOST);
    assert_eq!(b.service(), koru_ipc::SESSION_HOST);
    // **Session control never goes through the Gateway**, which is what
    // makes Gateway loss unable to touch a running session: this crate
    // speaks the private session wire and nothing else.
    //
    // The check reads every production module rather than the manifest: the
    // manifest is pinned by `normal_dependency_direction_is_the_declared_allowlist`
    // below, and this one holds the *code* to the same rule.
    for module in [
        include_str!("lib.rs"),
        include_str!("sink.rs"),
        include_str!("attachments.rs"),
    ] {
        let session_control = module
            .split("#[cfg(test)]")
            .next()
            .expect("the module's production half");
        assert!(
            !session_control.contains("koru_api_client"),
            "a session must never be reached through the Gateway"
        );
    }
    let manifest = include_str!("../Cargo.toml");
    assert!(!manifest.contains("reqwest"));
}

/// The crate is the seam a second host reuses, so its compile graph is the
/// contract (mirrors `koru-app-shell`'s allowlist test).
#[test]
fn normal_dependency_direction_is_the_declared_allowlist() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let section = manifest
        .split("[dependencies]")
        .nth(1)
        .unwrap()
        .split("[dev-dependencies]")
        .next()
        .unwrap();
    let actual: BTreeSet<_> = section
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
        .filter(|name| !name.is_empty() && !name.starts_with('#'))
        .map(|name| name.split('.').next().unwrap())
        .collect();
    let expected = BTreeSet::from([
        "image",
        "koru-channel",
        "koru-clipboard",
        "koru-ipc",
        "koru-session-client",
        "koru-session-profiles",
        "koru-session-protocol",
        "koru-session-view",
        "koru-session-wire",
        "tracing",
        "vello",
    ]);
    assert_eq!(
        actual, expected,
        "the allowlist is the compile graph, not a review comment"
    );
}

/// No window, no tab, no host application. The backend answers through a
/// [`BackendSink`] precisely so no UI toolkit ever enters this tree.
#[test]
fn normal_transitive_graph_names_no_window_and_no_tab() {
    let output = Command::new(env!("CARGO"))
        .args(["tree", "--manifest-path"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .args(["-e", "normal", "--prefix", "none"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8(output.stdout).unwrap();
    let packages: BTreeSet<_> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    for forbidden in [
        "winit",
        "koru-app-shell",
        "koru-window",
        "koru-ui",
        "koru-api-client",
    ] {
        assert!(
            !packages.contains(forbidden),
            "{forbidden} must not appear in koru-session-backend's normal dependency tree"
        );
    }
}

#[test]
fn resolved_ordinary_channel_root_meets_the_unrooted_session_host() {
    let Some(channel_root) = koru_channel::channel_root() else {
        return;
    };
    assert_eq!(
        direct_endpoint(Some(&channel_root)).unwrap(),
        direct_endpoint(None).unwrap()
    );
}

#[test]
fn isolated_session_endpoint_uses_the_product_service_rule() {
    let isolated = tempfile::tempdir().unwrap();
    assert_eq!(
        direct_endpoint(Some(isolated.path())).unwrap(),
        koru_ipc::Endpoint::for_service_channel_root(
            koru_ipc::SESSION_HOST,
            koru_channel::segment(),
            isolated.path(),
        )
        .unwrap()
    );
}

#[test]
fn interaction_queue_is_fixed_and_reports_pressure() {
    let (sender, _receive) = channel();
    for _ in 0..COMMAND_CAPACITY {
        assert!(sender.try_command(BackendCommand::Refresh));
    }
    assert!(!sender.try_command(BackendCommand::Refresh));
    assert!(sender.take_overflow());
    assert!(!sender.take_overflow());
}

#[test]
fn session_host_connection_recovers_when_the_supervised_host_is_late() {
    let (_sender, receive) = channel();
    let mut attempts = 0;
    let mut unavailable = 0;
    let connected = await_session_host(
        &receive,
        Duration::ZERO,
        || {
            attempts += 1;
            (attempts == 3)
                .then_some(42)
                .ok_or(ClientError::Disconnected)
        },
        |_| unavailable += 1,
        |_| panic!("no interaction was queued while connecting"),
    );
    assert_eq!(connected, Some(42));
    assert_eq!(attempts, 3);
    assert_eq!(unavailable, 1, "one outage produces one UI transition");
}

#[test]
fn closing_while_the_session_host_is_late_stops_retrying() {
    let (sender, receive) = channel();
    sender.detach();
    let mut attempts = 0;
    assert_eq!(
        await_session_host(
            &receive,
            Duration::ZERO,
            || {
                attempts += 1;
                Err::<(), _>(ClientError::Disconnected)
            },
            |_| {},
            |_| panic!("detach is a retry stop signal"),
        ),
        None
    );
    assert_eq!(attempts, 1);
}

#[test]
fn logging_closes_unknown_wire_error_tokens() {
    let sentinel = "private-transcript-or-provider-output";
    let code = ErrorCode::Unknown(sentinel.into());
    assert_eq!(closed_error_code(&code), "unknown");
    assert!(!closed_error_code(&code).contains(sentinel));
}

#[test]
fn v10_managed_and_presented_capabilities_are_explicit() {
    let offer = protocol_offer();
    // v10 added the negotiated `projects` capability (SP-3), and **SP-10
    // is the packet that consumes it**: Welcome lists recent projects, so
    // `ListProjects` has to be negotiated. Nothing else about projects is
    // offered — the mutation verbs, the tree and the placement of a launch
    // are SP-11's, and the capability grants a *read* rather than those.
    //
    // v11 added the workspace-lifecycle capability, which is the Gateway's
    // alone — the window offers the current max and omits it. v12 added a
    // second verb to that same capability (SP-15a's project rescope) and no
    // capability at all, so only the number below moved.
    //
    // **v13 is the window's own**: `SetProjectScope` is the orphan
    // area's second resolution, it is called from this window's context
    // menu, and it rides `projects` — which is already offered above, and
    // which names the project family rather than any one of its verbs. So
    // again only the number moves, and the pin is what makes that a
    // decision each time rather than a drift: a window that silently
    // offered a version it had not been taught to speak would negotiate
    // frames it cannot handle.
    //
    // **v14 is the delegation family** (ADR-341), and P1.16 is where the
    // window started offering it: the run views' *Take the session* and
    // *Give the session back* are `request_control` and `return_control`,
    // and the host refuses both to a peer that did not negotiate this. It
    // was withheld until a window could take control, and now one can.
    //
    // **Offering it changes nothing about manual managed profiles.** The
    // capability admits three verbs the window sends only from a run
    // surface, plus the `handover_changed` event this backend already
    // refreshed on; the window still creates no delegated session, which
    // is the orchestrator's alone. A v13 host negotiates none of it and a
    // manual terminal against one behaves exactly as it did.
    // **v16 is session origin** (AGENTUX, ADR-428): a chat's sessions carry
    // their origin and the AI service holds the control connection. This
    // backend offers the version and neither capability; the window creates
    // no chat-origin session and the host treats its creates as manual.
    // **v17 is a tool's own words** (ADR-431, ADR-432): optional fields on
    // the tool events a terminal view leaves unread; no capability.
    assert_eq!(offer.versions.max, 17);
    assert!(offer.capabilities.supports(capability::PROVIDER_MODELS));
    assert!(
        offer
            .capabilities
            .supports(koru_session_protocol::capability::DELEGATION)
    );
    assert!(
        offer
            .capabilities
            .supports(koru_session_protocol::capability::PROJECTS)
    );
    // The cursor form of the event subscription, so a status stream that
    // overflows is resumable rather than permanently silent (R2).
    assert!(
        offer
            .capabilities
            .supports(koru_session_protocol::capability::OBSERVATION_CURSORS)
    );
    assert!(
        !offer
            .capabilities
            .supports(koru_session_protocol::capability::WORKSPACE_LIFECYCLE)
    );
    for capability in [
        capability::MANAGED_SESSIONS,
        capability::MANAGED_EVENTS,
        capability::MANAGED_CANCEL,
        capability::MANAGED_APPROVAL,
        capability::PRESENTED_ATTENTION,
    ] {
        assert!(offer.capabilities.supports(capability), "{capability}");
    }
}

#[test]
fn one_surface_boundary_preserves_typed_managed_commands() {
    let (mut sender, receive) = channel();
    let turn_id = koru_session_protocol::AgentTurnId::mint();
    SessionSurfaceInteraction::dispatch(
        &mut sender,
        SessionSurfaceIntent::Managed(ManagedIntent::CancelTurn { turn_id }),
    );
    assert!(matches!(
        receive.try_recv().unwrap(),
        BackendCommand::ManagedIntent(ManagedIntent::CancelTurn { turn_id: observed })
            if observed == turn_id
    ));
}

#[test]
fn ordinary_raw_policy_profiles_launch_as_raw_ptys() {
    let profiles: Vec<_> = koru_session_profiles::built_in_profiles()
        .into_iter()
        .filter(|profile| {
            profile
                .provider
                .as_ref()
                .is_some_and(|provider| provider.as_str() != "codex")
        })
        .collect();
    assert_eq!(
        profiles
            .iter()
            .map(|profile| profile.provider.as_ref().unwrap().as_str())
            .collect::<Vec<_>>(),
        // Every active agent but Codex keeps its ordinary raw launch policy.
        // The loop below launches each as a raw PTY, so a provider added to
        // the catalogue is exercised here by being named here.
        [
            "claude",
            "opencode",
            "grok",
            "kimi",
            "copilot",
            "qwen",
            "antigravity",
            "muse",
        ]
    );
    for profile in profiles {
        let (created, fallback) = create_with_raw_fallback(
            &profile,
            LaunchIntent::Ordinary,
            &[],
            |_| panic!("unsupported provider was offered managed launch"),
            || Ok::<_, ClientError>("raw"),
        )
        .unwrap();
        assert_eq!(created, "raw");
        assert_eq!(fallback, None);
    }
}

#[test]
fn codex_uses_managed_only_after_successful_compatibility_handshake() {
    let profile = koru_session_profiles::built_in_profiles()
        .into_iter()
        .find(|profile| {
            profile
                .provider
                .as_ref()
                .is_some_and(|provider| provider.as_str() == "codex")
        })
        .unwrap();
    let (created, fallback) = create_with_raw_fallback(
        &profile,
        LaunchIntent::Ordinary,
        &[ready_profile(&profile)],
        |mode| {
            assert!(matches!(mode, SessionMode::Managed { .. }));
            Ok::<_, ClientError>("managed")
        },
        || panic!("compatible managed launch fell back"),
    )
    .unwrap();
    assert_eq!(created, "managed");
    assert_eq!(fallback, None);

    let (created, fallback) = create_with_raw_fallback(
        &profile,
        LaunchIntent::Ordinary,
        &[ready_profile(&profile)],
        |_| {
            Err(ClientError::Session(failure(
                ErrorCode::ProviderIncompatible,
            )))
        },
        || Ok("raw"),
    )
    .unwrap();
    assert_eq!(created, "raw");
    assert_eq!(fallback, Some(ErrorCode::ProviderIncompatible));

    let error = create_with_raw_fallback::<&str>(
        &profile,
        LaunchIntent::Ordinary,
        &[ready_profile(&profile)],
        |_| Err(ClientError::Session(failure(ErrorCode::ProviderFailed))),
        || panic!("provider execution failure was hidden by raw fallback"),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ClientError::Session(SessionFailure {
            code: ErrorCode::ProviderFailed,
            ..
        })
    ));

    let (created, fallback) = create_with_raw_fallback(
        &profile,
        LaunchIntent::Raw,
        &[],
        |_| panic!("explicit raw fallback attempted managed launch"),
        || Ok::<_, ClientError>("raw"),
    )
    .unwrap();
    assert_eq!(created, "raw");
    assert_eq!(fallback, None);
}

#[test]
fn built_in_raw_profiles_are_resolved_by_the_host_not_bare_terminal_argv() {
    let root = tempfile::tempdir().unwrap();
    let mut host = IsolatedHost::start(root.path());
    let client = host.client();
    let mut profile = koru_session_profiles::built_in_profiles()
        .into_iter()
        .find(|profile| {
            matches!(
                &profile.id,
                SessionProfileKey::BuiltIn(key) if key.as_str() == "platform-shell"
            )
        })
        .unwrap();
    // A built-in's argv is presentation metadata at this boundary. If the
    // Terminal bypasses CreateProfileSession, this deliberately impossible
    // command fails instead of letting the host resolve the platform shell.
    profile.argv = vec!["koru-command-that-must-not-be-spawned".into()];
    let session = create_raw_profile(
        &client,
        &profile,
        "Host-resolved raw profile".into(),
        root.path().to_path_buf(),
        GridSize::new(80, 24).unwrap(),
        None,
    )
    .unwrap();
    assert_eq!(session.mode, SessionMode::RawPty);
    let (lease, render) = attach_raw(
        &client,
        session.id,
        ViewportGeneration::new(1).unwrap(),
        80,
        2,
    )
    .unwrap();
    #[cfg(windows)]
    let marker = "echo KORU_HOST_RESOLVED_RAW\r";
    #[cfg(unix)]
    let marker = "echo KORU_HOST_RESOLVED_RAW\n";
    send_raw(
        &client,
        session.id,
        lease.token,
        RawCommand::WriteText {
            text: marker.into(),
        },
    )
    .unwrap();
    render_contains(&render, "KORU_HOST_RESOLVED_RAW");
    send_raw(&client, session.id, lease.token, RawCommand::Terminate).unwrap();
    drop(render);
    wait_until_absent(&client, session.id);
    client.disconnect();
    host.stop();
}

#[test]
fn raw_surface_types_detaches_reconnects_and_terminates_on_an_isolated_host() {
    let root = tempfile::tempdir().unwrap();
    let mut host = IsolatedHost::start(root.path());
    let client = host.client();
    let session = client.create(interactive_shell(root.path())).unwrap();
    let (lease, render) = attach_raw(
        &client,
        session.id,
        ViewportGeneration::new(1).unwrap(),
        80,
        2,
    )
    .unwrap();
    let (mut sender, receive) = channel();
    #[cfg(windows)]
    let first = "echo KORU_RAW_FIRST\r";
    #[cfg(unix)]
    let first = "echo KORU_RAW_FIRST\n";
    dispatch_raw_text(
        &mut sender,
        &receive,
        &client,
        session.id,
        lease.token,
        first,
    );
    render_contains(&render, "KORU_RAW_FIRST");

    let resized = GridSize::new(100, 30).unwrap();
    send_raw(
        &client,
        session.id,
        lease.token,
        RawCommand::Resize { size: resized },
    )
    .unwrap();
    render_reaches_size(&render, resized);
    #[cfg(windows)]
    let after_resize = "echo KORU_RAW_AFTER_RESIZE\r";
    #[cfg(unix)]
    let after_resize = "echo KORU_RAW_AFTER_RESIZE\n";
    dispatch_raw_text(
        &mut sender,
        &receive,
        &client,
        session.id,
        lease.token,
        after_resize,
    );
    render_contains(&render, "KORU_RAW_AFTER_RESIZE");

    client.detach(session.id, lease.token).unwrap();
    client.disconnect();
    drop(render);

    let replacement = host.client();
    let (replacement_lease, replacement_render) = attach_raw(
        &replacement,
        session.id,
        // A fresh controller may start at one; generations do not leak
        // across a detached controller lease.
        ViewportGeneration::new(1).unwrap(),
        80,
        2,
    )
    .unwrap();
    #[cfg(windows)]
    let second = "echo KORU_RAW_SECOND\r";
    #[cfg(unix)]
    let second = "echo KORU_RAW_SECOND\n";
    dispatch_raw_text(
        &mut sender,
        &receive,
        &replacement,
        session.id,
        replacement_lease.token,
        second,
    );
    render_contains(&replacement_render, "KORU_RAW_SECOND");

    sender.terminate();
    assert!(matches!(receive.recv().unwrap(), BackendCommand::Terminate));
    let mut active = Some(Active {
        session_id: session.id,
        lease: replacement_lease,
        terminal_generation: None,
        viewport_generation: ViewportGeneration::new(1).unwrap(),
        pending_viewport: None,
        resize_in_flight: None,
        mode: SessionMode::RawPty,
        epoch: 1,
        generation: 1,
        columns: 80,
        managed_client: None,
    });
    terminate(&replacement, &mut active).unwrap();
    assert!(active.is_none());
    assert!(replacement.controller().is_none());
    drop(replacement_render);
    wait_until_absent(&replacement, session.id);
    replacement.disconnect();
    host.stop();
}

#[test]
fn an_exit_winning_the_detach_race_does_not_poison_the_next_attach() {
    let root = tempfile::tempdir().unwrap();
    let mut host = IsolatedHost::start(root.path());
    let client = host.client();
    let first = client.create(interactive_shell(root.path())).unwrap();
    let (first_lease, first_render) = attach_raw(
        &client,
        first.id,
        ViewportGeneration::new(1).unwrap(),
        80,
        2,
    )
    .unwrap();
    send_raw(&client, first.id, first_lease.token, RawCommand::Terminate).unwrap();
    drop(first_render);
    wait_until_absent(&client, first.id);

    let mut stale_active = Some(Active {
        session_id: first.id,
        lease: first_lease,
        terminal_generation: None,
        viewport_generation: ViewportGeneration::new(1).unwrap(),
        pending_viewport: None,
        resize_in_flight: None,
        mode: SessionMode::RawPty,
        epoch: 1,
        generation: 1,
        columns: 80,
        managed_client: None,
    });
    detach(&client, &mut stale_active).unwrap();
    assert!(stale_active.is_none());
    assert!(
        client.controller().is_none(),
        "a dead process cannot leave a local controller lease behind"
    );

    let next = client.create(interactive_shell(root.path())).unwrap();
    let (next_lease, next_render) =
        attach_raw(&client, next.id, ViewportGeneration::new(1).unwrap(), 80, 2).unwrap();
    send_raw(&client, next.id, next_lease.token, RawCommand::Terminate).unwrap();
    drop(next_render);
    wait_until_absent(&client, next.id);
    client.disconnect();
    host.stop();
}

// ------------------------------------------------------------------ W3b —
// connection-generation recovery. Every test here crosses the real wire
// against the isolated host fixture: a host is started, attached to, stopped
// and replaced, and the assertions are about what the backend *presents* —
// the truth of loss or resume — and about what it never does: replay.

/// The sink a recovery test hands the backend: every event lands on a channel
/// the test drains, in delivery order.
#[derive(Clone)]
struct ChannelSink(mpsc::Sender<BackendEvent>);

impl BackendSink for ChannelSink {
    fn deliver(&self, event: BackendEvent) -> bool {
        self.0.send(event).is_ok()
    }
}

fn event_label(event: &BackendEvent) -> &'static str {
    match event {
        BackendEvent::Profiles(_) => "Profiles",
        BackendEvent::Providers(_) => "Providers",
        BackendEvent::ProviderModels { .. } => "ProviderModels",
        BackendEvent::Connected(_) => "Connected",
        BackendEvent::Sessions(_) => "Sessions",
        BackendEvent::Projects(_) => "Projects",
        BackendEvent::Attached { .. } => "Attached",
        BackendEvent::Observing { .. } => "Observing",
        BackendEvent::RawFallback { .. } => "RawFallback",
        BackendEvent::Terminated { .. } => "Terminated",
        BackendEvent::Render(_) => "Render",
        BackendEvent::ManagedBatch { .. } => "ManagedBatch",
        BackendEvent::ManagedGap { .. } => "ManagedGap",
        BackendEvent::ManagedCommandFailure { .. } => "ManagedCommandFailure",
        BackendEvent::ManagedStreamFailure { .. } => "ManagedStreamFailure",
        BackendEvent::Scrollback(_) => "Scrollback",
        BackendEvent::Attachments { .. } => "Attachments",
        BackendEvent::AttachmentIngesting { .. } => "AttachmentIngesting",
        BackendEvent::AttachmentIngested { .. } => "AttachmentIngested",
        BackendEvent::AttachmentPreview { .. } => "AttachmentPreview",
        BackendEvent::AttachmentRemoved { .. } => "AttachmentRemoved",
        BackendEvent::PendingInputSent { .. } => "PendingInputSent",
        BackendEvent::PendingInputFailure { .. } => "PendingInputFailure",
        BackendEvent::PendingDraftPaste { .. } => "PendingDraftPaste",
        BackendEvent::TerminalPaste { .. } => "TerminalPaste",
        BackendEvent::OperationFailure { .. } => "OperationFailure",
        BackendEvent::Failure(_) => "Failure",
        BackendEvent::Handover { .. } => "Handover",
        BackendEvent::Disconnected => "Disconnected",
        BackendEvent::Recovered { .. } => "Recovered",
        BackendEvent::SessionsLost { .. } => "SessionsLost",
        BackendEvent::Backpressure => "Backpressure",
    }
}

/// Drain sink events until `pick` claims one, remembering what was skipped so
/// a failure names the traffic that actually happened.
fn wait_for_event<T>(
    events: &mpsc::Receiver<BackendEvent>,
    what: &str,
    mut pick: impl FnMut(BackendEvent) -> Option<T>,
) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut skipped = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(remaining) {
            Ok(event) => {
                let label = event_label(&event);
                if let Some(found) = pick(event) {
                    return found;
                }
                skipped.push(label);
            }
            Err(_) => break,
        }
    }
    panic!("{what} did not arrive; saw {skipped:?}")
}

fn render_text(update: &RenderUpdate) -> String {
    let rows = match update {
        RenderUpdate::Snapshot { snapshot } => snapshot.rows.clone(),
        RenderUpdate::Delta { delta } => delta.rows.clone(),
        RenderUpdate::Gap { .. } => Vec::new(),
    };
    rows.into_iter()
        .flat_map(|row| row.cells)
        .map(|cell| cell.text)
        .collect()
}

/// A marker only an *executed* command prints: the typed form is split, so the
/// contiguous spelling can only come from the shell running it. Its count in a
/// full snapshot is therefore the number of times the input was delivered —
/// which is how the no-replay guarantee becomes an assertion. The marker is
/// deliberately not `KORU_`-prefixed: koru-env's registry sentinel greps the
/// source for `KORU_*` identifiers, and a marker string is not an env var.
#[cfg(windows)]
const RAN_ONCE_INPUT: &str = "echo ('TERMRAN' + '_ONCE')\r";
#[cfg(unix)]
const RAN_ONCE_INPUT: &str = "echo \"TERMRAN\"\"_ONCE\"\n";
const RAN_ONCE_OUTPUT: &str = "TERMRAN_ONCE";

#[cfg(windows)]
const RAN_AGAIN_INPUT: &str = "echo ('TERMRAN' + '_AGAIN')\r";
#[cfg(unix)]
const RAN_AGAIN_INPUT: &str = "echo \"TERMRAN\"\"_AGAIN\"\n";
const RAN_AGAIN_OUTPUT: &str = "TERMRAN_AGAIN";

fn shell_launch_profile() -> LaunchProfile {
    // PowerShell on Windows — the split-marker inputs above are its syntax —
    // and the platform shell elsewhere.
    #[cfg(windows)]
    let key = "powershell";
    #[cfg(unix)]
    let key = "platform-shell";
    koru_session_profiles::built_in_profiles()
        .into_iter()
        .find(|profile| {
            matches!(
                &profile.id,
                SessionProfileKey::BuiltIn(found) if found.as_str() == key
            )
        })
        .expect("the built-in shell profile")
}

fn wait_detached(client: &SessionClient, session_id: SessionId) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let released = client.list().unwrap().into_iter().any(|summary| {
            summary.id == session_id
                && summary.controller == koru_session_protocol::ControllerState::Detached
        });
        if released {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the host did not release the dead connection's lease"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The pieces a recover-level test drives by hand: exactly the state `run`
/// owns, minus the loop.
struct RecoveryRig {
    endpoint: koru_ipc::Endpoint,
    sink: ChannelSink,
    events: mpsc::Receiver<BackendEvent>,
    #[allow(dead_code)]
    sender: InteractionSender,
    commands: mpsc::Receiver<BackendCommand>,
    internal: SyncSender<BackendCommand>,
    connection: Connection,
    generation: u64,
    active: Option<Active>,
    observing: Option<Observing>,
    next_epoch: u64,
    subscribed: BTreeSet<SessionId>,
}

impl RecoveryRig {
    fn connect(root: &Path) -> Self {
        let endpoint = direct_endpoint(Some(root)).unwrap();
        let (event_send, events) = mpsc::channel();
        let sink = ChannelSink(event_send);
        let (sender, commands) = channel();
        let internal = sender.internal_sender();
        let client = Arc::new(
            SessionClient::connect(&endpoint, "koru-session-backend", protocol_offer()).unwrap(),
        );
        let connection = establish(client, 1, &endpoint, &sink, &internal);
        Self {
            endpoint,
            sink,
            events,
            sender,
            commands,
            internal,
            connection,
            generation: 1,
            active: None,
            observing: None,
            next_epoch: 1,
            subscribed: BTreeSet::new(),
        }
    }

    fn attach(&mut self, session_id: SessionId) {
        attach(
            &self.connection.client,
            AttachContext {
                endpoint: &self.endpoint,
                sink: &self.sink,
                internal: &self.internal,
                attachment_worker: &self.connection.attachment_worker,
                generation: self.generation,
            },
            &mut self.active,
            &mut self.next_epoch,
            session_id,
            80,
        )
        .unwrap();
    }

    fn recover(&mut self) -> bool {
        recover(
            &self.sink,
            &self.endpoint,
            &self.commands,
            &self.internal,
            &mut self.connection,
            &mut self.generation,
            &mut self.active,
            &mut self.observing,
            &mut self.next_epoch,
            &mut self.subscribed,
        )
    }

    /// Wait for a rendered marker on the internal channel — the command
    /// thread's inbox, which is where render readers publish. Only frames
    /// stamped with `epoch` count; a frame stamped otherwise is what
    /// `observe_render` drops in the real loop, so it is skipped here too.
    fn wait_rendered(&self, epoch: u64, marker: &str) -> RenderUpdate {
        let deadline = Instant::now() + Duration::from_secs(10);
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match self.commands.recv_timeout(remaining) {
                Ok(BackendCommand::ObservedRender {
                    epoch: observed,
                    update,
                }) if observed == epoch => {
                    if render_text(&update).contains(marker) {
                        return update;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        panic!("the render stream never showed {marker}")
    }

    /// Force a fresh full snapshot and wait for it: deltas carry only damaged
    /// rows, so counting a marker needs the whole grid.
    fn wait_full_snapshot(&self, epoch: u64, marker: &str) -> String {
        let current = self.active.as_ref().expect("an attached session");
        self.connection
            .client
            .recover_snapshot(current.session_id, current.lease.token)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match self.commands.recv_timeout(remaining) {
                Ok(BackendCommand::ObservedRender {
                    epoch: observed,
                    update: update @ RenderUpdate::Snapshot { .. },
                }) if observed == epoch => {
                    let text = render_text(&update);
                    if text.contains(marker) {
                        return text;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        panic!("no full snapshot arrived showing {marker}")
    }

    fn drain_events(&self) {
        while self.events.try_recv().is_ok() {}
    }
}

/// **Same instance, raw session attached.** The transport drops while the host
/// survives; recovery reconnects, the handshake identity matches, and the
/// session resumes under a fresh lease and epoch — with the input typed before
/// the outage executed exactly once, which is the no-replay guarantee made
/// visible.
#[test]
fn a_transport_drop_with_the_host_alive_resumes_the_raw_attachment_quietly() {
    let root = tempfile::tempdir().unwrap();
    let mut host = IsolatedHost::start(root.path());
    let mut rig = RecoveryRig::connect(root.path());
    let session = rig
        .connection
        .client
        .create(interactive_shell(root.path()))
        .unwrap();
    rig.attach(session.id);
    let first_epoch = rig.active.as_ref().unwrap().epoch;
    send_raw(
        &rig.connection.client,
        session.id,
        rig.active.as_ref().unwrap().lease.token,
        RawCommand::WriteText {
            text: RAN_ONCE_INPUT.into(),
        },
    )
    .unwrap();
    rig.wait_rendered(first_epoch, RAN_ONCE_OUTPUT);

    // The transport dies; the host does not. (The polite goodbye also lets
    // the host release the lease, exactly as it does when a window process
    // vanishes and the pipe closes under it.)
    rig.connection.client.disconnect();
    wait_detached(&host.client(), session.id);
    rig.drain_events();

    assert!(rig.recover(), "recovery only stops for a closing host app");
    assert_eq!(rig.generation, 2, "one loss, one new generation");

    wait_for_event(&rig.events, "the connecting report", |event| {
        matches!(event, BackendEvent::Disconnected).then_some(())
    });
    let sessions = wait_for_event(&rig.events, "the quiet resume", |event| match event {
        BackendEvent::Recovered { sessions } => Some(sessions),
        BackendEvent::SessionsLost { .. } => {
            panic!("the same instance answered the handshake; this is not loss")
        }
        _ => None,
    });
    assert!(
        sessions.iter().any(|summary| summary.id == session.id),
        "the catalog still knows the session the host kept alive"
    );
    let epoch = wait_for_event(&rig.events, "the re-attach", |event| match event {
        BackendEvent::Attached {
            session_id, epoch, ..
        } => {
            assert_eq!(session_id, session.id);
            Some(epoch)
        }
        _ => None,
    });
    assert!(epoch > first_epoch, "a fresh epoch, never a reused one");
    assert_eq!(rig.active.as_ref().unwrap().generation, 2);

    // The old generation is fenced: a frame stamped with the superseded epoch
    // is dropped by the command owner, never presented.
    rig.drain_events();
    let stale = RenderUpdate::Gap {
        gap: koru_session_protocol::StreamGap {
            session_id: session.id,
            terminal_generation: TerminalGeneration::new(1).unwrap(),
            viewport_generation: ViewportGeneration::new(1).unwrap(),
            expected: koru_session_protocol::StreamSequence::new(1).unwrap(),
            actual: koru_session_protocol::StreamSequence::new(2).unwrap(),
            resnapshot_required: true,
        },
    };
    observe_render(
        &rig.connection.client,
        &rig.sink,
        &rig.internal,
        &mut rig.active,
        first_epoch,
        &stale,
    )
    .unwrap();
    assert!(
        rig.events.try_recv().is_err(),
        "a superseded generation's frame must never reach the host"
    );

    // The resumed attachment is live: new input renders under the new lease,
    // and the pre-outage command ran exactly once — nothing was replayed.
    send_raw(
        &rig.connection.client,
        session.id,
        rig.active.as_ref().unwrap().lease.token,
        RawCommand::WriteText {
            text: RAN_AGAIN_INPUT.into(),
        },
    )
    .unwrap();
    rig.wait_rendered(epoch, RAN_AGAIN_OUTPUT);
    let text = rig.wait_full_snapshot(epoch, RAN_AGAIN_OUTPUT);
    assert_eq!(
        text.matches(RAN_ONCE_OUTPUT).count(),
        1,
        "the pre-outage input executed exactly once: recovery replays nothing"
    );

    let _ = detach(&rig.connection.client, &mut rig.active);
    rig.connection.client.disconnect();
    host.stop();
}

/// **Nothing attached** — the case where loss and resume look identical on
/// screen until the handshake speaks. The same instance answering means quiet
/// resume with the catalog intact; a restarted instance means loss, even
/// though its catalog is merely empty.
#[test]
fn nothing_attached_the_handshake_identity_separates_resume_from_loss() {
    let root = tempfile::tempdir().unwrap();
    let mut host = IsolatedHost::start(root.path());
    let mut rig = RecoveryRig::connect(root.path());
    let session = rig
        .connection
        .client
        .create(interactive_shell(root.path()))
        .unwrap();
    rig.drain_events();

    // Phase one: the transport drops, the host survives. Same instance_id and
    // supervisor_launch_id answer the reconnect: resume, quietly.
    rig.connection.client.disconnect();
    assert!(rig.recover());
    assert_eq!(rig.generation, 2);
    wait_for_event(&rig.events, "the connecting report", |event| {
        matches!(event, BackendEvent::Disconnected).then_some(())
    });
    let sessions = wait_for_event(&rig.events, "the quiet resume", |event| match event {
        BackendEvent::Recovered { sessions } => Some(sessions),
        BackendEvent::SessionsLost { .. } => {
            panic!("a surviving instance was presented as a restart")
        }
        BackendEvent::Attached { .. } => {
            panic!("nothing was attached, so recovery must attach nothing")
        }
        _ => None,
    });
    assert!(
        sessions.iter().any(|summary| summary.id == session.id),
        "the unattached session is still in the surviving host's catalog"
    );

    // Phase two: the host is replaced. The new instance's catalog is empty —
    // and empty on a *new* instance is loss, never "nothing changed".
    host.stop();
    let replacement = IsolatedHost::start(root.path());
    rig.drain_events();
    assert!(rig.recover());
    assert_eq!(rig.generation, 3);
    wait_for_event(&rig.events, "the connecting report", |event| {
        matches!(event, BackendEvent::Disconnected).then_some(())
    });
    let sessions = wait_for_event(&rig.events, "the loss report", |event| match event {
        BackendEvent::SessionsLost { sessions } => Some(sessions),
        BackendEvent::Recovered { .. } => {
            panic!("a restarted instance was presented as a resume")
        }
        BackendEvent::Attached { .. } => {
            panic!("recovery re-attached against a host that lost the session")
        }
        _ => None,
    });
    assert!(
        sessions.is_empty(),
        "the restarted host started empty; the backend reports that truth"
    );
    assert!(rig.active.is_none());
    drop(replacement);
}

/// The checked-in Codex app-server fixture, laid out the way npm installs it,
/// prepended to `PATH` so an isolated host running in this process can spawn
/// it. Prepend-only: nothing already resolvable stops resolving.
fn prepend_codex_fixture_to_path(root: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/codex-app-server/0.150.1/bin")
        .canonicalize()
        .expect("checked-in Codex fixture directory");
    let destination = root.join("codex-fixture-bin");
    std::fs::create_dir_all(&destination).unwrap();
    #[cfg(windows)]
    {
        // Match npm's real layout: the extensionless sibling is a Unix shim
        // and must never win over the PATHEXT command shim on Windows.
        std::fs::write(destination.join("codex"), "not a Windows executable").unwrap();
        std::fs::copy(source.join("codex.cmd"), destination.join("codex.cmd")).unwrap();
        std::fs::copy(
            source.join("codex-fixture.ps1"),
            destination.join("codex-fixture.ps1"),
        )
        .unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let executable = destination.join("codex");
        std::fs::copy(source.join("codex"), &executable).unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(executable, permissions).unwrap();
    }
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let joined =
        std::env::join_paths(std::iter::once(destination).chain(std::env::split_paths(&inherited)))
            .unwrap();
    unsafe { std::env::set_var("PATH", joined) };
}

/// **Managed session attached across a restart.** The provider, its event
/// stream and its lease all died with the old host; recovery reports loss and
/// delivers nothing more on the superseded stream — no batch, no gap, no
/// re-sent prompt, no recreated session.
#[test]
fn a_managed_session_attached_across_a_host_restart_is_lost_with_nothing_replayed() {
    let root = tempfile::tempdir().unwrap();
    prepend_codex_fixture_to_path(root.path());
    let mut host = IsolatedHost::start(root.path());
    let mut rig = RecoveryRig::connect(root.path());
    let provider = koru_session_protocol::ProviderKind::from_str("codex").unwrap();
    let session = rig
        .connection
        .client
        .create_profile_in(
            IdempotencyKey::mint(),
            SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("codex").unwrap()),
            None,
            SessionMode::Managed {
                provider: provider.clone(),
            },
            None,
            root.path().to_path_buf(),
            GridSize::new(80, 24).unwrap(),
        )
        .unwrap();
    assert_eq!(session.mode, SessionMode::Managed { provider });
    rig.attach(session.id);
    wait_for_event(&rig.events, "the managed attach", |event| match event {
        BackendEvent::Attached {
            session_id, mode, ..
        } => {
            assert_eq!(session_id, session.id);
            assert!(matches!(mode, SessionMode::Managed { .. }));
            Some(())
        }
        _ => None,
    });

    host.stop();
    let replacement = IsolatedHost::start(root.path());
    rig.drain_events();
    assert!(rig.recover());
    wait_for_event(&rig.events, "the connecting report", |event| {
        matches!(event, BackendEvent::Disconnected).then_some(())
    });
    let sessions = wait_for_event(&rig.events, "the loss report", |event| match event {
        BackendEvent::SessionsLost { sessions } => Some(sessions),
        BackendEvent::Recovered { .. } => {
            panic!("a restarted instance was presented as a resume")
        }
        BackendEvent::Attached { .. } => {
            panic!("recovery re-attached a managed session the host lost")
        }
        _ => None,
    });
    assert!(sessions.is_empty());
    assert!(rig.active.is_none(), "no lease survives the old host");

    // Grace window: the superseded managed stream must stay silent, and no
    // input, launch or delivery may have been replayed on the new host.
    let deadline = Instant::now() + Duration::from_millis(600);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match rig.events.recv_timeout(remaining) {
            Ok(
                event @ (BackendEvent::ManagedBatch { .. }
                | BackendEvent::ManagedGap { .. }
                | BackendEvent::Attached { .. }
                | BackendEvent::PendingInputSent { .. }),
            ) => panic!(
                "a superseded stream reached the host: {}",
                event_label(&event)
            ),
            Ok(_) | Err(_) => {}
        }
    }
    assert!(
        replacement.client().list().unwrap().is_empty(),
        "nothing was recreated on the person's behalf"
    );
    drop(replacement);
}

/// **The whole loop, over a real restart.** The spawned backend — command
/// thread, readers, recovery and all — attaches a raw session, loses its host,
/// reports the loss against the replacement's handshake, replays nothing, and
/// is immediately usable for the person's own next launch. No window restart
/// anywhere.
#[test]
fn the_running_backend_survives_a_host_restart_and_reports_loss_not_a_false_resume() {
    let root = tempfile::tempdir().unwrap();
    let host = IsolatedHost::start(root.path());
    let (event_send, events) = mpsc::channel();
    let (sender, commands) = channel();
    let internal = sender.internal_sender();
    let owned_root = root.path().to_path_buf();
    let _ = std::thread::Builder::new()
        .name("koru-session-backend session client".into())
        .spawn(move || {
            run(
                ChannelSink(event_send),
                Some(owned_root),
                Vec::new(),
                commands,
                internal,
                false,
            )
        });
    wait_for_event(&events, "the initial connect", |event| match event {
        BackendEvent::Connected(sessions) => {
            assert!(sessions.is_empty());
            Some(())
        }
        _ => None,
    });

    sender.launch(
        shell_launch_profile(),
        "Recovery fixture shell".into(),
        GridSize::new(80, 24).unwrap(),
        LaunchIntent::Raw,
        None,
    );
    let session_id = wait_for_event(&events, "the launch attach", |event| match event {
        BackendEvent::Attached { session_id, .. } => Some(session_id),
        _ => None,
    });
    SessionSurfaceInteraction::dispatch(
        &mut sender.clone(),
        SessionSurfaceIntent::Terminal(TerminalIntent::Raw(RawCommand::WriteText {
            text: RAN_ONCE_INPUT.into(),
        })),
    );
    wait_for_event(&events, "the executed marker", |event| match event {
        BackendEvent::Render(update) => {
            render_text(&update).contains(RAN_ONCE_OUTPUT).then_some(())
        }
        _ => None,
    });

    // The host dies under an attached session. The backend's own readers must
    // notice — nothing here tells it.
    drop(host);
    wait_for_event(&events, "the connecting report", |event| {
        matches!(event, BackendEvent::Disconnected).then_some(())
    });

    // A replacement host on the same endpoint: a different instance, an empty
    // catalog — which is loss, and said so.
    let replacement = IsolatedHost::start(root.path());
    let sessions = wait_for_event(&events, "the loss report", |event| match event {
        BackendEvent::SessionsLost { sessions } => Some(sessions),
        BackendEvent::Recovered { .. } => {
            panic!("a restarted instance was presented as a resume")
        }
        _ => None,
    });
    assert!(sessions.is_empty());

    // Nothing replayed: no re-attach, no re-render of the dead session, no
    // recreated session on the new host.
    let deadline = Instant::now() + Duration::from_millis(600);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(remaining) {
            Ok(
                event @ (BackendEvent::Attached { .. }
                | BackendEvent::Render(_)
                | BackendEvent::PendingInputSent { .. }),
            ) => panic!(
                "recovery replayed into the new host: {}",
                event_label(&event)
            ),
            Ok(_) | Err(_) => {}
        }
    }
    assert!(replacement.client().list().unwrap().is_empty());

    // And the person's own next move works without a window restart: a fresh
    // launch attaches and renders on the rebuilt connection and readers.
    sender.launch(
        shell_launch_profile(),
        "After the restart".into(),
        GridSize::new(80, 24).unwrap(),
        LaunchIntent::Raw,
        None,
    );
    let relaunched = wait_for_event(&events, "the post-recovery attach", |event| match event {
        BackendEvent::Attached { session_id, .. } => Some(session_id),
        _ => None,
    });
    assert_ne!(relaunched, session_id, "a new session, never a revived id");
    wait_for_event(&events, "the post-recovery render", |event| {
        matches!(event, BackendEvent::Render(_)).then_some(())
    });
    sender.terminate();
    drop(replacement);
}

fn ready_profile(profile: &LaunchProfile) -> koru_session_protocol::ProviderReadiness {
    koru_session_protocol::ProviderReadiness {
        kind: profile.provider.clone().unwrap(),
        availability: koru_session_protocol::ExecutableAvailability::Available { version: None },
        capabilities: CapabilitySet::new([capability::MANAGED_DELEGATION.parse().unwrap()]),
        failure: None,
    }
}

#[test]
fn ordinary_managed_preference_calls_live_creation_while_discovery_is_checking() {
    let profile = koru_session_profiles::built_in_profiles()
        .into_iter()
        .find(|profile| {
            profile
                .provider
                .as_ref()
                .and_then(koru_session_profiles::managed_provider)
                .is_some_and(|entry| {
                    entry.launch_policy == koru_session_profiles::ManagedLaunchPolicy::PreferManaged
                })
        })
        .unwrap();
    let row = koru_session_protocol::ProviderReadiness {
        availability: koru_session_protocol::ExecutableAvailability::Checking,
        ..ready_profile(&profile)
    };
    let (created, fallback) = create_with_raw_fallback(
        &profile,
        LaunchIntent::Ordinary,
        &[row],
        |mode| {
            assert!(matches!(mode, SessionMode::Managed { .. }));
            Ok("live managed")
        },
        || panic!("an unfinished cache must not silently select raw"),
    )
    .unwrap();
    assert_eq!(created, "live managed");
    assert_eq!(fallback, None);
}

/// A window that offers a request the host refuses on an un-negotiated
/// connection shows `Session failed` (ADR-474): the offer must carry
/// `launch-resume`, and the host's own offer must agree to it.
#[test]
fn the_windows_offer_negotiates_launch_resume_with_the_host() {
    let agreement =
        koru_session_protocol::negotiate(&protocol_offer(), &koru_session_host::protocol_offer())
            .expect("the window and the host agree on a protocol");
    assert!(
        agreement
            .capabilities
            .supports(koru_session_protocol::capability::LAUNCH_RESUME),
        "launch-resume must be negotiated"
    );
}

/// **Two connections, two live terminals.** Each connection holds one
/// controller lease, so a window that shows two terminals gives each its own
/// connection (`mainview/session_lanes.rs`). The host must serve both at once:
/// each types into and reads its own session, and neither sees the other's.
#[test]
fn two_connections_drive_two_sessions_at_once_on_an_isolated_host() {
    let root = tempfile::tempdir().unwrap();
    let mut host = IsolatedHost::start(root.path());
    let first_client = host.client();
    let second_client = host.client();
    let first = first_client.create(interactive_shell(root.path())).unwrap();
    let second = first_client.create(interactive_shell(root.path())).unwrap();

    let (first_lease, first_render) = attach_raw(
        &first_client,
        first.id,
        ViewportGeneration::new(1).unwrap(),
        80,
        2,
    )
    .unwrap();
    let (second_lease, second_render) = attach_raw(
        &second_client,
        second.id,
        ViewportGeneration::new(1).unwrap(),
        80,
        2,
    )
    .expect("a second connection attaches a different session while the first holds its own");

    let (mut sender, receive) = channel();
    #[cfg(windows)]
    let (one, two) = ("echo KORU_LANE_ONE\r", "echo KORU_LANE_TWO\r");
    #[cfg(unix)]
    let (one, two) = ("echo KORU_LANE_ONE\n", "echo KORU_LANE_TWO\n");
    dispatch_raw_text(
        &mut sender,
        &receive,
        &first_client,
        first.id,
        first_lease.token,
        one,
    );
    dispatch_raw_text(
        &mut sender,
        &receive,
        &second_client,
        second.id,
        second_lease.token,
        two,
    );
    render_contains(&first_render, "KORU_LANE_ONE");
    render_contains(&second_render, "KORU_LANE_TWO");

    // One connection cannot take the other's session while it is held.
    assert!(
        attach_raw(
            &first_client,
            second.id,
            ViewportGeneration::new(2).unwrap(),
            80,
            2,
        )
        .is_err(),
        "a session already driven by another connection is not taken by attaching"
    );

    send_raw(
        &first_client,
        first.id,
        first_lease.token,
        RawCommand::Terminate,
    )
    .unwrap();
    send_raw(
        &second_client,
        second.id,
        second_lease.token,
        RawCommand::Terminate,
    )
    .unwrap();
    drop(first_render);
    drop(second_render);
    first_client.disconnect();
    second_client.disconnect();
    host.stop();
}

/// A terminal's own connection ends when the window is done with it: `close`
/// stops the thread (the session it held keeps running), where dropping the
/// sender alone would not — the backend holds its own sender to itself.
#[test]
fn a_lane_connection_stops_when_closed() {
    let root = tempfile::tempdir().unwrap();
    let host = IsolatedHost::start(root.path());
    let (event_send, events) = mpsc::channel();
    let (sender, commands) = channel();
    let internal = sender.internal_sender();
    let owned_root = root.path().to_path_buf();
    let thread = std::thread::Builder::new()
        .name("koru-session-backend terminal lane".into())
        .spawn(move || {
            run(
                ChannelSink(event_send),
                Some(owned_root),
                Vec::new(),
                commands,
                internal,
                true,
            )
        })
        .unwrap();

    // A lane asks for no catalog: connecting produces no `Connected`.
    assert!(
        events.recv_timeout(Duration::from_millis(500)).is_err()
            || !events
                .try_iter()
                .any(|event| matches!(event, BackendEvent::Connected(_))),
        "a lane does not read the catalog"
    );

    sender.close();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !thread.is_finished() {
        assert!(Instant::now() < deadline, "the lane's thread did not stop");
        std::thread::sleep(Duration::from_millis(10));
    }
    thread.join().unwrap();
    drop(host);
}
