// SPDX-License-Identifier: MPL-2.0

//! Explicit live acceptance, never part of the ordinary suite. The installed
//! runtime receives only synthetic identities and uses this test's disposable
//! Koru root and working folder. The test never changes global provider settings.
//!
//! Build koru-mcp beside the test's koru-agent first. Then opt into exactly one:
//! `cargo test -p koru-agent --test gateway_ai managed_mini_apps_live::live_codex_creates_and_refines_over_stdio_mcp -- --exact --ignored --nocapture --test-threads=1`
//! (or the corresponding `live_claude_...` name), with
//! `KORU_LIVE_PROVIDER_SMOKE=1`. A missing runtime/login/bridge is a failure,
//! never a silently skipped success. These tests exercise the real provider,
//! stdio MCP, gateway, AI worker and typed stream; no native tab is opened.
//! Tool checks detect unexpected observed activity; they are not an execution
//! sandbox. Installed provider permission rules still apply; no approval is sent.
//!
//! Set `KORU_TELEMETRY_ENABLED=0` explicitly. For production provider readiness,
//! compile this integration target first, then build normal koru-agent/koru-mcp
//! into that same target directory and run this already compiled test executable
//! directly. Running cargo test again rebuilds the Agent with its test-only
//! no-provider-probes guard. Keep the exact provider filter and --ignored.

use super::*;
use koru_ai_core::{
    AGENT_DEFAULT_MODEL, AgentToolKind, CallShape, DocumentId, ProfileId, ToolActivity,
    ToolActivityStatus, ToolMode, WorkspaceId,
    mini_apps::{self, HostContext, RefinementTarget},
};
use koru_ai_wire::{StreamEvent, UpdateChatFolderRequest, UpdateChatToolPolicyRequest};
use std::{
    collections::BTreeSet,
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
};

mod diagnostics;
mod failure_log;
#[path = "../sessions_harness/mod.rs"]
mod sessions_harness;

const TURN_LIMIT: Duration = Duration::from_secs(150);
const PUMP_CLOSE_LIMIT: Duration = Duration::from_secs(5);
const SESSION_CLOSE_LIMIT: Duration = Duration::from_secs(10);
const MAX_STREAM_EVENTS: usize = 4096;
const LIST_NAME: &str = "MCP smoke list";
const DOCUMENT_TITLE: &str = "MCP smoke document";

#[test]
#[ignore = "uses installed Codex and an authenticated model; explicit live opt-in only"]
fn live_codex_creates_and_refines_over_stdio_mcp() {
    live("agent-codex");
}

#[test]
#[ignore = "uses installed Claude ACP and an authenticated model; explicit live opt-in only"]
fn live_claude_creates_and_refines_over_stdio_mcp() {
    live("agent-claude");
}

/// Keep this composition in koru-agent's integration target. The explicit
/// telemetry-off gate also covers a production Agent replacing the test build
/// at the same compiled child path. Both roles dispatch before native UI.
struct Stack {
    client: Option<Client>,
    gateway: Option<Gateway>,
    sessions: Option<sessions_harness::Host>,
    active: Option<koru_ai_core::InferenceAttemptId>,
    pump: Option<Pump>,
    root: tempfile::TempDir,
}

impl Stack {
    fn start(provider: &str) -> Self {
        let bridge = gateway_process::agent_exe().with_file_name(if cfg!(windows) {
            "koru-mcp.exe"
        } else {
            "koru-mcp"
        });
        assert!(
            bridge.is_file(),
            "build koru-mcp beside the integration-test agent first"
        );
        // Outside the checkout, so the runtime cannot inherit this repository's
        // AGENTS.md/project configuration as instructions for its empty cwd.
        let root = tempfile::Builder::new()
            .prefix(&format!("koru-mini-app-{provider}-"))
            .tempdir()
            .expect("allocate fresh disposable Koru root");
        let sessions = sessions_harness::Host::start(root.path());
        // Production composition independently supplies the same rooted
        // session-host endpoint to the AI worker. It does not require the
        // gateway's optional sessions-management adapter/lifecycle authority.
        let (gateway, client) = start_with_ai(root.path(), false);
        Self {
            client: Some(client),
            gateway: Some(gateway),
            sessions: Some(sessions),
            active: None,
            pump: None,
            root,
        }
    }

    fn client(&self) -> &Client {
        self.client.as_ref().expect("live client")
    }

    fn close_pump(&mut self) -> Result<(), &'static str> {
        let Some(mut pump) = self.pump.take() else {
            return Ok(());
        };
        if pump.done.recv_timeout(PUMP_CLOSE_LIMIT).is_err() {
            // Never join a reader which has not confirmed it stopped. It owns
            // only a bounded HTTP reader; closing owned workers breaks its link.
            return Err("stream reader did not finish within its close budget");
        }
        pump.handle
            .take()
            .expect("reader handle")
            .join()
            .map_err(|_| "stream reader panicked")
    }

    fn cancel_active(&mut self) -> bool {
        let Some(attempt) = self.active else {
            return true;
        };
        let cancelled = self
            .client
            .as_ref()
            .is_some_and(|client| client.ai().cancel_attempt(attempt).is_ok());
        if cancelled {
            self.active = None;
        }
        cancelled
    }

    fn shutdown(&mut self) -> Result<(), &'static str> {
        self.cancel_active();
        // Kill/reap only these exact owned children. Ask the session role to
        // dispose its provider trees before the host itself exits, including
        // platforms where killing the host alone does not kill descendants.
        self.gateway.take();
        let mut sessions_stopped = true;
        if let Some(mut sessions) = self.sessions.take() {
            sessions_stopped =
                sessions_harness::client(&sessions.endpoint, "koru-agent-supervisor")
                    .and_then(|client| client.stop_host(sessions.authority.clone()))
                    .is_ok();
            let deadline = Instant::now() + SESSION_CLOSE_LIMIT;
            while sessions.child.try_wait().ok().flatten().is_none() {
                if Instant::now() >= deadline {
                    sessions_stopped = false;
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            // Existing owned-child Drop is the fallback kill/reap.
        }
        self.client.take();
        self.close_pump()?;
        if !sessions_stopped {
            return Err("session host did not confirm disposal within its close budget");
        }
        Ok(())
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

struct Pump {
    events: Receiver<Result<StreamEvent, &'static str>>,
    done: Receiver<()>,
    handle: Option<JoinHandle<()>>,
    assistant: std::sync::Arc<std::sync::Mutex<diagnostics::AssistantReply>>,
}

impl Pump {
    fn start(mut stream: koru_api_client::AiResponseStream) -> Self {
        let (send, events) = mpsc::sync_channel(64);
        let (finished, done) = mpsc::sync_channel(1);
        let assistant =
            std::sync::Arc::new(std::sync::Mutex::new(diagnostics::AssistantReply::default()));
        let collecting = assistant.clone();
        let handle = thread::spawn(move || {
            for (index, item) in (&mut stream.events).enumerate() {
                if index >= MAX_STREAM_EVENTS {
                    let _ = send.try_send(Err("stream exceeded event budget"));
                    break;
                }
                let event = match item {
                    Ok(event) => event,
                    Err(_) => {
                        let _ = send.try_send(Err("stream transport failed"));
                        break;
                    }
                };
                let terminal = event.event.is_terminal();
                if let PublicAiEvent::TextDelta { text, .. } = &event.event {
                    collecting.lock().unwrap().push(text);
                }
                // Only sanitized public assistant text is retained, under a
                // separate byte bound and scrubbed for failure diagnostics.
                // Reasoning, prompts and raw tool bodies are never printed.
                if matches!(
                    event.event,
                    PublicAiEvent::Started { .. }
                        | PublicAiEvent::ToolActivity { .. }
                        | PublicAiEvent::MiniAppCreated { .. }
                        | PublicAiEvent::MiniAppUpdated { .. }
                        | PublicAiEvent::Notice { .. }
                        | PublicAiEvent::WebSearch { .. }
                        | PublicAiEvent::Completed { .. }
                        | PublicAiEvent::Failed { .. }
                        | PublicAiEvent::Cancelled { .. }
                ) && send.try_send(Ok(event)).is_err()
                {
                    break;
                }
                if terminal {
                    break;
                }
            }
            drop(stream);
            drop(send);
            let _ = finished.send(());
        });
        Self {
            events,
            done,
            handle: Some(handle),
            assistant,
        }
    }
}

struct Seed {
    profile: ProfileId,
    workspace: WorkspaceId,
    document: DocumentId,
    list: Uuid,
    chat: ChatId,
}

fn seed(stack: &Stack, provider: &str) -> Result<Seed, String> {
    let client = stack.client();
    let created = client
        .store()
        .create_profile_with_workspace(&koru_store_wire::CreateProfileWithWorkspace {
            id: ProfileId::new(),
            name: "Managed MCP smoke".into(),
            locale: Some("en".into()),
            purpose: None,
            avatar: None,
            icon: None,
            color: None,
            workspace: koru_store_wire::FirstWorkspace {
                name: "Synthetic mini-app workspace".into(),
                icon: None,
                color: None,
            },
        })
        .map_err(|_| "synthetic profile/workspace creation failed")?;
    let workspace = created.workspace.id;
    let document = DocumentId::new();
    let mut source = koru_core::schema::WorkspaceDoc::create(
        koru_core::WorkspaceId::from_uuid(workspace.as_uuid()),
        "Synthetic source",
    )
    .map_err(|_| "synthetic document construction failed")?;
    source
        .stamp_document_id(koru_core::DocumentId::from_uuid(document.as_uuid()))
        .map_err(|_| "stamp synthetic document")?;
    let root = source
        .insert(
            koru_core::schema::Parent::Root,
            koru_core::node::Node::document(DOCUMENT_TITLE)
                .map_err(|_| "synthetic document node")?,
        )
        .map_err(|_| "insert synthetic document root")?;
    source
        .insert(
            koru_core::schema::Parent::Node(root),
            koru_core::node::Node::paragraph("Only synthetic smoke-test content.")
                .map_err(|_| "synthetic paragraph")?,
        )
        .map_err(|_| "insert synthetic paragraph")?;
    client
        .store()
        .import_document_at(
            workspace,
            document,
            DOCUMENT_TITLE,
            koru_core::explore::KIND_DOCUMENT,
            None,
            source
                .export_snapshot()
                .map_err(|_| "export synthetic document")?,
        )
        .map_err(|_| "register synthetic document")?;
    let mut system =
        koru_core::system::SystemDoc::create().map_err(|_| "create synthetic list registry")?;
    let list = system
        .create_list(LIST_NAME, None, 0)
        .map_err(|_| "create synthetic list")?
        .as_uuid();
    client
        .store()
        .import_document(
            workspace,
            "Synthetic list registry",
            koru_core::explore::KIND_SYSTEM,
            system
                .export_snapshot()
                .map_err(|_| "export synthetic registry")?,
        )
        .map_err(|_| "register synthetic list registry")?;
    let chat = ChatId::new();
    client
        .ai()
        .create_chat(&CreateChatRequest {
            chat_id: chat,
            title: "Synthetic managed mini-app smoke".into(),
            profile_id: Some(created.profile.id),
            model: ModelSelection {
                provider: AiProviderId::parse(provider).expect("fixed provider"),
                requested_model: AGENT_DEFAULT_MODEL.into(),
                default_reasoning: ReasoningChoice::Default,
            },
        })
        .map_err(|error| format!("synthetic chat creation refused: {}", error.code))?;
    let cwd = stack.root.path().join("synthetic-cwd");
    std::fs::create_dir(&cwd).map_err(|_| "create synthetic working folder")?;
    client
        .ai()
        .update_chat_folder(
            chat,
            &UpdateChatFolderRequest {
                folder: Some(cwd.to_string_lossy().into_owned()),
            },
        )
        .map_err(|error| format!("set synthetic chat folder: {}", error.code))?;
    client
        .ai()
        .update_chat_tool_policy(
            chat,
            &UpdateChatToolPolicyRequest {
                mode: ToolMode::Manual,
                workspace_ids: vec![workspace],
            },
        )
        .map_err(|error| format!("grant only synthetic workspace: {}", error.code))?;
    Ok(Seed {
        profile: created.profile.id,
        workspace,
        document,
        list,
        chat,
    })
}

enum Offer {
    Created(mini_apps::Declaration),
    Updated(mini_apps::UpdateEnvelope),
}

fn audit_tool(
    activity: &ToolActivity,
    allowed: &[&str],
    seen: &mut BTreeSet<String>,
) -> Result<(), String> {
    if activity.status == ToolActivityStatus::ApprovalRequired
        || activity
            .live
            .as_ref()
            .is_some_and(|live| !live.questions.is_empty())
    {
        return Err(
            "provider requested permission or human input; nothing was autoapproved".into(),
        );
    }
    let named = activity.agent_label.as_deref().and_then(CallShape::of);
    let alias_flag = |server: &str| {
        activity.agent_label.as_deref().is_some_and(|label| {
            [
                format!("mcp__{server}__"),
                format!("{server}."),
                format!("{server}/"),
                format!("{server}:"),
            ]
            .iter()
            .any(|prefix| label.starts_with(prefix))
        })
    };
    let diagnostic = format!(
        "kind={:?}, status={:?}, koru_alias={}, desktop_alias={}",
        AgentToolKind::parse(&activity.tool_name),
        activity.status,
        alias_flag(koru_session_profiles::MCP_SERVER_NAME),
        alias_flag(koru_session_profiles::CODEX_DESKTOP_MCP_SERVER_NAME)
    );
    if let Some(CallShape::Koru(name)) = named {
        if !allowed.contains(&name.as_str()) {
            return Err(format!(
                "provider attempted an unexpected Koru tool; {diagnostic}"
            ));
        }
        seen.insert(name);
    } else if allowed.contains(&activity.tool_name.as_str()) {
        seen.insert(activity.tool_name.clone());
    } else if AgentToolKind::parse(&activity.tool_name) != Some(AgentToolKind::Tool)
        || activity.status.is_terminal()
        || activity.agent_label.is_some()
    {
        return Err(format!(
            "provider attempted a non-allowlisted tool; {diagnostic}; no permission was granted"
        ));
    }
    Ok(())
}

/// Classify only messages owned by the adapter. Never echo an unrecognized
/// provider message: it may contain text, paths, arguments or credentials.
fn terminal_reason(error: &koru_ai_core::AiError) -> &'static str {
    match error.public_message.as_str() {
        "The agent could not be started." => "agent_start_refused",
        "The agent refused that model." => "agent_model_refused",
        "The agent could not finish this turn." => "agent_turn_failed",
        "Part of the agent's answer was lost." => "agent_event_gap",
        "The agent session failed." => "agent_session_failed",
        "The agent's session ended unexpectedly." => "agent_session_ended",
        "The agent service is not answering." => "agent_service_not_answering",
        "The agent service is not available right now." => "agent_service_unavailable",
        "The agent is still starting. Try again." => "agent_still_starting",
        "This agent is not installed." => "agent_not_installed",
        koru_ai_core::AGENT_REASONING_REJECTED => "agent_reasoning_refused",
        _ => "unclassified_message_withheld",
    }
}

fn session_error_code(error: &koru_session_client::ClientError) -> String {
    use koru_session_client::ClientError;
    match error {
        ClientError::Session(failure) => safe_session_code(&failure.code),
        ClientError::Disconnected => "disconnected".into(),
        ClientError::TimedOut => "timed_out".into(),
        ClientError::Ipc(_) => "ipc".into(),
        ClientError::Handshake(_) => "handshake".into(),
        ClientError::Protocol(_) => "protocol".into(),
        ClientError::UnexpectedResponse => "unexpected_response".into(),
    }
}

fn safe_session_code(code: &koru_session_protocol::ErrorCode) -> String {
    // ErrorCode also has Unknown(String). Never echo an unknown wire value.
    koru_session_protocol::ErrorCode::ALL
        .iter()
        .find(|known| *known == code)
        .map_or_else(
            || "unknown_code_withheld".into(),
            |known| known.as_str().into(),
        )
}

/// Diagnostic queries are exact-provider, root-qualified and prompt-free.
/// They run only after a failed exercise, before its owned host is disposed.
/// The catalog request has the client's existing 30-second request deadline.
fn report_failure_diagnostics(stack: &Stack, provider: &str) {
    use koru_session_protocol::{ExecutableAvailability, ProcessState};
    let Some(host) = &stack.sessions else { return };
    let client = match sessions_harness::client(&host.endpoint, "managed-mini-app-diagnostics") {
        Ok(client) => client,
        Err(error) => {
            eprintln!(
                "managed_mini_app_diagnostic: connect={}",
                session_error_code(&error)
            );
            return;
        }
    };
    let kind = provider.strip_prefix("agent-").expect("fixed provider");
    let print_readiness = |phase| match client.subscribe_providers() {
        Ok((rows, subscription)) => {
            drop(subscription);
            if let Some(row) = rows.iter().find(|row| row.kind.as_str() == kind) {
                let (availability, code) = match &row.availability {
                    ExecutableAvailability::Checking => ("checking", None),
                    ExecutableAvailability::Available { .. } => ("available", None),
                    ExecutableAvailability::Missing => ("missing", None),
                    ExecutableAvailability::SignedOut { code } => ("signed_out", Some(code)),
                    ExecutableAvailability::Incompatible { code, .. } => {
                        ("incompatible", Some(code))
                    }
                    ExecutableAvailability::Failed { code } => ("failed", Some(code)),
                };
                let code = code.map(safe_session_code);
                eprintln!(
                    "managed_mini_app_readiness: phase={phase}, provider={provider}, availability={availability}, code={code:?}, failure={:?}",
                    row.failure
                );
            } else {
                eprintln!(
                    "managed_mini_app_readiness: phase={phase}, provider={provider}, row=absent"
                );
            }
        }
        Err(error) => eprintln!(
            "managed_mini_app_readiness: phase={phase}, error={}",
            session_error_code(&error)
        ),
    };
    match client.list() {
        Ok(sessions) => {
            eprintln!("managed_mini_app_sessions: count={}", sessions.len());
            let still_active = sessions
                .iter()
                .any(|session| session.active_turn_id.is_some());
            for session in sessions {
                let (process, code) = match &session.process {
                    ProcessState::Starting => ("starting", None),
                    ProcessState::Running => ("running", None),
                    ProcessState::Exited { .. } => ("exited", None),
                    ProcessState::Failed { error } => ("failed", Some(&error.code)),
                };
                let code = code.map(safe_session_code);
                eprintln!(
                    "managed_mini_app_session: process={process}, code={code:?}, managed={:?}, model_requested={}, model_reported={}, active_turn={}",
                    session.managed,
                    session
                        .model
                        .as_ref()
                        .is_some_and(|model| model.requested.is_some()),
                    session
                        .model
                        .as_ref()
                        .is_some_and(|model| model.reported.is_some()),
                    session.active_turn_id.is_some()
                );
            }
            // The HTTP cancellation can be acknowledged before the managed
            // runtime finishes stopping. Do not spend longer on diagnostics
            // while that runtime still reports an active turn.
            if still_active {
                eprintln!("managed_mini_app_diagnostic: skipped=active_session_cleanup_first");
                return;
            }
        }
        Err(error) => {
            eprintln!(
                "managed_mini_app_sessions: error={}",
                session_error_code(&error)
            );
            return;
        }
    }
    print_readiness("after_turn");
    match client.discover_provider_catalog(
        kind.parse().expect("fixed provider kind"),
        stack.root.path().join("synthetic-cwd"),
    ) {
        Ok((models, modes)) => eprintln!(
            "managed_mini_app_catalog: provider={provider}, models={}, modes_present={}, current_mode_present={}",
            models.models().len(),
            modes.is_some(),
            modes
                .as_ref()
                .is_some_and(|modes| modes.current().is_some())
        ),
        Err(error) => eprintln!(
            "managed_mini_app_catalog: provider={provider}, error={}",
            session_error_code(&error)
        ),
    }
    print_readiness("after_catalog");
}

fn turn(
    stack: &mut Stack,
    seed: &Seed,
    host: HostContext,
    prompt: String,
    allowed: &[&str],
) -> Result<(Offer, BTreeSet<String>), String> {
    let deadline = Instant::now() + TURN_LIMIT;
    let stream = stack
        .client()
        .ai()
        .send_turn(&SendTurnRequest {
            mini_app_host: Some(host),
            operation_id: Uuid::new_v4(),
            chat_id: seed.chat,
            surface: AiSurface::ContextPane,
            markdown: prompt,
            explicit_refs: vec![],
            web_search: false,
            reasoning_override: None,
            system_prompt_override: None,
            replace_pinned_documents: false,
            attachment_ids: vec![],
            answers: None,
            skills: vec![],
        })
        .map_err(|error| format!("send live turn refused: {}", error.code))?;
    let attempt = stream.accepted.attempt_id;
    stack.active = Some(attempt);
    stack.pump = Some(Pump::start(stream));
    let mut offer = None;
    let mut tools = BTreeSet::new();
    let mut sequence = None;
    let mut started = false;
    let assistant = stack.pump.as_ref().unwrap().assistant.clone();
    let mut activity_summary = diagnostics::ActivitySummary::default();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("live turn exceeded its wall-clock budget".into());
        }
        let item = stack
            .pump
            .as_ref()
            .expect("live reader")
            .events
            .recv_timeout(remaining.min(Duration::from_secs(1)));
        let event = match item {
            Ok(event) => event.map_err(str::to_owned)?,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("live stream ended before a confirmed completion".into());
            }
        };
        if event.event.attempt_id() != attempt
            || sequence.is_some_and(|prior| event.sequence <= prior)
        {
            return Err("live stream identity or ordering mismatch".into());
        }
        sequence = Some(event.sequence);
        match event.event {
            PublicAiEvent::Started { resolved_model, .. } => {
                started = true;
                eprintln!(
                    "managed_mini_app_model: reported={}",
                    resolved_model.is_some()
                );
            }
            PublicAiEvent::ToolActivity { activity, .. } => {
                audit_tool(&activity, allowed, &mut tools)?;
                activity_summary.observe(&activity);
            }
            PublicAiEvent::MiniAppCreated {
                chat_id,
                profile_id,
                host: actual,
                call_id,
                envelope,
                ..
            } => {
                if offer.is_some()
                    || host.refinement.is_some()
                    || chat_id != seed.chat
                    || profile_id != seed.profile
                    || actual != host
                    || call_id.is_empty()
                {
                    return Err("unexpected or duplicate creation offering".into());
                }
                let declaration =
                    mini_apps::Declaration::validate(envelope).map_err(str::to_owned)?;
                if declaration.selection != mini_apps::TaskSelection::List(seed.list)
                    || declaration.document != Some(seed.document)
                    || !declaration.swapped
                {
                    return Err(
                        "model did not create the requested non-default bound declaration".into(),
                    );
                }
                offer = Some(Offer::Created(declaration));
            }
            PublicAiEvent::MiniAppUpdated {
                chat_id,
                profile_id,
                host: actual,
                call_id,
                envelope,
                ..
            } => {
                if offer.is_some()
                    || host.refinement.is_none()
                    || chat_id != seed.chat
                    || profile_id != seed.profile
                    || actual != host
                    || call_id.is_empty()
                {
                    return Err("unexpected or duplicate refinement offering".into());
                }
                if envelope.arrangement().map_err(str::to_owned)? {
                    return Err("model did not put tasks on the left".into());
                }
                if envelope.update.surface_id
                    != host.refinement.expect("checked target").surface_name()
                {
                    return Err("refinement targeted a different live surface".into());
                }
                offer = Some(Offer::Updated(envelope));
            }
            PublicAiEvent::Completed { .. } => {
                stack.active = None;
                stack.close_pump().map_err(str::to_owned)?;
                let Some(offered) = offer else {
                    activity_summary.report();
                    assistant.lock().unwrap().report();
                    return Err(format!(
                        "provider completed without the typed mini-app offering; expected_tools_seen={tools:?}"
                    ));
                };
                return Ok((offered, tools));
            }
            PublicAiEvent::Failed { error, .. } => {
                stack.active = None;
                activity_summary.report();
                assistant.lock().unwrap().report();
                return Err(format!(
                    "live provider failed: class={:?}, reason={}, retryable={}, started={started}, offered={}, expected_tools_seen={tools:?}",
                    error.class,
                    terminal_reason(&error),
                    error.retryable,
                    offer.is_some()
                ));
            }
            PublicAiEvent::Cancelled { .. } => {
                stack.active = None;
                return Err("live provider cancelled".into());
            }
            PublicAiEvent::Notice { .. } => {
                return Err("live mini-app capability became unavailable".into());
            }
            PublicAiEvent::WebSearch { .. } => {
                return Err("unexpected web-search tool activity".into());
            }
            _ => {}
        }
    }
}

fn exercise(stack: &mut Stack, provider: &str) -> Result<(), String> {
    let seed = seed(stack, provider)?;
    let renderer = Uuid::new_v4();
    let host = HostContext {
        renderer_id: renderer,
        request_id: Uuid::new_v4(),
        workspace_id: seed.workspace,
        refinement: None,
    };
    let restriction = "Use only the Koru desktop stdio MCP bridge named in Koru's injected context, the tools named below, and the live capability injected by Koru. Do not use shell commands, files, web/network tools, other MCP servers, delegation or questions. Never print the capability. If a required tool is unavailable, say so and stop.";
    let (created, mut tools) = turn(
        stack,
        &seed,
        host,
        format!(
            "{restriction} First call {} with query {LIST_NAME:?}. Then call {} to create a mini app using that list, documentId {}, and document on the left/tasks on the right. Use the declared task/document catalog. Return the app by actually invoking the MCP tool, not by printing JSON or describing a hypothetical app.",
            mini_apps::LIST_TOOL,
            mini_apps::CREATE_TOOL,
            seed.document
        ),
        &[mini_apps::LIST_TOOL, mini_apps::CREATE_TOOL],
    )?;
    let Offer::Created(declaration) = created else {
        return Err("creation turn returned an update".into());
    };
    if !tools.contains(mini_apps::LIST_TOOL) || !tools.contains(mini_apps::CREATE_TOOL) {
        return Err("creation did not observe both expected MCP tool calls".into());
    }
    let surface_id = declaration
        .envelope
        .surface
        .surface_id
        .strip_prefix("mini-app-")
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or("creation did not allocate a normalized live surface identity")?;
    let target = RefinementTarget {
        tab_id: 1,
        surface_id,
        base_revision: 0,
    };
    let refined_host = HostContext {
        renderer_id: renderer,
        request_id: Uuid::new_v4(),
        workspace_id: seed.workspace,
        refinement: Some(target),
    };
    let (updated, update_tools) = turn(
        stack,
        &seed,
        refined_host,
        format!(
            "{restriction} Call {} for the current live surface from Koru's injected context. Put tasks on the left and the document on the right (tasks-first). Keep the same selector, tasks, search and document children; change only the root arrangement. Do not create a second app or change list/document bindings.",
            mini_apps::UPDATE_TOOL
        ),
        &[mini_apps::UPDATE_TOOL],
    )?;
    let Offer::Updated(update) = updated else {
        return Err("refinement turn returned a creation".into());
    };
    if update.update.surface_id != target.surface_name()
        || !update_tools.contains(mini_apps::UPDATE_TOOL)
    {
        return Err("refinement did not observe its expected MCP call/target".into());
    }
    tools.extend(update_tools);
    let transcript = stack
        .client()
        .ai()
        .read_chat(seed.chat)
        .map_err(|error| format!("read synthetic transcript: {}", error.code))?;
    let stored =
        serde_json::to_string(&transcript).map_err(|_| "serialize synthetic transcript")?;
    if stored.contains("koru-mini-") {
        return Err("live capability leaked into the durable chat transcript".into());
    }
    eprintln!(
        "managed_mini_app_smoke: provider={provider}, created=1, updated=1, expected_mcp_tools={}",
        tools.len()
    );
    Ok(())
}

fn live(provider: &str) {
    assert!(
        koru_env::switch(koru_env::name::KORU_LIVE_PROVIDER_SMOKE),
        "explicit live-provider opt-in required"
    );
    assert_eq!(
        std::env::var_os(koru_env::name::KORU_TELEMETRY_ENABLED),
        Some("0".into()),
        "explicit telemetry-off environment required for every child"
    );
    let started = Instant::now();
    let mut stack = Stack::start(provider);
    let outcome = exercise(&mut stack, provider);
    if outcome.is_err() {
        // An unexpected action or a deadline must not leave the model running
        // while bounded diagnostic queries inspect the host. Shutdown retries
        // an unacknowledged cancellation before disposing the owned processes.
        let cancelled = stack.cancel_active();
        eprintln!(
            "managed_mini_app_failure_cancel: acknowledged={}",
            cancelled
        );
        if cancelled {
            report_failure_diagnostics(&stack, provider);
        }
    }
    let cleanup = stack.shutdown();
    if outcome.is_err() {
        failure_log::report(stack.root.path(), provider);
    }
    assert!(
        cleanup.is_ok(),
        "owned live-process cleanup did not finish within its budget"
    );
    assert!(
        outcome.is_ok(),
        "managed_mini_app_smoke: provider={provider}, {}",
        outcome.err().unwrap_or_default()
    );
    eprintln!(
        "managed_mini_app_smoke_done: provider={provider}, elapsed_ms={}",
        started.elapsed().as_millis()
    );
}
