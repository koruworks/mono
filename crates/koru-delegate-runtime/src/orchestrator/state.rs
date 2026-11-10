// SPDX-License-Identifier: MPL-2.0

//! What the orchestrator holds in memory about one run: where its document
//! is, the handle it writes through, and the attempt it is driving.
//!
//! **Nothing here is durable, and nothing here is canonical.** The Run
//! document holds the status (§7.1); `runs.db` holds the attempt row. This
//! is the working memory a restart rebuilds from those two — which turn is
//! the orchestrator's, how many tool calls a turn has coalesced so far,
//! which usage identity a turn's reports share, whether a handover is
//! owed a release.

use std::{collections::HashMap, path::PathBuf};

use koru_delegate_core::{
    AgentTurnId, AmendmentRevision, ApprovalId, AttemptGeneration, DocumentId, EventId, ModelRef,
    ProviderKind, RunAttemptId, SessionId, ToolFailures, TurnOriginator, UsageFactId, WorkspaceId,
};

use crate::{CapturedContext, RunDocumentHandle};

pub struct RunState {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    pub doc: Box<dyn RunDocumentHandle>,
    /// The attempt this device is driving, if any.
    pub attempt: Option<Attempt>,
    /// The last attempt this device drove — so a late usage report (§17.2)
    /// still names the attempt it belongs to after the attempt ended.
    pub last_attempt: Option<(RunAttemptId, AttemptGeneration)>,
    /// Provider identity from the session; requested model from retained
    /// explicit intent, falling back to the session's configured default.
    pub provider: Option<ProviderKind>,
    pub requested_model: Option<String>,
    /// Retained model identities on this run, by what the provider said
    /// (§17.3): one reference per distinct (provider, requested, reported).
    pub models: HashMap<(String, Option<String>, Option<String>), ModelRef>,
    /// Usage identities by turn, kept past the attempt for late reports.
    pub usage: HashMap<(RunAttemptId, AgentTurnId), UsageIdentity>,
}

impl RunState {
    pub fn new(
        workspace: WorkspaceId,
        document: DocumentId,
        doc: Box<dyn RunDocumentHandle>,
    ) -> Self {
        Self {
            workspace,
            document,
            doc,
            attempt: None,
            last_attempt: None,
            provider: None,
            requested_model: None,
            models: HashMap::new(),
            usage: HashMap::new(),
        }
    }

    /// The attempt a fact about this run names: the live one, else the last.
    pub fn attempt_ids(&self) -> Option<(RunAttemptId, AttemptGeneration)> {
        self.attempt
            .as_ref()
            .map(|attempt| (attempt.id, attempt.generation))
            .or(self.last_attempt)
    }
}

/// One attempt being driven.
pub struct Attempt {
    pub id: RunAttemptId,
    pub generation: AttemptGeneration,
    pub session: Option<SessionId>,
    /// The turn this orchestrator submitted and has not seen end.
    pub orchestrator_turn: Option<AgentTurnId>,
    /// A live recovered turn without positive proof that this attempt submitted it.
    /// It fences automatic submission until its exact terminal event is observed.
    pub recovered_unowned_turn: Option<AgentTurnId>,
    pub turns: HashMap<AgentTurnId, TurnState>,
    /// Current permission requests, derived from the normalized event stream.
    pub pending_approvals: HashMap<ApprovalId, AgentTurnId>,
    /// A truncated recovery tail cannot prove which approvals remain open.
    pub approval_evidence_complete: bool,
    /// A handover waits for the turn's end or a proved pending approval
    /// boundary (ADR-389); either transfers only the existing lease.
    pub release_owed: bool,
    /// Whether this orchestrator holds the controller.
    pub holds_lease: bool,
    /// The AI observer has read this exact prepared execution before BeginTurn.
    pub bot_observed: bool,
    pub is_bot: bool,
    /// Provider completion is not durable chat delivery. P1.5 acknowledges it.
    pub bot_terminal_turn: Option<AgentTurnId>,
    /// Retry only already-interrupted/terminal Bot cleanup, never healthy work.
    pub bot_cleanup: Option<BotCleanupRetry>,
    pub pending_questions: HashMap<koru_delegate_core::QuestionRequestId, AgentTurnId>,
    /// The admitted working directory.
    pub run_workspace: PathBuf,
    pub captured: CapturedContext,
}

impl Attempt {
    pub fn new(id: RunAttemptId, generation: AttemptGeneration) -> Self {
        Self {
            id,
            generation,
            session: None,
            orchestrator_turn: None,
            recovered_unowned_turn: None,
            turns: HashMap::new(),
            pending_approvals: HashMap::new(),
            approval_evidence_complete: true,
            release_owed: false,
            holds_lease: false,
            bot_observed: false,
            is_bot: false,
            bot_terminal_turn: None,
            bot_cleanup: None,
            pending_questions: HashMap::new(),
            run_workspace: PathBuf::new(),
            captured: CapturedContext::default(),
        }
    }

    pub fn can_release_for_approval(&self) -> bool {
        self.approval_evidence_complete
            && self.orchestrator_turn.is_some_and(|turn| {
                self.pending_approvals
                    .values()
                    .any(|pending| *pending == turn)
            })
    }

    /// Who submitted `turn`: the orchestrator's own, or a person's.
    pub fn originator_of(&self, turn: AgentTurnId) -> TurnOriginator {
        if let Some(state) = self.turns.get(&turn) {
            return state.originator;
        }
        if self.orchestrator_turn == Some(turn) {
            TurnOriginator::Orchestrator
        } else {
            TurnOriginator::Human
        }
    }
}

pub struct BotCleanupRetry {
    pub due: std::time::Instant,
    pub round: u8,
}

/// One turn's running tally: who asked, and the tool completions not yet
/// written as a step (§7.5: bounded, coalesced).
#[derive(Clone, Copy, Debug)]
pub struct TurnState {
    pub originator: TurnOriginator,
    pub tool_calls: u32,
    pub tool_failures: u32,
    /// **Which tools failed and how**, bounded by type — the breakdown the
    /// coalesced step carries, so *two of them failed* is something a
    /// person can act on rather than only worry about.
    pub failure_breakdown: ToolFailures,
}

impl TurnState {
    pub fn new(originator: TurnOriginator) -> Self {
        Self {
            originator,
            tool_calls: 0,
            tool_failures: 0,
            failure_breakdown: ToolFailures::default(),
        }
    }
}

/// The stable identity of one turn's usage accounting (§17.2): a retry
/// repeats id, revision and event; a correction raises the revision and
/// mints a new event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageIdentity {
    pub id: UsageFactId,
    pub revision: AmendmentRevision,
    pub event: EventId,
    pub last: (Option<u64>, Option<u64>, Option<u64>, ModelRef),
}
