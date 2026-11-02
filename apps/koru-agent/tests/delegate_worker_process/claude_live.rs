// SPDX-License-Identifier: MPL-2.0

// The route is only mutated by the Windows PATH trim below.
#![cfg_attr(not(windows), allow(unused_mut))]
//! Opt-in qualification against Koru's real gateway and artifact authority.
use super::*;
use koru_session_protocol::{
    AgentEvent, AgentTurnId, ApprovalDecision, ApprovalIntent, DelegationLaunch, McpToolAllowlist,
    SessionId,
};
use koru_session_runtime::providers::{
    AcpConfig, AcpControl, AcpRoute, AgentControl, AgentInput, AgentStart, ChildSessionPolicy,
    ProviderVersion,
};
use std::str::FromStr;
use std::sync::Arc;

struct Control(AcpControl);
impl Drop for Control {
    fn drop(&mut self) {
        let _ = self.0.shutdown();
    }
}

#[test]
fn live_claude_wrapper_registers_through_koru_endpoint_and_cancels_when_opted_in() {
    if !koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE) {
        return;
    }
    // Node's ESM loader needs an ordinary Windows path, not canonicalize's
    // verbatim namespace path. The manifest directory is already absolute.
    let tree = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let tools = tree.join("target/generic-wrapper-tools");
    let package = tools.join("node_modules/@agentclientprotocol/claude-agent-acp");
    let pin: serde_json::Value = serde_json::from_slice(
        &std::fs::read(package.join("package.json"))
            .expect("install pinned wrapper under target/generic-wrapper-tools"),
    )
    .unwrap();
    assert_eq!(pin["version"], "0.76.0");
    let root = tempfile::Builder::new()
        .prefix("koru-live-")
        .tempdir_in(&tools)
        .unwrap();
    let agent = Agent::with_root(None, root);
    let client = agent.gateway();
    let (_, workspace) = workspace(&client);
    let run = RunId::mint();
    let cwd = agent.root().join("wrapper-workspace");
    let staging = cwd.join(".artifacts").join(run.to_storage());
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("fixture.txt"), "KORU_WRAPPER_QUALIFICATION").unwrap();
    let issued = client
        .artifacts()
        .issue_run_authority(&koru_artifact_wire::authority::IssueAuthorityRequest {
            run,
            workspace,
            generation: koru_delegate_core::AttemptGeneration::FIRST,
            grants: std::collections::BTreeSet::from([workspace]),
            run_workspace: koru_artifact_wire::authority::RunWorkspace::new(&cwd).unwrap(),
        })
        .expect("issue isolated run authority");
    let delegation = DelegationLaunch {
        model: None,
        mcp_endpoint: issued.url.parse().unwrap(),
        credential: Some(issued.token.parse().unwrap()),
        tools: McpToolAllowlist::new(
            koru_artifact_wire::run_tools::TOOL_NAMES
                .iter()
                .map(|name| name.parse().unwrap()),
        )
        .unwrap(),
        resume: None,
    };
    let mut route = AcpRoute::claude_code();
    // Endpoint exercise has no built-in tools. Effective native child
    // exclusions are qualified separately under conflicting allow settings.
    assert_eq!(route.child_sessions, ChildSessionPolicy::Disabled);
    route.session_new_meta.as_mut().unwrap()["claudeCode"]["options"]["tools"] =
        serde_json::json!([]);
    let node = koru_session_profiles::resolve_executable("node").expect("Node available");
    route.arguments = vec![package.join("dist/index.js").into_os_string()];
    let config = AcpConfig::new(
        route,
        node,
        cwd,
        ProviderVersion::from_str("0.76.0").unwrap(),
        Arc::new(koru_session_runtime::NoopAttachmentPathResolver),
    )
    .unwrap()
    .with_delegation(Some(delegation));
    let mut control = Control(AcpControl::new(config));
    let _ = control
        .0
        .start(AgentStart::fresh(SessionId::mint(), None))
        .expect("pinned authenticated wrapper starts");
    let turn = AgentTurnId::mint();
    control.0.submit(AgentInput {
        turn_id:turn,
        text:"Call the koru_run MCP create tool exactly once with path fixture.txt, title Wrapper qualification, and mime text/plain. The fixture file is already staged. Use no other tools, no shell, no child agents. Then finish.".into(),
        attachments:Vec::new(),
        mode: None,
    }).unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut completed = false;
    while Instant::now() < deadline {
        match control.0.next_event(Duration::from_millis(250)) {
            Ok(Some(AgentEvent::ApprovalRequired {
                approval_id,
                choices,
                ..
            })) => {
                let choice = choices
                    .iter()
                    .find(|c| c.intent == ApprovalIntent::Allow)
                    .expect("one-time approval");
                control
                    .0
                    .answer_approval(approval_id, ApprovalDecision::Approve, Some(choice.id))
                    .unwrap();
            }
            Ok(Some(AgentEvent::TurnCompleted { .. })) => {
                completed = true;
                break;
            }
            Ok(Some(AgentEvent::TurnFailed { .. } | AgentEvent::TurnCancelled { .. })) | Err(_) => {
                break;
            }
            _ => {}
        }
    }
    assert!(completed, "wrapper did not complete the fixture tool turn");
    let versions = client
        .artifacts()
        .run_versions(run)
        .expect("read real artifact versions");
    assert_eq!(
        versions.versions.len(),
        1,
        "wrapper must call Koru's authenticated artifact tool"
    );
    assert_eq!(
        versions.versions[0].size_bytes,
        "KORU_WRAPPER_QUALIFICATION".len() as u64
    );
    assert_eq!(versions.versions[0].produced_by.as_ref().unwrap().run, run);

    let cancel = AgentTurnId::mint();
    control
        .0
        .submit(AgentInput {
            turn_id: cancel,
            text: "Produce a long numbered explanation of arithmetic without using any tools."
                .into(),
            attachments: Vec::new(),
            mode: None,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut streaming = false;
    while Instant::now() < deadline {
        match control.0.next_event(Duration::from_millis(100)) {
            Ok(Some(AgentEvent::MessageDelta { turn_id, .. })) if turn_id == cancel => {
                streaming = true;
                break;
            }
            Err(_) | Ok(Some(AgentEvent::TurnFailed { .. } | AgentEvent::TurnCompleted { .. })) => {
                break;
            }
            _ => {}
        }
    }
    assert!(streaming, "wait for real provider output before cancelling");
    control.0.cancel(cancel).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut cancelled = false;
    while Instant::now() < deadline {
        match control.0.next_event(Duration::from_millis(100)) {
            Ok(Some(AgentEvent::TurnCancelled { turn_id, .. })) if turn_id == cancel => {
                cancelled = true;
                break;
            }
            Err(_) | Ok(Some(AgentEvent::TurnFailed { .. })) => break,
            _ => {}
        }
    }
    assert!(cancelled, "live cancellation must produce a cancelled turn");
    control.0.shutdown().unwrap();
    client
        .artifacts()
        .revoke_run_authority(run)
        .expect("revoke live fixture authority");
}

#[test]
fn live_claude_factory_probe_matches_registered_wrapper_when_opted_in() {
    if !koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE) {
        return;
    }
    use koru_session_runtime::providers::{
        AcpFactory, DelegationVerdict, ManagedProviderFactory, ProbeLimits,
    };
    let tree = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let tools = tree.join("target/generic-wrapper-tools");
    let executable = tools.join(if cfg!(windows) {
        "node_modules/.bin/claude-agent-acp.cmd"
    } else {
        "node_modules/.bin/claude-agent-acp"
    });
    let cwd = tempfile::Builder::new()
        .prefix("koru-probe-")
        .tempdir_in(&tools)
        .unwrap();
    let mut route = AcpRoute::claude_code();
    // Cargo adds DLL search directories until PATH exceeds cmd.exe's 8191
    // character limit. Give only this child a normal runtime PATH; this also
    // proves the bundled wrapper does not require a raw Claude executable.
    #[cfg(windows)]
    {
        let node = koru_session_profiles::resolve_executable("node").expect("Node installed");
        let windows = PathBuf::from(std::env::var_os("SystemRoot").expect("Windows root"));
        let path = std::env::join_paths([
            node.parent().unwrap().to_path_buf(),
            windows.join("System32"),
            windows,
        ])
        .unwrap();
        eprintln!(
            "wrapper_probe_path_chars: inherited={}, child={}; node_resolved=true",
            std::env::var_os("PATH")
                .unwrap_or_default()
                .to_string_lossy()
                .chars()
                .count(),
            path.to_string_lossy().chars().count(),
        );
        route.environment.push(("PATH".into(), path));
    }
    let factory = AcpFactory::new(
        route,
        Arc::new(koru_session_runtime::NoopAttachmentPathResolver),
    );
    let probe = factory
        .compatibility_probe(executable.into_os_string(), cwd.path().to_path_buf())
        .expect("registered wrapper resolves");
    let caps = probe
        .probe(ProbeLimits::default())
        .expect("registered wrapper probe succeeds");
    assert_eq!(caps.executable_version.as_str(), "0.76.0");
    assert_eq!(
        caps.provider.as_str(),
        koru_session_profiles::CLAUDE_REGISTRATION.kind
    );
    assert!(DelegationVerdict::evaluate(&caps.capabilities, &caps.delegation).delegatable());
}
