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

/// A stream nothing is ever published on: a test drives events directly.
struct QuietEvents;

impl SessionEvents for QuietEvents {
    fn next(&mut self, _timeout: Duration) -> Option<SessionUpdate> {
        None
    }
}

impl Sessions for ScriptedSessions {
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

    fn summary(&mut self, _session: SessionId) -> PortResult<Option<SessionSummary>> {
        Err(self.script().unscripted("summary"))
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
        Ok(AgentEventObservation {
            events: Vec::new(),
            cursor: StreamSequence::new(koru_session_protocol::AGENT_EVENT_INITIAL_CURSOR)
                .expect("the initial cursor is a valid sequence"),
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
