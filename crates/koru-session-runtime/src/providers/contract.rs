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
    /// The session's creator draws an agent's questions (protocol v21,
    /// koru-ai D108). Off, the adapter declines an agent's question as it
    /// always has, and offers it no way to ask.
    pub questions: bool,
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

    /// **What the agent can be told to do by name in a folder** (koru-ai
    /// D121): Codex's skills, in a picker's order. Off the answering
    /// threads, like the models. An agent with nothing to offer answers an
    /// empty list, which is every agent but Codex today.
    fn discover_commands(
        &self,
        _executable: OsString,
        _working_directory: PathBuf,
    ) -> Result<Vec<koru_session_protocol::AgentCommand>, ProviderFailure> {
        Ok(Vec::new())
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

    /// Where the host's model catalog gets this provider's list.
    fn catalog_source(&self) -> CatalogSource {
        CatalogSource::OnDemand
    }

    /// **The approval modes Koru gives an agent that advertises none**
    /// (koru-ai D7): Codex's, which its adapter maps onto the app-server's
    /// own approval and sandbox settings. An agent that advertises its own
    /// has them read from its session start instead, and answers `None`.
    fn fixed_modes(&self) -> Option<koru_session_protocol::AgentModes> {
        None
    }

    /// Whether this provider's levels are read by walking its models once
    /// per version (agent-models D16), because its session offers levels for
    /// the model it is on and no other.
    fn walks_model_levels(&self) -> bool {
        false
    }

    /// The walk itself: every listed model, each with the levels it offers.
    /// Runs on the catalog's worker, holding a model slot.
    fn walk_model_levels(
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
}

/// Where the host's model catalog gets a provider's list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogSource {
    /// The readiness probe's own session start receives it, so keeping it
    /// costs no process (`CompatibilityProbe::probe_with_models`).
    Probe,
    /// A listing of its own, run in the background once per agent version.
    Listing,
    /// A listing run only when somebody asks.
    OnDemand,
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
    /// The approval mode the turn runs in (koru-ai D7): an adapter moves
    /// the session to it before the prompt and refuses the turn when it
    /// cannot. `None` leaves the session's mode as it is — except Codex's,
    /// whose turns always carry Koru's default, *Ask for approval*.
    pub mode: Option<koru_session_protocol::AgentModeId>,
    /// The model's Fast mode for this turn (koru-ai D11): set before the
    /// prompt when the model offers it, ignored when it does not. `None`
    /// leaves it as it is.
    pub fast: Option<bool>,
    /// The reasoning level this turn runs at (koru-ai D70): set before the
    /// prompt when it differs from the session's, and a turn the agent
    /// cannot take it for is refused, never run at another level. `None`
    /// leaves the level as it is.
    pub reasoning: Option<koru_session_protocol::LaunchReasoning>,
    /// The model this turn runs on, chosen after the start: an adapter
    /// moves the session to it before the prompt when it is on another, and
    /// a turn the agent cannot take it for is refused, never run on a model
    /// nobody chose. `None` leaves the model as it is. The level and Fast
    /// mode the turn carries are then the new model's.
    ///
    /// The session protocol carries no model with a message yet, so the
    /// host passes `None` until it does: the adapters take it, nothing
    /// sends it.
    pub model: Option<koru_session_protocol::ModelName>,
    /// The skills the person picked, by name (koru-ai D121); the text says
    /// `$name` where each was. An adapter names each to its agent beside
    /// the text, resolving where it lives from the agent's own listing; one
    /// that has no skills ignores them, and the text goes as typed.
    pub skills: Vec<String>,
}

impl AgentInput {
    pub fn validate(&self, maximum_text_bytes: usize) -> Result<(), ProviderFailure> {
        if (self.text.is_empty() && self.attachments.is_empty())
            || self.text.len() > maximum_text_bytes
            || !koru_session_protocol::valid_input_skills(&self.skills)
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
            .field("mode", &self.mode)
            .field("fast", &self.fast)
            .field("reasoning", &self.reasoning)
            .field("model", &self.model)
            .field("skill_count", &self.skills.len())
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

    /// **Start a turn whose input is a set's answers** (protocol v21,
    /// koru-ai D108): the set is one the agent asked in its reply and moved
    /// on from (`delivery: next_message`), and the adapter writes the
    /// answers as the person's next message in the agent's own form.
    /// `input` carries the turn's id and settings and no text of its own.
    /// The answers are checked against the set; a set that is not pending
    /// is `ApprovalStale`. An adapter that never asks so has none.
    fn submit_answers(
        &mut self,
        input: AgentInput,
        answers: koru_session_protocol::QuestionsReply,
    ) -> Result<(), ProviderFailure> {
        let _ = (input, answers);
        Err(ProviderFailure::new(
            ErrorCode::ApprovalStale,
            ProviderStage::Submit,
            ProviderDiagnostic::InvalidState,
        ))
    }

    /// The same answers steered into the turn that is running.
    fn steer_answers(
        &mut self,
        input: AgentInput,
        answers: koru_session_protocol::QuestionsReply,
    ) -> Result<(), ProviderFailure> {
        let _ = (input, answers);
        Err(ProviderFailure::new(
            ErrorCode::ApprovalStale,
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

    /// **Answer a set of questions the agent asked** (protocol v21, koru-ai
    /// D108), or skip it with `None`. The answers are checked against the
    /// set ([`koru_session_protocol::validate_answers`]); a set that is not
    /// pending is `ApprovalStale`, like an approval. An adapter that never
    /// asks has nothing pending.
    fn answer_questions(
        &mut self,
        request_id: koru_session_protocol::QuestionRequestId,
        answers: Option<Vec<koru_session_protocol::QuestionAnswer>>,
    ) -> Result<(), ProviderFailure> {
        let _ = (request_id, answers);
        Err(ProviderFailure::new(
            ErrorCode::ApprovalStale,
            ProviderStage::Approval,
            ProviderDiagnostic::InvalidState,
        ))
    }

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
