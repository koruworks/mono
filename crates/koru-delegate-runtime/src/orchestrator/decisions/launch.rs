// SPDX-License-Identifier: MPL-2.0

//! The recorded successor's physical launch and its exact continuation input.
//!
//! C has already allocated the successor attempt, its creation key and its
//! input key. This owner asks the host for that one creation (at most once
//! per process, and only for a reservation made in this process), recovers a
//! lost or older answer by the exact creation key, and sends the one
//! continuation input under its retained key. It never writes the Run
//! document: decision publication owns the sole handle for the whole window.
//! Same-session capacity-releasing hold is not offered here (unsupported).

use super::*;
use crate::orchestrator::state::DecisionLaunch;
use crate::{McpLaunch, SessionLaunch};
use koru_delegate_core::decisions::{DecisionExpected, DecisionResumeState};
use koru_delegate_core::{
    AgentTurnId, IdempotencyKey, PendingInput, RunAttemptId, SessionId, WorkspaceScope,
};
use koru_delegate_scope::{Admission, ScopeIdentity, StartOrigin};
use koru_delegate_sqlite::AttemptPhase;
use std::path::PathBuf;

/// Domain of the continuation fingerprint a successor reservation binds.
const CONTINUATION_DOMAIN: &str = "koru.bot.decision.continuation.v1";
/// Domain of the digest of one observed continuation acknowledgment.
const ACKNOWLEDGMENT_DOMAIN: &str = "koru.bot.decision.continuation.ack.v1";

/// Provider payload, never logged: the person's original captured task input,
/// why a new session continues it, and the one edit the person allowed,
/// exactly as retained in the decision binding (target, operation id and the
/// normalized arguments) so the successor can repeat that edit and nothing
/// else. No answer text or earlier conversation is added.
fn continuation_body(
    task: &str,
    decision: DecisionId,
    edit: &koru_delegate_core::decisions::DecisionEditBinding,
) -> Result<String, RunError> {
    let tool = match edit.action {
        koru_delegate_core::decisions::DecisionEditAction::EditDocument => "edit_document",
    };
    // The retained arguments are normalized JSON text; anything else is not
    // sent rather than sent garbled.
    let arguments =
        std::str::from_utf8(edit.payload.as_slice()).map_err(|_| refused("bot_invalid"))?;
    Ok(format!(
        "{task}\n\nKoru resumed this run in a new session after the person allowed decision `{decision}`. The earlier session's conversation is not available here; results it already saved remain. Do not repeat a write that already succeeded.\n\nThe person allowed exactly one edit. Make it once with `{tool}` on document `{target}`, with operation_id `{operation}` and these approved arguments unchanged:\n{arguments}",
        target = edit.target,
        operation = edit.operation_id,
    ))
}

/// The digest of one positive host acknowledgment of the continuation input:
/// its retained key and fingerprint, the bound session and the accepted turn.
fn acknowledgment_digest(
    key: IdempotencyKey,
    fingerprint: [u8; 32],
    session: SessionId,
    turn: AgentTurnId,
) -> Result<DecisionDigest, RunError> {
    let encoded = serde_json::to_vec(&(ACKNOWLEDGMENT_DOMAIN, key, fingerprint, session, turn))
        .map_err(|_| refused("bot_invalid"))?;
    Ok(DecisionDigest::from_bytes(
        *blake3::hash(&encoded).as_bytes(),
    ))
}

/// The decision still wants its successor started and fed: AllowOnce with
/// the resume prepared and the wait reserved for it. Stop, expiry, denial or
/// authority loss moves either phase away, and nothing new is sent then.
pub(super) fn resume_open(execution: &koru_delegate_sqlite::DecisionWaitExecution) -> bool {
    use koru_delegate_core::decisions::{BotWaitPhase, DecisionAuthorization, DecisionPhase};
    execution.wait.phase == BotWaitPhase::ResumeReserved
        && execution.decision_state.as_ref().is_some_and(|state| {
            state.authorization == DecisionAuthorization::Granted
                && state.phase == DecisionPhase::ResumePrepared
        })
}

impl Orchestrator {
    /// The fingerprint a successor reservation binds for this Run's open Koru
    /// decision: the exact continuation input it will send, less its key.
    /// Deterministic over retained inputs only (admission task input, the
    /// decision's identity and binding digest, the Run's working folder).
    pub fn bot_decision_continuation_fingerprint(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<[u8; 32], RunError> {
        let execution = self.decision_ticket(owner, run)?;
        let expected = Self::decision_expected(&execution)?;
        Ok(self
            .decision_continuation(owner, run, &expected, IdempotencyKey::mint())?
            .1)
    }

    /// The exact continuation input under `key` and its key-free fingerprint.
    /// Original captured task input only: no recapture, no assembled history,
    /// no session-scoped attachment from the original session.
    fn decision_continuation(
        &mut self,
        owner: BotOwner,
        run: RunId,
        expected: &DecisionExpected,
        key: IdempotencyKey,
    ) -> Result<(PendingInput, [u8; 32]), RunError> {
        let admission = self
            .bot_ledger()?
            .bot_admission(run)?
            .ok_or(RunError::UnknownRun)?;
        let submission = &admission.submission;
        if submission.provenance.owner != owner || submission.request.run != run {
            return Err(refused("bot_fenced"));
        }
        if submission.provenance.routine.is_some() {
            return Err(refused("bot_routine_decision_unsupported"));
        }
        let retained = self
            .bot_ledger()?
            .bot_decision(owner, run, expected.decision)?
            .ok_or_else(|| refused("bot_fenced"))?;
        if retained.receipt.binding_digest != expected.binding_digest {
            return Err(refused("bot_fenced"));
        }
        let DecisionBindingV1::KoruEdit(edit) = &retained.receipt.binding else {
            return Err(refused("bot_fenced"));
        };
        let workspace = self.decision_run_workspace(run)?;
        let task = &submission.input;
        let text = crate::orchestrator::bot_controls::native_prompt::compose(
            run,
            &workspace,
            &continuation_body(&task.text, expected.decision, edit)?,
        );
        let input = PendingInput {
            idempotency_key: key,
            text,
            attachment_ids: Vec::new(),
            references: task.references.clone(),
            mode: task.mode.clone(),
            fast: task.fast,
            reasoning: task.reasoning.clone(),
            skills: task.skills.clone(),
            model: task.model.clone(),
        };
        let encoded = serde_json::to_vec(&(
            CONTINUATION_DOMAIN,
            expected.decision,
            expected.binding_digest,
            &input.text,
            &input.attachment_ids,
            &input.references,
            &input.mode,
            &input.fast,
            &input.reasoning,
            &input.skills,
            &input.model,
        ))
        .map_err(|_| refused("bot_invalid"))?;
        Ok((input, *blake3::hash(&encoded).as_bytes()))
    }

    /// The Run's working folder, as the original attempt's Bot instructions
    /// named it. Unknown refuses; no folder is guessed.
    fn decision_run_workspace(&mut self, run: RunId) -> Result<PathBuf, RunError> {
        if let Some(path) = self
            .ports
            .mcp
            .run_workspace(run)?
            .filter(|path| !path.as_os_str().is_empty())
        {
            return Ok(path);
        }
        self.runs
            .get(&run)
            .and_then(|state| state.attempt.as_ref())
            .map(|attempt| attempt.run_workspace.clone())
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| refused("bot_folder_unavailable"))
    }

    /// A fresh proof of the Bot's current global policy, from the snapshot
    /// the AI owner synchronized, rechecked against the retained ledger row.
    /// Usable while decision publication owns the handle (the ordinary
    /// execution-policy read refuses then); a Routine Run refuses (O125).
    pub(in crate::orchestrator) fn decision_policy_proof(
        &mut self,
        run: RunId,
    ) -> Result<FreshBotPolicyProof, RunError> {
        let admitted = self
            .bot_ledger()?
            .bot_admission(run)?
            .ok_or(RunError::UnknownRun)?;
        self.linked_routine_admission(&admitted)?;
        let provenance = &admitted.submission.provenance;
        if provenance.routine.is_some() {
            return Err(refused("bot_routine_decision_unsupported"));
        }
        let current = self
            .native_ledger()?
            .current_native_policy(provenance.owner, provenance.bot)?
            .ok_or_else(|| refused("bot_policy_refresh_required"))?;
        self.current_policy_for_submission(
            &admitted.submission,
            current.config_revision,
            current.policy_revision,
        )
    }

    /// Move the Run's in-memory folder claim from the retired origin to the
    /// successor. Another holder refuses before anything is released.
    pub(super) fn decision_transfer_claim(
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

    /// Ask the host for the reserved successor under its exact creation key
    /// through the existing managed launch owner (current readiness, the
    /// Run's tool authority for the successor generation, the frozen grants),
    /// at most once in this process and only for a reservation made here. A
    /// lost or older answer is recovered by `lookup`, never a second creation.
    /// Binds through C only on the host's positive answer; an unknown answer
    /// records nothing and keeps occupancy.
    pub fn bot_launch_decision_successor(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionRecord>, RunError> {
        let execution = self.decision_ticket(owner, run)?;
        let resume = execution
            .resume
            .clone()
            .ok_or_else(|| refused("bot_fenced"))?;
        let (attempt, generation, session, creation) = resume.consumer_identity();
        if !matches!(resume.state, DecisionResumeState::Prepared {}) || session.is_some() {
            return Ok(None);
        }
        let row = execution.current_attempt;
        let host = row.host_instance.ok_or_else(|| refused("bot_fenced"))?;
        if row.attempt != attempt
            || row.generation != generation
            || row.retired
            || row.phase.is_terminal()
            || row.provider_session.map(|key| key.idempotency) != Some(creation)
        {
            return Err(refused("bot_fenced"));
        }
        let fresh = self.runs.get(&run).and_then(|state| state.decision_launch)
            == Some(DecisionLaunch::Reserved(creation));
        if !fresh {
            // Reconciling the exact key stays legal after Stop: a late known
            // session is retained so its cleanup can target it.
            return self.bot_recover_decision_successor(owner, run);
        }
        if !resume_open(&execution) {
            // Stop, expiry or authority loss superseded the resume before any
            // creation was asked for: nothing is created.
            return Ok(None);
        }
        if !self.ports.sessions.supports_delegation() {
            return Err(RunError::NotDelegatable);
        }
        if self.ports.sessions.host_instance()? != host {
            // The host the creation key was reserved on is gone; only the
            // cleanup owner may decide about that reservation now.
            return Err(refused("bot_fenced"));
        }
        let admission = self
            .bot_ledger()?
            .bot_admission(run)?
            .ok_or(RunError::UnknownRun)?;
        let submission = admission.submission;
        let provenance = submission.provenance;
        if provenance.owner != owner || provenance.routine.is_some() {
            return Err(refused("bot_fenced"));
        }
        let request = submission.request;
        if !self.ports.sessions.profile_delegatable(&request.agent)? {
            return Err(refused("bot_profile_unready"));
        }
        self.decision_policy_proof(run)?;
        let tool_authority =
            self.resolve_run_tool_authority(run, generation, request.executing_device)?;
        let directory = self.decision_run_workspace(run)?;
        let model = match request.requested_model.as_ref() {
            Some(model) => Some(model.as_str().parse().map_err(|_| refused("bot_invalid"))?),
            None => None,
        };
        self.decision_transfer_claim(
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
        self.open(run)?.decision_launch = Some(DecisionLaunch::Dispatched(creation));
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
                    "decision_successor_creation_unknown",
                    format_args!("{}", error.code),
                );
                return Ok(None);
            }
        };
        let record =
            self.bot_bind_decision_successor(owner, run, creation, host, created.session)?;
        self.open(run)?.provider = Some(created.provider);
        Ok(Some(record))
    }

    /// Send the one continuation input under its retained key to the bound,
    /// installed successor, then record the host's answer through C: the
    /// accepted turn only on its positive answer. Any other answer leaves the
    /// input unconfirmed; a later turn resends the same keyed input (the host
    /// pins its answer to the key). A closed or acknowledged resume never
    /// sends, and changed input bytes are never sent under the retained key.
    pub fn bot_submit_decision_continuation(
        &mut self,
        owner: BotOwner,
        run: RunId,
    ) -> Result<Option<DecisionRecord>, RunError> {
        let execution = self.decision_ticket(owner, run)?;
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
            // A superseded resume never sends input; its cleanup owner decides.
            return Ok(None);
        }
        let (attempt, generation, session, _) = resume.consumer_identity();
        let session = session.ok_or_else(|| refused("bot_successor_unbound"))?;
        let row = execution.current_attempt;
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
        let expected = Self::decision_expected(&execution)?;
        let (input, fingerprint) =
            self.decision_continuation(owner, run, &expected, resume.input_key)?;
        if fingerprint != resume.input_fingerprint {
            return Err(refused("bot_conflict"));
        }
        // Input that may start work needs current policy; a retained
        // acknowledgment never does (it is recorded below, not re-checked).
        self.decision_policy_proof(run)?;
        let turn = match self.ports.sessions.bot_submit(session, input) {
            Ok(turn) => turn,
            Err(error) => {
                // Delivered or not is unknown: never a refusal, never a turn.
                self.trace(
                    run,
                    "decision_continuation_unconfirmed",
                    format_args!("{}", error.code),
                );
                return if bound {
                    self.bot_record_decision_resume_input(owner, run, None)
                        .map(Some)
                } else {
                    Ok(None)
                };
            }
        };
        let digest = acknowledgment_digest(resume.input_key, fingerprint, session, turn)?;
        let record = self.bot_record_decision_resume_input(owner, run, Some((turn, digest)))?;
        if row.phase != AttemptPhase::Running {
            self.ports
                .store
                .advance(run, generation, AttemptPhase::Running)?;
        }
        self.bot_install_decision_successor(owner, run)?;
        Ok(Some(record))
    }
}
