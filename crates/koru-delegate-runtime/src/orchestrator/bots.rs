// SPDX-License-Identifier: MPL-2.0

//! Bot policy is owned by AI. This module admits its authenticated snapshot
//! through the existing Run journal and executes only through this orchestrator.
use super::{Orchestrator, RunError, Started};
use crate::{Port, PortError, StartOrigin};
use koru_delegate_core::{
    BotAdmissionBarrier, BotAdmissionState, BotAdmissionView, BotExecutionFence, BotOwner,
    BotRunKey, BotSubmission, BotSubmissionKey, DeviceId, RunId, RunStatus,
};
use koru_delegate_sqlite::IntentState;
use koru_delegate_sqlite::{
    BotExecutionRecord, BotLedger, BotLedgerError, BotReservation, BotSettlement,
    QueuedBotAdmission,
};
mod cleanup;
mod interrupt;

pub(crate) fn refused(code: &'static str) -> RunError {
    PortError::new(Port::Store, code, "The Bot operation cannot be admitted.").into()
}
impl From<BotLedgerError> for RunError {
    fn from(error: BotLedgerError) -> Self {
        match error {
            BotLedgerError::Ledger(error) => error.into(),
            BotLedgerError::Invalid => refused("bot_invalid"),
            BotLedgerError::Conflict => refused("bot_conflict"),
            BotLedgerError::Capacity => refused("bot_capacity"),
            BotLedgerError::Stale => refused("bot_stale"),
            BotLedgerError::Fenced => refused("bot_fenced"),
        }
    }
}
pub(super) fn owner_device(owner: BotOwner, device: DeviceId) -> Result<(), RunError> {
    if owner.device != device || device.as_uuid().is_nil() {
        return Err(refused("bot_owner_mismatch"));
    }
    Ok(())
}
impl Orchestrator {
    pub(crate) fn bot_ledger(&mut self) -> Result<&mut dyn BotLedger, RunError> {
        self.ports
            .store
            .bots()
            .ok_or_else(|| refused("bot_execution_unavailable"))
    }

    pub fn bot_submit(
        &mut self,
        submission: BotSubmission,
        device: DeviceId,
    ) -> Result<BotAdmissionView, RunError> {
        owner_device(submission.provenance.owner, device)?;
        if !submission.valid() {
            return Err(refused("bot_invalid"));
        }
        let record = self.bot_ledger()?.admit_bot(&submission)?;
        self.invalidate_boot_for_submission(&submission);
        if self
            .ports
            .store
            .intent(submission.request.run)?
            .is_some_and(|intent| intent.state == IntentState::Pending)
        {
            if self
                .complete_intent(submission.request.clone(), submission.workspace)
                .is_err()
            {
                if self
                    .ports
                    .store
                    .intent(submission.request.run)?
                    .is_some_and(|intent| intent.state == IntentState::Abandoned)
                {
                    self.settle_bot_admission(submission.request.run, BotSettlement::Refused)?;
                }
                return Err(refused("bot_context_unavailable"));
            }
        }
        let current = self
            .bot_ledger()?
            .bot_execution(record.submission.request.run)?
            .ok_or(RunError::UnknownRun)?;
        self.bot_admission_view(&current)
    }

    pub fn bot_reconcile(
        &mut self,
        key: &BotSubmissionKey,
        device: DeviceId,
    ) -> Result<Option<BotAdmissionView>, RunError> {
        owner_device(key.owner, device)?;
        self.bot_ledger()?
            .reconcile_bot_execution(key)?
            .map(|record| self.bot_admission_view(&record))
            .transpose()
    }

    /// What of one Bot's work is not settled, read from the ledger now: the
    /// reserved or executing admission and a bounded page of queued ones,
    /// each with its current fence and whether a person holds its session.
    /// The AI owner settles exactly this set before it archives or trashes
    /// the Bot, and asks again every time, so nothing it remembered from an
    /// earlier round can stand in for the ledger.
    pub fn bot_unsettled(
        &mut self,
        owner: BotOwner,
        bot: koru_delegate_core::BotId,
        limit: u16,
        device: DeviceId,
    ) -> Result<koru_delegate_core::BotUnsettledWork, RunError> {
        owner_device(owner, device)?;
        let (records, more) = self.bot_ledger()?.unsettled_bot(owner, bot, limit)?;
        let mut admissions = Vec::with_capacity(records.len());
        for record in records {
            let admission = self.bot_admission_view(&record)?;
            let held = self.bot_session_held(record.run)?;
            admissions.push(koru_delegate_core::BotUnsettledAdmission { admission, held });
        }
        Ok(koru_delegate_core::BotUnsettledWork { admissions, more })
    }

    /// Whether a person holds this Run's session or asked for it. The
    /// canonical control state says so while the Run is running; after an
    /// automatic interruption only the session owner's handover state does,
    /// so both are read. A queued Run, or one whose session is gone, is not
    /// held. This is a reason to wait: nothing here ends a session.
    fn bot_session_held(&mut self, run: RunId) -> Result<bool, RunError> {
        use koru_session_protocol::{CreationLookup, HandoverState};
        let Some(row) = self.ports.store.current_attempt(run)? else {
            return Ok(false);
        };
        if row.retired {
            return Ok(false);
        }
        if matches!(
            self.status(run)?.control(),
            Some(
                koru_delegate_core::ControlState::Human { .. }
                    | koru_delegate_core::ControlState::HandoverPending { .. }
            )
        ) {
            return Ok(true);
        }
        let Some(key) = row.provider_session else {
            return Ok(false);
        };
        Ok(match self.ports.sessions.lookup(key.idempotency)?.1 {
            CreationLookup::Known { session } => matches!(
                session.handover,
                HandoverState::Requested | HandoverState::Released | HandoverState::Taken
            ),
            _ => false,
        })
    }

    pub fn bot_barrier(
        &mut self,
        barrier: &BotAdmissionBarrier,
        device: DeviceId,
    ) -> Result<BotAdmissionBarrier, RunError> {
        owner_device(barrier.owner, device)?;
        let applied = self.bot_ledger()?.apply_bot_barrier(barrier)?;
        self.invalidate_boot_for_barrier(&applied);
        Ok(applied)
    }

    pub fn bot_candidates(
        &mut self,
        after: u64,
        limit: u16,
    ) -> Result<Vec<QueuedBotAdmission>, RunError> {
        Ok(self.bot_ledger()?.queued_bots(after, limit)?)
    }

    pub(crate) fn bot_record(
        &mut self,
        key: &BotRunKey,
        device: DeviceId,
    ) -> Result<BotExecutionRecord, RunError> {
        owner_device(key.owner, device)?;
        let record = self
            .bot_ledger()?
            .bot_execution(key.run)?
            .ok_or(RunError::UnknownRun)?;
        if record.owner != key.owner || record.bot != key.bot {
            return Err(refused("bot_owner_mismatch"));
        }
        Ok(record)
    }

    /// Historical read uses the retained immutable binding, not live Bot
    /// configuration. AI checked current workspace authority before this call.
    pub fn bot_read_allowed(&mut self, key: &BotRunKey, device: DeviceId) -> Result<(), RunError> {
        self.bot_record(key, device).map(|_| ())
    }

    pub(crate) fn bot_admission_view(
        &mut self,
        record: &BotExecutionRecord,
    ) -> Result<BotAdmissionView, RunError> {
        let run = record.run;
        let row = self.ports.store.current_attempt(run)?;
        let fence = row.and_then(|row| {
            let session = row.provider_session.and_then(|key| key.session)?;
            Some(BotExecutionFence {
                owner: record.owner,
                bot: record.bot,
                run,
                attempt: row.attempt,
                generation: row.generation,
                session,
                turn: self
                    .runs
                    .get(&run)
                    .and_then(|state| state.attempt.as_ref())
                    .and_then(|attempt| attempt.orchestrator_turn)
                    .or_else(|| record.initial_send.as_ref().and_then(|send| send.turn)),
            })
        });
        Ok(BotAdmissionView {
            key: BotSubmissionKey {
                owner: record.owner,
                bot: record.bot,
                request: record.request,
            },
            run,
            state: record.state,
            queue_sequence: record.sequence,
            fence,
        })
    }

    /// At most one selected candidate's payload is loaded. A recovered
    /// reservation with any journal attempt is never interpreted as permission
    /// to launch again; restart reconciles that attempt through its native key.
    pub fn bot_dispatch(
        &mut self,
        key: &BotRunKey,
        device: DeviceId,
    ) -> Result<Option<Started>, RunError> {
        self.bot_dispatch_reserved(key, device, &[])
    }

    pub fn bot_dispatch_reserved(
        &mut self,
        key: &BotRunKey,
        device: DeviceId,
        reserved: &[koru_delegate_scope::ScopeIdentity],
    ) -> Result<Option<Started>, RunError> {
        let record = self.bot_record(key, device)?;
        if !matches!(
            record.state,
            BotAdmissionState::Queued | BotAdmissionState::Reserved
        ) {
            return Ok(None);
        }
        if self.ports.store.current_attempt(key.run)?.is_some() {
            return Ok(None);
        }
        if !self.bot_boot_ack_current(
            key.owner,
            key.bot,
            record.config_revision,
            record.policy_revision,
        )? {
            return Ok(None);
        }
        if self
            .bot_ledger()?
            .bot_barrier(key.owner, key.bot)?
            .is_some_and(|barrier| barrier.paused)
        {
            return Ok(None);
        }
        let active = self.bot_ledger()?.active_bot_executions(device)?;
        if record.state == BotAdmissionState::Queued
            && (active.len() >= koru_delegate_sqlite::BOT_HOST_EXECUTION_LIMIT
                || active
                    .iter()
                    .any(|other| other.owner == key.owner && other.bot == key.bot))
        {
            return Ok(None);
        }
        let record = self
            .bot_ledger()?
            .bot_admission(key.run)?
            .ok_or(RunError::UnknownRun)?;
        // Resolve before reserving capacity. A busy scope does not consume a
        // host slot, and a missing persistent folder is never recreated.
        let scope = self
            .plan_scope(key.run, &record.submission.request.scope)
            .map_err(|_| refused("bot_folder_unavailable"))?;
        if !self.claims.holders_of(&scope.identity).is_empty()
            || reserved
                .iter()
                .any(|other| scope.identity.overlap(other).is_overlap())
        {
            return Ok(None);
        }
        // A Run interrupted while queued can never start; do not reserve a
        // host slot for it. Only an existing document is read here, so an
        // ordinary admission (still Created, or not yet materialized) is
        // unchanged and keeps the canonical check below.
        if self
            .ports
            .store
            .intent(key.run)?
            .is_some_and(|intent| intent.state == IntentState::Satisfied)
            && self.status(key.run)? != RunStatus::Created
        {
            return Ok(None);
        }
        match self.bot_ledger()?.reserve_bot(key.run)? {
            BotReservation::Acquired | BotReservation::AlreadyOwned => {}
            _ => return Ok(None),
        }
        if !self.bot_boot_ack_current(
            key.owner,
            key.bot,
            record.submission.provenance.config_revision,
            record.submission.provenance.policy_revision,
        )? {
            return Ok(None);
        }
        // The actor serializes barriers and dispatch. Retain the reservation if
        // an injected owner nevertheless reports a pause at this last boundary.
        if self
            .bot_ledger()?
            .bot_barrier(key.owner, key.bot)?
            .is_some_and(|barrier| barrier.paused)
        {
            return Ok(None);
        }
        let intent = self
            .ports
            .store
            .intent(key.run)?
            .ok_or(RunError::UnknownRun)?;
        match intent.state {
            IntentState::Pending => {
                if self
                    .complete_intent(record.submission.request, record.submission.workspace)
                    .is_err()
                {
                    if self
                        .ports
                        .store
                        .intent(key.run)?
                        .is_some_and(|intent| intent.state == IntentState::Abandoned)
                    {
                        self.settle_bot_admission(key.run, BotSettlement::Refused)?;
                    }
                    return Err(refused("bot_context_unavailable"));
                }
            }
            IntentState::Abandoned => {
                self.settle_bot_admission(key.run, BotSettlement::Refused)?;
                return Err(refused("bot_intent_abandoned"));
            }
            IntentState::Satisfied => {}
        }
        if self.status(key.run)? != RunStatus::Created {
            return Ok(None);
        }
        if !self.bot_boot_ack_current(key.owner, key.bot, 0, 0)? {
            return Ok(None);
        }
        let started = self.start_resolved(key.run, scope, StartOrigin::Bot { bot: key.bot })?;
        Ok(Some(started))
    }
}

impl Orchestrator {
    /// A private capability does not make an ordinary Run route a Bot route.
    pub fn is_bot_run(
        &mut self,
        run: RunId,
        workspace: Option<koru_delegate_core::WorkspaceId>,
    ) -> Result<bool, RunError> {
        if let Some(ledger) = self.ports.store.bots() {
            if ledger.bot_execution(run)?.is_some() {
                return Ok(true);
            }
        }
        Ok(self
            .open_in(run, workspace)?
            .doc
            .document()
            .header()?
            .bot
            .is_some())
    }

    /// Prove stopped/fenced through the existing session creation owner. The
    /// proof is the host's answer to `retire_creation`, never the terminate:
    /// an accepted terminate is not proof, and a refused one is not disproof.
    /// The host refuses to terminate a session it no longer holds
    /// (`session_not_found`: reaped after its process ended, or held by an
    /// earlier host incarnation), which is exactly when its answer proves the
    /// end; failing on that refusal left such a Run unprovable forever.
    ///
    /// No evidence proves the end whichever incarnation answers (ADR-341):
    /// from the same incarnation nothing runs under the key, and the retire
    /// just fenced it so nothing will; from a new one nothing survived, since
    /// the host's process trees end with it (its kill-on-close jobs). A Run
    /// whose session host restarted is therefore not running.
    pub(crate) fn bot_prove_end(&mut self, run: RunId, terminate: bool) -> Result<bool, RunError> {
        use koru_session_protocol::{CreationLookup, ProcessState};
        let Some(row) = self.ports.store.current_attempt(run)? else {
            return Ok(true);
        };
        if row.retired {
            return Ok(true);
        }
        let Some(key) = row.provider_session else {
            return Ok(true);
        };
        if terminate {
            let session = match key.session {
                Some(session) => Some(session),
                None => match self.ports.sessions.lookup(key.idempotency)?.1 {
                    CreationLookup::Known { session } => Some(session.id),
                    _ => None,
                },
            };
            if let Some(session) = session
                && let Err(error) = self.ports.sessions.terminate(session)
            {
                self.trace(run, "terminate_refused", format_args!("{}", error.code));
            }
        }
        let (_, lookup) = self.ports.sessions.retire(key.idempotency)?;
        Ok(match lookup {
            CreationLookup::KnownEnded { .. } | CreationLookup::NoEvidence => true,
            CreationLookup::Known { session } => matches!(
                session.process,
                ProcessState::Exited { .. } | ProcessState::Failed { .. }
            ),
            CreationLookup::Creating => false,
        })
    }

    pub(crate) fn bot_end_attempt(
        &mut self,
        run: RunId,
        phase: koru_delegate_sqlite::AttemptPhase,
        terminate: bool,
    ) -> Result<(), RunError> {
        use koru_delegate_sqlite::AttemptPhase;
        self.arm_bot_cleanup(run)?;
        let Some(row) = self.ports.store.current_attempt(run)? else {
            return Ok(());
        };
        if phase != AttemptPhase::Ended {
            // Loss of observation is not a stop. Retain the in-memory claim
            // and durable host/Bot reservation for later verified fencing.
            if !row.phase.is_terminal() {
                self.ports.store.advance(run, row.generation, phase)?;
            }
            return Ok(());
        }
        if terminate && !self.bot_prove_end_owned(run)? {
            return Err(refused("bot_stop_unconfirmed"));
        }
        if self.native_effects_pending(run)? {
            return Err(refused("bot_native_effects_pending"));
        }
        // A terminal process and drained effects do not acknowledge the Run
        // document. Open only its recorded identity; failed open stays an error.
        // Canonical cancellation/interruption must survive before capacity does.
        self.bot_settle_before_retirement(run)?;
        if !row.phase.is_terminal() {
            self.ports
                .store
                .advance(run, row.generation, AttemptPhase::Ended)?;
        }
        self.ports.store.retire(run, row.generation)?;
        // Retirement is the proof boundary. A lost settlement acknowledgement
        // must remain retryable without dropping the still-held folder claim.
        self.settle_bot_admission(run, BotSettlement::Settled)?;
        let state = self.open(run)?;
        let attempt = state.attempt.take();
        state.last_attempt = Some((row.attempt, row.generation));
        self.claims.release(row.attempt);
        if let Some(attempt) = attempt {
            if !attempt.run_workspace.as_os_str().is_empty() {
                self.ports.capture.cleanup(run, &attempt.run_workspace);
            }
        }
        Ok(())
    }

    pub fn bot_stop(
        &mut self,
        key: &BotRunKey,
        device: DeviceId,
        fence: Option<&BotExecutionFence>,
    ) -> Result<(), RunError> {
        self.bot_record(key, device)?;
        let row = self.ports.store.current_attempt(key.run)?;
        match row {
            Some(row)
                if !row.retired
                    && row
                        .provider_session
                        .as_ref()
                        .and_then(|key| key.session)
                        .is_none() =>
            {
                if fence.is_some() {
                    return Err(refused("bot_fenced"));
                }
            }
            Some(row) if !row.retired => {
                let fence = fence.ok_or_else(|| refused("bot_fence_required"))?;
                if fence.owner != key.owner
                    || fence.bot != key.bot
                    || fence.run != key.run
                    || fence.attempt != row.attempt
                    || fence.generation != row.generation
                    || row.provider_session.and_then(|key| key.session) != Some(fence.session)
                {
                    return Err(refused("bot_fenced"));
                }
            }
            Some(_) => {
                return self.bot_end_attempt(
                    key.run,
                    koru_delegate_sqlite::AttemptPhase::Ended,
                    false,
                );
            }
            None if fence.is_some() => return Err(refused("bot_fenced")),
            None => {}
        }
        // cancel specializes Bot retirement below; it never needs a human lease.
        self.cancel(key.run)?;
        self.settle(key.run)?;
        if self.ports.store.current_attempt(key.run)?.is_none() {
            self.bot_settle_before_retirement(key.run)?;
            self.settle_bot_admission(key.run, BotSettlement::Settled)?;
        }
        Ok(())
    }

    fn bot_settle_before_retirement(&mut self, run: RunId) -> Result<(), RunError> {
        let state = self.open(run)?;
        state.doc.settle()?;
        if !state.doc.durable() {
            return Err(refused("bot_stop_unconfirmed"));
        }
        Ok(())
    }

    /// Automatic cleanup never borrows the stronger person-authorized Stop.
    pub(crate) fn bot_prove_end_owned(&mut self, run: RunId) -> Result<bool, RunError> {
        if self.bot_prove_end(run, false)? {
            return Ok(true);
        }
        let key = self
            .ports
            .store
            .current_attempt(run)?
            .and_then(|row| row.provider_session);
        let session = match key {
            Some(key) => match key.session {
                Some(session) => Some(session),
                None => match self.ports.sessions.lookup(key.idempotency)?.1 {
                    koru_session_protocol::CreationLookup::Known { session } => Some(session.id),
                    _ => None,
                },
            },
            None => None,
        };
        let Some(session) = session else {
            return Ok(false);
        };
        if self.ports.sessions.terminate_owned(session).is_err() {
            return Ok(false);
        }
        self.bot_prove_end(run, false)
    }

    /// Only active in-memory Bot executions are examined by the actor deadline,
    /// never a walk of documents or queued task payloads.
    pub fn bot_budget_runs(&self) -> Vec<RunId> {
        self.runs
            .iter()
            .filter_map(|(run, state)| {
                state
                    .attempt
                    .as_ref()
                    .filter(|attempt| attempt.is_bot)
                    .map(|_| *run)
            })
            .collect()
    }

    pub fn bot_budget_tick(&mut self, run: RunId) -> Result<bool, RunError> {
        if let Some(released) = self.bot_cleanup_tick(run)? {
            return Ok(released);
        }
        let Some(record) = self.bot_ledger()?.bot_execution(run)? else {
            return Ok(false);
        };
        if record.active_elapsed_at(self.ports.clock.now_ms())
            < koru_delegate_sqlite::BOT_ACTIVE_BUDGET_MS
        {
            return Ok(false);
        }
        // Current identity is known by the owner; budget Stop has the same
        // proof requirement as a person's Stop and never frees unknown work.
        self.cancel(run)?;
        Ok(self
            .bot_ledger()?
            .bot_execution(run)?
            .is_some_and(|record| {
                matches!(
                    record.state,
                    BotAdmissionState::Settled | BotAdmissionState::Refused
                )
            }))
    }

    pub fn bot_budget_delay_ms(&mut self) -> Result<Option<u64>, RunError> {
        let now = self.ports.clock.now_ms();
        let mut nearest = None;
        for run in self.bot_budget_runs() {
            if let Some(retry) = self
                .open(run)?
                .attempt
                .as_ref()
                .and_then(|attempt| attempt.bot_cleanup.as_ref())
            {
                let delay = u64::try_from(
                    retry
                        .due
                        .saturating_duration_since(std::time::Instant::now())
                        .as_millis(),
                )
                .unwrap_or(u64::MAX)
                .max(1);
                nearest = Some(nearest.map_or(delay, |old: u64| old.min(delay)));
            }
            if let Some(record) = self.bot_ledger()?.bot_execution(run)? {
                if record.active_since_ms.is_some() {
                    let delay = koru_delegate_sqlite::BOT_ACTIVE_BUDGET_MS
                        .saturating_sub(record.active_elapsed_at(now))
                        .max(1000);
                    nearest = Some(nearest.map_or(delay, |old: u64| old.min(delay)));
                }
            }
        }
        Ok(nearest)
    }
}

impl Orchestrator {
    pub(crate) fn validate_bot_start_origin(
        &mut self,
        run: RunId,
        origin: &StartOrigin,
    ) -> Result<(), RunError> {
        let header = self.header(run)?;
        match (header.bot.as_ref(), origin) {
            (None, StartOrigin::Bot { .. })
            | (
                Some(_),
                StartOrigin::Manual
                | StartOrigin::Scheduled { .. }
                | StartOrigin::ClaimedSchedule { .. },
            ) => Err(refused("bot_private_route_required")),
            (Some(binding), StartOrigin::Bot { bot }) if binding.bot != *bot => {
                Err(refused("bot_fenced"))
            }
            _ => Ok(()),
        }
    }
}
