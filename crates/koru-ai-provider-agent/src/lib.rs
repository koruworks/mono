// SPDX-License-Identifier: MPL-2.0

//! A local coding agent as a Koru AI chat provider.
//!
//! Codex, Claude Code, OpenCode, Kimi, Grok and Antigravity run as **managed
//! sessions** the desktop's session host owns (`koru-agent.exe --worker
//! sessions`), speaking their own protocols behind one neutral
//! `AgentEvent`. This crate is the AI worker's side of that wire: an
//! [`AiProvider`] per agent kind that finds or starts **the chat's one
//! session**, sends the turn, and translates the session's events into the
//! chat's own [`AiEvent`]s. Nothing here speaks ACP or app-server; nothing
//! here draws; the chat's transcript is the record.
//!
//! Three threads, one connection each:
//!
//! - the **session-control thread** ([`control`]) holds the AI worker's one
//!   `ai_control` connection. Every chat-session create, lookup and terminate
//!   travels on it, and the host processes a connection's requests strictly
//!   in order — so a drain's lookup can never overtake an earlier create, and
//!   the generation the host handed this connection is what admits chat
//!   creates at all (protocol v16, `SessionOrigin`);
//! - a **turn** ([`turn`]) runs on the AI worker's provider pool, on a
//!   connection of its own that holds the session's lease for the length of
//!   the turn and reads its events in bounded waits;
//! - the AI worker's dispatcher never waits on session IPC: it answers an
//!   approval by dropping a message into the turn's channel ([`LiveTurns`]).
//!
//! Decided with the maintainers on 2026-09-24 (AGENTUX): the agent's wording for a
//! tool or a permission is live only and never stored; one conversation per
//! chat; no lifetime limit on restarts; a replacement session is handed the
//! conversation so far.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use koru_ai_core::{
    AiError, AiErrorClass, AiProvider, AiProviderId, AiRequest, Cancellation, ChatId,
    InferenceAttemptId, ProviderStream, ToolInvocationId,
};
use koru_session_profiles::MANAGED_PROVIDER_REGISTRATIONS;
use koru_session_protocol::{
    ApprovalChoiceId, ApprovalDecision, ApprovalId, BuiltInProfileKey, ChatOriginId, ProviderKind,
    ProviderReadiness, SessionOrigin, SessionProfileKey,
};

mod control;
mod joined;
mod turn;

pub use control::{AgentCatalog, AgentSessions, EndOutcome, EnsuredSession};

/// The product name this crate's connections announce to the host.
pub const PRODUCT: &str = "koru-ai-agent";

pub use koru_ai_core::AGENT_DEFAULT_MODEL;

/// One managed agent kind as a chat provider: its provider id
/// (`agent-<kind>`), its session-host provider kind, and the built-in
/// profile the host launches it from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProviderKind {
    pub id: AiProviderId,
    pub kind: ProviderKind,
    pub profile: SessionProfileKey,
}

/// Every managed agent the host registers, as a chat provider. The list is
/// the host's registration table, not a second one: a new managed route
/// appears in every picker with no change here.
pub fn agent_provider_kinds() -> Vec<AgentProviderKind> {
    MANAGED_PROVIDER_REGISTRATIONS
        .iter()
        .filter_map(|registration| {
            let kind = ProviderKind::from_str(registration.kind).ok()?;
            let id = AiProviderId::parse(format!("agent-{}", registration.kind)).ok()?;
            let profile =
                SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str(registration.kind).ok()?);
            Some(AgentProviderKind { id, kind, profile })
        })
        .collect()
}

/// Whether Koru AI's agent picker offers this agent.
///
/// **Every agent but Codex (Embedded)** (the user's call): it read *Needs a
/// supported version* on most machines, and a terminal's Agents menu already
/// offers the plain Codex CLI instead. Not offered is not removed — the
/// provider stays registered, so a chat already on it still runs, and Codex
/// still serves delegation.
pub fn offered_in_picker(kind: &AgentProviderKind) -> bool {
    kind.kind.as_str() != koru_session_profiles::CODEX_REGISTRATION.kind
}

/// The name a person knows the agent by — the profile's brand name, which
/// is the same in every language.
pub fn brand_label(kind: &AgentProviderKind) -> String {
    koru_session_profiles::resolve(&kind.profile)
        .map(|profile| koru_session_profiles::display_name(&profile, "", "").to_owned())
        .unwrap_or_else(|_| kind.kind.as_str().to_owned())
}

/// The agent kind behind a provider id, if it names one.
pub fn agent_kind_of(provider: &AiProviderId) -> Option<AgentProviderKind> {
    agent_provider_kinds()
        .into_iter()
        .find(|kind| &kind.id == provider)
}

/// Whether a provider id names a local agent.
pub fn is_agent_provider(provider: &AiProviderId) -> bool {
    provider.as_str().starts_with("agent-") && agent_kind_of(provider).is_some()
}

/// The session origin a chat's sessions carry. A chat id is a v4 UUID, so
/// the conversion cannot fail for an id Koru minted.
pub fn chat_origin(chat: ChatId) -> Result<SessionOrigin, AiError> {
    ChatOriginId::from_str(&chat.as_uuid().hyphenated().to_string())
        .map(|chat_id| SessionOrigin::Chat { chat_id })
        .map_err(|_| {
            AiError::new(
                AiErrorClass::Configuration,
                "This chat cannot own an agent session.",
                false,
            )
        })
}

/// An answer to a pending permission request, dropped into the turn's
/// channel by the dispatcher and sent to the host by the turn's own thread.
#[derive(Clone, Debug)]
pub(crate) struct ApprovalAnswer {
    pub(crate) approval: ApprovalId,
    pub(crate) decision: ApprovalDecision,
    pub(crate) choice: Option<ApprovalChoiceId>,
}

pub(crate) struct PendingApproval {
    pub(crate) approval: ApprovalId,
    pub(crate) choices: Vec<(ApprovalChoiceId, ApprovalDecision)>,
}

/// One running agent turn, as the dispatcher sees it: which chat, and the
/// permission requests still waiting for an answer.
pub(crate) struct LiveTurn {
    pub(crate) chat: ChatId,
    answers: SyncSender<ApprovalAnswer>,
    pub(crate) pending: HashMap<ToolInvocationId, PendingApproval>,
}

/// Why an approval could not be answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerError {
    /// No agent turn is live for this attempt: the worker restarted, or the
    /// turn already ended.
    NotLive,
    /// The row is not a pending permission request of this turn.
    NoSuchApproval,
    /// The agent offered choices and none was named. Two same-intent
    /// choices must never collapse, so a bare answer is refused.
    ChoiceRequired,
    /// The named choice is not one the agent offered.
    UnknownChoice,
    /// The turn's thread has gone; the answer cannot be delivered.
    Closed,
}

/// The live turn table: the dispatcher's non-blocking door into every
/// running agent turn. Entries live exactly as long as their turn.
#[derive(Default)]
pub struct LiveTurns {
    turns: Mutex<HashMap<InferenceAttemptId, LiveTurn>>,
}

impl LiveTurns {
    pub(crate) fn register(
        &self,
        attempt: InferenceAttemptId,
        chat: ChatId,
        answers: SyncSender<ApprovalAnswer>,
    ) {
        self.lock().insert(
            attempt,
            LiveTurn {
                chat,
                answers,
                pending: HashMap::new(),
            },
        );
    }

    pub(crate) fn unregister(&self, attempt: InferenceAttemptId) {
        self.lock().remove(&attempt);
    }

    pub(crate) fn offer(
        &self,
        attempt: InferenceAttemptId,
        invocation: ToolInvocationId,
        pending: PendingApproval,
    ) {
        if let Some(turn) = self.lock().get_mut(&attempt) {
            turn.pending.insert(invocation, pending);
        }
    }

    pub(crate) fn settle(&self, attempt: InferenceAttemptId, approval: ApprovalId) {
        if let Some(turn) = self.lock().get_mut(&attempt) {
            turn.pending
                .retain(|_, pending| pending.approval != approval);
        }
    }

    /// Whether an agent turn is live for the attempt.
    pub fn is_live(&self, attempt: InferenceAttemptId) -> bool {
        self.lock().contains_key(&attempt)
    }

    /// The chats with a live agent turn right now.
    pub fn live_chats(&self) -> Vec<ChatId> {
        self.lock().values().map(|turn| turn.chat).collect()
    }

    /// Answer a pending permission request. `approve` is what a bare
    /// approve or deny means when the agent offered no choices; when it
    /// offered some, the chosen id decides and `approve` must agree with it.
    pub fn answer(
        &self,
        attempt: InferenceAttemptId,
        invocation: ToolInvocationId,
        choice: Option<&str>,
        approve: bool,
    ) -> Result<(), AnswerError> {
        let mut turns = self.lock();
        let turn = turns.get_mut(&attempt).ok_or(AnswerError::NotLive)?;
        let pending = turn
            .pending
            .get(&invocation)
            .ok_or(AnswerError::NoSuchApproval)?;
        let (decision, chosen) = if pending.choices.is_empty() {
            (
                if approve {
                    ApprovalDecision::Approve
                } else {
                    ApprovalDecision::Reject
                },
                None,
            )
        } else {
            let choice = choice.ok_or(AnswerError::ChoiceRequired)?;
            let (id, decision) = pending
                .choices
                .iter()
                .find(|(id, _)| id.to_string() == choice)
                .copied()
                .ok_or(AnswerError::UnknownChoice)?;
            if (decision == ApprovalDecision::Approve) != approve {
                return Err(AnswerError::UnknownChoice);
            }
            (decision, Some(id))
        };
        let answer = ApprovalAnswer {
            approval: pending.approval,
            decision,
            choice: chosen,
        };
        match turn.answers.try_send(answer) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                Err(AnswerError::Closed)
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<InferenceAttemptId, LiveTurn>> {
        self.turns.lock().unwrap_or_else(|held| held.into_inner())
    }
}

/// The chat provider for one agent kind. Cheap to clone: it holds the
/// control thread's door and the shared live table.
#[derive(Clone)]
pub struct AgentProvider {
    kind: AgentProviderKind,
    sessions: AgentSessions,
    endpoint: koru_ipc::Endpoint,
    live: Arc<LiveTurns>,
}

impl AiProvider for AgentProvider {
    fn start(
        &self,
        request: AiRequest,
        cancellation: Cancellation,
    ) -> Result<ProviderStream, AiError> {
        turn::start(self, request, cancellation)
    }
}

/// How long a turn waits for the control thread to find or start the chat's
/// session: an agent's startup and handshake take seconds, not minutes.
pub(crate) const ENSURE_DEADLINE: Duration = Duration::from_secs(90);
/// How often a waiting turn checks for cancellation and pending answers.
pub(crate) const READ_INTERVAL: Duration = Duration::from_millis(250);
/// How long a cancelled turn waits for the host's terminal event before
/// declaring itself cancelled anyway.
pub(crate) const CANCEL_DEADLINE: Duration = Duration::from_secs(15);
/// How much of the conversation so far a replacement session is handed,
/// newest kept.
pub(crate) const PREFIX_BYTES: usize = 64 * 1024;
/// The grid a chat session is created with; nobody draws it.
pub(crate) const COLUMNS: u16 = 120;
pub(crate) const ROWS: u16 = 40;

/// The chat readiness of one agent, as the host reports it.
pub type AgentReadiness = Option<ProviderReadiness>;

#[cfg(test)]
mod tests {
    #[test]
    fn the_offer_carries_every_capability_a_turn_needs() {
        use koru_session_protocol::capability;
        let offer = crate::control::offer();
        for name in [
            capability::CONTROLLER_LEASES,
            capability::RENDER_DELTAS,
            capability::ATTACHMENTS,
            capability::MANAGED_SESSIONS,
            capability::MANAGED_EVENTS,
            capability::MANAGED_CANCEL,
            capability::MANAGED_APPROVAL,
            capability::OBSERVATION_CURSORS,
            capability::PROVIDER_READINESS,
            capability::PROVIDER_MODELS,
            capability::SESSION_ORIGIN,
            capability::LAUNCH_SELECTION,
        ] {
            assert!(offer.capabilities.supports(name), "{name} is not offered");
        }
    }

    use super::*;

    #[test]
    fn every_managed_registration_is_a_chat_provider_and_nothing_else_is() {
        let kinds = agent_provider_kinds();
        assert_eq!(kinds.len(), MANAGED_PROVIDER_REGISTRATIONS.len());
        for kind in &kinds {
            assert!(kind.id.as_str().starts_with("agent-"));
            assert!(is_agent_provider(&kind.id));
            assert_eq!(agent_kind_of(&kind.id).as_ref(), Some(kind));
        }
        assert!(!is_agent_provider(&AiProviderId::parse("openai").unwrap()));
        assert!(!is_agent_provider(
            &AiProviderId::parse("agent-nobody").unwrap()
        ));
    }

    /// **The picker leaves out Codex (Embedded)** and offers every other
    /// agent; the provider itself stays in the list, so chats already on it
    /// still run.
    #[test]
    fn the_picker_offers_every_agent_but_codex_embedded() {
        let kinds = agent_provider_kinds();
        let offered: Vec<&str> = kinds
            .iter()
            .filter(|kind| offered_in_picker(kind))
            .map(|kind| kind.kind.as_str())
            .collect();
        assert!(!offered.contains(&"codex"), "{offered:?}");
        assert_eq!(offered.len(), kinds.len() - 1, "{offered:?}");
        assert!(kinds.iter().any(|kind| kind.kind.as_str() == "codex"));
    }

    #[test]
    fn a_chat_id_becomes_a_chat_origin() {
        let chat = ChatId::new();
        let origin = chat_origin(chat).unwrap();
        assert_eq!(
            origin.chat().unwrap().to_string(),
            chat.as_uuid().hyphenated().to_string()
        );
    }

    #[test]
    fn answers_follow_the_offered_choices_or_refuse() {
        let live = LiveTurns::default();
        let attempt = InferenceAttemptId::new();
        let invocation = ToolInvocationId::new();
        let (answers, received) = std::sync::mpsc::sync_channel(4);
        assert_eq!(
            live.answer(attempt, invocation, None, true),
            Err(AnswerError::NotLive)
        );
        live.register(attempt, ChatId::new(), answers);
        assert_eq!(
            live.answer(attempt, invocation, None, true),
            Err(AnswerError::NoSuchApproval)
        );
        let allow = ApprovalChoiceId::mint();
        let allow_twice = ApprovalChoiceId::mint();
        let reject = ApprovalChoiceId::mint();
        let approval = ApprovalId::mint();
        live.offer(
            attempt,
            invocation,
            PendingApproval {
                approval,
                choices: vec![
                    (allow, ApprovalDecision::Approve),
                    (allow_twice, ApprovalDecision::Approve),
                    (reject, ApprovalDecision::Reject),
                ],
            },
        );
        // A bare answer cannot pick between two allow choices.
        assert_eq!(
            live.answer(attempt, invocation, None, true),
            Err(AnswerError::ChoiceRequired)
        );
        assert_eq!(
            live.answer(attempt, invocation, Some("nope"), true),
            Err(AnswerError::UnknownChoice)
        );
        // A choice whose meaning contradicts the verb is refused too.
        assert_eq!(
            live.answer(attempt, invocation, Some(&reject.to_string()), true),
            Err(AnswerError::UnknownChoice)
        );
        live.answer(attempt, invocation, Some(&allow_twice.to_string()), true)
            .unwrap();
        let sent = received.try_recv().unwrap();
        assert_eq!(sent.approval, approval);
        assert_eq!(sent.decision, ApprovalDecision::Approve);
        assert_eq!(sent.choice, Some(allow_twice));
        live.settle(attempt, approval);
        assert_eq!(
            live.answer(attempt, invocation, Some(&allow.to_string()), true),
            Err(AnswerError::NoSuchApproval)
        );
        live.unregister(attempt);
        assert!(!live.is_live(attempt));
    }

    #[test]
    fn a_legacy_two_button_approval_takes_the_verb() {
        let live = LiveTurns::default();
        let attempt = InferenceAttemptId::new();
        let invocation = ToolInvocationId::new();
        let (answers, received) = std::sync::mpsc::sync_channel(4);
        live.register(attempt, ChatId::new(), answers);
        let approval = ApprovalId::mint();
        live.offer(
            attempt,
            invocation,
            PendingApproval {
                approval,
                choices: Vec::new(),
            },
        );
        live.answer(attempt, invocation, None, false).unwrap();
        let sent = received.try_recv().unwrap();
        assert_eq!(sent.decision, ApprovalDecision::Reject);
        assert_eq!(sent.choice, None);
    }
}
