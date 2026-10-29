// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use koru_session_protocol::{
    AgentEvent, AgentTurnId, ApprovalDecision, ApprovalId, AttachmentId, AttachmentKind,
    CapabilitySet, DelegationLaunch, ErrorCode, LaunchSelection, ProviderKind, ProviderResumeToken,
    SessionId,
};

use super::DelegationFacts;

/// A bounded, log-safe provider version learned from executable output or a
/// structured initialize response.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProviderVersion(String);

impl ProviderVersion {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for ProviderVersion {
    type Err = ProviderFailure;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let valid = !value.is_empty()
            && value.len() <= 64
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+')
            });
        valid.then(|| Self(value.to_owned())).ok_or_else(|| {
            ProviderFailure::new(
                ErrorCode::ProviderIncompatible,
                ProviderStage::Version,
                ProviderDiagnostic::InvalidVersion,
            )
        })
    }
}

impl fmt::Debug for ProviderVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ProviderVersion")
            .field(&format_args!("<{} bytes>", self.0.len()))
            .finish()
    }
}

/// The only provider facts retained after a successful initialize handshake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentCapabilities {
    pub provider: ProviderKind,
    pub executable_version: ProviderVersion,
    pub protocol_version: ProviderVersion,
    pub capabilities: CapabilitySet,
    /// What the adapter declares about delegating to it: resume, MCP, child
    /// sessions, economics and model identity. Declared, not inferred —
    /// the seven-point verdict is computed from these and the capability
    /// tokens, never asserted by the adapter itself.
    pub delegation: DelegationFacts,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentStart {
    pub session_id: SessionId,
    pub parent_session_id: Option<SessionId>,
    /// The token of the attempt this start continues. Only an adapter that
    /// declares native resume ever sees one; every other adapter is started
    /// fresh and the orchestrator re-submits the objective (§7.6's fallback).
    pub resume: Option<ProviderResumeToken>,
}

impl AgentStart {
    pub fn fresh(session_id: SessionId, parent_session_id: Option<SessionId>) -> Self {
        Self {
            session_id,
            parent_session_id,
            resume: None,
        }
    }
}

/// Root-resolved process facts for one managed session. The executable and
/// working directory are deliberately redacted from `Debug` because either
/// can contain user-authored path components.
#[derive(Clone)]
pub struct ManagedProviderRequest {
    pub provider: ProviderKind,
    pub executable: OsString,
    pub working_directory: PathBuf,
    /// Present on a delegated launch. The adapter renders the endpoint and
    /// the allowlist into its own configuration form privately; nothing
    /// above the adapter knows how a provider spells "MCP server".
    pub delegation: Option<DelegationLaunch>,
    /// An explicit model and effort for a manual managed session (protocol
    /// v16). Applied through the same private route a delegated model takes,
    /// and acknowledged before the first prompt or refused.
    pub selection: Option<LaunchSelection>,
    /// MCP servers the agent is to start beside itself and offer to the
    /// model — a chat's desktop bridge (AGENTUX UC16). The adapter renders
    /// each into its own configuration form privately, as it does the
    /// delegated endpoint.
    pub mcp_servers: Vec<McpServerLaunch>,
}

/// One stdio MCP server the agent starts as its own child: the program,
/// its arguments and the environment it needs. Never logged whole — the
/// arguments may name a root.
#[derive(Clone, PartialEq, Eq)]
pub struct McpServerLaunch {
    /// The server's name as the agent's configuration spells it, a plain
    /// identifier.
    pub name: String,
    pub command: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

impl fmt::Debug for McpServerLaunch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpServerLaunch")
            .field("name", &self.name)
            .field("args", &self.args.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ManagedProviderRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedProviderRequest")
            .field("provider", &self.provider)
            .field("delegated", &self.delegation.is_some())
            .finish_non_exhaustive()
    }
}

/// Wakes an adapter blocked on provider input so its owner can service a local
/// command. A wake carries no provider or user payload and consumes no event
/// queue capacity.
pub trait ProviderWake: Send + Sync + 'static {
    fn wake(&self);
}

pub struct ManagedProvider {
    pub control: Box<dyn AgentControl>,
    pub wake: Arc<dyn ProviderWake>,
}

/// Host-injected creation edge for one provider process. Implementations own
/// no root discovery: attachment storage is captured when the host constructs
/// the factory, and every request carries an already-resolved cwd/executable.
pub trait ManagedProviderFactory: Send + Sync + 'static {
    fn create(&self, request: ManagedProviderRequest) -> Result<ManagedProvider, ProviderFailure>;

    /// Explicit selection is opt-in per factory, not implied by host capability.
    fn supports_model_selection(&self) -> bool {
        false
    }

    /// Resolve configured choices off all answering and interaction threads.
    fn discover_models(
        &self,
        _executable: OsString,
        _working_directory: PathBuf,
    ) -> Result<koru_session_protocol::ProviderModels, ProviderFailure> {
        Err(ProviderFailure::new(
            ErrorCode::CapabilityNotNegotiated,
            ProviderStage::Discovery,
            ProviderDiagnostic::HandshakeRejected,
        ))
    }

    /// Construct a fresh probe. The host calls this off its answering threads.
    fn compatibility_probe(
        &self,
        _executable: OsString,
        _working_directory: PathBuf,
    ) -> Result<Box<dyn super::CompatibilityProbe>, ProviderFailure> {
        Err(ProviderFailure::new(
            ErrorCode::ProviderIncompatible,
            ProviderStage::Discovery,
            ProviderDiagnostic::HandshakeRejected,
        ))
    }
}

/// Provider-neutral attachment metadata. The adapter resolves an id through
/// its rooted attachment owner; paths and bytes never enter this contract.
#[derive(Clone, PartialEq, Eq)]
pub struct AgentAttachment {
    pub id: AttachmentId,
    pub kind: AttachmentKind,
    pub media_type: String,
}

impl fmt::Debug for AgentAttachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentAttachment")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// One logical managed turn. `turn_id` is also the adapter's idempotency key.
#[derive(Clone, PartialEq, Eq)]
pub struct AgentInput {
    pub turn_id: AgentTurnId,
    pub text: String,
    pub attachments: Vec<AgentAttachment>,
}

impl AgentInput {
    pub fn validate(&self, maximum_text_bytes: usize) -> Result<(), ProviderFailure> {
        if (self.text.is_empty() && self.attachments.is_empty())
            || self.text.len() > maximum_text_bytes
        {
            return Err(ProviderFailure::new(
                ErrorCode::InvalidRequest,
                ProviderStage::Submit,
                ProviderDiagnostic::InvalidInput,
            ));
        }
        let ids: BTreeSet<_> = self.attachments.iter().map(|item| item.id).collect();
        if ids.len() != self.attachments.len() {
            return Err(ProviderFailure::new(
                ErrorCode::InvalidRequest,
                ProviderStage::Submit,
                ProviderDiagnostic::InvalidInput,
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for AgentInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentInput")
            .field("turn_id", &self.turn_id)
            .field("text_bytes", &self.text.len())
            .field("attachment_count", &self.attachments.len())
            .finish()
    }
}

pub use koru_session_protocol::{ProviderDiagnostic, ProviderStage};

/// A stable code for clients plus deliberately content-free developer detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderFailure {
    pub code: ErrorCode,
    pub stage: ProviderStage,
    pub diagnostic: ProviderDiagnostic,
    pub os_code: Option<i32>,
}

impl ProviderFailure {
    pub fn new(code: ErrorCode, stage: ProviderStage, diagnostic: ProviderDiagnostic) -> Self {
        Self {
            code,
            stage,
            diagnostic,
            os_code: None,
        }
    }

    pub(crate) fn io(stage: ProviderStage, error: &std::io::Error) -> Self {
        Self {
            code: ErrorCode::ProviderFailed,
            stage,
            diagnostic: ProviderDiagnostic::Io,
            os_code: error.raw_os_error(),
        }
    }
}

impl fmt::Display for ProviderFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "provider operation failed: {} at {:?} ({:?})",
            self.code, self.stage, self.diagnostic
        )
    }
}

impl std::error::Error for ProviderFailure {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProviderDiagnostics {
    pub frames_read: u64,
    pub unknown_events: u64,
    pub malformed_frames: u64,
    pub maximum_queue_depth: usize,
}

/// Smallest provider-neutral managed-session contract.
///
/// Implementations own exactly one provider process/session and privately map
/// Koru ids to provider-native ids. All methods may block and therefore run on
/// a session worker, never a window or Agent event thread.
pub trait AgentControl: Send {
    /// Starts the provider transport and completes its initialize handshake.
    fn start(&mut self, start: AgentStart) -> Result<AgentCapabilities, ProviderFailure>;

    /// Returns the completed handshake, or `None` before `start` succeeds.
    fn capabilities(&self) -> Option<&AgentCapabilities>;

    /// Submits one Koru-minted turn. Repeating the same id must not create a
    /// second provider turn or resend accepted content.
    fn submit(&mut self, input: AgentInput) -> Result<(), ProviderFailure>;

    fn cancel(&mut self, turn_id: AgentTurnId) -> Result<(), ProviderFailure>;

    /// Adds text to the turn that is running, instead of starting another.
    /// `input.turn_id` names the running turn. A provider with no such call
    /// answers `InvalidTransition`, which is what input during a turn has
    /// always been told.
    fn steer(&mut self, input: AgentInput) -> Result<(), ProviderFailure> {
        let _ = input;
        Err(ProviderFailure::new(
            ErrorCode::InvalidTransition,
            ProviderStage::Submit,
            ProviderDiagnostic::InvalidState,
        ))
    }

    fn answer_approval(
        &mut self,
        approval_id: ApprovalId,
        decision: ApprovalDecision,
        choice: Option<koru_session_protocol::ApprovalChoiceId>,
    ) -> Result<(), ProviderFailure>;

    /// Returns normalized events only. Unknown provider events increment the
    /// diagnostic counter and are not surfaced as invented Koru semantics.
    fn next_event(&mut self, timeout: Duration) -> Result<Option<AgentEvent>, ProviderFailure>;

    fn diagnostics(&self) -> ProviderDiagnostics;

    /// The resume seam. An adapter that declared native resume returns the
    /// opaque token a later `start` can be handed to continue this provider
    /// session; every other adapter returns `None`, and its absence does not
    /// disqualify the provider from delegation (§5, requirement 4).
    fn resume_token(&self) -> Option<ProviderResumeToken> {
        None
    }

    /// Performs a bounded orderly shutdown. The owning session worker remains
    /// responsible for forceful process-tree cleanup after an error/timeout.
    fn shutdown(&mut self) -> Result<(), ProviderFailure>;
}
