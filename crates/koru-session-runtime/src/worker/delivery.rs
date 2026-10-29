// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeSet;
use std::path::PathBuf;

use koru_events::{TerminalDeliveryMode, TerminalDeliveryOutcome, TerminalInputKind};
use koru_session_observability::{AttachmentDeliveredFact, InputSubmittedFact};
use koru_session_protocol::{
    AgentTurnId, AttachmentDeliveryPolicy, AttachmentDeliveryResult, AttachmentId, CapabilitySet,
    ControllerLeaseToken, DeliveryMode, DeliveryState, ErrorCode, PendingInput, ProviderKind,
    SendPendingInputResult, SessionFailure, SessionId, SessionMode, ShellQuoting, capability,
};
use koru_terminal_emulator::encode_paste;

use super::{
    SessionHandle, SessionIo, Shared, State, map_transport, refresh_pending_attachment_count,
    require_lease,
};
use crate::providers::{AgentAttachment, AgentInput};

/// Rooted attachment-path lookup supplied by the process that owns storage.
/// Implementations return a previously verified path and perform no disk I/O.
pub trait AttachmentPathResolver: Send + Sync + 'static {
    fn resolve(
        &self,
        session_id: SessionId,
        attachment_id: AttachmentId,
    ) -> Result<PathBuf, ErrorCode>;
}

#[derive(Default)]
pub struct NoopAttachmentPathResolver;

impl AttachmentPathResolver for NoopAttachmentPathResolver {
    fn resolve(
        &self,
        _session_id: SessionId,
        _attachment_id: AttachmentId,
    ) -> Result<PathBuf, ErrorCode> {
        Err(ErrorCode::DeliveryFailed)
    }
}

#[derive(Clone)]
pub(super) enum ResolvedDelivery {
    Native { provider: ProviderKind },
    QuotedPath { shell: ShellQuoting },
    Unsupported { raw_text: bool },
}

impl ResolvedDelivery {
    pub(super) fn mode(&self) -> DeliveryMode {
        match self {
            Self::Native { .. } => DeliveryMode::NativeStructured,
            Self::QuotedPath { .. } => DeliveryMode::QuotedPath,
            Self::Unsupported { .. } => DeliveryMode::Unsupported,
        }
    }
}

pub(super) fn resolve_delivery(
    policy: AttachmentDeliveryPolicy,
    mode: &SessionMode,
    capabilities: &CapabilitySet,
) -> ResolvedDelivery {
    match (policy, mode) {
        (AttachmentDeliveryPolicy::QuotedPath { shell }, SessionMode::RawPty)
            if capabilities.supports(capability::QUOTED_PATH_DELIVERY) =>
        {
            ResolvedDelivery::QuotedPath { shell }
        }
        (AttachmentDeliveryPolicy::NativeStructured, SessionMode::Managed { provider })
            if capabilities.supports(capability::NATIVE_ATTACHMENT_DELIVERY) =>
        {
            ResolvedDelivery::Native {
                provider: provider.clone(),
            }
        }
        (_, SessionMode::RawPty) => ResolvedDelivery::Unsupported { raw_text: true },
        _ => ResolvedDelivery::Unsupported { raw_text: false },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ReportedOutcome {
    Delivered,
    Failed,
    Unsupported,
}

pub(super) fn mark_reported(
    state: &mut State,
    turn_id: AgentTurnId,
    attachment_id: AttachmentId,
    outcome: ReportedOutcome,
) -> bool {
    state
        .pending_inputs
        .values_mut()
        .find(|submission| submission.turn_id == turn_id)
        .is_some_and(|submission| submission.reported.insert((attachment_id, outcome)))
}

pub(super) fn publish_delivery_fact(
    shared: &Shared,
    attachment_id: AttachmentId,
    mode: DeliveryMode,
    outcome: ReportedOutcome,
) {
    let event = AttachmentDeliveredFact {
        session_id: shared.id,
        attachment_id,
        delivery_mode: metric_delivery_mode(mode),
        outcome: metric_delivery_outcome(outcome),
    }
    .commit(shared.trace);
    shared.lifecycle.publish(event.into());
}

pub(super) struct PendingSubmission {
    fingerprint: blake3::Hash,
    turn_id: AgentTurnId,
    command_sent: bool,
    in_flight: bool,
    reported: BTreeSet<(AttachmentId, ReportedOutcome)>,
}

impl SessionHandle {
    pub fn send_pending_input(
        &self,
        lease: ControllerLeaseToken,
        input: PendingInput,
    ) -> Result<SendPendingInputResult, ErrorCode> {
        validate_input(&input)?;
        if let Some(result) = self.steer_running_turn(lease, &input)? {
            return Ok(result);
        }
        let (turn_id, report_input) = self.begin_pending_input(lease, &input)?;
        if report_input {
            let input_kind = input_kind(&input);
            let event = InputSubmittedFact {
                session_id: self.shared.id,
                input_kind,
            }
            .commit(self.shared.trace);
            self.shared.lifecycle.publish(event.into());
        }

        let outcome = match &self.shared.attachment_delivery {
            ResolvedDelivery::Native { provider } => {
                self.deliver_native(lease, &input, turn_id, provider.clone())
            }
            ResolvedDelivery::QuotedPath { shell } => {
                self.deliver_raw(lease, &input, turn_id, Some(*shell))
            }
            ResolvedDelivery::Unsupported { raw_text: true } => {
                self.deliver_raw(lease, &input, turn_id, None)
            }
            ResolvedDelivery::Unsupported { raw_text: false } if input.text.is_empty() => Ok(()),
            ResolvedDelivery::Unsupported { raw_text: false } => {
                // Image support does not gate text. The structured submit path
                // includes only attachments whose delivery mode is native.
                let provider = match &self.shared.lock().summary.mode {
                    SessionMode::Managed { provider } => provider.clone(),
                    SessionMode::RawPty => return Err(ErrorCode::DeliveryUnsupported),
                };
                self.deliver_native(lease, &input, turn_id, provider)
            }
        };

        self.finish_pending_input(&input, turn_id, outcome)
    }

    /// Text sent while a managed turn is running goes into that turn when the
    /// provider can take it (`AgentControl::steer`), instead of being refused.
    /// `Ok(None)` means this is not that case and the ordinary path decides:
    /// no running turn, a retry of an input already seen, or an attachment
    /// (steering carries text only).
    fn steer_running_turn(
        &self,
        lease: ControllerLeaseToken,
        input: &PendingInput,
    ) -> Result<Option<SendPendingInputResult>, ErrorCode> {
        let active = {
            let state = self.shared.lock();
            require_lease(&state, lease)?;
            let running = matches!(state.summary.mode, SessionMode::Managed { .. })
                && state.summary.process == koru_session_protocol::ProcessState::Running;
            match state.summary.active_turn_id {
                Some(active)
                    if running
                        && input.attachment_ids.is_empty()
                        && !state.pending_inputs.contains_key(&input.idempotency_key) =>
                {
                    active
                }
                _ => return Ok(None),
            }
        };
        let SessionIo::Managed(managed) = &self.shared.io else {
            return Ok(None);
        };
        managed.steer(AgentInput {
            turn_id: active,
            text: input.text.clone(),
            attachments: Vec::new(),
        })?;
        // Recorded as sent, so the same key repeated is answered and not sent
        // twice. The running turn stays the session's active one.
        self.shared.lock().pending_inputs.insert(
            input.idempotency_key,
            PendingSubmission {
                fingerprint: input_fingerprint(input),
                turn_id: active,
                command_sent: true,
                in_flight: false,
                reported: BTreeSet::new(),
            },
        );
        let event = InputSubmittedFact {
            session_id: self.shared.id,
            input_kind: input_kind(input),
        }
        .commit(self.shared.trace);
        self.shared.lifecycle.publish(event.into());
        Ok(Some(SendPendingInputResult {
            turn_id: active,
            attachments: Vec::new(),
        }))
    }

    fn begin_pending_input(
        &self,
        lease: ControllerLeaseToken,
        input: &PendingInput,
    ) -> Result<(AgentTurnId, bool), ErrorCode> {
        let mut state = self.shared.lock();
        require_lease(&state, lease)?;
        if state.summary.process != koru_session_protocol::ProcessState::Running {
            return Err(ErrorCode::SessionExited);
        }
        let fingerprint = input_fingerprint(input);
        if let Some(submission) = state.pending_inputs.get_mut(&input.idempotency_key) {
            if submission.fingerprint != fingerprint {
                return Err(ErrorCode::IdempotencyConflict);
            }
            if submission.in_flight {
                return Err(ErrorCode::Backpressure);
            }
            let turn_id = submission.turn_id;
            let command_sent = submission.command_sent;
            if !command_sent && matches!(state.summary.mode, SessionMode::Managed { .. }) {
                if state
                    .summary
                    .active_turn_id
                    .is_some_and(|active| active != turn_id)
                {
                    return Err(ErrorCode::InvalidTransition);
                }
                state.summary.active_turn_id = Some(turn_id);
            }
            submission_mut(&mut state, input.idempotency_key)?.in_flight = true;
            return Ok((turn_id, false));
        }

        if matches!(state.summary.mode, SessionMode::Managed { .. })
            && state.summary.active_turn_id.is_some()
        {
            return Err(ErrorCode::InvalidTransition);
        }

        let unique: BTreeSet<_> = input.attachment_ids.iter().copied().collect();
        if unique.len() != input.attachment_ids.len() {
            return Err(ErrorCode::InvalidRequest);
        }
        for attachment_id in &input.attachment_ids {
            let attachment = state
                .attachments
                .get(attachment_id)
                .ok_or(ErrorCode::AttachmentNotFound)?;
            if attachment.turn_id.is_some() {
                return Err(ErrorCode::InvalidTransition);
            }
        }

        let turn_id = AgentTurnId::mint();
        for attachment_id in &input.attachment_ids {
            let attachment = state
                .attachments
                .get_mut(attachment_id)
                .expect("validated attachment remains present");
            attachment.associate_turn(turn_id)?;
            attachment.delivery_mode = self.shared.attachment_delivery.mode();
        }
        state.summary.active_turn_id = Some(turn_id);
        state.pending_inputs.insert(
            input.idempotency_key,
            PendingSubmission {
                fingerprint,
                turn_id,
                command_sent: false,
                in_flight: true,
                reported: BTreeSet::new(),
            },
        );
        Ok((turn_id, true))
    }

    fn deliver_raw(
        &self,
        lease: ControllerLeaseToken,
        input: &PendingInput,
        turn_id: AgentTurnId,
        shell: Option<ShellQuoting>,
    ) -> Result<(), ErrorCode> {
        let SessionIo::Raw { writer, .. } = &self.shared.io else {
            return Err(ErrorCode::CapabilityNotNegotiated);
        };
        let (text, modes, candidates) = {
            let mut state = self.shared.lock();
            require_lease(&state, lease)?;
            let command_sent = submission(&state, input.idempotency_key)?.command_sent;
            let mut candidates = Vec::new();
            if shell.is_some() {
                for attachment_id in &input.attachment_ids {
                    let attachment = state
                        .attachments
                        .get_mut(attachment_id)
                        .ok_or(ErrorCode::AttachmentNotFound)?;
                    if attachment.delivery_mode == DeliveryMode::QuotedPath
                        && matches!(
                            attachment.delivery,
                            DeliveryState::Pending | DeliveryState::Failed { .. }
                        )
                    {
                        attachment.delivery = DeliveryState::Delivering { turn_id };
                        candidates.push(*attachment_id);
                    }
                }
            }
            (
                (!command_sent).then(|| input.text.clone()),
                state
                    .terminal
                    .as_ref()
                    .expect("raw session owns terminal state")
                    .emulator
                    .modes(),
                candidates,
            )
        };

        let mut resolved = Vec::new();
        let mut failures = Vec::new();
        for attachment_id in candidates {
            match self
                .shared
                .attachment_paths
                .resolve(self.shared.id, attachment_id)
                .and_then(|path| {
                    let value = path.to_str().ok_or(ErrorCode::DeliveryFailed)?;
                    Ok(quote_path(
                        value,
                        shell.expect("candidates require a shell"),
                    ))
                }) {
                Ok(path) => resolved.push((attachment_id, path)),
                Err(code) => failures.push((attachment_id, code)),
            }
        }
        if !failures.is_empty() {
            let mut state = self.shared.lock();
            for (attachment_id, code) in failures {
                if let Some(attachment) = state.attachments.get_mut(&attachment_id) {
                    attachment.delivery = DeliveryState::Failed {
                        turn_id,
                        error: failure(code),
                    };
                }
            }
        }

        let mut command = text.unwrap_or_default();
        for (_, path) in &resolved {
            if !command.is_empty() {
                command.push(' ');
            }
            command.push_str(path);
        }
        if command.is_empty() {
            return Ok(());
        }

        let mut bytes = encode_paste(&command, modes);
        bytes.push(b'\r');
        let write = writer.write_bytes(&bytes).map_err(map_transport);
        let mut state = self.shared.lock();
        require_lease(&state, lease)?;
        if write.is_ok() {
            submission_mut(&mut state, input.idempotency_key)?.command_sent = true;
            for (attachment_id, _) in &resolved {
                if let Some(attachment) = state.attachments.get_mut(attachment_id) {
                    attachment.delivery = DeliveryState::Delivered { turn_id };
                }
            }
            self.shared.stats.add(
                koru_session_observability::Counter::InputBytes,
                bytes.len().try_into().unwrap_or(u64::MAX),
            );
            self.shared.stats.observe_high_water(
                koru_session_observability::Counter::QueueHighWater,
                writer.queued_bytes().try_into().unwrap_or(u64::MAX),
            );
        } else {
            let code = write.clone().expect_err("failed write has an error");
            for (attachment_id, _) in &resolved {
                if let Some(attachment) = state.attachments.get_mut(attachment_id) {
                    attachment.delivery = DeliveryState::Failed {
                        turn_id,
                        error: failure(code.clone()),
                    };
                }
            }
        }
        drop(state);
        if input.attachment_ids.is_empty() {
            write
        } else {
            Ok(())
        }
    }

    fn deliver_native(
        &self,
        lease: ControllerLeaseToken,
        input: &PendingInput,
        turn_id: AgentTurnId,
        provider: ProviderKind,
    ) -> Result<(), ErrorCode> {
        let candidates = {
            let mut state = self.shared.lock();
            require_lease(&state, lease)?;
            if !matches!(
                &state.summary.mode,
                SessionMode::Managed { provider: active } if active == &provider
            ) {
                return Err(ErrorCode::CapabilityNotNegotiated);
            }
            let command_sent = submission(&state, input.idempotency_key)?.command_sent;
            if command_sent {
                return Ok(());
            }
            let mut candidates = Vec::new();
            for attachment_id in &input.attachment_ids {
                let attachment = state
                    .attachments
                    .get_mut(attachment_id)
                    .ok_or(ErrorCode::AttachmentNotFound)?;
                if attachment.delivery_mode == DeliveryMode::NativeStructured
                    && matches!(
                        attachment.delivery,
                        DeliveryState::Pending | DeliveryState::Failed { .. }
                    )
                {
                    attachment.delivery = DeliveryState::Delivering { turn_id };
                    candidates.push(AgentAttachment {
                        id: *attachment_id,
                        kind: attachment.kind,
                        media_type: attachment.media_type.clone(),
                    });
                }
            }
            candidates
        };

        let SessionIo::Managed(managed) = &self.shared.io else {
            return Err(ErrorCode::CapabilityNotNegotiated);
        };
        let response = managed.submit(AgentInput {
            turn_id,
            text: input.text.clone(),
            attachments: candidates.clone(),
        });
        let mut state = self.shared.lock();
        // The request was lease-authorized before dispatch. Losing that lease
        // after provider acceptance cannot undo the accepted command.
        match response {
            Ok(()) => {
                submission_mut(&mut state, input.idempotency_key)?.command_sent = true;
                Ok(())
            }
            Err(code) => {
                for candidate in candidates {
                    if let Some(attachment) = state.attachments.get_mut(&candidate.id) {
                        attachment.delivery = DeliveryState::Failed {
                            turn_id,
                            error: failure(code.clone()),
                        };
                    }
                }
                if !input.text.is_empty() || input.attachment_ids.is_empty() {
                    Err(code)
                } else {
                    Ok(())
                }
            }
        }
    }

    fn finish_pending_input(
        &self,
        input: &PendingInput,
        turn_id: AgentTurnId,
        operation: Result<(), ErrorCode>,
    ) -> Result<SendPendingInputResult, ErrorCode> {
        let (result, reports, delivered) = {
            let mut state = self.shared.lock();
            let mut reports = Vec::new();
            let mut delivered = Vec::new();
            let attachment_results = input
                .attachment_ids
                .iter()
                .filter_map(|attachment_id| state.attachments.get(attachment_id))
                .map(|attachment| AttachmentDeliveryResult {
                    attachment_id: attachment.id,
                    mode: attachment.delivery_mode,
                    state: attachment.delivery.clone(),
                })
                .collect::<Vec<_>>();
            let reportable = attachment_results
                .iter()
                .filter_map(|attachment| {
                    reportable(attachment).map(|outcome| (attachment, outcome))
                })
                .map(|(attachment, outcome)| (attachment.attachment_id, attachment.mode, outcome))
                .collect::<Vec<_>>();
            let submission = submission_mut(&mut state, input.idempotency_key)?;
            submission.in_flight = false;
            let command_sent = submission.command_sent;
            for (attachment_id, mode, outcome) in reportable {
                if submission.reported.insert((attachment_id, outcome)) {
                    reports.push((attachment_id, mode, outcome));
                    if outcome == ReportedOutcome::Delivered {
                        delivered.push(attachment_id);
                    }
                }
            }
            if !command_sent
                && matches!(state.summary.mode, SessionMode::Managed { .. })
                && state.summary.active_turn_id == Some(turn_id)
            {
                state.summary.active_turn_id = None;
            }
            refresh_pending_attachment_count(&mut state);
            (
                SendPendingInputResult {
                    turn_id,
                    attachments: attachment_results,
                },
                reports,
                delivered,
            )
        };

        for attachment_id in delivered {
            self.shared
                .publish_event(koru_session_protocol::SessionEvent::AttachmentDelivered {
                    attachment_id,
                    turn_id,
                });
        }
        for (attachment_id, mode, outcome) in reports {
            publish_delivery_fact(&self.shared, attachment_id, mode, outcome);
        }
        operation.map(|()| result)
    }
}

fn validate_input(input: &PendingInput) -> Result<(), ErrorCode> {
    if input.text.is_empty() && input.attachment_ids.is_empty() {
        return Err(ErrorCode::InvalidRequest);
    }
    if input.text.len() > crate::MAX_QUEUED_WRITE_BYTES {
        return Err(ErrorCode::InputTooLarge);
    }
    Ok(())
}

fn input_kind(input: &PendingInput) -> TerminalInputKind {
    match (input.text.is_empty(), input.attachment_ids.is_empty()) {
        (false, true) => TerminalInputKind::Text,
        (true, false) => TerminalInputKind::Image,
        (false, false) => TerminalInputKind::Mixed,
        (true, true) => unreachable!("empty input was rejected"),
    }
}

fn submission(
    state: &State,
    key: koru_session_protocol::IdempotencyKey,
) -> Result<&PendingSubmission, ErrorCode> {
    state
        .pending_inputs
        .get(&key)
        .ok_or(ErrorCode::InvalidTransition)
}

fn submission_mut(
    state: &mut State,
    key: koru_session_protocol::IdempotencyKey,
) -> Result<&mut PendingSubmission, ErrorCode> {
    state
        .pending_inputs
        .get_mut(&key)
        .ok_or(ErrorCode::InvalidTransition)
}

fn reportable(result: &AttachmentDeliveryResult) -> Option<ReportedOutcome> {
    match (&result.mode, &result.state) {
        (_, DeliveryState::Delivered { .. }) => Some(ReportedOutcome::Delivered),
        (_, DeliveryState::Failed { .. }) => Some(ReportedOutcome::Failed),
        (DeliveryMode::Unsupported, DeliveryState::Pending) => Some(ReportedOutcome::Unsupported),
        _ => None,
    }
}

fn metric_delivery_mode(mode: DeliveryMode) -> TerminalDeliveryMode {
    match mode {
        DeliveryMode::NativeStructured => TerminalDeliveryMode::Native,
        DeliveryMode::QuotedPath => TerminalDeliveryMode::Path,
        DeliveryMode::Unsupported => TerminalDeliveryMode::Unsupported,
    }
}

fn metric_delivery_outcome(outcome: ReportedOutcome) -> TerminalDeliveryOutcome {
    match outcome {
        ReportedOutcome::Delivered => TerminalDeliveryOutcome::Delivered,
        ReportedOutcome::Failed => TerminalDeliveryOutcome::Failed,
        ReportedOutcome::Unsupported => TerminalDeliveryOutcome::Unsupported,
    }
}

fn failure(code: ErrorCode) -> SessionFailure {
    SessionFailure {
        message: code.as_str().to_owned(),
        code,
    }
}

fn input_fingerprint(input: &PendingInput) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"koru-session-pending-input-v1\0");
    hasher.update(&(input.text.len() as u64).to_le_bytes());
    hasher.update(input.text.as_bytes());
    hasher.update(&(input.attachment_ids.len() as u64).to_le_bytes());
    for attachment_id in &input.attachment_ids {
        hasher.update(attachment_id.as_uuid().as_bytes());
    }
    hasher.finalize()
}

pub fn quote_path(path: &str, shell: ShellQuoting) -> String {
    match shell {
        ShellQuoting::Posix => format!("'{}'", path.replace('\'', "'\"'\"'")),
        ShellQuoting::PowerShell => format!("'{}'", path.replace('\'', "''")),
    }
}
