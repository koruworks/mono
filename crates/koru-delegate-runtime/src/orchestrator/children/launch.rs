// SPDX-License-Identifier: MPL-2.0

//! The physical launch of a parent's successor after a child wait, and its
//! one continuation input (contract 4.5, steps 2 to 4).
//!
//! The ledger has already opened the successor attempt and minted its
//! creation key and input key. This owner asks the host for that one
//! creation (at most once per process, and only for a reservation made in
//! this process), recovers a lost or older answer by the exact creation key,
//! takes over the parent's folder claim, and sends the one continuation
//! input under its retained key. It never writes the Run document: the
//! wait's publication owns the sole handle for the whole window.
//!
//! The same steps serve an interactive and a Routine parent. The launch
//! re-reads the admission and keeps its provenance as it is, Routine
//! included; the Routine's `Fired` stays owned by the original initial send.

use super::continuation::acknowledgment_digest;
use super::*;
use crate::orchestrator::state::Attempt;
use crate::{McpLaunch, SessionLaunch};
use koru_delegate_core::children::ChildResume;
use koru_delegate_core::decisions::{
    BotWaitPhase, DecisionInputAcknowledgment, DecisionResumeState, DecisionTransitionId,
};
use koru_delegate_core::{AgentTurnId, AttemptGeneration, RunAttemptId, SessionId, WorkspaceScope};
use koru_delegate_scope::{Admission, ScopeIdentity, StartOrigin};
use koru_delegate_sqlite::AttemptPhase;
use koru_session_protocol::CreationLookup;

/// The wait still wants its successor started and fed: reserved for it, not
/// being cleaned up, and its resume row not closed.
pub(super) fn resume_open(execution: &ChildWaitExecution) -> bool {
    execution.wait.phase == BotWaitPhase::ResumeReserved
        && execution.wait.cleanup.is_none()
        && execution
            .resume
            .as_ref()
            .is_some_and(|resume| !matches!(resume.state, DecisionResumeState::Closed { .. }))
}

fn consumer(
    resume: &ChildResume,
) -> Result<
    (
        RunAttemptId,
        AttemptGeneration,
        Option<SessionId>,
        IdempotencyKey,
    ),
    RunError,
> {
    resume
        .consumer_identity()
        .ok_or_else(|| refused("bot_fenced"))
}

impl Orchestrator {
    /// Move the Run's in-memory folder claim from the retired origin to the
    /// successor. Another holder refuses before anything is released.
    fn child_transfer_claim(
        &mut self,
        run: RunId,
        origin: RunAttemptId,
        successor: RunAttemptId,
        bot: BotId,
        scope: &WorkspaceScope,
    ) -> Result<(), RunError> {
        if self.claims.claims().any(|claim| claim.attempt == successor) {
            return Ok(());
        }
        let identity = match self.claims.claims().find(|claim| claim.attempt == origin) {
            Some(claim) => claim.scope.clone(),
            None => ScopeIdentity::resolve(scope, run, self.ports.roots.as_ref())
                .map_err(|_| refused("bot_folder_unavailable"))?,
        };
        if self
            .claims
            .holders_of(&identity)
            .iter()
            .any(|holder| holder.attempt != origin && holder.attempt != successor)
        {
            return Err(refused("bot_folder_unavailable"));
        }
        self.claims.release(origin);
        match self
            .claims
            .admit_resolved(run, successor, identity, StartOrigin::Bot { bot })
        {
            Admission::Admitted { .. } => Ok(()),
            _ => Err(refused("bot_folder_unavailable")),
        }
    }

    /// Bind the created successor session through the ledger, from the
    /// host's positive answer for the reserved creation key.
    fn child_bind_successor(
        &mut self,
        owner: BotOwner,
        run: RunId,
        creation: IdempotencyKey,
        host: koru_delegate_core::HostInstanceId,
        session: SessionId,
    ) -> Result<ChildWaitExecution, RunError> {
        let execution = self.child_ticket(owner, run)?;
        Ok(self
            .bot_ledger()?
            .bind_child_successor(&execution.expected, creation, host, session)?)
    }

    /// The successor's tool authority. While the Run document is attached it
    /// is exactly `resolve_run_tool_authority`'s. After a restart the
    /// document stays unopened until a projection is owed, and the Resume
    /// projection is owed only after this launch: the same value is then
    /// read from the ledger's admission, whose provenance the header was
    /// created from. Nothing is opened on this thread.
    fn child_tool_authority(
        &mut self,
        run: RunId,
        generation: AttemptGeneration,
        device: koru_delegate_core::DeviceId,
    ) -> Result<koru_delegate_core::RunToolAuthority, RunError> {
        let attached = self
            .runs
            .get(&run)
            .is_some_and(|state| state.doc.document().is_ok());
        if attached {
            return self.resolve_run_tool_authority(run, generation, device);
        }
        let row = self
            .ports
            .store
            .current_attempt(run)?
            .ok_or(RunError::UnknownRun)?;
        if row.retired || row.phase.is_terminal() || row.generation != generation {
            return Err(refused("bot_fenced"));
        }
        let admission = self
            .bot_ledger()?
            .bot_admission(run)?
            .ok_or(RunError::UnknownRun)?;
        if admission.submission.request.run != run
            || admission.submission.request.executing_device != device
            || !matches!(
                admission.state,
                koru_delegate_core::BotAdmissionState::Executing
            )
        {
            return Err(refused("bot_fenced"));
        }
        self.linked_routine_admission(&admission)?;
        Ok(koru_delegate_core::RunToolAuthority::Bot {
            provenance: admission.submission.provenance,
            policy: admission.submission.policy,
        })
    }

    /// Recover a reserved successor creation by its exact key. A known
    /// session is bound through the ledger; still creating, ended before
    /// binding or no evidence leaves the reservation as it is.
    pub fn bot_recover_child_successor(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<ChildWaitExecution>, RunError> {
        let execution = self.child_ticket(owner, run)?;
        let resume = execution
            .resume
            .clone()
            .ok_or_else(|| refused("bot_fenced"))?;
        if !matches!(resume.state, DecisionResumeState::Prepared {}) {
            return Ok(None);
        }
        let (_, _, _, creation) = consumer(&resume)?;
        let (host, lookup) = self.ports.sessions.lookup(creation)?;
        let session = match lookup {
            CreationLookup::Known { session } => session.id,
            CreationLookup::KnownEnded { .. }
            | CreationLookup::Creating
            | CreationLookup::NoEvidence => return Ok(None),
        };
        self.child_bind_successor(owner, run, creation, host, session)
            .map(Some)
    }

    /// Ask the host for the reserved successor under its exact creation key
    /// through the managed launch owner, at most once in this process and
    /// only for a reservation made here. Binds through the ledger only on
    /// the host's positive answer; an unknown answer records nothing.
    pub fn bot_launch_child_successor(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<ChildWaitExecution>, RunError> {
        let execution = self.child_ticket(owner, run)?;
        let resume = execution
            .resume
            .clone()
            .ok_or_else(|| refused("bot_fenced"))?;
        let (attempt, generation, session, creation) = consumer(&resume)?;
        if !matches!(resume.state, DecisionResumeState::Prepared {}) || session.is_some() {
            return Ok(None);
        }
        let row = execution.current_attempt.clone();
        let host = row.host_instance.ok_or_else(|| refused("bot_fenced"))?;
        if row.attempt != attempt
            || row.generation != generation
            || row.retired
            || row.phase.is_terminal()
            || row.provider_session.map(|key| key.idempotency) != Some(creation)
        {
            return Err(refused("bot_fenced"));
        }
        if self.children.launches.get(&run) != Some(&ChildLaunch::Reserved(creation)) {
            // Dispatched already, or reserved by an earlier process: the
            // exact key is looked up, never created a second time.
            return self.bot_recover_child_successor(owner, run);
        }
        if !resume_open(&execution) {
            // A Stop superseded the resume before any creation was asked for.
            return Ok(None);
        }
        if !self.ports.sessions.supports_delegation() {
            return Err(RunError::NotDelegatable);
        }
        if self.ports.sessions.host_instance()? != host {
            // The host the creation key was reserved on is gone.
            return Err(refused("bot_fenced"));
        }
        let admission = self
            .bot_ledger()?
            .bot_admission(run)?
            .ok_or(RunError::UnknownRun)?;
        // The Routine link is required for a Routine parent, and its
        // provenance is kept exactly as admitted.
        self.linked_routine_admission(&admission)?;
        let submission = admission.submission;
        let provenance = submission.provenance;
        if provenance.owner != owner {
            return Err(refused("bot_fenced"));
        }
        let request = submission.request;
        if !self.ports.sessions.profile_delegatable(&request.agent)? {
            return Err(refused("bot_profile_unready"));
        }
        self.child_policy_proof(run)?;
        let tool_authority =
            self.child_tool_authority(run, generation, request.executing_device)?;
        let directory = self.child_run_workspace(run)?;
        let model = match request.requested_model.as_ref() {
            Some(model) => Some(model.as_str().parse().map_err(|_| refused("bot_invalid"))?),
            None => None,
        };
        self.child_transfer_claim(
            run,
            execution.wait.origin.attempt,
            attempt,
            provenance.bot,
            &request.scope,
        )?;
        let workspace = self.open(run)?.workspace;
        let mut delegation = self.ports.mcp.launch(McpLaunch {
            authority: tool_authority,
            run,
            generation,
            workspace,
            grants: &request.grants,
            run_workspace: &directory,
        })?;
        delegation.model = model;
        if row.phase == AttemptPhase::Created {
            self.ports
                .store
                .advance(run, generation, AttemptPhase::Starting)?;
        }
        // From here the creation may exist: this process never asks again.
        self.children
            .launches
            .insert(run, ChildLaunch::Dispatched(creation));
        let created = match self.ports.sessions.create_delegated(SessionLaunch {
            is_bot: true,
            key: creation,
            profile: request.agent.clone(),
            project: request.scope.project(),
            delegation,
            working_directory: directory,
        }) {
            Ok(created) => created,
            Err(error) => {
                self.trace(
                    run,
                    "child_successor_creation_unknown",
                    format_args!("{}", error.code),
                );
                return Ok(None);
            }
        };
        let bound = self.child_bind_successor(owner, run, creation, host, created.session)?;
        self.open(run)?.provider = Some(created.provider);
        Ok(Some(bound))
    }

    /// Mirror the retained successor into working memory: the new attempt at
    /// its generation on its bound session, holding the controller only
    /// after the host granted it, with the acknowledged turn only once the
    /// ledger recorded the acknowledgment.
    pub fn bot_install_child_successor(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<(), RunError> {
        let execution = self.child_ticket(owner, run)?;
        let resume = execution
            .resume
            .clone()
            .ok_or_else(|| refused("bot_fenced"))?;
        let (attempt, generation, session, _) = consumer(&resume)?;
        let session = session.ok_or_else(|| refused("bot_successor_unbound"))?;
        let row = &execution.current_attempt;
        if row.attempt != attempt
            || row.generation != generation
            || row.retired
            || row.phase.is_terminal()
            || row.provider_session.and_then(|key| key.session) != Some(session)
        {
            return Err(refused("bot_fenced"));
        }
        let turn = resume.accepted_turn();
        let (same, holds) = self
            .runs
            .get(&run)
            .and_then(|state| state.attempt.as_ref())
            .filter(|memory| {
                memory.id == attempt
                    && memory.generation == generation
                    && memory.session == Some(session)
            })
            .map_or((false, false), |memory| (true, memory.holds_lease));
        if !same {
            // A consumer recovered by lookup (or after a restart) takes the
            // Run's folder claim here; a launch made here already moved it.
            let admission = self
                .bot_ledger()?
                .bot_admission(run)?
                .ok_or(RunError::UnknownRun)?;
            let submission = admission.submission;
            self.child_transfer_claim(
                run,
                execution.wait.origin.attempt,
                attempt,
                submission.provenance.bot,
                &submission.request.scope,
            )?;
        }
        if !holds {
            // The controller is positive evidence only from the host's grant.
            self.ports.sessions.attach(session)?;
        }
        self.children.launches.remove(&run);
        let state = self.open(run)?;
        if same {
            let memory = state.attempt.as_mut().expect("matched above");
            memory.holds_lease = true;
            if let Some(turn) = turn
                && memory.resume_turn.is_none()
            {
                memory.resume_turn = Some(turn);
                if memory.orchestrator_turn.is_none() && memory.bot_terminal_turn != Some(turn) {
                    memory.orchestrator_turn = Some(turn);
                }
            }
            return Ok(());
        }
        let (run_workspace, captured, is_bot) = match state.attempt.take() {
            Some(old) => {
                if old.id != attempt {
                    state.last_attempt = Some((old.id, old.generation));
                }
                (old.run_workspace, old.captured, old.is_bot)
            }
            None => (Default::default(), Default::default(), true),
        };
        let mut successor = Attempt::new(attempt, generation);
        successor.session = Some(session);
        successor.is_bot = is_bot;
        successor.holds_lease = true;
        successor.bot_observed = true;
        successor.orchestrator_turn = turn;
        successor.resume_turn = turn;
        successor.run_workspace = run_workspace;
        successor.captured = captured;
        state.attempt = Some(successor);
        Ok(())
    }

    /// Send the one continuation input under its retained key to the bound,
    /// installed successor, then record the host's answer through the
    /// ledger: the accepted turn only on its positive answer. Any other
    /// answer leaves the input unconfirmed; a later turn resends the same
    /// keyed input. A closed or acknowledged resume never sends.
    pub fn bot_submit_child_continuation(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<ChildWaitExecution>, RunError> {
        let execution = self.child_ticket(owner, run)?;
        let resume = execution
            .resume
            .clone()
            .ok_or_else(|| refused("bot_fenced"))?;
        let bound = match resume.state {
            DecisionResumeState::SessionBound {} => true,
            DecisionResumeState::InputUnconfirmed {} => false,
            _ => return Ok(None),
        };
        if !resume_open(&execution) {
            return Ok(None);
        }
        // The waiting marker must be applied before the Control that clears
        // it can be composed (ledger correction 15): nothing is sent while a
        // projection of this Run is still owed.
        if self
            .bot_ledger()?
            .next_decision_projection(owner, run)?
            .is_some()
        {
            return Err(crate::publication_pending().into());
        }
        let (attempt, generation, session, _) = consumer(&resume)?;
        let session = session.ok_or_else(|| refused("bot_successor_unbound"))?;
        let row = execution.current_attempt.clone();
        if row.attempt != attempt
            || row.generation != generation
            || row.retired
            || row.phase.is_terminal()
            || row.provider_session.and_then(|key| key.session) != Some(session)
        {
            return Err(refused("bot_fenced"));
        }
        let installed = self
            .runs
            .get(&run)
            .and_then(|state| state.attempt.as_ref())
            .is_some_and(|memory| {
                memory.id == attempt
                    && memory.generation == generation
                    && memory.session == Some(session)
                    && memory.holds_lease
            });
        if !installed {
            return Err(refused("bot_controller_required"));
        }
        let (input, fingerprint) =
            self.child_continuation(owner, run, &execution, resume.input_key)?;
        if fingerprint != resume.input_fingerprint {
            return Err(refused("bot_conflict"));
        }
        // A Routine parent's input needs its current Routine proof; an
        // interactive parent continues under its captured policy.
        self.child_policy_proof(run)?;
        let turn = match self.ports.sessions.bot_submit(session, input) {
            Ok(turn) => turn,
            Err(error) => {
                // Delivered or not is unknown: never a refusal, never a turn.
                self.trace(
                    run,
                    "child_continuation_unconfirmed",
                    format_args!("{}", error.code),
                );
                return if bound {
                    self.child_record_input(owner, run, &resume, None).map(Some)
                } else {
                    Ok(None)
                };
            }
        };
        let digest = acknowledgment_digest(resume.input_key, fingerprint, session, turn)?;
        let recorded = self.child_record_input(owner, run, &resume, Some((turn, digest)))?;
        if row.phase != AttemptPhase::Running {
            self.ports
                .store
                .advance(run, generation, AttemptPhase::Running)?;
        }
        self.bot_install_child_successor(owner, run)?;
        Ok(Some(recorded))
    }

    /// Record the continuation input under its retained key and fingerprint,
    /// with the accepted turn only when the host positively acknowledged it.
    fn child_record_input(
        &mut self,
        owner: BotOwner,
        run: RunId,
        resume: &ChildResume,
        accepted: Option<(AgentTurnId, koru_delegate_core::decisions::DecisionDigest)>,
    ) -> Result<ChildWaitExecution, RunError> {
        let execution = self.child_ticket(owner, run)?;
        let acknowledgment = accepted.map(|(turn, receipt_digest)| DecisionInputAcknowledgment {
            turn,
            receipt_digest,
        });
        Ok(self.bot_ledger()?.record_child_resume_input(
            &execution.expected,
            DecisionTransitionId::mint(),
            resume.input_key,
            resume.input_fingerprint,
            acknowledgment.as_ref(),
        )?)
    }

    /// The durably acknowledged continuation turn of this exact recorded
    /// child-wait successor (attempt, generation, bound session), read from
    /// the ledger even after the wait settled or the process restarted.
    /// `None` is "no positive evidence", never permission.
    pub(in crate::orchestrator) fn child_resume_turn(
        &mut self,
        run: RunId,
        attempt: RunAttemptId,
        generation: AttemptGeneration,
        session: SessionId,
    ) -> Result<Option<AgentTurnId>, RunError> {
        let Some(admission) = self.bot_ledger()?.bot_admission(run)? else {
            return Ok(None);
        };
        let owner = admission.submission.provenance.owner;
        // A ledger that does not model child requests has no such successor.
        let Ok(Some(continuation)) = self
            .bot_ledger()?
            .child_acknowledged_continuation(owner, run)
        else {
            return Ok(None);
        };
        Ok((continuation.attempt == attempt
            && continuation.generation == generation
            && continuation.session == session)
            .then_some(continuation.turn))
    }
}
