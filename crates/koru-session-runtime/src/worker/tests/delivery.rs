// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::providers::{
    AgentCapabilities, AgentControl, AgentInput, AgentStart, ManagedProvider,
    ManagedProviderFactory, ManagedProviderRequest, ProviderDiagnostics, ProviderFailure,
    ProviderVersion, ProviderWake,
};
use koru_session_protocol::AgentTurnId;

#[derive(Default)]
struct MutablePaths(Mutex<BTreeMap<AttachmentId, Result<PathBuf, ErrorCode>>>);

impl MutablePaths {
    fn set(&self, id: AttachmentId, value: Result<PathBuf, ErrorCode>) {
        self.0.lock().unwrap().insert(id, value);
    }
}

impl AttachmentPathResolver for MutablePaths {
    fn resolve(
        &self,
        _session_id: SessionId,
        attachment_id: AttachmentId,
    ) -> Result<PathBuf, ErrorCode> {
        self.0
            .lock()
            .unwrap()
            .get(&attachment_id)
            .cloned()
            .unwrap_or(Err(ErrorCode::AttachmentNotFound))
    }
}

#[derive(Default)]
struct NativeState {
    inner: Mutex<NativeInner>,
    ready: Condvar,
}

#[derive(Default)]
struct NativeInner {
    calls: Vec<AgentInput>,
    failing: BTreeSet<AttachmentId>,
    events: VecDeque<AgentEvent>,
    woken: bool,
    stopped: bool,
    reject_submit: bool,
    hold_events: bool,
    hold_submit: bool,
    /// A provider that takes text into the running turn.
    steerable: bool,
    steered: Vec<AgentInput>,
}

struct NativeWake(Arc<NativeState>);

impl ProviderWake for NativeWake {
    fn wake(&self) {
        let mut inner = self.0.inner.lock().unwrap();
        inner.woken = true;
        drop(inner);
        self.0.ready.notify_one();
    }
}

struct NativeControl {
    state: Arc<NativeState>,
    capabilities: AgentCapabilities,
}

impl AgentControl for NativeControl {
    fn start(&mut self, _start: AgentStart) -> Result<AgentCapabilities, ProviderFailure> {
        Ok(self.capabilities.clone())
    }

    fn capabilities(&self) -> Option<&AgentCapabilities> {
        Some(&self.capabilities)
    }

    fn submit(&mut self, input: AgentInput) -> Result<(), ProviderFailure> {
        let mut inner = self.state.inner.lock().unwrap();
        if inner.reject_submit {
            return Err(ProviderFailure::new(
                ErrorCode::DeliveryUnsupported,
                crate::providers::ProviderStage::Submit,
                crate::providers::ProviderDiagnostic::InvalidInput,
            ));
        }
        let failing = inner.failing.clone();
        inner.events.push_back(AgentEvent::TurnStarted {
            turn_id: input.turn_id,
        });
        for attachment in &input.attachments {
            inner.events.push_back(if failing.contains(&attachment.id) {
                AgentEvent::AttachmentRejected {
                    turn_id: input.turn_id,
                    attachment_id: attachment.id,
                    code: ErrorCode::ProviderUnavailable,
                }
            } else {
                AgentEvent::AttachmentAccepted {
                    turn_id: input.turn_id,
                    attachment_id: attachment.id,
                }
            });
        }
        inner.calls.push(input);
        self.state.ready.notify_all();
        while inner.hold_submit {
            let (next, waited) = self
                .state
                .ready
                .wait_timeout(inner, Duration::from_secs(2))
                .unwrap();
            inner = next;
            assert!(
                !waited.timed_out(),
                "fixture submit must stay held until the lease changes"
            );
        }
        drop(inner);
        self.state.ready.notify_one();
        Ok(())
    }

    fn steer(&mut self, input: AgentInput) -> Result<(), ProviderFailure> {
        let mut inner = self.state.inner.lock().unwrap();
        if !inner.steerable {
            return Err(ProviderFailure::new(
                ErrorCode::InvalidTransition,
                crate::providers::ProviderStage::Submit,
                crate::providers::ProviderDiagnostic::InvalidState,
            ));
        }
        inner.steered.push(input);
        Ok(())
    }

    fn cancel(&mut self, turn_id: AgentTurnId) -> Result<(), ProviderFailure> {
        self.state
            .inner
            .lock()
            .unwrap()
            .events
            .push_back(AgentEvent::TurnCancelled {
                turn_id,
                reason: Default::default(),
            });
        self.state.ready.notify_one();
        Ok(())
    }

    fn answer_approval(
        &mut self,
        _approval_id: ApprovalId,
        _decision: ApprovalDecision,
        _choice: Option<koru_session_protocol::ApprovalChoiceId>,
    ) -> Result<(), ProviderFailure> {
        Ok(())
    }

    fn next_event(&mut self, timeout: Duration) -> Result<Option<AgentEvent>, ProviderFailure> {
        let mut inner = self.state.inner.lock().unwrap();
        let deadline = Instant::now().checked_add(timeout);
        loop {
            if !inner.hold_events
                && let Some(event) = inner.events.pop_front()
            {
                return Ok(Some(event));
            }
            if inner.woken || inner.stopped {
                inner.woken = false;
                return Ok(None);
            }
            let Some(deadline) = deadline else {
                inner = self.state.ready.wait(inner).unwrap();
                continue;
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let (next, waited) = self.state.ready.wait_timeout(inner, remaining).unwrap();
            inner = next;
            if waited.timed_out() {
                return Ok(None);
            }
        }
    }

    fn diagnostics(&self) -> ProviderDiagnostics {
        ProviderDiagnostics::default()
    }

    fn shutdown(&mut self) -> Result<(), ProviderFailure> {
        self.state.inner.lock().unwrap().stopped = true;
        self.state.ready.notify_all();
        Ok(())
    }
}

struct NativeFactory {
    state: Arc<NativeState>,
    capabilities: CapabilitySet,
}

impl ManagedProviderFactory for NativeFactory {
    fn create(&self, request: ManagedProviderRequest) -> Result<ManagedProvider, ProviderFailure> {
        let capabilities = AgentCapabilities {
            provider: request.provider,
            executable_version: ProviderVersion::from_str("1.0.0").unwrap(),
            protocol_version: ProviderVersion::from_str("fixture-v1").unwrap(),
            capabilities: self.capabilities.clone(),
            delegation: crate::providers::DelegationFacts::undeclared(),
        };
        Ok(ManagedProvider {
            control: Box::new(NativeControl {
                state: Arc::clone(&self.state),
                capabilities,
            }),
            wake: Arc::new(NativeWake(Arc::clone(&self.state))),
        })
    }
}

struct ManagedHarness {
    _registry: SessionRegistry,
    handle: SessionHandle,
    state: Arc<NativeState>,
}

fn managed_harness(capabilities: CapabilitySet) -> ManagedHarness {
    let state = Arc::new(NativeState::default());
    let mut providers = crate::providers::ManagedProviderRegistry::default();
    providers
        .register(
            koru_session_profiles::CODEX_REGISTRATION,
            Arc::new(NativeFactory {
                state: Arc::clone(&state),
                capabilities: capabilities.clone(),
            }),
        )
        .unwrap();
    let registry = SessionRegistry::new(SessionRuntimeConfig {
        stats_enabled: true,
        trace: SessionTrace::new(false),
        lifecycle: Arc::new(NoopLifecycleSink),
        resources: Arc::new(NoopResourceLifecycle),
        attachment_paths: Arc::new(NoopAttachmentPathResolver),
        managed_providers: Arc::new(providers),
    });
    let cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let provider = ProviderKind::from_str("codex").unwrap();
    let handle = registry
        .create(SessionCreate {
            profile: SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("codex").unwrap()),
            parent_session_id: None,
            profile_kind: ProfileKind::Agent,
            display_name: "Managed fixture".into(),
            working_directory: cwd,
            attachment_delivery: AttachmentDeliveryPolicy::NativeStructured,
            capabilities: capabilities.clone(),
            launch: SessionLaunch::Managed {
                provider,
                executable: "fixture".into(),
                delegation: None,
                selection: None,
                mcp_servers: Vec::new(),
            },
            created_at_ms: 10,
            origin: Default::default(),
        })
        .unwrap();
    ManagedHarness {
        _registry: registry,
        handle,
        state,
    }
}

fn delivery_capability(name: &str) -> CapabilitySet {
    CapabilitySet::new([CapabilityName::from_str(name).unwrap()])
}

fn pending_attachment(session_id: SessionId, id: AttachmentId) -> Attachment {
    Attachment {
        id,
        session_id,
        kind: AttachmentKind::Image,
        source: AttachmentSource::Clipboard,
        media_type: "image/png".into(),
        byte_count: 1,
        dimensions: None,
        status: AttachmentStatus::Pending,
        delivery_mode: DeliveryMode::Unsupported,
        delivery: DeliveryState::Pending,
        turn_id: None,
        preview_available: true,
        created_at_ms: 1,
    }
}

#[test]
fn quoted_path_submission_is_ordered_partial_retryable_and_idempotent() {
    let paths = Arc::new(MutablePaths::default());
    let harness = harness_with(
        AttachmentDeliveryPolicy::QuotedPath {
            shell: ShellQuoting::PowerShell,
        },
        delivery_capability(capability::QUOTED_PATH_DELIVERY),
        paths.clone(),
    );
    let handle = harness.owner.handle();
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    let first = AttachmentId::mint();
    let second = AttachmentId::mint();
    handle
        .add_attachment(attached.lease.token, pending_attachment(handle.id(), first))
        .unwrap();
    handle
        .add_attachment(
            attached.lease.token,
            pending_attachment(handle.id(), second),
        )
        .unwrap();
    let first_path = PathBuf::from("C:/root/one image.png");
    let second_path = PathBuf::from("C:/root/two's image.png");
    paths.set(first, Ok(first_path.clone()));
    paths.set(second, Err(ErrorCode::DeliveryFailed));

    let input = PendingInput {
        idempotency_key: IdempotencyKey::mint(),
        text: "echo".into(),
        attachment_ids: vec![first, second],
    };
    let first_result = handle
        .send_pending_input(attached.lease.token, input.clone())
        .unwrap();
    assert!(matches!(
        first_result.attachments[0].state,
        DeliveryState::Delivered { .. }
    ));
    assert!(matches!(
        first_result.attachments[1].state,
        DeliveryState::Failed { .. }
    ));
    let first_command = format!(
        "echo {}\r",
        quote_path(first_path.to_str().unwrap(), ShellQuoting::PowerShell)
    );
    eventually(|| harness.capture.0.lock().unwrap().bytes == first_command.as_bytes());

    paths.set(second, Ok(second_path.clone()));
    let retry = handle
        .send_pending_input(attached.lease.token, input.clone())
        .unwrap();
    assert_eq!(retry.turn_id, first_result.turn_id);
    assert!(
        retry
            .attachments
            .iter()
            .all(|attachment| matches!(attachment.state, DeliveryState::Delivered { .. }))
    );
    let retry_command = format!(
        "{}\r",
        quote_path(second_path.to_str().unwrap(), ShellQuoting::PowerShell)
    );
    let expected = format!("{first_command}{retry_command}");
    eventually(|| harness.capture.0.lock().unwrap().bytes == expected.as_bytes());
    assert_eq!(handle.summary().pending_attachment_count, 0);

    let replay = handle
        .send_pending_input(attached.lease.token, input.clone())
        .unwrap();
    assert_eq!(replay, retry);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(harness.capture.0.lock().unwrap().bytes, expected.as_bytes());
    let names = harness.events.0.lock().unwrap();
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == "terminal.input.submitted")
            .count(),
        1
    );
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == "terminal.attachment.delivered")
            .count(),
        3
    );
    drop(names);

    let mut conflict = input;
    conflict.text = "different".into();
    assert_eq!(
        handle
            .send_pending_input(attached.lease.token, conflict)
            .unwrap_err(),
        ErrorCode::IdempotencyConflict
    );
}

#[test]
fn unsupported_delivery_stays_pending_visible_and_does_not_write_binary() {
    let harness = harness();
    let handle = harness.owner.handle();
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    let attachment_id = AttachmentId::mint();
    handle
        .add_attachment(
            attached.lease.token,
            pending_attachment(handle.id(), attachment_id),
        )
        .unwrap();
    let input = PendingInput {
        idempotency_key: IdempotencyKey::mint(),
        text: String::new(),
        attachment_ids: vec![attachment_id],
    };
    let result = handle
        .send_pending_input(attached.lease.token, input.clone())
        .unwrap();
    assert_eq!(result.attachments[0].mode, DeliveryMode::Unsupported);
    assert_eq!(result.attachments[0].state, DeliveryState::Pending);
    assert_eq!(handle.summary().pending_attachment_count, 1);
    assert!(harness.capture.0.lock().unwrap().bytes.is_empty());
    let replay = handle
        .send_pending_input(attached.lease.token, input)
        .unwrap();
    assert_eq!(replay.turn_id, result.turn_id);
    let names = harness.events.0.lock().unwrap();
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == "terminal.input.submitted")
            .count(),
        1
    );
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == "terminal.attachment.delivered")
            .count(),
        1
    );
}

#[test]
fn pending_input_idempotency_survives_the_whole_live_session() {
    let harness = harness();
    let handle = harness.owner.handle();
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    let first = PendingInput {
        idempotency_key: IdempotencyKey::mint(),
        text: "first".into(),
        attachment_ids: Vec::new(),
    };
    let first_result = handle
        .send_pending_input(attached.lease.token, first.clone())
        .unwrap();
    for index in 0..1_100 {
        handle
            .send_pending_input(
                attached.lease.token,
                PendingInput {
                    idempotency_key: IdempotencyKey::mint(),
                    text: format!("next-{index}"),
                    attachment_ids: Vec::new(),
                },
            )
            .unwrap();
    }
    eventually(|| {
        harness
            .capture
            .0
            .lock()
            .unwrap()
            .bytes
            .ends_with(b"next-1099\r")
    });
    let byte_count = harness.capture.0.lock().unwrap().bytes.len();
    let replay = handle
        .send_pending_input(attached.lease.token, first)
        .unwrap();
    assert_eq!(replay.turn_id, first_result.turn_id);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(harness.capture.0.lock().unwrap().bytes.len(), byte_count);
}

#[test]
fn native_delivery_uses_only_an_advertised_adapter_and_retries_the_same_turn() {
    let first = AttachmentId::mint();
    let second = AttachmentId::mint();
    let capabilities = CapabilitySet::new([
        CapabilityName::from_str(capability::NATIVE_ATTACHMENT_DELIVERY).unwrap(),
        CapabilityName::from_str(capability::MANAGED_EVENTS).unwrap(),
    ]);
    let harness = managed_harness(capabilities);
    harness.state.inner.lock().unwrap().failing.insert(second);
    let handle = &harness.handle;
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    handle
        .add_attachment(attached.lease.token, pending_attachment(handle.id(), first))
        .unwrap();
    handle
        .add_attachment(
            attached.lease.token,
            pending_attachment(handle.id(), second),
        )
        .unwrap();
    let input = PendingInput {
        idempotency_key: IdempotencyKey::mint(),
        text: "inspect these".into(),
        attachment_ids: vec![first, second],
    };
    let initial = handle
        .send_pending_input(attached.lease.token, input.clone())
        .unwrap();
    assert!(initial.attachments.iter().all(|attachment| {
        matches!(
            attachment.state,
            DeliveryState::Delivering { .. }
                | DeliveryState::Delivered { .. }
                | DeliveryState::Failed { .. }
        )
    }));
    eventually(|| {
        let attachments = handle.attachments();
        attachments.iter().any(|attachment| {
            attachment.id == first && matches!(attachment.delivery, DeliveryState::Delivered { .. })
        }) && attachments.iter().any(|attachment| {
            attachment.id == second && matches!(attachment.delivery, DeliveryState::Failed { .. })
        })
    });
    harness.state.inner.lock().unwrap().failing.clear();
    let retried = handle
        .send_pending_input(attached.lease.token, input)
        .unwrap();
    assert_eq!(retried.turn_id, initial.turn_id);
    assert!(retried.attachments.iter().any(|attachment| {
        attachment.attachment_id == first
            && matches!(attachment.state, DeliveryState::Delivered { .. })
    }));
    assert!(retried.attachments.iter().any(|attachment| {
        attachment.attachment_id == second
            && matches!(attachment.state, DeliveryState::Failed { .. })
    }));
    let inner = harness.state.inner.lock().unwrap();
    assert_eq!(inner.calls.len(), 1);
    assert_eq!(
        inner.calls[0]
            .attachments
            .iter()
            .map(|attachment| attachment.id)
            .collect::<Vec<_>>(),
        [first, second]
    );
    assert_eq!(inner.calls[0].turn_id, initial.turn_id);
}

#[test]
fn native_profile_without_the_negotiated_capability_never_calls_the_adapter() {
    let harness = managed_harness(CapabilitySet::default());
    let handle = &harness.handle;
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    let attachment_id = AttachmentId::mint();
    let attachment = handle
        .add_attachment(
            attached.lease.token,
            pending_attachment(handle.id(), attachment_id),
        )
        .unwrap();
    assert_eq!(attachment.delivery_mode, DeliveryMode::Unsupported);
    let result = handle
        .send_pending_input(
            attached.lease.token,
            PendingInput {
                idempotency_key: IdempotencyKey::mint(),
                text: String::new(),
                attachment_ids: vec![attachment_id],
            },
        )
        .unwrap();
    assert_eq!(result.attachments[0].state, DeliveryState::Pending);
    assert!(harness.state.inner.lock().unwrap().calls.is_empty());
    assert_eq!(handle.summary().active_turn_id, None);
}

#[test]
fn managed_text_without_images_preserves_the_accepted_turn_until_immediate_cancel() {
    let capabilities = CapabilitySet::new([
        capability::MANAGED_EVENTS.parse().unwrap(),
        capability::MANAGED_CANCEL.parse().unwrap(),
    ]);
    let harness = managed_harness(capabilities);
    harness.state.inner.lock().unwrap().hold_events = true;
    let handle = &harness.handle;
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    let input = PendingInput {
        idempotency_key: IdempotencyKey::mint(),
        text: "text fixture".into(),
        attachment_ids: Vec::new(),
    };
    let accepted = handle
        .send_pending_input(attached.lease.token, input.clone())
        .unwrap();
    assert_eq!(handle.summary().active_turn_id, Some(accepted.turn_id));
    assert_eq!(
        handle.summary().managed,
        Some(koru_session_protocol::ManagedState::Idle)
    );
    assert_eq!(
        handle
            .send_pending_input(attached.lease.token, input)
            .unwrap()
            .turn_id,
        accepted.turn_id
    );
    let competing = PendingInput {
        idempotency_key: IdempotencyKey::mint(),
        text: "another input".into(),
        attachment_ids: Vec::new(),
    };
    assert_eq!(
        handle
            .send_pending_input(attached.lease.token, competing)
            .unwrap_err(),
        ErrorCode::InvalidTransition
    );
    assert_eq!(handle.summary().active_turn_id, Some(accepted.turn_id));
    handle
        .cancel_turn(
            attached.lease.token,
            IdempotencyKey::mint(),
            accepted.turn_id,
            CancelReason::Run,
        )
        .unwrap();
    {
        let mut inner = harness.state.inner.lock().unwrap();
        assert_eq!(inner.calls.len(), 1);
        assert_eq!(inner.calls[0].text, "text fixture");
        assert!(inner.calls[0].attachments.is_empty());
        inner.hold_events = false;
    }
    harness.state.ready.notify_one();
    eventually(|| handle.summary().active_turn_id.is_none());
    let observation = handle.observe_agent_events().unwrap();
    assert!(observation.events.iter().any(|entry| matches!(entry.event,
        AgentEvent::TurnStarted { turn_id } if turn_id == accepted.turn_id)));
    assert!(observation.events.iter().any(|entry| matches!(entry.event,
        AgentEvent::TurnCancelled { turn_id, reason: CancelReason::Run } if turn_id == accepted.turn_id)));
}

#[test]
fn managed_text_does_not_smuggle_unsupported_images_into_the_provider() {
    let harness = managed_harness(delivery_capability(capability::MANAGED_EVENTS));
    let handle = &harness.handle;
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    let attachment = AttachmentId::mint();
    handle
        .add_attachment(
            attached.lease.token,
            pending_attachment(handle.id(), attachment),
        )
        .unwrap();
    let result = handle
        .send_pending_input(
            attached.lease.token,
            PendingInput {
                idempotency_key: IdempotencyKey::mint(),
                text: "text with an unsupported image".into(),
                attachment_ids: vec![attachment],
            },
        )
        .unwrap();
    assert_eq!(result.attachments[0].mode, DeliveryMode::Unsupported);
    assert_eq!(result.attachments[0].state, DeliveryState::Pending);
    assert_eq!(handle.summary().pending_attachment_count, 1);
    let inner = harness.state.inner.lock().unwrap();
    assert_eq!(inner.calls.len(), 1);
    assert_eq!(inner.calls[0].text, "text with an unsupported image");
    assert!(inner.calls[0].attachments.is_empty());
}

#[test]
fn a_refused_managed_text_submission_retries_without_replacing_another_turn() {
    let harness = managed_harness(CapabilitySet::new([
        capability::MANAGED_EVENTS.parse().unwrap(),
        capability::MANAGED_CANCEL.parse().unwrap(),
    ]));
    let handle = &harness.handle;
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    harness.state.inner.lock().unwrap().reject_submit = true;
    let input = PendingInput {
        idempotency_key: IdempotencyKey::mint(),
        text: "refused fixture".into(),
        attachment_ids: Vec::new(),
    };
    assert_eq!(
        handle
            .send_pending_input(attached.lease.token, input.clone())
            .unwrap_err(),
        ErrorCode::DeliveryUnsupported
    );
    assert_eq!(handle.summary().active_turn_id, None);
    assert_eq!(
        handle.summary().managed,
        Some(koru_session_protocol::ManagedState::Idle)
    );
    assert!(handle.observe_agent_events().unwrap().events.is_empty());
    {
        let mut inner = harness.state.inner.lock().unwrap();
        inner.reject_submit = false;
        inner.hold_events = true;
    }
    let other = PendingInput {
        idempotency_key: IdempotencyKey::mint(),
        text: "different turn".into(),
        attachment_ids: Vec::new(),
    };
    let first = handle
        .send_pending_input(attached.lease.token, other.clone())
        .unwrap();
    assert_eq!(
        handle
            .send_pending_input(attached.lease.token, input.clone())
            .unwrap_err(),
        ErrorCode::InvalidTransition
    );
    assert_eq!(handle.summary().active_turn_id, Some(first.turn_id));
    handle
        .cancel_turn(
            attached.lease.token,
            IdempotencyKey::mint(),
            first.turn_id,
            CancelReason::Run,
        )
        .unwrap();
    harness.state.inner.lock().unwrap().hold_events = false;
    harness.state.ready.notify_all();
    eventually(|| handle.summary().active_turn_id.is_none());
    harness.state.inner.lock().unwrap().hold_events = true;
    let retried = handle
        .send_pending_input(attached.lease.token, input)
        .unwrap();
    assert_eq!(handle.summary().active_turn_id, Some(retried.turn_id));
    assert_eq!(
        handle
            .send_pending_input(attached.lease.token, other)
            .unwrap()
            .turn_id,
        first.turn_id
    );
    assert_eq!(handle.summary().active_turn_id, Some(retried.turn_id));
    handle
        .cancel_turn(
            attached.lease.token,
            IdempotencyKey::mint(),
            retried.turn_id,
            CancelReason::Run,
        )
        .unwrap();
    harness.state.inner.lock().unwrap().hold_events = false;
    harness.state.ready.notify_all();
    eventually(|| handle.summary().active_turn_id.is_none());
    assert_eq!(harness.state.inner.lock().unwrap().calls.len(), 2);
}

#[test]
fn an_accepted_managed_submission_survives_lease_loss_before_its_reply() {
    let harness = managed_harness(delivery_capability(capability::MANAGED_EVENTS));
    let handle = &harness.handle;
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    {
        let mut inner = harness.state.inner.lock().unwrap();
        inner.hold_submit = true;
        inner.hold_events = true;
    }
    let input = PendingInput {
        idempotency_key: IdempotencyKey::mint(),
        text: "accepted before lease loss".into(),
        attachment_ids: Vec::new(),
    };
    let sender = handle.clone();
    let submitted = input.clone();
    let pending =
        std::thread::spawn(move || sender.send_pending_input(attached.lease.token, submitted));
    eventually(|| harness.state.inner.lock().unwrap().calls.len() == 1);
    handle.detach(attached.lease.token).unwrap();
    let next = handle.attach(viewport(2, 0, 6), 21).unwrap();
    harness.state.inner.lock().unwrap().hold_submit = false;
    harness.state.ready.notify_all();
    let accepted = pending.join().unwrap().unwrap();
    assert_eq!(handle.summary().active_turn_id, Some(accepted.turn_id));
    assert_eq!(
        handle
            .send_pending_input(next.lease.token, input)
            .unwrap()
            .turn_id,
        accepted.turn_id
    );
    assert_eq!(harness.state.inner.lock().unwrap().calls.len(), 1);
}

#[test]
fn text_sent_during_a_turn_steers_it_when_the_provider_can_and_is_refused_when_it_cannot() {
    let capabilities = CapabilitySet::new([
        capability::MANAGED_EVENTS.parse().unwrap(),
        capability::MANAGED_CANCEL.parse().unwrap(),
    ]);
    let harness = managed_harness(capabilities);
    {
        let mut inner = harness.state.inner.lock().unwrap();
        inner.hold_events = true;
    }
    let handle = &harness.handle;
    let attached = handle.attach(viewport(1, 0, 6), 20).unwrap();
    let accepted = handle
        .send_pending_input(
            attached.lease.token,
            PendingInput {
                idempotency_key: IdempotencyKey::mint(),
                text: "first".into(),
                attachment_ids: Vec::new(),
            },
        )
        .unwrap();

    let steer = PendingInput {
        idempotency_key: IdempotencyKey::mint(),
        text: "and also this".into(),
        attachment_ids: Vec::new(),
    };
    // A provider with no steer call says what it has always said.
    assert_eq!(
        handle
            .send_pending_input(attached.lease.token, steer.clone())
            .unwrap_err(),
        ErrorCode::InvalidTransition
    );
    assert!(harness.state.inner.lock().unwrap().steered.is_empty());

    harness.state.inner.lock().unwrap().steerable = true;
    let steered = handle
        .send_pending_input(attached.lease.token, steer.clone())
        .unwrap();
    assert_eq!(
        steered.turn_id, accepted.turn_id,
        "it joined the running turn"
    );
    assert_eq!(handle.summary().active_turn_id, Some(accepted.turn_id));
    {
        let inner = harness.state.inner.lock().unwrap();
        assert_eq!(inner.calls.len(), 1, "no second turn was started");
        assert_eq!(inner.steered.len(), 1);
        assert_eq!(inner.steered[0].text, "and also this");
        assert_eq!(inner.steered[0].turn_id, accepted.turn_id);
    }

    // The same key again is answered, not sent twice.
    let again = handle
        .send_pending_input(attached.lease.token, steer)
        .unwrap();
    assert_eq!(again.turn_id, accepted.turn_id);
    assert_eq!(harness.state.inner.lock().unwrap().steered.len(), 1);

    // And the turn it joined can still be cancelled as before.
    handle
        .cancel_turn(
            attached.lease.token,
            IdempotencyKey::mint(),
            accepted.turn_id,
            CancelReason::Run,
        )
        .unwrap();
    harness.state.inner.lock().unwrap().hold_events = false;
    harness.state.ready.notify_one();
    eventually(|| handle.summary().active_turn_id.is_none());
}
