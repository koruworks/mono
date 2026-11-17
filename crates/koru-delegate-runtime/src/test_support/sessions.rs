// SPDX-License-Identifier: MPL-2.0

//! The scripted session host behind [`super::decision_chain`]. It answers
//! only what a test scripted: every creation, retirement, lookup and input
//! acknowledgment is queued by name, and anything unscripted is refused with
//! the code `unscripted` (recorded as [`SessionCall::Refused`]). There is no
//! default success. What it records names keys and sessions only; no input
//! text, prompt or provider payload is kept where `Debug` can reach it.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use koru_delegate_core::{
    AgentTurnId, HostInstanceId, IdempotencyKey, PendingInput, SessionId, SessionProfileKey,
};
use koru_session_protocol::{
    AgentEventObservation, CancelReason, CreationLookup, ProviderResumeToken, SessionSummary,
    StreamSequence,
};

use crate::{
    CreatedSession, Port, PortError, PortResult, SessionEvents, SessionLaunch, SessionUpdate,
    Sessions,
};

/// One call the scripted host received, in order. Identity only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionCall {
    /// `create_delegated` under this creation key.
    Create {
        key: IdempotencyKey,
    },
    Lookup(IdempotencyKey),
    Retire(IdempotencyKey),
    Attach(SessionId),
    Subscribe(SessionId),
    Observe(SessionId),
    Terminate(SessionId),
    /// One typed Bot input (`bot_submit`) under its idempotency key.
    Input {
        session: SessionId,
        key: IdempotencyKey,
    },
    /// A call the script had no answer for, by method name.
    Refused(&'static str),
}

/// What the scripted host will answer, and what it was asked.
///
/// Built by [`super::decision_chain`] and shared with the orchestrator's
/// session port through `Arc<Mutex<_>>`, so a test keeps reading it after
/// the orchestrator has moved elsewhere (into a host actor, say).
pub struct SessionScript {
    host: HostInstanceId,
    delegation: bool,
    /// The sessions the next creations answer with, oldest first.
    creations: VecDeque<SessionId>,
    /// Sessions this host created and has not terminated: the only ones it
    /// attaches, subscribes, observes or accepts input for.
    live: BTreeSet<SessionId>,
    retirements: HashMap<IdempotencyKey, CreationLookup>,
    lookups: HashMap<IdempotencyKey, CreationLookup>,
    /// The turns the next new keyed inputs are acknowledged as.
    turns: VecDeque<AgentTurnId>,
    /// Every acknowledged input, pinned to its key as a real host pins it:
    /// the same key and bytes replay the turn, changed bytes conflict.
    inputs: HashMap<IdempotencyKey, (SessionId, PendingInput, AgentTurnId)>,
    created: Vec<(IdempotencyKey, SessionId)>,
    attached: BTreeSet<SessionId>,
    /// Live sessions whose last turn completed (M6 delivery evidence): their
    /// summary reads Completed and idle, their observation ends with it.
    completed: HashMap<SessionId, AgentTurnId>,
    /// Whether this host serves the decision-window stream, as the real
    /// session adapter does. Off by default: `subscribe_decision` refuses.
    decision_stream: bool,
    /// Sessions this host terminated. With the decision stream on, each
    /// reports its process end on the Session stream, as a real host does.
    ended: BTreeSet<SessionId>,
    calls: Vec<SessionCall>,
}

impl std::fmt::Debug for SessionScript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Counts and identities only: the pinned inputs carry task text.
        f.debug_struct("SessionScript")
            .field("host", &self.host)
            .field("delegation", &self.delegation)
            .field("scripted_creations", &self.creations.len())
            .field("scripted_turns", &self.turns.len())
            .field("created", &self.created)
            .field("acknowledged_inputs", &self.inputs.len())
            .field("calls", &self.calls)
            .finish()
    }
}

impl SessionScript {
    /// A host that refuses everything: no delegation, nothing scripted.
    pub(super) fn new(host: HostInstanceId) -> Self {
        Self {
            host,
            delegation: false,
            creations: VecDeque::new(),
            live: BTreeSet::new(),
            retirements: HashMap::new(),
            lookups: HashMap::new(),
            turns: VecDeque::new(),
            inputs: HashMap::new(),
            created: Vec::new(),
            attached: BTreeSet::new(),
            completed: HashMap::new(),
            decision_stream: false,
            ended: BTreeSet::new(),
            calls: Vec::new(),
        }
    }

    /// This host's instance id (every answer names it).
    pub fn host(&self) -> HostInstanceId {
        self.host
    }

    /// Whether the host negotiated delegation and calls the Bot's profile
    /// delegatable. The builder turns it on; a test may turn it off.
    pub fn set_delegation(&mut self, delegation: bool) {
        self.delegation = delegation;
    }

    /// The next creation under a new key answers `session`, which is then
    /// live on this host. A repeated creation key answers its first session.
    pub fn script_creation(&mut self, session: SessionId) {
        self.creations.push_back(session);
    }

    /// The next keyed input not seen before is acknowledged as `turn`.
    pub fn script_turn(&mut self, turn: AgentTurnId) {
        self.turns.push_back(turn);
    }

    /// `retire(key)` answers `answer` on this host (every time).
    pub fn script_retirement(&mut self, key: IdempotencyKey, answer: CreationLookup) {
        self.retirements.insert(key, answer);
    }

    /// `lookup(key)` answers `answer` on this host (every time).
    pub fn script_lookup(&mut self, key: IdempotencyKey, answer: CreationLookup) {
        self.lookups.insert(key, answer);
    }

    /// `turn` completed on the live `session`: its summary is Completed with
    /// no active turn and its observation ends with that `TurnCompleted`,
    /// the evidence a delivery receipt needs. Identities only.
    pub fn script_completed_turn(&mut self, session: SessionId, turn: AgentTurnId) {
        self.completed.insert(session, turn);
    }

    /// Serve the decision-window stream the way the real session adapter
    /// does: a subscription to a session this host terminated reports
    /// `SessionEvent::Exited` on the Session stream, once, past the
    /// subscriber's frontier. Without this the scripted host refuses the
    /// stream, so no test ever saw a stopped session report its own end.
    pub fn report_session_ends(&mut self) {
        self.decision_stream = true;
    }

    /// The process of `session` ended by itself (it crashed or exited): no
    /// one asked this host to terminate it. With the decision stream on, its
    /// subscribers read that end.
    pub fn end_session(&mut self, session: SessionId) {
        if self.live.remove(&session) {
            self.attached.remove(&session);
            self.ended.insert(session);
        }
    }

    /// Every call received, oldest first.
    pub fn calls(&self) -> &[SessionCall] {
        &self.calls
    }

    /// How many times a creation was asked for under `key`.
    pub fn creations_under(&self, key: IdempotencyKey) -> usize {
        self.calls
            .iter()
            .filter(|call| matches!(call, SessionCall::Create { key: asked } if *asked == key))
            .count()
    }

    /// The sessions this host created, by creation key, in order.
    pub fn created(&self) -> &[(IdempotencyKey, SessionId)] {
        &self.created
    }

    /// How many typed inputs were sent under `key` (every physical send,
    /// acknowledged or not).
    pub fn inputs_with_key(&self, key: IdempotencyKey) -> usize {
        self.calls
            .iter()
            .filter(|call| matches!(call, SessionCall::Input { key: sent, .. } if *sent == key))
            .count()
    }

    /// The keys of the typed inputs sent to `session`, in order.
    pub fn inputs_to(&self, session: SessionId) -> Vec<IdempotencyKey> {
        self.calls
            .iter()
            .filter_map(|call| match call {
                SessionCall::Input { session: to, key } if *to == session => Some(*key),
                _ => None,
            })
            .collect()
    }

    /// The turn this host acknowledged for the input under `key`, if it did.
    pub fn acknowledged_turn(&self, key: IdempotencyKey) -> Option<AgentTurnId> {
        self.inputs.get(&key).map(|(_, _, turn)| *turn)
    }

    /// Whether the controller of `session` was granted by `attach`.
    pub fn attached(&self, session: SessionId) -> bool {
        self.attached.contains(&session)
    }

    fn unscripted(&mut self, method: &'static str) -> PortError {
        self.calls.push(SessionCall::Refused(method));
        PortError::new(
            Port::Sessions,
            "unscripted",
            format!("the scripted session host has no answer for {method}"),
        )
    }

    fn not_live(&self, method: &'static str) -> PortError {
        PortError::new(
            Port::Sessions,
            "session_not_found",
            format!("{method}: the scripted host holds no such session"),
        )
    }
}

/// The session port over a shared [`SessionScript`].
pub(super) struct ScriptedSessions {
    pub(super) script: Arc<Mutex<SessionScript>>,
}

impl ScriptedSessions {
    fn script(&self) -> std::sync::MutexGuard<'_, SessionScript> {
        self.script
            .lock()
            .expect("the session script is not poisoned")
    }
}

/// The Session-stream sequence at which a scripted session reports its end.
const EXIT_SEQUENCE: u64 = 3;

/// The decision-window stream of one scripted session: nothing while the
/// session is live; its process end once the host has terminated it, unless
/// the subscriber's Session frontier is already past it.
struct EndEvents {
    script: Arc<Mutex<SessionScript>>,
    session: SessionId,
    seen: bool,
}

impl SessionEvents for EndEvents {
    fn next(&mut self, timeout: Duration) -> Option<SessionUpdate> {
        let ended = self
            .script
            .lock()
            .expect("the session script is not poisoned")
            .ended
            .contains(&self.session);
        if self.seen || !ended {
            std::thread::sleep(timeout.min(Duration::from_millis(5)));
            return None;
        }
        self.seen = true;
        Some(SessionUpdate::Session(
            koru_session_protocol::SequencedSessionEvent {
                session_id: self.session,
                sequence: StreamSequence::new(EXIT_SEQUENCE).expect("a valid sequence"),
                event: koru_session_protocol::SessionEvent::Exited { code: Some(0) },
            },
        ))
    }
}

/// A stream nothing is ever published on: a test drives events directly.
struct QuietEvents;

impl SessionEvents for QuietEvents {
    fn next(&mut self, _timeout: Duration) -> Option<SessionUpdate> {
        None
    }
}

impl Sessions for ScriptedSessions {
    fn subscribe_decision(
        &mut self,
        session: SessionId,
        after: &koru_delegate_core::decisions::DecisionSourceFrontiers,
    ) -> PortResult<Box<dyn SessionEvents>> {
        let mut script = self.script();
        if !script.decision_stream {
            // The trait's own answer for an adapter without the stream.
            return Err(PortError::new(
                Port::Sessions,
                "decision_stream_unavailable",
                "the session adapter does not support decision source ownership",
            ));
        }
        script.calls.push(SessionCall::Subscribe(session));
        if !script.live.contains(&session) && !script.ended.contains(&session) {
            return Err(script.not_live("subscribe_decision"));
        }
        Ok(Box::new(EndEvents {
            script: Arc::clone(&self.script),
            session,
            seen: after.session_after.is_some_and(|seen| {
                seen >= StreamSequence::new(EXIT_SEQUENCE).expect("a valid sequence")
            }),
        }))
    }

    fn supports_delegation(&self) -> bool {
        self.script().delegation
    }

    fn profile_delegatable(&mut self, _profile: &SessionProfileKey) -> PortResult<bool> {
        Ok(self.script().delegation)
    }

    fn host_instance(&mut self) -> PortResult<HostInstanceId> {
        Ok(self.script().host)
    }

    fn create_delegated(&mut self, launch: SessionLaunch) -> PortResult<CreatedSession> {
        let mut script = self.script();
        script.calls.push(SessionCall::Create { key: launch.key });
        let existing = script
            .created
            .iter()
            .find(|(key, _)| *key == launch.key)
            .map(|(_, session)| *session);
        let session = match existing {
            Some(session) => session,
            None => {
                let Some(session) = script.creations.pop_front() else {
                    return Err(script.unscripted("create_delegated"));
                };
                script.created.push((launch.key, session));
                script.live.insert(session);
                session
            }
        };
        Ok(CreatedSession {
            session,
            provider: "codex".parse().expect("a provider kind"),
            requested_model: None,
        })
    }

    fn lookup(&mut self, key: IdempotencyKey) -> PortResult<(HostInstanceId, CreationLookup)> {
        let mut script = self.script();
        script.calls.push(SessionCall::Lookup(key));
        let answer = script.lookups.get(&key).cloned();
        match answer {
            Some(answer) => Ok((script.host, answer)),
            None => Err(script.unscripted("lookup")),
        }
    }

    fn retire(&mut self, key: IdempotencyKey) -> PortResult<(HostInstanceId, CreationLookup)> {
        let mut script = self.script();
        script.calls.push(SessionCall::Retire(key));
        let answer = script.retirements.get(&key).cloned();
        match answer {
            Some(answer) => Ok((script.host, answer)),
            None => Err(script.unscripted("retire")),
        }
    }

    fn summary(&mut self, session: SessionId) -> PortResult<Option<SessionSummary>> {
        let mut script = self.script();
        if !script.live.contains(&session) || !script.completed.contains_key(&session) {
            return Err(script.unscripted("summary"));
        }
        Ok(Some(SessionSummary {
            id: session,
            parent_session_id: None,
            profile: SessionProfileKey::BuiltIn("codex".parse().expect("a built-in profile")),
            display_name: String::new(),
            working_directory: None,
            mode: koru_session_protocol::SessionMode::Managed {
                provider: "codex".parse().expect("a provider kind"),
            },
            process: koru_session_protocol::ProcessState::Running,
            controller: koru_session_protocol::ControllerState::Detached,
            managed: Some(koru_session_protocol::ManagedState::Completed),
            active_turn_id: None,
            attention: Default::default(),
            pending_attachment_count: 0,
            capabilities: Default::default(),
            project_id: None,
            handover: Default::default(),
            model: None,
            origin: Default::default(),
            created_at_ms: 0,
        }))
    }

    fn attach(&mut self, session: SessionId) -> PortResult<()> {
        let mut script = self.script();
        script.calls.push(SessionCall::Attach(session));
        if !script.live.contains(&session) {
            return Err(script.not_live("attach"));
        }
        script.attached.insert(session);
        Ok(())
    }

    fn release_control(&mut self, _session: SessionId) -> PortResult<()> {
        Err(self.script().unscripted("release_control"))
    }

    fn submit(&mut self, _session: SessionId, _text: String) -> PortResult<AgentTurnId> {
        Err(self.script().unscripted("submit"))
    }

    fn bot_submit(&mut self, session: SessionId, input: PendingInput) -> PortResult<AgentTurnId> {
        let mut script = self.script();
        let key = input.idempotency_key;
        script.calls.push(SessionCall::Input { session, key });
        if !script.live.contains(&session) {
            return Err(script.not_live("bot_submit"));
        }
        if let Some((pinned_session, pinned, turn)) = script.inputs.get(&key) {
            return if *pinned_session == session && *pinned == input {
                Ok(*turn)
            } else {
                Err(PortError::new(
                    Port::Sessions,
                    "conflict",
                    "a different input under an acknowledged key",
                ))
            };
        }
        let Some(turn) = script.turns.pop_front() else {
            return Err(script.unscripted("bot_submit"));
        };
        script.inputs.insert(key, (session, input, turn));
        Ok(turn)
    }

    fn cancel_turn(
        &mut self,
        _session: SessionId,
        _turn: AgentTurnId,
        _reason: CancelReason,
    ) -> PortResult<()> {
        Err(self.script().unscripted("cancel_turn"))
    }

    fn terminate(&mut self, session: SessionId) -> PortResult<()> {
        let mut script = self.script();
        script.calls.push(SessionCall::Terminate(session));
        if !script.live.remove(&session) {
            return Err(script.not_live("terminate"));
        }
        script.attached.remove(&session);
        script.ended.insert(session);
        Ok(())
    }

    fn resume_token(&mut self, _session: SessionId) -> PortResult<Option<ProviderResumeToken>> {
        Err(self.script().unscripted("resume_token"))
    }

    fn observe(&mut self, session: SessionId) -> PortResult<AgentEventObservation> {
        let mut script = self.script();
        script.calls.push(SessionCall::Observe(session));
        if !script.live.contains(&session) {
            return Err(script.not_live("observe"));
        }
        let events = script
            .completed
            .get(&session)
            .map(|turn| koru_session_protocol::SequencedAgentEvent {
                session_id: session,
                sequence: StreamSequence::new(
                    koru_session_protocol::AGENT_EVENT_INITIAL_CURSOR + 1,
                )
                .expect("a valid sequence"),
                event: koru_session_protocol::AgentEvent::TurnCompleted { turn_id: *turn },
            })
            .into_iter()
            .collect::<Vec<_>>();
        let cursor = koru_session_protocol::AGENT_EVENT_INITIAL_CURSOR + events.len() as u64;
        Ok(AgentEventObservation {
            events,
            cursor: StreamSequence::new(cursor).expect("the cursor is a valid sequence"),
            earlier_truncated: false,
            commands: None,
            session_cursor: None,
        })
    }

    fn subscribe(
        &mut self,
        session: SessionId,
        _after: Option<StreamSequence>,
    ) -> PortResult<Box<dyn SessionEvents>> {
        let mut script = self.script();
        script.calls.push(SessionCall::Subscribe(session));
        if !script.live.contains(&session) {
            return Err(script.not_live("subscribe"));
        }
        Ok(Box::new(QuietEvents))
    }
}
