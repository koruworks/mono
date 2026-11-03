// SPDX-License-Identifier: MPL-2.0

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{BufReader, Cursor, Read};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::Duration;

use koru_session_protocol::{
    AgentEvent, AgentTurnId, ApprovalDecision, ApprovalId, AttachmentId, AttachmentKind,
    CapabilityName, CapabilitySet, ErrorCode, ProviderKind, SessionId,
};
use koru_session_runtime::providers::{
    AgentAttachment, AgentCapabilities, AgentControl, AgentInput, AgentStart, ChildSessionPolicy,
    CompatibilityProbe, DelegationFacts, DelegationRequirement, DelegationVerdict, DiscoveryRun,
    EconomicsDeclaration, JsonLineReader, ManagedAvailability, ManagedProviderRequest, McpSupport,
    ProbeCommand, ProbeLimits, ProviderDiagnostic, ProviderDiagnostics, ProviderFailure,
    ProviderStage, ProviderVersion, ResumeSupport, UsageReporting, provider_event_queue,
    run_probe_command,
};
use serde::Deserialize;

const SECRET: &str = "PROVIDER_SECRET_SENTINEL_92741";

fn id<T: FromStr>(value: &str) -> T
where
    T::Err: std::fmt::Debug,
{
    value.parse().unwrap()
}

fn provider() -> ProviderKind {
    "fake".parse().unwrap()
}

fn capabilities() -> AgentCapabilities {
    AgentCapabilities {
        provider: provider(),
        executable_version: "1.2.3".parse().unwrap(),
        protocol_version: "7".parse().unwrap(),
        capabilities: CapabilitySet::new([
            CapabilityName::from_str("managed-events").unwrap(),
            CapabilityName::from_str("managed-cancel").unwrap(),
            CapabilityName::from_str("managed-approval").unwrap(),
            CapabilityName::from_str("managed-child-sessions").unwrap(),
        ]),
        delegation: delegation_facts(),
    }
}

/// A fixture that meets every declared requirement, so each test can take
/// one away and watch the verdict turn.
fn delegation_facts() -> DelegationFacts {
    DelegationFacts {
        starts_in_cwd: true,
        distinguishes_outcomes: true,
        follows_up: true,
        reports_tool_activity: true,
        resume: ResumeSupport::Unsupported,
        mcp: McpSupport::Supported,
        child_sessions: ChildSessionPolicy::NotOffered,
        economics: EconomicsDeclaration {
            usage: UsageReporting::Replacement,
            cached_input: koru_session_protocol::CachedInputRelationship::Separate,
            model: koru_session_protocol::ModelIdentity::default(),
        },
    }
}

/// **`Delegatable` is granted only on all seven.** Each requirement is a
/// declared fact or a negotiated capability; taking any one away turns the
/// verdict, and the verdict names what is missing in §5's order. A missing
/// native resume is not one of the seven.
#[test]
fn delegatable_requires_all_seven() {
    let all = capabilities();
    let verdict = DelegationVerdict::evaluate(&all.capabilities, &all.delegation);
    assert!(verdict.delegatable(), "{:?}", verdict.missing());
    assert!(verdict.missing().is_empty());
    for requirement in DelegationRequirement::ALL {
        assert!(verdict.meets(requirement), "{requirement:?}");
    }

    let without = |facts: DelegationFacts, capabilities: CapabilitySet| {
        DelegationVerdict::evaluate(&capabilities, &facts)
    };
    let drop_capability = |name: &str| {
        CapabilitySet::new(
            all.capabilities
                .iter()
                .filter(|capability| capability.as_str() != name)
                .cloned(),
        )
    };
    let cases: Vec<(DelegationRequirement, DelegationVerdict)> = vec![
        (
            DelegationRequirement::StartInCwd,
            without(
                DelegationFacts {
                    starts_in_cwd: false,
                    ..delegation_facts()
                },
                all.capabilities.clone(),
            ),
        ),
        (
            DelegationRequirement::StructuredEvents,
            without(delegation_facts(), drop_capability("managed-events")),
        ),
        (
            DelegationRequirement::DistinctOutcomes,
            without(
                DelegationFacts {
                    distinguishes_outcomes: false,
                    ..delegation_facts()
                },
                all.capabilities.clone(),
            ),
        ),
        (
            DelegationRequirement::FollowUp,
            without(
                DelegationFacts {
                    follows_up: false,
                    ..delegation_facts()
                },
                all.capabilities.clone(),
            ),
        ),
        (
            DelegationRequirement::Cancel,
            without(delegation_facts(), drop_capability("managed-cancel")),
        ),
        (
            DelegationRequirement::AuditStructure,
            without(
                DelegationFacts {
                    reports_tool_activity: false,
                    ..delegation_facts()
                },
                all.capabilities.clone(),
            ),
        ),
        (
            DelegationRequirement::McpTools,
            without(
                DelegationFacts {
                    mcp: McpSupport::Unsupported,
                    ..delegation_facts()
                },
                all.capabilities.clone(),
            ),
        ),
    ];
    for (requirement, verdict) in cases {
        assert!(!verdict.delegatable(), "{requirement:?} was not required");
        assert!(!verdict.meets(requirement), "{requirement:?}");
        assert!(
            verdict.missing().contains(&requirement),
            "{requirement:?} is not named as missing"
        );
    }

    // Native resume is not one of the seven: declaring it changes nothing,
    // and its absence disqualifies nothing (§7.6's fallback covers it).
    let native = without(
        DelegationFacts {
            resume: ResumeSupport::Native,
            ..delegation_facts()
        },
        all.capabilities.clone(),
    );
    assert!(native.delegatable());

    // Raw PTY is never delegatable (D4): it has no capabilities to evaluate.
    let raw = DelegationVerdict::raw_pty();
    assert!(!raw.delegatable());
    assert_eq!(raw.missing(), DelegationRequirement::ALL.to_vec());
}

/// **A delegation failure never demotes a managed profile.** Discovery's
/// answer is the handshake; the verdict is a different question over the
/// same facts, and a provider that fails it is still managed-compatible.
#[test]
fn failing_delegation_never_demotes_a_managed_profile() {
    let raw_only = AgentCapabilities {
        delegation: DelegationFacts {
            mcp: McpSupport::Unsupported,
            ..delegation_facts()
        },
        ..capabilities()
    };
    let caller = std::thread::current().id();
    let run = DiscoveryRun::spawn(
        vec![Box::new(ThreadProbe {
            provider: provider(),
            caller,
            observed: Arc::new(Mutex::new(None)),
            result: Ok(raw_only.clone()),
            raw_available: true,
        })],
        ProbeLimits::default(),
    )
    .unwrap();
    let discovered = loop {
        if let Some(result) = run.recv_timeout(Duration::from_secs(2)).unwrap() {
            break result;
        }
    };
    let ManagedAvailability::Compatible(handshake) = discovered.managed else {
        panic!(
            "a compatible handshake was demoted: {:?}",
            discovered.managed
        );
    };
    assert!(discovered.raw_available);
    let verdict = DelegationVerdict::evaluate(&handshake.capabilities, &handshake.delegation);
    assert!(!verdict.delegatable());
    assert_eq!(verdict.missing(), vec![DelegationRequirement::McpTools]);
}

/// The three opaque launch values are bounded on the way in and printed
/// on the way out as nothing: an endpoint names a run and a resume token
/// names a provider session, and both are replayable.
#[test]
fn launch_secrets_are_bounded_and_never_printed() {
    use koru_session_protocol::{
        DelegationLaunch, MAX_MCP_TOOLS, MAX_OPAQUE_LAUNCH_BYTES, McpBearer, McpEndpoint,
        McpToolAllowlist, McpToolName, ProviderResumeToken,
    };

    let endpoint: McpEndpoint = format!("http://127.0.0.1:9/{SECRET}").parse().unwrap();
    assert!(!format!("{endpoint:?}").contains(SECRET));
    assert!(
        endpoint.as_str().contains(SECRET),
        "the value itself is intact"
    );
    assert!(McpEndpoint::from_str("ftp://127.0.0.1/x").is_err());
    assert!(McpEndpoint::from_str("http://a b").is_err());
    assert!(McpEndpoint::from_str("http://x\"y").is_err());
    assert!(McpEndpoint::from_str("http://x'y").is_err());
    let long = format!("http://{}", "a".repeat(MAX_OPAQUE_LAUNCH_BYTES));
    assert!(McpEndpoint::from_str(&long).is_err());

    let bearer: McpBearer = SECRET.parse().unwrap();
    assert!(!format!("{bearer:?}").contains(SECRET));
    assert_eq!(bearer.as_str(), SECRET, "the value itself is intact");
    assert!(McpBearer::from_str("").is_err());
    assert!(McpBearer::from_str("with space").is_err());
    assert!(McpBearer::from_str(&"t".repeat(MAX_OPAQUE_LAUNCH_BYTES)).is_ok());
    assert!(McpBearer::from_str(&"t".repeat(MAX_OPAQUE_LAUNCH_BYTES + 1)).is_err());

    let token: ProviderResumeToken = SECRET.parse().unwrap();
    assert!(!format!("{token:?}").contains(SECRET));
    assert!(ProviderResumeToken::from_str("").is_err());
    assert!(ProviderResumeToken::from_str("with space").is_err());
    assert!(ProviderResumeToken::from_str(&"t".repeat(MAX_OPAQUE_LAUNCH_BYTES)).is_ok());
    assert!(ProviderResumeToken::from_str(&"t".repeat(MAX_OPAQUE_LAUNCH_BYTES + 1)).is_err());

    assert!(McpToolName::from_str("koru_register_artifact").is_ok());
    assert!(McpToolName::from_str("Register").is_err());
    assert!(McpToolName::from_str("koru-register").is_err());
    assert!(McpToolName::from_str("").is_err());
    let many = (0..=MAX_MCP_TOOLS).map(|index| format!("tool_{index}").parse().unwrap());
    assert!(McpToolAllowlist::new(many).is_err());
    let duplicates = McpToolAllowlist::new([
        "same".parse::<McpToolName>().unwrap(),
        "same".parse::<McpToolName>().unwrap(),
    ])
    .unwrap();
    assert_eq!(duplicates.len(), 1);

    let launch = DelegationLaunch {
        model: None,
        mcp_endpoint: endpoint,
        credential: Some(bearer),
        tools: duplicates,
        resume: Some(token),
    };
    assert!(!format!("{launch:?}").contains(SECRET));
    let request = ManagedProviderRequest {
        provider: provider(),
        executable: SECRET.into(),
        working_directory: std::path::PathBuf::from(SECRET),
        delegation: Some(launch),
        selection: None,
        mcp_servers: Vec::new(),
    };
    let printed = format!("{request:?}");
    assert!(!printed.contains(SECRET), "{printed}");
    assert!(printed.contains("delegated: true"), "{printed}");
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum FakeWireEvent {
    MessageDelta { turn_id: AgentTurnId, text: String },
    TurnCompleted { turn_id: AgentTurnId },
}

struct Fragmented {
    chunks: VecDeque<Vec<u8>>,
    offset: usize,
}

impl Fragmented {
    fn new(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
        Self {
            chunks: chunks.into_iter().collect(),
            offset: 0,
        }
    }
}

impl Read for Fragmented {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let Some(chunk) = self.chunks.front() else {
            return Ok(0);
        };
        let available = &chunk[self.offset..];
        let count = available.len().min(output.len());
        output[..count].copy_from_slice(&available[..count]);
        self.offset += count;
        if self.offset == chunk.len() {
            self.chunks.pop_front();
            self.offset = 0;
        }
        Ok(count)
    }
}

#[test]
fn fragmented_jsonl_accepts_unknown_fields_and_skips_unknown_events() {
    let turn = "11111111-1111-4111-8111-111111111111";
    let wire = format!(
        "{{\"kind\":\"future_event\",\"secret\":\"{SECRET}\"}}\n\
         {{\"kind\":\"message_delta\",\"turn_id\":\"{turn}\",\"text\":\"hello\",\"additive\":{{\"future\":true}}}}\r\n\
         {{\"kind\":\"turn_completed\",\"turn_id\":\"{turn}\"}}"
    );
    let chunks = wire
        .as_bytes()
        .chunks(7)
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>();
    let mut reader = JsonLineReader::new(BufReader::new(Fragmented::new(chunks)), 4096).unwrap();
    let mut diagnostics = ProviderDiagnostics::default();
    let mut normalized = Vec::new();
    while let Some(frame) = reader.next_frame().unwrap() {
        diagnostics.frames_read += 1;
        match serde_json::from_value::<FakeWireEvent>(frame.into_value()) {
            Ok(FakeWireEvent::MessageDelta { turn_id, text }) => {
                normalized.push(AgentEvent::MessageDelta { turn_id, text });
            }
            Ok(FakeWireEvent::TurnCompleted { turn_id }) => {
                normalized.push(AgentEvent::TurnCompleted { turn_id });
            }
            Err(error) if error.to_string().contains("unknown variant") => {
                diagnostics.unknown_events += 1;
            }
            Err(error) => panic!("known fake event failed: {error}"),
        }
    }

    assert_eq!(diagnostics.frames_read, 3);
    assert_eq!(diagnostics.unknown_events, 1);
    assert_eq!(normalized.len(), 2);
    assert_eq!(
        normalized[0],
        AgentEvent::MessageDelta {
            turn_id: id(turn),
            text: "hello".into(),
        }
    );
}

#[test]
fn malformed_and_oversized_frames_fail_without_echoing_payloads() {
    let mut malformed = JsonLineReader::new(Cursor::new(format!("{{{SECRET}\n")), 1024).unwrap();
    let failure = malformed.next_frame().unwrap_err();
    assert_eq!(failure.diagnostic, ProviderDiagnostic::MalformedFrame);
    assert!(!format!("{failure:?}").contains(SECRET));

    let mut oversized = JsonLineReader::new(Cursor::new(vec![b'x'; 65]), 64).unwrap();
    let failure = oversized.next_frame().unwrap_err();
    assert_eq!(failure.diagnostic, ProviderDiagnostic::FrameTooLarge);

    let split_cr = Fragmented::new([
        br#"{"text":"value""#.to_vec(),
        b"\r".to_vec(),
        br#"}"#.to_vec(),
    ]);
    let mut split_cr = JsonLineReader::new(BufReader::new(split_cr), 1024).unwrap();
    assert_eq!(
        split_cr.next_frame().unwrap().unwrap().value()["text"],
        "value"
    );
}

#[test]
fn normalized_event_queue_applies_explicit_backpressure() {
    let turn_id = AgentTurnId::mint();
    let (sender, receiver) = provider_event_queue(1).unwrap();
    assert_eq!(
        sender
            .try_send(AgentEvent::TurnStarted { turn_id })
            .unwrap(),
        1
    );
    let failure = sender
        .try_send(AgentEvent::TurnCompleted { turn_id })
        .unwrap_err();
    assert_eq!(failure.code, ErrorCode::Backpressure);
    assert_eq!(
        receiver.try_recv().unwrap(),
        Some(AgentEvent::TurnStarted { turn_id })
    );
    assert_eq!(receiver.try_recv().unwrap(), None);
    sender.close();
    assert_eq!(
        receiver.try_recv().unwrap_err().diagnostic,
        ProviderDiagnostic::UnexpectedExit
    );
}

struct FakeControl {
    capabilities: Option<AgentCapabilities>,
    turns: BTreeSet<AgentTurnId>,
    terminal_turns: BTreeSet<AgentTurnId>,
    approvals: BTreeMap<ApprovalId, AgentTurnId>,
    events: VecDeque<Result<AgentEvent, ProviderFailure>>,
    diagnostics: ProviderDiagnostics,
    shut_down: bool,
}

impl FakeControl {
    fn new() -> Self {
        Self {
            capabilities: None,
            turns: BTreeSet::new(),
            terminal_turns: BTreeSet::new(),
            approvals: BTreeMap::new(),
            events: VecDeque::new(),
            diagnostics: ProviderDiagnostics::default(),
            shut_down: false,
        }
    }

    fn request_approval(&mut self, turn_id: AgentTurnId, approval_id: ApprovalId) {
        self.approvals.insert(approval_id, turn_id);
        self.events.push_back(Ok(AgentEvent::ApprovalRequired {
            turn_id,
            approval_id,
            summary: "fixture approval".into(),
            choices: Vec::new(),
        }));
    }

    fn complete(&mut self, turn_id: AgentTurnId) {
        self.terminal_turns.insert(turn_id);
        self.events
            .push_back(Ok(AgentEvent::TurnCompleted { turn_id }));
    }

    fn exit_unexpectedly(&mut self) {
        self.events.push_back(Err(ProviderFailure::new(
            ErrorCode::ProviderFailed,
            ProviderStage::Read,
            ProviderDiagnostic::UnexpectedExit,
        )));
    }
}

impl AgentControl for FakeControl {
    fn start(&mut self, _start: AgentStart) -> Result<AgentCapabilities, ProviderFailure> {
        let capabilities = capabilities();
        self.capabilities = Some(capabilities.clone());
        Ok(capabilities)
    }

    fn capabilities(&self) -> Option<&AgentCapabilities> {
        self.capabilities.as_ref()
    }

    fn submit(&mut self, input: AgentInput) -> Result<(), ProviderFailure> {
        input.validate(1024)?;
        if !self.turns.insert(input.turn_id) {
            return Ok(());
        }
        self.events.push_back(Ok(AgentEvent::TurnStarted {
            turn_id: input.turn_id,
        }));
        Ok(())
    }

    fn cancel(&mut self, turn_id: AgentTurnId) -> Result<(), ProviderFailure> {
        if !self.turns.contains(&turn_id) {
            return Err(ProviderFailure::new(
                ErrorCode::TurnNotFound,
                ProviderStage::Cancel,
                ProviderDiagnostic::InvalidState,
            ));
        }
        if self.terminal_turns.insert(turn_id) {
            self.events.push_back(Ok(AgentEvent::TurnCancelled {
                turn_id,
                reason: Default::default(),
            }));
        }
        Ok(())
    }

    fn answer_approval(
        &mut self,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        choice: Option<koru_session_protocol::ApprovalChoiceId>,
    ) -> Result<(), ProviderFailure> {
        let turn_id = self.approvals.remove(&approval_id).ok_or_else(|| {
            ProviderFailure::new(
                ErrorCode::ApprovalStale,
                ProviderStage::Approval,
                ProviderDiagnostic::InvalidState,
            )
        })?;
        self.events.push_back(Ok(AgentEvent::ApprovalResolved {
            turn_id,
            approval_id,
            decision,
            choice,
        }));
        Ok(())
    }

    fn next_event(&mut self, _timeout: Duration) -> Result<Option<AgentEvent>, ProviderFailure> {
        self.events.pop_front().transpose()
    }

    fn diagnostics(&self) -> ProviderDiagnostics {
        self.diagnostics
    }

    fn shutdown(&mut self) -> Result<(), ProviderFailure> {
        self.shut_down = true;
        Ok(())
    }
}

#[test]
fn contract_covers_idempotency_cancellation_races_approval_children_and_exit() {
    let mut control = FakeControl::new();
    let session_id = SessionId::mint();
    assert_eq!(
        control
            .start(AgentStart {
                session_id,
                parent_session_id: None,
                resume: None,
            })
            .unwrap(),
        capabilities()
    );

    let turn_id = AgentTurnId::mint();
    let input = AgentInput {
        turn_id,
        text: "review".into(),
        attachments: Vec::new(),
        mode: None,
        fast: None,
        reasoning: None,
    };
    control.submit(input.clone()).unwrap();
    control.submit(input).unwrap();
    assert_eq!(
        control.next_event(Duration::ZERO).unwrap(),
        Some(AgentEvent::TurnStarted { turn_id })
    );
    assert_eq!(control.next_event(Duration::ZERO).unwrap(), None);

    let approval_id = ApprovalId::mint();
    control.request_approval(turn_id, approval_id);
    assert!(matches!(
        control.next_event(Duration::ZERO).unwrap(),
        Some(AgentEvent::ApprovalRequired { .. })
    ));
    control
        .answer_approval(approval_id, ApprovalDecision::Approve, None)
        .unwrap();
    assert!(matches!(
        control.next_event(Duration::ZERO).unwrap(),
        Some(AgentEvent::ApprovalResolved { .. })
    ));
    assert_eq!(
        control
            .answer_approval(approval_id, ApprovalDecision::Reject, None)
            .unwrap_err()
            .code,
        ErrorCode::ApprovalStale
    );

    let child_session_id = SessionId::mint();
    control
        .events
        .push_back(Ok(AgentEvent::ChildSessionCreated {
            turn_id,
            child_session_id,
        }));
    assert_eq!(
        control.next_event(Duration::ZERO).unwrap(),
        Some(AgentEvent::ChildSessionCreated {
            turn_id,
            child_session_id,
        })
    );

    control.complete(turn_id);
    control.cancel(turn_id).unwrap();
    assert_eq!(
        control.next_event(Duration::ZERO).unwrap(),
        Some(AgentEvent::TurnCompleted { turn_id })
    );
    assert_eq!(control.next_event(Duration::ZERO).unwrap(), None);

    control.exit_unexpectedly();
    assert_eq!(
        control.next_event(Duration::ZERO).unwrap_err().diagnostic,
        ProviderDiagnostic::UnexpectedExit
    );
    control.shutdown().unwrap();
    assert!(control.shut_down);
}

struct ThreadProbe {
    provider: ProviderKind,
    caller: ThreadId,
    observed: Arc<Mutex<Option<ThreadId>>>,
    result: Result<AgentCapabilities, ProviderFailure>,
    raw_available: bool,
}

impl CompatibilityProbe for ThreadProbe {
    fn provider(&self) -> ProviderKind {
        self.provider.clone()
    }

    fn raw_available(&self) -> bool {
        assert_ne!(std::thread::current().id(), self.caller);
        self.raw_available
    }

    fn probe(&self, _limits: ProbeLimits) -> Result<AgentCapabilities, ProviderFailure> {
        let current = std::thread::current().id();
        assert_ne!(current, self.caller);
        *self.observed.lock().unwrap() = Some(current);
        self.result.clone()
    }
}

#[test]
fn discovery_runs_off_caller_and_only_a_matching_handshake_enables_managed_mode() {
    let caller = std::thread::current().id();
    let observed = Arc::new(Mutex::new(None));
    let wrong = AgentCapabilities {
        provider: "other".parse().unwrap(),
        ..capabilities()
    };
    let run = DiscoveryRun::spawn(
        vec![
            Box::new(ThreadProbe {
                provider: provider(),
                caller,
                observed: Arc::clone(&observed),
                result: Ok(capabilities()),
                raw_available: true,
            }),
            Box::new(ThreadProbe {
                provider: "wrong".parse().unwrap(),
                caller,
                observed: Arc::new(Mutex::new(None)),
                result: Ok(wrong),
                raw_available: true,
            }),
            Box::new(ThreadProbe {
                provider: "missing".parse().unwrap(),
                caller,
                observed: Arc::new(Mutex::new(None)),
                result: Err(ProviderFailure::new(
                    ErrorCode::ProviderUnavailable,
                    ProviderStage::Spawn,
                    ProviderDiagnostic::MissingExecutable,
                )),
                raw_available: false,
            }),
        ],
        ProbeLimits::default(),
    )
    .unwrap();

    let mut results = Vec::new();
    while results.len() < 3 {
        if let Some(result) = run.recv_timeout(Duration::from_secs(2)).unwrap() {
            results.push(result);
        }
    }
    assert!(observed.lock().unwrap().is_some());
    assert!(results.iter().any(|result| result.managed.enabled()));
    assert!(
        results
            .iter()
            .any(|result| matches!(result.managed, ManagedAvailability::Incompatible { .. }))
    );
    assert!(results.iter().any(|result| {
        !result.raw_available && matches!(result.managed, ManagedAvailability::Missing { .. })
    }));
}

#[test]
fn sensitive_inputs_and_probe_material_are_redacted_from_debug() {
    let input = AgentInput {
        turn_id: AgentTurnId::mint(),
        text: SECRET.into(),
        attachments: vec![AgentAttachment {
            id: AttachmentId::mint(),
            kind: AttachmentKind::Image,
            media_type: format!("image/png;{SECRET}"),
        }],
        mode: None,
        fast: None,
        reasoning: None,
    };
    assert!(!format!("{input:?}").contains(SECRET));

    let mut reader =
        JsonLineReader::new(Cursor::new(format!(r#"{{"secret":"{SECRET}"}}"#)), 1024).unwrap();
    let frame = reader.next_frame().unwrap().unwrap();
    assert!(!format!("{frame:?}").contains(SECRET));

    let command = ProbeCommand::new(SECRET)
        .arg(SECRET)
        .stdin(SECRET.as_bytes().to_vec());
    assert!(!format!("{command:?}").contains(SECRET));
    let version = ProviderVersion::from_str(SECRET).unwrap();
    assert!(!format!("{version:?}").contains(SECRET));
}

#[test]
fn command_probe_reports_versions_and_missing_tools_without_leaking_output() {
    #[cfg(windows)]
    let command = ProbeCommand::new("cmd.exe")
        .arg("/D")
        .arg("/S")
        .arg("/C")
        .arg(format!("echo 1.2.3-{SECRET}"));
    #[cfg(not(windows))]
    let command = ProbeCommand::new("sh")
        .arg("-c")
        .arg(format!("printf '1.2.3-{SECRET}'"));

    let output = run_probe_command(command, ProbeLimits::default()).unwrap();
    assert!(output.success());
    assert!(String::from_utf8_lossy(output.stdout()).contains(SECRET));
    assert!(!format!("{output:?}").contains(SECRET));

    let missing = run_probe_command(
        ProbeCommand::new("koru-provider-fixture-definitely-missing-92741"),
        ProbeLimits::default(),
    )
    .unwrap_err();
    assert_eq!(missing.code, ErrorCode::ProviderUnavailable);
    assert_eq!(missing.diagnostic, ProviderDiagnostic::MissingExecutable);
}
