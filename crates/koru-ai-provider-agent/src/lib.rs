// SPDX-License-Identifier: MPL-2.0

//! A local coding agent as a Koru AI chat provider.
//!
//! Codex, Claude Code, OpenCode, Kimi, Grok, Antigravity and Devin run as **managed
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

/// The name a person knows the agent by — the profile's brand name, which
/// is the same in every language.
///
/// **Codex reads "Codex"** in a chat. Its managed route is the app-server
/// behind the `codex` profile, which a terminal calls *Codex (Embedded)* to
/// tell it from the plain CLI row; a chat has only this one Codex, so the
/// parenthesis would explain nothing (the user, 2026-11-02: Codex is back in
/// the picker).
pub fn brand_label(kind: &AgentProviderKind) -> String {
    koru_session_profiles::resolve(&kind.profile)
        .map(|profile| match profile.label {
            koru_session_profiles::ProfileLabel::CodexEmbedded => {
                koru_session_profiles::ProfileLabel::CodexCli
                    .brand_name()
                    .unwrap_or("Codex")
                    .to_owned()
            }
            _ => koru_session_profiles::display_name(&profile, "", "").to_owned(),
        })
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
    steers: SyncSender<SteerInput>,
    pub(crate) pending: HashMap<ToolInvocationId, PendingApproval>,
}

/// **Text the person sends into the running turn** (a steer), dropped into
/// the turn's channel by the dispatcher and sent by the turn's own thread on
/// its own lease — as `SendPendingInput`, which the session runtime hands to
/// the agent's `AgentControl::steer` while a turn runs.
pub(crate) struct SteerInput {
    pub(crate) text: String,
    /// The same operation steered twice is sent once: the session runtime
    /// answers a repeated key without sending it again.
    pub(crate) key: koru_session_protocol::IdempotencyKey,
    pub(crate) reply: SteerReply,
}

/// What a steer came to, called once from the turn's thread.
pub type SteerDone = Box<dyn FnOnce(Result<(), SteerError>) + Send>;

/// The answer owed to a steer's caller. A steer dropped unanswered — its
/// turn ended with it still in the channel — answers `Closed`, so nobody
/// waits on a reply that will never come.
pub(crate) struct SteerReply(Option<SteerDone>);

impl SteerReply {
    pub(crate) fn send(mut self, outcome: Result<(), SteerError>) {
        if let Some(done) = self.0.take() {
            done(outcome);
        }
    }

    /// Never answer: the caller has been told some other way.
    fn disarm(&mut self) -> Option<SteerDone> {
        self.0.take()
    }
}

impl Drop for SteerReply {
    fn drop(&mut self) {
        if let Some(done) = self.0.take() {
            done(Err(SteerError::Closed));
        }
    }
}

/// Why a steer did not reach the running turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteerError {
    /// No agent turn is live for this attempt.
    NotLive,
    /// The turn has more steers waiting than it takes at once.
    Busy,
    /// The agent took no text just then — it has no mid-turn input, or its
    /// turn had already settled (Claude's `promptRequired`).
    Refused,
    /// The session would not carry it: the connection or the host failed.
    Failed,
    /// The turn ended before the steer was sent.
    Closed,
}

/// **Whether an agent kind takes text into a running turn** — the fixed fact
/// a client labels its Steer by. Codex does, through the app-server's
/// `turn/steer`; Claude does, through its wrapper's `_session/steering`, when
/// the wrapper advertised it at initialize — which the pinned 0.76.0 does,
/// and which the adapter checks again at the moment. Every other agent is
/// steered by stopping the reply and sending.
pub fn steers_natively(kind: &AgentProviderKind) -> bool {
    matches!(kind.kind.as_str(), "codex" | "claude")
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
        steers: SyncSender<SteerInput>,
    ) {
        self.lock().insert(
            attempt,
            LiveTurn {
                chat,
                answers,
                steers,
                pending: HashMap::new(),
            },
        );
    }

    /// **Steer the running turn**: hand `text` to the turn's thread, which
    /// sends it on its lease and calls `done` with what came of it. The
    /// dispatcher never waits; `Err` is answered here, before anything was
    /// handed over, and `done` is then not called. `operation` is the
    /// caller's operation id, a v4 UUID, which becomes the input's
    /// idempotency key.
    pub fn steer(
        &self,
        attempt: InferenceAttemptId,
        operation: &str,
        text: String,
        done: SteerDone,
    ) -> Result<(), SteerError> {
        let turns = self.lock();
        let turn = turns.get(&attempt).ok_or(SteerError::NotLive)?;
        let key = koru_session_protocol::IdempotencyKey::from_str(operation)
            .unwrap_or_else(|_| koru_session_protocol::IdempotencyKey::mint());
        let input = SteerInput {
            text,
            key,
            reply: SteerReply(Some(done)),
        };
        match turn.steers.try_send(input) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(mut input)) => {
                input.reply.disarm();
                Err(SteerError::Busy)
            }
            Err(TrySendError::Disconnected(mut input)) => {
                input.reply.disarm();
                Err(SteerError::Closed)
            }
        }
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

    /// **Codex is offered again, as "Codex"** (the user, 2026-11-02): every
    /// managed agent is a chat provider, and Codex's app-server route reads
    /// as the plain brand rather than the terminal's *Codex (Embedded)*.
    #[test]
    fn codex_is_offered_and_named_plainly() {
        let kinds = agent_provider_kinds();
        let codex = kinds
            .iter()
            .find(|kind| kind.kind.as_str() == "codex")
            .expect("Codex is a chat provider");
        assert_eq!(codex.id.as_str(), "agent-codex");
        assert_eq!(brand_label(codex), "Codex");
        let labels: Vec<String> = kinds.iter().map(brand_label).collect();
        assert!(
            labels.iter().all(|label| !label.contains("Embedded")),
            "{labels:?}"
        );
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
        let (steers, _steered) = std::sync::mpsc::sync_channel(4);
        live.register(attempt, ChatId::new(), answers, steers);
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
        let (steers, _steered) = std::sync::mpsc::sync_channel(4);
        live.register(attempt, ChatId::new(), answers, steers);
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

    /// **A steer goes into the live turn's channel**, keyed by its
    /// operation, and its caller always hears back: from the turn's thread,
    /// or `Closed` when the turn ends with it unsent. A turn that is not
    /// live, or has its steers full, refuses it at once without calling.
    #[test]
    fn a_steer_reaches_the_live_turn_and_always_answers() {
        use std::sync::mpsc::sync_channel;
        let live = LiveTurns::default();
        let attempt = InferenceAttemptId::new();
        let (answered, outcomes) = sync_channel(8);
        let done = |answered: &std::sync::mpsc::SyncSender<Result<(), SteerError>>| -> SteerDone {
            let answered = answered.clone();
            Box::new(move |outcome| {
                let _ = answered.send(outcome);
            })
        };
        let operation = "6f1c6b0e-2a7b-4c1e-9d3a-4b2f8e1a7c55";
        assert_eq!(
            live.steer(attempt, operation, "x".into(), done(&answered)),
            Err(SteerError::NotLive)
        );
        assert!(outcomes.try_recv().is_err(), "refused here, not called");

        let (answers, _answers) = sync_channel(1);
        let (steers, steered) = sync_channel(1);
        live.register(attempt, ChatId::new(), answers, steers);
        live.steer(attempt, operation, "also this".into(), done(&answered))
            .unwrap();
        assert_eq!(
            live.steer(attempt, operation, "again".into(), done(&answered)),
            Err(SteerError::Busy)
        );
        let input = steered.try_recv().unwrap();
        assert_eq!(input.text, "also this");
        assert_eq!(input.key.to_string(), operation, "the operation is the key");
        input.reply.send(Ok(()));
        assert_eq!(outcomes.try_recv().unwrap(), Ok(()));

        // A steer still waiting when its turn ends answers Closed.
        live.steer(attempt, operation, "late".into(), done(&answered))
            .unwrap();
        live.unregister(attempt);
        drop(steered);
        assert_eq!(outcomes.try_recv().unwrap(), Err(SteerError::Closed));
    }

    /// Codex and Claude take text mid-turn; every other agent is steered by
    /// stopping the reply.
    #[test]
    fn only_codex_and_claude_steer_natively() {
        for kind in agent_provider_kinds() {
            assert_eq!(
                steers_natively(&kind),
                matches!(kind.kind.as_str(), "codex" | "claude"),
                "{}",
                kind.kind
            );
        }
    }
}
