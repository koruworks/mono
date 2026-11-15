// SPDX-License-Identifier: MPL-2.0

//! §7.6: what a restart can prove, and what it does about each answer.
//!
//! In order: every pending intent is completed forward from its own
//! payload (§7.1); every live attempt is classified with the ledger's
//! table over the host's **non-creating** lookup and its incarnation —
//! reattach, wait and re-ask, reconcile, or `Interrupted` (never `Failed`,
//! D10; *prior attempt unknown* is never auto-resumed); the scope claims
//! are rebuilt from what stayed live (P1.5); and a run that existed and had
//! never started is started.
//!
//! **Reattach never submits again.** The orchestrator reacquires the
//! controller, records a gap **when the host says the observation tail was
//! evicted** and not otherwise, reconciles the turn from the session's
//! summary, and subscribes from the session's cursor forward. The turn that
//! was running goes on being the turn that is running.

use std::collections::HashMap;

use koru_delegate_core::{
    PriorAttempt, RunEvent, RunOrigin, RunStatus, StepKind, TimelineRef, TimelineRefs,
    TurnOriginator, WorkspaceScope,
};
use koru_delegate_scope::{
    DurableClaim, DurableClaims, Liveness, ScheduleRef, ScopeClaims, StartOrigin,
};
use koru_delegate_sqlite::{
    AttemptPhase, AttemptRow, IntentState, LookupAnswer, RestartVerdict, classify_at_restart,
};
use koru_session_protocol::{CreationLookup, ManagedState};

use crate::{
    RunFact,
    orchestrator::state::{Attempt, TurnState},
};

use super::{
    Orchestrator, RestartReport, RunError, ScheduledScopeReservation, ScopeReservationResolution,
    Started,
};

/// The session host's answer when a controller is already held. A crash
/// between release and attach is the one case where that is news rather
/// than a failure (§7.4).
const CONTROLLER_CONFLICT: &str = "controller_conflict";

/// The claims a restart rebuilds from: the live rows, each with the scope
/// its Run document names.
struct Recorded(Vec<DurableClaim>);

impl DurableClaims for Recorded {
    type Error = RunError;

    fn claims_live_at_shutdown(&self) -> Result<Vec<DurableClaim>, RunError> {
        Ok(self.0.clone())
    }
}

impl Orchestrator {
    pub fn restart(&mut self) -> Result<RestartReport, RunError> {
        let mut report = RestartReport::default();

        // §7.1: an unsatisfied intent is completed forward.
        for pending in self.ports.store.pending_intents()? {
            let scheduled_scope = matches!(pending.payload.origin, RunOrigin::Scheduled { .. })
                .then(|| pending.payload.scope.clone());
            match self.complete_intent(pending.payload, pending.workspace) {
                Ok(created) => report.completed_forward.push(created.run),
                Err(error) => {
                    if let Some(scope) = scheduled_scope {
                        report
                            .pending_scheduled_scopes
                            .push(self.scheduled_scope_reservation(pending.run, scope));
                    }
                    tracing::warn!(
                        target: "koru::delegate",
                        run = ?pending.run,
                        %error,
                        "a pending request could not be completed forward"
                    );
                }
            }
        }

        // §7.6: every attempt a process may still exist for.
        let mut liveness: HashMap<koru_delegate_core::RunAttemptId, Liveness> = HashMap::new();
        let mut recorded = Vec::new();
        let mut restart_rows = self.ports.store.live_attempts()?;
        // Ordinary live_attempts excludes terminal Unknown/Interrupted phases.
        // A Bot's unretired row still owns capacity and its folder. This is a
        // one-time startup intent scan, never a timer or queue payload scan.
        for intent in self.ports.store.intents()? {
            let Some(record) = self
                .ports
                .store
                .bots()
                .map(|ledger| ledger.bot_execution(intent.run))
                .transpose()?
                .flatten()
            else {
                continue;
            };
            if matches!(
                record.state,
                koru_delegate_core::BotAdmissionState::Settled
                    | koru_delegate_core::BotAdmissionState::Refused
            ) {
                continue;
            }
            if let Some(row) = self.ports.store.current_attempt(intent.run)? {
                // Only retained unsettled Bot admissions reach this point.
                // Legacy retirement may have outrun the canonical ack; repair
                // it through the same document-first reconciliation below.
                if !restart_rows
                    .iter()
                    .any(|known| known.attempt == row.attempt)
                {
                    restart_rows.push(row);
                }
            }
        }
        for row in restart_rows {
            let run = row.run;
            self.restore_publication_owner(run)?;
            if self.run_publication_pending(run) {
                // Canonical status is deliberately unavailable during cold
                // publication. Recover the immutable scope from its retained
                // admission and keep the claim, including a retired origin
                // whose decision/cleanup publication is not yet drained.
                let admission = self
                    .bot_ledger()?
                    .bot_admission(run)?
                    .ok_or_else(crate::publication_pending)?;
                let state = self.runs.get(&run).ok_or_else(crate::publication_pending)?;
                if admission.submission.request.run != run
                    || admission.submission.workspace != state.workspace
                {
                    return Err(crate::publication_pending().into());
                }
                recorded.push(DurableClaim {
                    run,
                    attempt: row.attempt,
                    scope: admission.submission.request.scope,
                });
                liveness.insert(row.attempt, Liveness::Unknown);
                report.unresolved.push(run);
                continue;
            }
            let header = match self.header(run) {
                Ok(header) => header,
                Err(error) => {
                    tracing::warn!(target: "koru::delegate", run = ?run, %error, "a live attempt's run could not be opened");
                    continue;
                }
            };
            if !row.retired && !row.phase.is_terminal() && !header.status.is_terminal() {
                if let Some(device) = header.executing_device() {
                    // Publish immutable discovery before recovery can wait on a
                    // provider that synchronously asks for its tool catalog.
                    self.refresh_run_tool_authority(run, device)?;
                }
            }
            recorded.push(DurableClaim {
                run,
                attempt: row.attempt,
                scope: header.scope.clone(),
            });
            if header.bot.is_some()
                && (row.retired
                    || row.phase.is_terminal()
                    || header.status.is_terminal()
                    || matches!(
                        header.status,
                        RunStatus::Interrupted {
                            prior_attempt: PriorAttempt::Unknown
                        }
                    ))
            {
                let mut attempt = Attempt::new(row.attempt, row.generation);
                attempt.is_bot = true;
                attempt.session = row.provider_session.and_then(|key| key.session);
                self.open(run)?.attempt = Some(attempt);
                if !header.status.is_terminal()
                    && !matches!(header.status, RunStatus::Interrupted { .. })
                {
                    self.apply(
                        run,
                        &RunEvent::Interrupt {
                            prior_attempt: PriorAttempt::Unknown,
                        },
                    )?;
                    self.step(
                        run,
                        StepKind::Interrupted {
                            prior_attempt: PriorAttempt::Unknown,
                        },
                        TimelineRefs::new([TimelineRef::Attempt {
                            attempt: row.attempt,
                        }])
                        .expect("one attempt"),
                        Some(RunFact::Interrupted {
                            run,
                            attempt: row.attempt,
                            generation: row.generation,
                            prior_attempt: PriorAttempt::Unknown,
                        }),
                    )?;
                }
                self.arm_bot_cleanup(run)?;
                let ended = if header.status == RunStatus::Cancelled {
                    self.bot_prove_end(run, true)
                } else {
                    self.bot_prove_end_owned(run)
                };
                if ended.unwrap_or(false) && !self.native_effects_pending(run).unwrap_or(true) {
                    self.bot_end_attempt(run, AttemptPhase::Ended, false)?;
                    liveness.insert(row.attempt, Liveness::Ended);
                    report.reconciled.push(run);
                } else {
                    liveness.insert(row.attempt, Liveness::Unknown);
                    report.unresolved.push(run);
                }
                continue;
            }
            if header.bot.is_some() {
                // Even an unresolved creation needs the same active-budget
                // deadline and owner Stop path after restart.
                let mut attempt = Attempt::new(row.attempt, row.generation);
                attempt.is_bot = true;
                attempt.session = row.provider_session.and_then(|key| key.session);
                self.open(run)?.attempt = Some(attempt);
            }
            let (verdict, lookup) = match self.classify(&row) {
                Ok(answer) => answer,
                Err(_) if header.bot.is_some() => {
                    liveness.insert(row.attempt, Liveness::Unknown);
                    report.unresolved.push(run);
                    continue;
                }
                Err(error) => return Err(error),
            };
            self.trace(run, "restart", format_args!("{}", verdict.name()));
            let verdict = match (&verdict, &lookup) {
                // The host still lists the session but its process is gone:
                // nothing to reattach to, and the summary says how it ended.
                (RestartVerdict::Reattach, Some(CreationLookup::Known { session }))
                    if matches!(
                        session.process,
                        koru_session_protocol::ProcessState::Exited { .. }
                            | koru_session_protocol::ProcessState::Failed { .. }
                    ) =>
                {
                    RestartVerdict::ReconcileEnded
                }
                _ => verdict,
            };
            match verdict {
                RestartVerdict::Reattach => {
                    let session = match &lookup {
                        Some(CreationLookup::Known { session }) => session.clone(),
                        _ => {
                            liveness.insert(row.attempt, Liveness::Unknown);
                            report.unresolved.push(run);
                            continue;
                        }
                    };
                    liveness.insert(row.attempt, Liveness::Live);
                    match self.reattach(&row, session.id, session.managed, session.active_turn_id) {
                        Ok(started) => report.reattached.push(started),
                        Err(error) => {
                            tracing::warn!(target: "koru::delegate", run = ?run, %error, "the attempt could not be reattached");
                            liveness.insert(row.attempt, Liveness::Unknown);
                            report.unresolved.push(run);
                        }
                    }
                }
                RestartVerdict::WaitThenReask => {
                    liveness.insert(row.attempt, Liveness::Unknown);
                    report.unresolved.push(run);
                }
                RestartVerdict::ReconcileEnded => {
                    liveness.insert(row.attempt, Liveness::Ended);
                    let summary = match &lookup {
                        Some(CreationLookup::KnownEnded { session })
                        | Some(CreationLookup::Known { session }) => Some(session.clone()),
                        _ => None,
                    };
                    self.reconcile_ended(&row, summary.map(|s| (s.managed, s.active_turn_id)))?;
                    report.reconciled.push(run);
                }
                RestartVerdict::Interrupted { prior, .. } => {
                    self.interrupt(&row, prior, verdict.records_as())?;
                    let ended = if header.bot.is_some() {
                        self.ports
                            .store
                            .current_attempt(run)?
                            .is_some_and(|current| current.retired)
                    } else {
                        true
                    };
                    liveness.insert(
                        row.attempt,
                        if ended {
                            Liveness::Ended
                        } else {
                            Liveness::Unknown
                        },
                    );
                    report.interrupted.push(run);
                }
            }
        }

        // P1.5: the registry, rebuilt. Unknown is never free.
        let (claims, claims_report) =
            ScopeClaims::reconcile(&Recorded(recorded), self.ports.roots.as_ref(), |attempt| {
                liveness.get(&attempt).copied().unwrap_or(Liveness::Unknown)
            })?;
        self.claims = claims;
        report.claims = claims_report;

        // A run that exists and never started. Inspect every Created intent
        // before starting any of them: a claimed scheduled run has precedence
        // over a manual Created run regardless of SQLite/document order.
        let mut scheduled = Vec::new();
        let mut manual = Vec::new();
        for record in self.ports.store.intents()? {
            if record.state != IntentState::Satisfied
                || self.ports.store.current_attempt(record.run)?.is_some()
            {
                continue;
            }
            if let Ok(RunStatus::Created) = self.status(record.run) {
                let header = match self.header(record.run) {
                    Ok(header) => header,
                    Err(error) => {
                        tracing::warn!(target: "koru::delegate", run = ?record.run, %error, "a created run could not be opened after restart");
                        continue;
                    }
                };
                if header.bot.is_some() {
                    continue;
                }
                match header.origin {
                    koru_delegate_core::RunOrigin::Manual => {
                        manual.push((record.run, header.scope))
                    }
                    koru_delegate_core::RunOrigin::Scheduled { schedule, .. } => {
                        if !report
                            .pending_scheduled_scopes
                            .iter()
                            .any(|reservation| reservation.run == record.run)
                        {
                            report.pending_scheduled_scopes.push(
                                self.scheduled_scope_reservation(record.run, header.scope.clone()),
                            );
                        }
                        scheduled.push((record.run, header.scope, schedule));
                    }
                }
            }
        }

        // Claimed scheduled starts go first. A successful start establishes a
        // live ScopeClaim and no longer needs the pre-attempt reservation; a
        // retryable failure keeps the reservation for the Actor/scheduler.
        for (run, authored_scope, schedule) in scheduled {
            let started = match self.plan_scope(run, &authored_scope) {
                Ok(scope) => self.start_claimed(
                    run,
                    scope,
                    StartOrigin::ClaimedSchedule {
                        schedule: ScheduleRef::from_uuid(schedule.as_uuid()),
                    },
                ),
                Err(_) => Err(crate::PortError::new(
                    crate::Port::Scope,
                    "claimed_scope_resolve",
                    "the claimed scope could not be resolved",
                )
                .into()),
            };
            match started {
                Ok(started) => {
                    report
                        .pending_scheduled_scopes
                        .retain(|reservation| reservation.run != run);
                    report.started.push(started);
                }
                Err(error) => {
                    if !matches!(self.status(run), Ok(RunStatus::Created))
                        || !matches!(self.ports.store.current_attempt(run), Ok(None))
                    {
                        report
                            .pending_scheduled_scopes
                            .retain(|reservation| reservation.run != run);
                    }
                    tracing::warn!(target: "koru::delegate", run = ?run, %error, "a claimed scheduled run could not be started after restart");
                }
            }
        }

        // Manual Created runs retain the ordinary restart behavior, but do
        // not pass an unsettled scheduled reservation. The Actor installs
        // those reservations as soon as this report returns.
        for (run, authored_scope) in manual {
            let scope = match self.plan_scope(run, &authored_scope) {
                Ok(scope) => scope,
                Err(_) => {
                    tracing::warn!(target: "koru::delegate", run = ?run, "a manual created run's scope could not be resolved after restart");
                    continue;
                }
            };
            let reserved = report.pending_scheduled_scopes.iter().any(|reservation| {
                let reserved_identity = match reservation.resolution {
                    ScopeReservationResolution::Exact => reservation.identity.clone(),
                    ScopeReservationResolution::Refresh => self
                        .plan_scope_identity(reservation.run, &reservation.scope)
                        .ok(),
                };
                match reserved_identity {
                    Some(identity) => scope.identity.overlap(&identity).is_overlap(),
                    None => {
                        authored_scope.project().is_some()
                            && authored_scope.project() == reservation.scope.project()
                    }
                }
            });
            if reserved {
                tracing::warn!(target: "koru::delegate", run = ?run, "a manual created run waits behind a claimed scheduled scope after restart");
                report.deferred_manual_starts.push(run);
                continue;
            }
            match self.start_resolved(run, scope, StartOrigin::Manual) {
                Ok(started) => report.started.push(started),
                Err(error) => {
                    tracing::warn!(target: "koru::delegate", run = ?run, %error, "a manual created run could not be started after restart");
                }
            }
        }
        Ok(report)
    }

    fn scheduled_scope_reservation(
        &self,
        run: koru_delegate_core::RunId,
        scope: WorkspaceScope,
    ) -> ScheduledScopeReservation {
        let identity = if matches!(scope, WorkspaceScope::Temporary) {
            self.ports
                .roots
                .planned_temporary(run)
                .ok()
                .map(|resolved| resolved.identity)
        } else {
            koru_delegate_scope::ResolvedScope::resolve(&scope, run, self.ports.roots.as_ref())
                .ok()
                .map(|resolved| resolved.identity)
        };
        ScheduledScopeReservation {
            run,
            scope,
            identity,
            resolution: ScopeReservationResolution::Refresh,
        }
    }

    /// The table, with the wait it names: a reservation in flight is
    /// re-asked through the `Sleep` seam, a bounded number of times.
    fn classify(
        &mut self,
        row: &AttemptRow,
    ) -> Result<(RestartVerdict, Option<CreationLookup>), RunError> {
        let Some(key) = row.provider_session else {
            // Never asked a host for anything: nothing can be running.
            let host = self.ports.sessions.host_instance()?;
            return Ok((
                classify_at_restart(row, LookupAnswer::NoEvidence, host),
                None,
            ));
        };
        let mut asked = 0;
        loop {
            let (host, lookup) = self.ports.sessions.lookup(key.idempotency)?;
            let answer = match &lookup {
                CreationLookup::Known { .. } => LookupAnswer::Known,
                CreationLookup::Creating => LookupAnswer::Creating,
                CreationLookup::KnownEnded { .. } => LookupAnswer::KnownEnded,
                CreationLookup::NoEvidence => LookupAnswer::NoEvidence,
            };
            let verdict = classify_at_restart(row, answer, host);
            if verdict == RestartVerdict::WaitThenReask && asked < self.options.reask_attempts {
                asked += 1;
                self.ports.sleep.sleep(self.options.reask_delay);
                continue;
            }
            return Ok((verdict, Some(lookup)));
        }
    }

    /// *Known*: the session is live. Reacquire it, mark the gap, reconcile
    /// the turn, and hand the host the stream from the cursor forward.
    fn reattach(
        &mut self,
        row: &AttemptRow,
        session: koru_delegate_core::SessionId,
        managed: Option<ManagedState>,
        active_turn: Option<koru_delegate_core::AgentTurnId>,
    ) -> Result<Started, RunError> {
        let run = row.run;
        let mut attempt = Attempt::new(row.attempt, row.generation);
        let provenance = self.header(run)?.bot;
        attempt.is_bot = provenance.is_some();
        let is_bot = attempt.is_bot;
        let bot_send = if is_bot {
            // Reattachment consumes retained authority, not the cheap execution
            // projection. Decode every Bot version so a JSON-only downgrade
            // cannot reissue authority or recover a provider under v1.
            let admitted = self
                .bot_ledger()?
                .bot_admission(run)?
                .ok_or_else(|| super::bots::refused("bot_fenced"))?;
            if provenance.as_ref() != Some(&admitted.submission.provenance) {
                return Err(super::bots::refused("bot_provenance_mismatch"));
            }
            self.linked_routine_admission(&admitted)?;
            admitted.initial_send
        } else {
            None
        };
        let bot_turn = bot_send.as_ref().and_then(|send| send.turn);
        attempt.session = Some(session);
        // A session summary proves activity, not who asked for it. Read only
        // a bounded window of existing content-free steps for positive ownership.
        let timeline = self.open(run)?.doc.document()?.timeline_window(0, 256);
        if let Some(turn) = active_turn {
            let originator = if is_bot {
                (bot_turn == Some(turn)).then_some(TurnOriginator::Orchestrator)
            } else {
                recovered_originator(&timeline, row.attempt, turn)
            };
            if originator == Some(TurnOriginator::Orchestrator) {
                attempt.orchestrator_turn = Some(turn);
            } else {
                attempt.recovered_unowned_turn = Some(turn);
            }
            if let Some(originator) = originator {
                attempt.turns.insert(turn, TurnState::new(originator));
            }
        }
        self.open(run)?.attempt = Some(attempt);
        let status = self.status(run)?;
        if !matches!(status, RunStatus::Running { .. }) && (!is_bot || bot_turn.is_some()) {
            // The document says the provider was not up yet: the attempt
            // is `Starting` and the session exists, so it is up now.
            self.apply(run, &RunEvent::ProviderStarted)?;
        }
        if !is_bot || bot_turn.is_some() {
            self.ports
                .store
                .advance(run, row.generation, AttemptPhase::Running)?;
        }
        let observation = self.ports.sessions.observe(session)?;
        // An inactive summary can be paired with the latest observed terminal,
        // but only complete coverage and the same positive ownership proof
        // authorize recovery of that turn's approval identities.
        let recovery_turn = active_turn.or_else(|| {
            if !observation_covers_session(&observation, session) {
                return None;
            }
            use koru_session_protocol::AgentEvent;
            let latest = observation.events.iter().rev().find(|entry| {
                matches!(
                    entry.event,
                    AgentEvent::TurnStarted { .. }
                        | AgentEvent::TurnCompleted { .. }
                        | AgentEvent::TurnFailed { .. }
                        | AgentEvent::TurnCancelled { .. }
                )
            })?;
            let turn = match (&latest.event, managed) {
                (AgentEvent::TurnCompleted { turn_id }, Some(ManagedState::Completed))
                | (AgentEvent::TurnFailed { turn_id, .. }, Some(ManagedState::Failed)) => *turn_id,
                _ => return None,
            };
            (if is_bot {
                bot_turn == Some(turn)
            } else {
                recovered_originator(&timeline, row.attempt, turn)
                    == Some(TurnOriginator::Orchestrator)
            })
            .then_some(turn)
        });
        let clear_recovered_approval;
        {
            let attempt = self.attempt_mut(run)?;
            let mut saw_active_approval = false;
            attempt.approval_evidence_complete = observation_covers_session(&observation, session)
                && attempt.recovered_unowned_turn.is_none();
            for entry in &observation.events {
                match &entry.event {
                    koru_session_protocol::AgentEvent::ApprovalRequired {
                        turn_id,
                        approval_id,
                        ..
                    } if Some(*turn_id) == recovery_turn => {
                        saw_active_approval = true;
                        attempt.pending_approvals.insert(*approval_id, *turn_id);
                    }
                    koru_session_protocol::AgentEvent::ApprovalResolved {
                        turn_id,
                        approval_id,
                        ..
                    }
                    | koru_session_protocol::AgentEvent::ApprovalExpired {
                        turn_id,
                        approval_id,
                        ..
                    } if attempt.pending_approvals.get(approval_id) == Some(turn_id) => {
                        attempt.pending_approvals.remove(approval_id);
                    }
                    koru_session_protocol::AgentEvent::QuestionsRequired {
                        turn_id,
                        request_id,
                        ..
                    } if is_bot && Some(*turn_id) == recovery_turn => {
                        attempt.pending_questions.insert(*request_id, *turn_id);
                    }
                    koru_session_protocol::AgentEvent::QuestionsResolved { request_id, .. }
                        if is_bot =>
                    {
                        attempt.pending_questions.remove(request_id);
                    }
                    _ => {}
                }
            }
            // An old attention bit without retained identity is uncertainty,
            // not proof that all permissions resolved while we were absent.
            let approval_attention = status.attention().is_some_and(|attention| {
                attention.contains(koru_delegate_core::Attention::ApprovalRequired)
            });
            clear_recovered_approval = approval_attention
                && attempt.approval_evidence_complete
                && saw_active_approval
                && attempt.pending_approvals.is_empty();
            if approval_attention
                && attempt.pending_approvals.is_empty()
                && !clear_recovered_approval
            {
                attempt.approval_evidence_complete = false;
            }
            attempt.release_owed = matches!(
                status.control(),
                Some(koru_delegate_core::ControlState::HandoverPending { .. })
            );
        }
        if clear_recovered_approval {
            // The complete normalized history proves every recovered request
            // closed. Reconcile the bit without inventing another reply fact.
            self.apply(run, &RunEvent::ApprovalResolved)?;
        }
        if !observation_covers_session(&observation, session) {
            // §7.1: *facts that cannot be recovered are shown as gaps*. The
            // host retained only a tail of the agent-event stream and says
            // so; what fell off it is gone, and how much is not a number
            // anybody has — the count stays `None` rather than invented.
            // A tail that survived intact lost nothing, and a gap nobody
            // suffered is noise on the timeline.
            self.step(
                run,
                StepKind::Gap { events_lost: None },
                TimelineRefs::new([TimelineRef::Attempt {
                    attempt: row.attempt,
                }])
                .expect("one ref"),
                Some(RunFact::Gap {
                    run,
                    attempt: row.attempt,
                    events_lost: None,
                }),
            )?;
        }
        // §7.4's crash between release and attach: the orchestrator
        // re-attaches when no controller exists, and when one does it
        // records the takeover it *discovers* — timestamped at discovery
        // and marked as such, because the moment the person actually took
        // the lease was never observed. Observation stays live either way,
        // so the timeline does not go blind while somebody types.
        let discovered = match self.ports.sessions.attach(session) {
            Ok(()) => {
                self.attempt_mut(run)?.holds_lease = true;
                let recovered = self.attempt_mut(run)?;
                let may_return = recovered.recovered_unowned_turn.is_none()
                    && (recovered.orchestrator_turn.is_some()
                        || recovered.approval_evidence_complete);
                if may_return
                    && matches!(
                        status.control(),
                        Some(koru_delegate_core::ControlState::Human { .. })
                    )
                {
                    let returned = self.apply(run, &RunEvent::ControlReturned)?;
                    self.step(
                        run,
                        StepKind::ControlReturned,
                        TimelineRefs::new([TimelineRef::Attempt {
                            attempt: row.attempt,
                        }])
                        .expect("one ref"),
                        Some(RunFact::ControlReturned {
                            run,
                            attempt: row.attempt,
                        }),
                    )?;
                    if returned.is_terminal() {
                        self.terminal(run, &returned)?;
                        return Ok(Started {
                            run,
                            attempt: row.attempt,
                            generation: row.generation,
                            status: returned,
                            events: None,
                        });
                    }
                }
                false
            }
            Err(error) if error.code == CONTROLLER_CONFLICT => {
                self.attempt_mut(run)?.holds_lease = false;
                if is_bot && matches!(status, RunStatus::Starting) {
                    self.bot_mark_started(run)?;
                }
                if !matches!(
                    status.control(),
                    Some(koru_delegate_core::ControlState::Human { .. })
                ) {
                    self.apply(run, &RunEvent::ControlTaken)?;
                    self.step(
                        run,
                        StepKind::ControlTaken { discovered: true },
                        TimelineRefs::new([TimelineRef::Attempt {
                            attempt: row.attempt,
                        }])
                        .expect("one ref"),
                        Some(RunFact::ControlTaken {
                            run,
                            attempt: row.attempt,
                            discovered: true,
                        }),
                    )?;
                    self.trace(run, "takeover_discovered", format_args!("at reattach"));
                }
                true
            }
            Err(error) => return Err(error.into()),
        };
        self.release_at_approval(run)?;
        let events = self
            .ports
            .sessions
            .subscribe(session, Some(observation.cursor))?;
        // The turn that ended while nobody watched. Not while a person
        // holds the session: an orchestrator turn cannot end a run that
        // is under human control (§7.5), and the completion this device
        // did not see is theirs to finish.
        if !is_bot
            && active_turn.is_none()
            && !discovered
            && (recovery_turn.is_some()
                || !matches!(
                    status.control(),
                    Some(koru_delegate_core::ControlState::Human { .. })
                ))
            && self.attempt_mut(run)?.approval_evidence_complete
        {
            match managed {
                Some(ManagedState::Completed) => {
                    let registered = self.ports.registrations.registered(run, row.attempt, None);
                    let artifacts_registered = u32::try_from(registered.len()).unwrap_or(u32::MAX);
                    let status = self.apply(
                        run,
                        &RunEvent::TurnCompleted {
                            originator: TurnOriginator::Orchestrator,
                            artifacts_registered,
                        },
                    )?;
                    self.step(
                        run,
                        StepKind::TurnCompleted {
                            originator: TurnOriginator::Orchestrator,
                            artifacts_registered,
                        },
                        TimelineRefs::new([TimelineRef::Attempt {
                            attempt: row.attempt,
                        }])
                        .expect("one ref"),
                        Some(RunFact::TurnCompleted {
                            run,
                            attempt: row.attempt,
                            turn: recovery_turn
                                .unwrap_or_else(koru_delegate_core::AgentTurnId::mint),
                            originator: TurnOriginator::Orchestrator,
                            artifacts_registered,
                        }),
                    )?;
                    if status.is_terminal() {
                        self.terminal(run, &status)?;
                        return Ok(Started {
                            run,
                            attempt: row.attempt,
                            generation: row.generation,
                            status,
                            events: None,
                        });
                    }
                }
                Some(ManagedState::Failed) => {
                    let status = self.apply(
                        run,
                        &RunEvent::TurnFailed {
                            originator: TurnOriginator::Orchestrator,
                            stage: koru_delegate_core::FailureStage::Turn,
                        },
                    )?;
                    self.terminal(run, &status)?;
                    return Ok(Started {
                        run,
                        attempt: row.attempt,
                        generation: row.generation,
                        status,
                        events: None,
                    });
                }
                _ => {}
            }
        }
        if is_bot {
            if active_turn.is_none() {
                self.attempt_mut(run)?.bot_terminal_turn = recovery_turn;
            }
            self.bot_refresh_active_clock(run)?;
        }
        Ok(Started {
            run,
            attempt: row.attempt,
            generation: row.generation,
            status: self.status(run)?,
            events: Some(events),
        })
    }

    /// *Known-ended*: the host retained the session's end. A turn that
    /// completed is a completion; anything else ended the attempt.
    fn reconcile_ended(
        &mut self,
        row: &AttemptRow,
        summary: Option<(
            Option<ManagedState>,
            Option<koru_delegate_core::AgentTurnId>,
        )>,
    ) -> Result<(), RunError> {
        let run = row.run;
        let mut attempt = Attempt::new(row.attempt, row.generation);
        attempt.is_bot = self.header(run)?.bot.is_some();
        attempt.session = row.provider_session.and_then(|key| key.session);
        let is_bot = attempt.is_bot;
        self.open(run)?.attempt = Some(attempt);
        let status = self.status(run)?;
        if status.is_terminal() {
            self.end_attempt(run, AttemptPhase::Ended, false)?;
            return Ok(());
        }
        if let Some((Some(ManagedState::Completed), None)) = summary
            && !is_bot
            && matches!(status, RunStatus::Running { .. })
        {
            let registered = self.ports.registrations.registered(run, row.attempt, None);
            let artifacts_registered = u32::try_from(registered.len()).unwrap_or(u32::MAX);
            let status = self.apply(
                run,
                &RunEvent::TurnCompleted {
                    originator: TurnOriginator::Orchestrator,
                    artifacts_registered,
                },
            )?;
            if status.is_terminal() {
                self.terminal(run, &status)?;
                return Ok(());
            }
        }
        // The provider is gone and the objective is not done: the run is
        // interrupted (D10), and the ledger records what the host proved —
        // the session ended.
        self.session_lost_as(run, AttemptPhase::Ended)?;
        Ok(())
    }

    /// *No evidence*: nothing is running. `Interrupted` with what recovery
    /// could prove; a person decides about an unknown prior attempt.
    fn interrupt(
        &mut self,
        row: &AttemptRow,
        prior: PriorAttempt,
        phase: Option<AttemptPhase>,
    ) -> Result<(), RunError> {
        let run = row.run;
        let status = self.status(run)?;
        if self.header(run)?.bot.is_some() {
            let mut attempt = Attempt::new(row.attempt, row.generation);
            attempt.is_bot = true;
            attempt.session = row.provider_session.and_then(|key| key.session);
            self.open(run)?.attempt = Some(attempt);
            let ended = prior == PriorAttempt::Ended && self.bot_prove_end(run, false)?;
            if !status.is_terminal() {
                self.apply(
                    run,
                    &RunEvent::Interrupt {
                        prior_attempt: if ended {
                            PriorAttempt::Ended
                        } else {
                            PriorAttempt::Unknown
                        },
                    },
                )?;
            }
            if ended {
                self.bot_end_attempt(run, AttemptPhase::Ended, false)?;
            } else if !row.phase.is_terminal() {
                self.ports
                    .store
                    .advance(run, row.generation, AttemptPhase::Unknown)?;
            }
            self.arm_bot_cleanup(run)?;
            return Ok(());
        }
        if status.is_terminal() {
            self.ports
                .store
                .advance(run, row.generation, AttemptPhase::Ended)?;
            self.ports.store.retire(run, row.generation)?;
            return Ok(());
        }
        self.apply(
            run,
            &RunEvent::Interrupt {
                prior_attempt: prior,
            },
        )?;
        self.step(
            run,
            StepKind::Interrupted {
                prior_attempt: prior,
            },
            TimelineRefs::new([TimelineRef::Attempt {
                attempt: row.attempt,
            }])
            .expect("one ref"),
            Some(RunFact::Interrupted {
                run,
                attempt: row.attempt,
                generation: row.generation,
                prior_attempt: prior,
            }),
        )?;
        self.ports.store.advance(
            run,
            row.generation,
            phase.unwrap_or(AttemptPhase::Interrupted),
        )?;
        self.open(run)?.last_attempt = Some((row.attempt, row.generation));
        Ok(())
    }
}

/// No scan beyond the already bounded window: absence is not ownership.
fn recovered_originator(
    timeline: &[koru_delegate_document::TimelineItem],
    attempt: koru_delegate_core::RunAttemptId,
    turn: koru_delegate_core::AgentTurnId,
) -> Option<TurnOriginator> {
    for item in timeline {
        let Some(step) = item.step() else { continue };
        if !step
            .refs
            .iter()
            .any(|r| matches!(r, TimelineRef::Attempt { attempt: id } if *id == attempt))
            || !step
                .refs
                .iter()
                .any(|r| matches!(r, TimelineRef::Turn { turn: id } if *id == turn))
        {
            continue;
        }
        match step.kind {
            StepKind::TurnStarted { originator } => return Some(originator),
            StepKind::TurnCompleted { .. }
            | StepKind::TurnFailed { .. }
            | StepKind::TurnCancelled { .. } => return None,
            _ => {}
        }
    }
    None
}

/// The stream starts at `AGENT_EVENT_INITIAL_CURSOR`, before its first event
/// (`AGENT_EVENT_FIRST_SEQUENCE`). An oversized event can empty the replay
/// without setting earlier_truncated.
fn observation_covers_session(
    observation: &koru_session_protocol::AgentEventObservation,
    session: koru_delegate_core::SessionId,
) -> bool {
    if observation.earlier_truncated {
        return false;
    }
    let mut sequence = koru_session_protocol::AGENT_EVENT_INITIAL_CURSOR;
    for entry in &observation.events {
        let Some(next) = sequence.checked_add(1) else {
            return false;
        };
        if entry.session_id != session || entry.sequence.get() != next {
            return false;
        }
        sequence = next;
    }
    observation.cursor.get() == sequence
}
