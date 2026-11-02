// SPDX-License-Identifier: MPL-2.0

// The routes are only mutated by the Windows shims below.
#![cfg_attr(not(windows), allow(unused_mut))]
use super::*;

/// No prompt or model call. OpenCode is excluded because its inherited config
/// loader may migrate global settings or install plugins before ACP startup.
#[test]
fn live_registered_routes_report_sanitized_handshake_outcomes_when_opted_in() {
    if !koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE) {
        return;
    }
    let tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let parent = tree.join("target/generic-factory-probes");
    std::fs::create_dir_all(&parent).unwrap();
    for (mut route, executable) in [(AcpRoute::kimi(), "kimi"), (AcpRoute::grok(), "grok")] {
        let cwd = tempfile::tempdir_in(&parent).unwrap();
        let kind = route.provider.clone();
        #[cfg(windows)]
        {
            let node = koru_session_profiles::resolve_executable("node").expect("Node installed");
            let windows = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
            // Cargo's injected DLL PATH can exceed cmd.exe's 8191 limit.
            // Only these live fixture children receive a normal runtime PATH.
            let path = std::env::join_paths([
                node.parent().unwrap().to_path_buf(),
                windows.join("System32"),
                windows,
            ])
            .unwrap();
            route.environment.push(("PATH".into(), path));
        }
        let factory = AcpFactory::new(route, Arc::new(NoopAttachmentPathResolver));
        let started = Instant::now();
        let result = factory
            .compatibility_probe(executable.into(), cwd.path().to_owned())
            .and_then(|probe| probe.probe(ProbeLimits::default()));
        match result {
            Ok(caps) => eprintln!(
                "factory_probe: provider={}, version={}, protocol={}, http={}, delegatable={}, elapsed_ms={}",
                kind.as_str(),
                caps.executable_version.as_str(),
                caps.protocol_version.as_str(),
                caps.delegation.mcp == McpSupport::Supported,
                DelegationVerdict::evaluate(&caps.capabilities, &caps.delegation).delegatable(),
                started.elapsed().as_millis()
            ),
            Err(error) => eprintln!(
                "factory_probe: provider={}, stage={:?}, diagnostic={:?}, elapsed_ms={}",
                kind.as_str(),
                error.stage,
                error.diagnostic,
                started.elapsed().as_millis()
            ),
        }
    }
}

/// Opt-in: Google's Antigravity ACP server as a chat meets it (AGENTUX I9) —
/// the readiness probe, the picker's model discovery, and a session started
/// with the desktop bridge beside it — each followed by the check that
/// matters for this server: it never exits on EOF, so shutdown must end the
/// bootloader, its Python child and `localharness_external.exe`, and no
/// `agy.exe` may be left either. No prompt is sent. Needs the server on PATH
/// (`scripts/setup/fetch-antigravity-acp.ps1`) and its sign-in (`-SignIn`).
#[cfg(windows)]
#[test]
fn live_antigravity_route_starts_lists_models_and_leaves_no_process_when_opted_in() {
    use std::collections::BTreeSet;
    if !koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE) {
        return;
    }
    let Ok(executable) = resolve_probe_executable(std::ffi::OsStr::new("agy_acp_server")) else {
        eprintln!(
            "antigravity: agy_acp_server is not on PATH; run scripts/setup/fetch-antigravity-acp.ps1"
        );
        return;
    };
    // Process ids by image name; comparing against a snapshot taken first
    // keeps an unrelated `agy` the person has open out of the verdict.
    fn tree() -> BTreeSet<u32> {
        let mut pids = BTreeSet::new();
        for image in ["agy_acp_server.exe", "localharness_external.exe", "agy.exe"] {
            let filter = format!("IMAGENAME eq {image}");
            let output = std::process::Command::new("tasklist")
                .args(["/FO", "CSV", "/NH", "/FI", &filter])
                .output()
                .expect("tasklist");
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if let Some(pid) = line
                    .split("\",\"")
                    .nth(1)
                    .and_then(|pid| pid.trim_matches('"').parse().ok())
                {
                    pids.insert(pid);
                }
            }
        }
        pids
    }
    // The bundle's unpack folders, and the private temp folders Koru gives it.
    fn unpacked() -> BTreeSet<String> {
        std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name.starts_with("_MEI") || name.starts_with("koru-acp-"))
            .collect()
    }
    fn assert_nothing_left(before: &(BTreeSet<u32>, BTreeSet<String>), stage: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let processes = tree().difference(&before.0).count();
            let folders = unpacked().difference(&before.1).count();
            if processes == 0 && folders == 0 {
                eprintln!("antigravity: {stage}: no process and no temp folder left");
                return;
            }
            assert!(
                Instant::now() < deadline,
                "antigravity: {stage}: {processes} process(es) and {folders} temp folder(s) outlived shutdown"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let tree_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let parent = tree_root.join("target/generic-factory-probes");
    std::fs::create_dir_all(&parent).unwrap();
    let before = (tree(), unpacked());
    let factory = AcpFactory::new(
        AcpRoute::antigravity(),
        Arc::new(NoopAttachmentPathResolver),
    );

    // 1. Readiness: the same probe the host runs, inside its 20 s bound.
    let cwd = tempfile::tempdir_in(&parent).unwrap();
    let started = Instant::now();
    let caps = factory
        .compatibility_probe(executable.clone(), cwd.path().to_owned())
        .and_then(|probe| probe.probe(ProbeLimits::default()))
        .unwrap_or_else(|error| {
            panic!(
                "antigravity: readiness: code={} stage={:?} diagnostic={:?}",
                error.code, error.stage, error.diagnostic
            )
        });
    let elapsed = started.elapsed();
    eprintln!(
        "antigravity: readiness: version={}, protocol={}, http={}, models={}, delegatable={}, elapsed_ms={}",
        caps.executable_version.as_str(),
        caps.protocol_version.as_str(),
        caps.delegation.mcp == McpSupport::Supported,
        caps.capabilities.supports(capability::PROVIDER_MODELS),
        DelegationVerdict::evaluate(&caps.capabilities, &caps.delegation).delegatable(),
        elapsed.as_millis()
    );
    assert_eq!(caps.executable_version.as_str(), "1.2.1");
    assert!(
        elapsed < ProbeLimits::default().timeout.saturating_mul(4),
        "the host's readiness bound would have cut this probe short"
    );
    assert_nothing_left(&before, "readiness");

    // 2. The picker's model list: a discovery session, no prompt.
    let cwd = tempfile::tempdir_in(&parent).unwrap();
    let started = Instant::now();
    let models = ManagedProviderFactory::discover_models(
        &factory,
        executable.clone(),
        cwd.path().to_owned(),
    )
    .unwrap_or_else(|error| {
        panic!(
            "antigravity: models: stage={:?} diagnostic={:?}",
            error.stage, error.diagnostic
        )
    });
    let mut groups: Vec<&str> = models
        .models()
        .iter()
        .map(|model| model.group.as_str())
        .collect();
    groups.dedup();
    eprintln!(
        "antigravity: models: count={}, groups={groups:?}, elapsed_ms={}",
        models.models().len(),
        started.elapsed().as_millis()
    );
    assert!(!models.models().is_empty());
    assert_nothing_left(&before, "model discovery");

    // 3. A chat's session: a model chosen from that list (the last one, so
    // not the server's default), the desktop bridge beside it when this tree
    // has built one, then shutdown with the tree seen running first.
    let chosen = models.models().last().unwrap().id.clone();
    let bridge = tree_root.join("target/debug/koru-mcp.exe");
    let cwd = tempfile::tempdir_in(&parent).unwrap();
    let route = AcpRoute::antigravity();
    let version = route.supported_versions[0].clone();
    let mut config = AcpConfig::new(
        route,
        executable,
        cwd.path().to_owned(),
        version,
        Arc::new(NoopAttachmentPathResolver),
    )
    .unwrap()
    .with_selection(Some(koru_session_protocol::LaunchSelection {
        model: Some(chosen),
        reasoning: None,
        mode: None,
    }));
    if bridge.is_file() {
        config = config.with_mcp_servers(vec![crate::providers::McpServerLaunch {
            name: "koru".into(),
            command: bridge.clone().into_os_string(),
            args: vec!["serve".into(), "--stdio".into()],
            env: Vec::new(),
        }]);
    } else {
        eprintln!(
            "antigravity: session: no koru-mcp built in this tree; started without the bridge"
        );
    }
    let mut control = AcpControl::new(config);
    let started = Instant::now();
    let result = control.start(AgentStart::fresh(SessionId::mint(), None));
    let running = tree().difference(&before.0).count();
    let private = unpacked().difference(&before.1).count();
    let shutdown = control.shutdown();
    let caps = result.unwrap_or_else(|error| {
        panic!(
            "antigravity: session: stage={:?} diagnostic={:?}",
            error.stage, error.diagnostic
        )
    });
    eprintln!(
        "antigravity: session: started, model_selected={}, bridge={}, running_processes={running}, private_temp_folders={private}, elapsed_ms={}",
        caps.delegation.economics.model.requested.is_some(),
        bridge.is_file(),
        started.elapsed().as_millis()
    );
    assert!(running >= 2, "the owned tree was not visible while running");
    // The unpack went into Koru's private folder, not loose into temp.
    assert_eq!(
        private, 1,
        "exactly the one private temp folder while running"
    );
    shutdown.expect("terminating the owned tree reports success");
    assert_nothing_left(&before, "session shutdown");
}

/// Explicit acceptance only: configured catalog discovery, no ACP session/prompt.
#[test]
#[ignore = "uses the installed OpenCode configuration; explicit account acceptance only"]
fn live_opencode_configured_models_without_prompt() {
    assert!(
        koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE),
        "explicit live-provider opt-in required"
    );
    let cwd = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    let mut route = AcpRoute::opencode();
    #[cfg(windows)]
    {
        let node = koru_session_profiles::resolve_executable("node").expect("Node installed");
        let windows = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let path = std::env::join_paths([
            node.parent().unwrap().to_path_buf(),
            windows.join("System32"),
            windows,
        ])
        .unwrap();
        route.environment.push(("PATH".into(), path));
    }
    let factory = AcpFactory::new(route, Arc::new(NoopAttachmentPathResolver));
    let started = Instant::now();
    match factory.discover_models("opencode".into(), cwd) {
        Ok(models) => eprintln!(
            "configured_models: provider=opencode success=true count={} elapsed_ms={}",
            models.models().len(),
            started.elapsed().as_millis()
        ),
        Err(error) => panic!(
            "configured_models: provider=opencode success=false code={} stage={:?} diagnostic={:?} elapsed_ms={}",
            error.code,
            error.stage,
            error.diagnostic,
            started.elapsed().as_millis()
        ),
    }
}

/// Limited protocol acceptance: installed OpenCode accepts a selected connected
/// model. No MCP server or task prompt; this does not prove delegated execution.
#[test]
#[ignore = "uses installed OpenCode account metadata and creates an empty ACP session"]
fn live_opencode_accepts_connected_model_config_without_mcp_or_prompt() {
    assert!(
        koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE),
        "explicit live-provider opt-in required"
    );
    let cwd = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    let mut route = AcpRoute::opencode();
    #[cfg(windows)]
    {
        let node = koru_session_profiles::resolve_executable("node").expect("Node installed");
        let windows = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let path = std::env::join_paths([
            node.parent().unwrap().to_path_buf(),
            windows.join("System32"),
            windows,
        ])
        .unwrap();
        route.environment.push(("PATH".into(), path));
    }
    let factory = AcpFactory::new(route.clone(), Arc::new(NoopAttachmentPathResolver));
    let catalog = factory
        .discover_models("opencode".into(), cwd.clone())
        .expect("connected catalog discovery");
    let selected = catalog
        .models()
        .first()
        .expect("one explicit connected model required")
        .id
        .clone();
    let executable = resolve_probe_executable(std::ffi::OsStr::new("opencode")).unwrap();
    let config = AcpConfig::new(
        route.clone(),
        executable,
        cwd.clone(),
        route.supported_versions[0].clone(),
        Arc::new(NoopAttachmentPathResolver),
    )
    .unwrap();
    let mut control = AcpControl::new(config);
    let started = Instant::now();
    let result = (|| -> Result<(), ProviderFailure> {
        control.ensure_transport()?;
        let initialized = control.call(
            "initialize",
            json!({
                "protocolVersion": ACP_PROTOCOL_VERSION,
                "clientInfo": {"name":"koru", "version":env!("CARGO_PKG_VERSION")},
                "clientCapabilities": {}
            }),
        )?;
        decode::<acp::InitializeResponse>(&initialized).map_err(|_| handshake_rejected())?;
        let session = control.call("session/new", json!({"cwd":cwd, "mcpServers":[]}))?;
        decode::<acp::NewSessionResponse>(&session).map_err(|_| handshake_rejected())?;
        models::validate_selection(&session, &selected)?;
        let native = session["sessionId"]
            .as_str()
            .filter(|value| valid_native_id(value))
            .ok_or_else(handshake_rejected)?;
        let applied = control.call(
            "session/set_config_option",
            json!({"sessionId":native, "configId":"model", "value":selected.as_str()}),
        )?;
        models::validate_applied(&applied, &selected)
    })();
    let shutdown = control.shutdown();
    eprintln!(
        "connected_model_selection: provider=opencode success={} catalog_count={} no_mcp=true no_prompt=true shutdown_success={} elapsed_ms={}",
        result.is_ok(),
        catalog.models().len(),
        shutdown.is_ok(),
        started.elapsed().as_millis()
    );
    if let Err(error) = result {
        panic!(
            "connected_model_selection: code={} stage={:?} diagnostic={:?}",
            error.code, error.stage, error.diagnostic
        );
    }
    shutdown.expect("bounded ACP shutdown");
}

/// Opt-in: what each installed route advertises under its `model` option,
/// through the same discovery session the chat's picker uses. Counts and
/// group ids only; no account content, no prompt.
#[test]
fn live_named_routes_list_session_models_when_opted_in() {
    if !koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE) {
        return;
    }
    let tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let parent = tree.join("target/generic-factory-probes");
    std::fs::create_dir_all(&parent).unwrap();
    for (mut route, executable) in [
        (AcpRoute::claude_code(), "claude-agent-acp"),
        (AcpRoute::kimi(), "kimi"),
        (AcpRoute::grok(), "grok"),
    ] {
        let cwd = tempfile::tempdir_in(&parent).unwrap();
        let kind = route.provider.clone();
        #[cfg(windows)]
        {
            let node = koru_session_profiles::resolve_executable("node").expect("Node installed");
            let windows = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
            let path = std::env::join_paths([
                node.parent().unwrap().to_path_buf(),
                windows.join("System32"),
                windows,
            ])
            .unwrap();
            route.environment.push(("PATH".into(), path));
        }
        let factory = AcpFactory::new(route, Arc::new(NoopAttachmentPathResolver));
        let started = Instant::now();
        match ManagedProviderFactory::discover_models(
            &factory,
            executable.into(),
            cwd.path().to_owned(),
        ) {
            Ok(models) => {
                let mut groups: Vec<&str> = models
                    .models()
                    .iter()
                    .map(|model| model.group.as_str())
                    .collect();
                groups.sort_unstable();
                groups.dedup();
                eprintln!(
                    "session_models: provider={}, count={}, groups={:?}, elapsed_ms={}",
                    kind.as_str(),
                    models.models().len(),
                    groups,
                    started.elapsed().as_millis()
                );
            }
            Err(error) => eprintln!(
                "session_models: provider={}, stage={:?}, diagnostic={:?}, elapsed_ms={}",
                kind.as_str(),
                error.stage,
                error.diagnostic,
                started.elapsed().as_millis()
            ),
        }
    }
}

/// Opt-in: each installed route starts a session with the desktop MCP
/// bridge beside it, exactly as a chat's session does — `koru-mcp serve
/// --stdio` from this tree's `target/debug` — and shuts down. No prompt.
/// The evidence is that the route accepts the stdio server and reaches
/// `acp.ready` with it; what the bridge offers is the acceptance's to ask.
#[test]
fn live_named_routes_start_with_the_desktop_bridge_when_opted_in() {
    if !koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE) {
        return;
    }
    let tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let bridge = tree.join(if cfg!(windows) {
        "target/debug/koru-mcp.exe"
    } else {
        "target/debug/koru-mcp"
    });
    if !bridge.is_file() {
        eprintln!("bridge_start: no koru-mcp built at {}", bridge.display());
        return;
    }
    let parent = tree.join("target/generic-factory-probes");
    std::fs::create_dir_all(&parent).unwrap();
    for (mut route, executable) in [
        (AcpRoute::claude_code(), "claude-agent-acp"),
        (AcpRoute::kimi(), "kimi"),
        (AcpRoute::grok(), "grok"),
    ] {
        let cwd = tempfile::tempdir_in(&parent).unwrap();
        let kind = route.provider.clone();
        #[cfg(windows)]
        {
            let node = koru_session_profiles::resolve_executable("node").expect("Node installed");
            let windows = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
            let path = std::env::join_paths([
                node.parent().unwrap().to_path_buf(),
                windows.join("System32"),
                windows,
            ])
            .unwrap();
            route.environment.push(("PATH".into(), path));
        }
        let started = Instant::now();
        let outcome = (|| {
            let executable = crate::providers::resolve_probe_executable(executable.as_ref())?;
            let probe = AcpCompatibilityProbe::new(
                route.clone(),
                executable.clone(),
                cwd.path().to_owned(),
                Arc::new(NoopAttachmentPathResolver),
            );
            let version = probe.version(ProbeLimits::default())?;
            let config = AcpConfig::new(
                route.clone(),
                executable,
                cwd.path().to_owned(),
                version,
                Arc::new(NoopAttachmentPathResolver),
            )?
            .with_limits(ProbeLimits {
                timeout: Duration::from_secs(20),
                ..ProbeLimits::default()
            })?
            .with_mcp_servers(vec![crate::providers::McpServerLaunch {
                name: "koru".into(),
                command: bridge.clone().into_os_string(),
                args: vec!["serve".into(), "--stdio".into()],
                env: Vec::new(),
            }]);
            let mut control = AcpControl::new(config);
            let result = control.start(AgentStart::fresh(SessionId::mint(), None));
            let shutdown = control.shutdown();
            let capabilities = result?;
            shutdown?;
            Ok::<_, ProviderFailure>(capabilities)
        })();
        match outcome {
            Ok(caps) => eprintln!(
                "bridge_start: provider={}, version={}, http={}, elapsed_ms={}",
                kind.as_str(),
                caps.executable_version.as_str(),
                caps.delegation.mcp == McpSupport::Supported,
                started.elapsed().as_millis()
            ),
            Err(error) => eprintln!(
                "bridge_start: provider={}, stage={:?}, diagnostic={:?}, elapsed_ms={}",
                kind.as_str(),
                error.stage,
                error.diagnostic,
                started.elapsed().as_millis()
            ),
        }
    }
}

/// Explicit acceptance: does Grok list every bridge tool? Starts the installed
/// `grok` through Koru's own ACP route with this tree's `koru-mcp` attached,
/// as a chat's session does, `GROK_BRIDGE_STARTS` times, each in a fresh
/// folder; then reads what Grok itself recorded about the `koru` server in
/// its session's `events.jsonl` under `~/.grok/sessions/`. No prompt.
///
/// Grok CLI 1.0.34 failed to decode the bridge's `tools/list` reply in two of
/// seven chat sessions while it was one 57,071-byte line, losing the first
/// 8,192 bytes of it, and then registered no Koru tool (ADR-433-compacttools).
/// The line to look for is `bridge_tools: connected=N decode_errors=N …`.
#[test]
#[ignore = "starts the installed Grok ten times; explicit acceptance only"]
fn live_grok_lists_every_bridge_tool() {
    const GROK_BRIDGE_STARTS: usize = 10;
    // What `koru-mcp` publishes today; a start that registers fewer is a
    // failure even without a decode error.
    const BRIDGE_TOOLS: u64 = 21;
    assert!(
        koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE),
        "explicit live-provider opt-in required"
    );
    let tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let bridge = tree.join(if cfg!(windows) {
        "target/debug/koru-mcp.exe"
    } else {
        "target/debug/koru-mcp"
    });
    assert!(
        bridge.is_file(),
        "build koru-mcp first: {}",
        bridge.display()
    );
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(std::path::PathBuf::from)
        .expect("a home directory");
    let sessions = home.join(".grok").join("sessions");
    let parent = tree.join("target").join("generic-factory-probes");
    std::fs::create_dir_all(&parent).unwrap();

    let (mut connected, mut decode_errors, mut other) = (0, 0, 0);
    for attempt in 1..=GROK_BRIDGE_STARTS {
        let cwd = tempfile::tempdir_in(&parent).unwrap();
        let mut route = AcpRoute::grok();
        #[cfg(windows)]
        {
            let node = koru_session_profiles::resolve_executable("node").expect("Node installed");
            let windows = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
            let path = std::env::join_paths([
                node.parent().unwrap().to_path_buf(),
                windows.join("System32"),
                windows,
            ])
            .unwrap();
            route.environment.push(("PATH".into(), path));
        }
        let started = Instant::now();
        let outcome = (|| {
            let executable = crate::providers::resolve_probe_executable("grok".as_ref())?;
            let probe = AcpCompatibilityProbe::new(
                route.clone(),
                executable.clone(),
                cwd.path().to_owned(),
                Arc::new(NoopAttachmentPathResolver),
            );
            let version = probe.version(ProbeLimits::default())?;
            let config = AcpConfig::new(
                route.clone(),
                executable,
                cwd.path().to_owned(),
                version,
                Arc::new(NoopAttachmentPathResolver),
            )?
            .with_limits(ProbeLimits {
                timeout: Duration::from_secs(40),
                ..ProbeLimits::default()
            })?
            .with_mcp_servers(vec![crate::providers::McpServerLaunch {
                name: "koru".into(),
                command: bridge.clone().into_os_string(),
                args: vec!["serve".into(), "--stdio".into()],
                env: Vec::new(),
            }]);
            let mut control = AcpControl::new(config);
            let result = control.start(AgentStart::fresh(SessionId::mint(), None));
            let shutdown = control.shutdown();
            result?;
            shutdown?;
            Ok::<_, ProviderFailure>(())
        })();
        // Grok names a session's folder after its cwd, percent-encoded; the
        // temporary folder's own name is unique enough to find it by.
        let marker = cwd
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let events: Vec<String> = std::fs::read_dir(&sessions)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|folder| folder.file_name().to_string_lossy().ends_with(&marker))
            .flat_map(|folder| {
                std::fs::read_dir(folder.path())
                    .into_iter()
                    .flatten()
                    .flatten()
            })
            .filter_map(|session| std::fs::read_to_string(session.path().join("events.jsonl")).ok())
            .flat_map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
            .filter(|line| line.contains("\"server_name\":\"koru\""))
            .collect();
        let tool_count = events
            .iter()
            .filter(|line| line.contains("\"type\":\"mcp_server_connected\""))
            .find_map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).ok()?;
                value["tool_count"].as_u64()
            });
        let decode_error = events
            .iter()
            .any(|line| line.contains("\"type\":\"mcp_transport_decode_error\""));
        match (tool_count, decode_error) {
            (Some(BRIDGE_TOOLS), false) => connected += 1,
            (_, true) => decode_errors += 1,
            _ => other += 1,
        }
        eprintln!(
            "bridge_tools_attempt: n={attempt}, start_ok={}, tool_count={tool_count:?}, decode_error={decode_error}, koru_events={}, elapsed_ms={}",
            outcome.is_ok(),
            events.len(),
            started.elapsed().as_millis()
        );
        if let Err(error) = &outcome {
            eprintln!(
                "bridge_tools_failure: stage={:?}, diagnostic={:?}",
                error.stage, error.diagnostic
            );
        }
    }
    eprintln!(
        "bridge_tools: connected={connected} decode_errors={decode_errors} other={other} of {GROK_BRIDGE_STARTS}"
    );
    assert_eq!(
        connected, GROK_BRIDGE_STARTS,
        "every start lists the bridge's tools"
    );
}

/// Explicit acceptance: does each installed ACP agent page through the
/// bridge's `tools/list`? Starts every route a chat can use — Claude
/// (claude-agent-acp), Kimi, Grok, OpenCode and Antigravity — once, with this
/// tree's `koru-mcp` attached as a chat attaches it, and reads the bridge's
/// own log of the `tools/list` requests it answered and the cursors they
/// carried (`providers::bridge_log_live`). A route whose executable is not
/// installed is skipped and says so. No prompt is sent, so an agent that
/// starts its MCP servers only at the first prompt shows no request at all.
///
/// The line per agent is `bridge_pages: provider=… followed_every_cursor=…`.
/// OpenCode runs with its installed configuration, as in a chat.
#[test]
#[ignore = "starts every installed ACP agent; explicit acceptance only"]
fn live_acp_agents_page_through_the_bridge_tool_list() {
    use crate::providers::bridge_log_live;
    assert!(
        koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE),
        "explicit live-provider opt-in required"
    );
    let tree = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let parent = tree.join("target").join("generic-factory-probes");
    std::fs::create_dir_all(&parent).unwrap();
    let mut incomplete = Vec::new();
    for (mut route, executable) in [
        (AcpRoute::claude_code(), "claude-agent-acp"),
        (AcpRoute::kimi(), "kimi"),
        (AcpRoute::grok(), "grok"),
        (AcpRoute::opencode(), "opencode"),
        (AcpRoute::antigravity(), "agy_acp_server"),
    ] {
        let kind = route.provider.as_str().to_owned();
        let Ok(resolved) = resolve_probe_executable(std::ffi::OsStr::new(executable)) else {
            eprintln!("bridge_pages: provider={kind}, skipped=not_installed ({executable})");
            continue;
        };
        #[cfg(windows)]
        {
            let node = koru_session_profiles::resolve_executable("node").expect("Node installed");
            let windows = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
            let path = std::env::join_paths([
                node.parent().unwrap().to_path_buf(),
                windows.join("System32"),
                windows,
            ])
            .unwrap();
            route.environment.push(("PATH".into(), path));
        }
        let cwd = tempfile::tempdir_in(&parent).unwrap();
        let bridge_root = tempfile::tempdir_in(&parent).unwrap();
        let bridge =
            bridge_log_live::bridge_launch(bridge_root.path()).expect("build koru-mcp first");
        let started = Instant::now();
        let outcome = (|| {
            let version = if route.is_antigravity() {
                route.supported_versions[0].clone()
            } else {
                AcpCompatibilityProbe::new(
                    route.clone(),
                    resolved.clone(),
                    cwd.path().to_owned(),
                    Arc::new(NoopAttachmentPathResolver),
                )
                .version(ProbeLimits::default())?
            };
            let config = AcpConfig::new(
                route.clone(),
                resolved.clone(),
                cwd.path().to_owned(),
                version,
                Arc::new(NoopAttachmentPathResolver),
            )?
            .with_limits(ProbeLimits {
                timeout: Duration::from_secs(60),
                ..ProbeLimits::default()
            })?
            .with_mcp_servers(vec![bridge.clone()]);
            let mut control = AcpControl::new(config);
            let result = control.start(AgentStart::fresh(SessionId::mint(), None));
            // An agent may list after `session/new` answers; give it the
            // time a person would wait before typing.
            let log =
                bridge_log_live::wait_for_the_end(bridge_root.path(), Duration::from_secs(20));
            let shutdown = control.shutdown();
            result?;
            shutdown?;
            Ok::<_, ProviderFailure>(log)
        })();
        match outcome {
            Ok(log) => {
                eprintln!(
                    "bridge_pages: provider={kind}, {}, elapsed_ms={}",
                    log.summary(),
                    started.elapsed().as_millis()
                );
                if !(log.reached_the_end() && log.followed_every_cursor()) {
                    incomplete.push(kind);
                }
            }
            Err(error) => {
                eprintln!(
                    "bridge_pages: provider={kind}, start_failed stage={:?} diagnostic={:?}, {}",
                    error.stage,
                    error.diagnostic,
                    bridge_log_live::read(bridge_root.path()).summary()
                );
                incomplete.push(kind);
            }
        }
    }
    assert!(
        incomplete.is_empty(),
        "did not page to the end: {incomplete:?}"
    );
}
