// SPDX-License-Identifier: MPL-2.0

//! Pure schedule planning and the effects coordinator that consumes it.

use std::collections::HashMap;

use koru_core::time::ZoneRules;
use koru_delegate_core::{DeviceId, DocumentId, RunId, ScheduleId, WorkspaceId};
use koru_delegate_document::{ScheduleClaimPlan, ScheduleDocumentError, ScheduleWrite};
use koru_delegate_schedule::{
    DelegationTemplate, RecurrenceRule, ScheduleContextFailureStage, ScheduleDefinition,
    ScheduleFiring, ScheduleFiringOutcome, ScheduleHeader, ScheduleLifecycle, ScheduleName,
    SchedulePauseReason, ScheduleState,
};
use koru_delegate_scope::{ResolvedScope, ScheduleRef, ScopeIdentity, StartOrigin};
use koru_delegate_sqlite::{SlotClaim, SlotOutcome, SlotState};
use koru_recurrence::{EvaluationError, OCCURRENCE_PRECISION_MS, OccurrenceTiming};

use crate::{
    CaptureStage, ListedSchedule, NoScheduleEvents, Orchestrator, Port, PortError, PortResult,
    PreparedScheduleEvent, ScheduleClock, ScheduleDocumentHandle, ScheduleDocuments,
    ScheduleEventMaintenance, ScheduleEventRef, ScheduleEvents, ScheduleLedger,
    ScheduleOccurrenceEvent, SchedulePreparation, Started,
};

mod routines;

/// One bounded action from an evaluation. A month of missed minute slots is
/// represented by one `Missed` action whose durable observation cursor skips
/// the old range. A currently due minute precedes that representative miss,
/// so a crash between the two can never suppress runnable work.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlannedAction {
    Missed { occurrence_ms: i64 },
    Due { occurrence_ms: i64 },
}

/// Everything the worker needs from one pure evaluation.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SchedulePlan {
    /// At most one representative miss and one due firing.
    pub actions: Vec<PlannedAction>,
    /// The next future occurrence, if the rule has one.
    pub next_deadline_ms: Option<i64>,
}

/// Evaluate one active definition without reading a clock or mutating state.
///
/// `observed_through_ms` is the maximum of the local ledger and synced
/// schedule history. The definition's effective time is inclusive, while
/// recurrence exploration is strictly-after, hence the saturated `- 1`.
pub fn plan_schedule<R: ZoneRules + ?Sized>(
    definition: &ScheduleDefinition,
    observed_through_ms: Option<i64>,
    now_ms: i64,
    floating_zone: &R,
) -> Result<SchedulePlan, EvaluationError> {
    let effective_cursor = definition.effective_at_ms.saturating_sub(1);
    let cursor = observed_through_ms
        .map(|observed| observed.max(effective_cursor))
        .unwrap_or(effective_cursor);
    let Some(first) = definition.recurrence.next_after(cursor, floating_zone)? else {
        return Ok(SchedulePlan::default());
    };

    match first.timing_at(now_ms) {
        OccurrenceTiming::Upcoming => Ok(SchedulePlan {
            actions: Vec::new(),
            next_deadline_ms: Some(first.instant_ms()),
        }),
        OccurrenceTiming::Due => Ok(SchedulePlan {
            actions: vec![PlannedAction::Due {
                occurrence_ms: first.instant_ms(),
            }],
            next_deadline_ms: definition
                .recurrence
                .next_after(first.instant_ms(), floating_zone)?
                .map(|occurrence| occurrence.instant_ms()),
        }),
        OccurrenceTiming::Missed => {
            let mut actions = Vec::with_capacity(2);

            // A strict exploration from one precision unit before `now`
            // jumps directly to the current minute rather than enumerating
            // every old occurrence. It also preserves a due slot while the
            // representative miss advances the observation cursor.
            let current_minute = now_ms.saturating_sub(now_ms.rem_euclid(OCCURRENCE_PRECISION_MS));
            let due_floor = current_minute.saturating_sub(1);
            let current = definition.recurrence.next_after(due_floor, floating_zone)?;
            if let Some(current) = current
                && current.timing_at(now_ms) == OccurrenceTiming::Due
            {
                actions.push(PlannedAction::Due {
                    occurrence_ms: current.instant_ms(),
                });
            }
            actions.push(PlannedAction::Missed {
                occurrence_ms: first.instant_ms(),
            });
            let next_reference = actions
                .iter()
                .filter_map(|action| match action {
                    PlannedAction::Due { occurrence_ms } => Some(*occurrence_ms),
                    PlannedAction::Missed { .. } => None,
                })
                .max()
                .unwrap_or(now_ms);
            Ok(SchedulePlan {
                actions,
                next_deadline_ms: definition
                    .recurrence
                    .next_after(next_reference, floating_zone)?
                    .map(|occurrence| occurrence.instant_ms()),
            })
        }
    }
}

/// Effects produced by one background evaluation.
#[derive(Default)]
pub struct ScheduleEvaluation {
    pub started: Vec<Started>,
    pub next_deadline_ms: Option<i64>,
    pub evaluated: u32,
    /// Per-schedule failures isolated so one bad document cannot starve the
    /// rest of the catalog. Content-free; the host logs only port/code.
    pub failures: u32,
    /// One owned candidate, consumed after the host releases its ordinary gate.
    pub routine_work: Option<crate::RoutineWork>,
}

/// Canonical read model produced on the scheduler thread. Public wire mapping
/// belongs to the host, while recurrence/ledger authority stays here.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScheduleListing {
    pub workspace: WorkspaceId,
    pub document: DocumentId,
    pub name: ScheduleName,
    pub header: ScheduleHeader,
    pub state: ScheduleState,
    pub next: ScheduleNext,
    pub firings: Vec<ScheduleFiring>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScheduleList {
    pub schedules: Vec<ScheduleListing>,
    /// Content-free honesty bit for a partial Store catalog. One unreadable
    /// schedule must not hide the healthy schedules beside it.
    pub incomplete: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScheduleNext {
    None,
    Exact(i64),
    OwnerLocal,
}

#[derive(Clone, Copy)]
struct DueFiring {
    workspace: koru_delegate_core::WorkspaceId,
    document: DocumentId,
    occurrence_ms: i64,
    backlog_missed: Option<i64>,
    evaluated_at_ms: i64,
}

#[derive(Clone, Copy)]
struct DueSkip {
    schedule: ScheduleEventRef,
    occurrence_ms: i64,
    run: RunId,
    outcome: ScheduleFiringOutcome,
    busy_holder: Option<RunId>,
    backlog_missed: Option<i64>,
    claimed_at_ms: i64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScheduleAdmission {
    Busy(RunId),
    Granted,
}

/// The scheduler-to-run-actor seam. `begin` is the short admission
/// arbitration. The scheduler then holds the lease while it consumes the live
/// authority into the atomic claim+intent write; `finish` enters the existing
/// background run executor and may perform ordinary run launch I/O. The actor
/// defers manual scope-claiming commands until `finish` or `abort` releases the
/// lease. None of this runs on a socket connection thread.
pub trait ScheduleEffects {
    /// Routine arbitration needs the actor's acknowledged lease owner. Ordinary
    /// direct Orchestrator implementations deliberately cannot grant this phase.
    fn begin_routine(
        &mut self,
        _prepared: &koru_delegate_core::RoutinePrepared,
        _admission: &koru_delegate_core::BotAdmissionBarrier,
        _proof: &koru_delegate_core::FreshBotPolicyProof,
        _scope: ScopeIdentity,
        _authored_scope: koru_delegate_core::WorkspaceScope,
        _deadline: std::time::Instant,
    ) -> PortResult<ScheduleAdmission> {
        Err(PortError::new(
            Port::Schedules,
            "routine_actor_required",
            "Routine arbitration requires the actor",
        ))
    }
    fn release_routine(&mut self, _run: RunId, _deadline: std::time::Instant) -> PortResult<()> {
        Err(PortError::new(
            Port::Schedules,
            "routine_actor_required",
            "Routine release requires the actor",
        ))
    }
    fn begin(
        &mut self,
        run: RunId,
        scope: ScopeIdentity,
        authored_scope: koru_delegate_core::WorkspaceScope,
    ) -> PortResult<ScheduleAdmission>;
    fn finish(
        &mut self,
        run: RunId,
        claim: SlotClaim,
        scope: koru_delegate_scope::ResolvedScope,
        authored_scope: koru_delegate_core::WorkspaceScope,
    ) -> PortResult<Option<Started>>;
    fn abort(&mut self, run: RunId);
    fn reconcile(
        &mut self,
        claim: SlotClaim,
        authored_scope: koru_delegate_core::WorkspaceScope,
    ) -> PortResult<Option<Started>>;
}

/// The background schedule effects owner. Discovery, recurrence, provider
/// readiness, filesystem resolution and context capture all happen through
/// its own ports on the scheduler thread. It never owns the user-facing
/// orchestrator.
pub struct ScheduleCoordinator {
    schedules: Box<dyn ScheduleDocuments>,
    ledger: Box<dyn ScheduleLedger>,
    preparation: Box<dyn SchedulePreparation>,
    clock: Box<dyn ScheduleClock>,
    events: Box<dyn ScheduleEvents>,
    claimed_scopes: HashMap<RunId, (ResolvedScope, koru_delegate_core::WorkspaceScope)>,
    device: DeviceId,
    routines: Option<routines::Routines>,
}

impl ScheduleCoordinator {
    pub fn new(
        schedules: Box<dyn ScheduleDocuments>,
        ledger: Box<dyn ScheduleLedger>,
        preparation: Box<dyn SchedulePreparation>,
        clock: Box<dyn ScheduleClock>,
        device: DeviceId,
    ) -> Self {
        Self {
            schedules,
            ledger,
            preparation,
            clock,
            events: Box::new(NoScheduleEvents),
            claimed_scopes: HashMap::new(),
            device,
            routines: None,
        }
    }

    /// Shipping composition installs the durable schedule-event adapter;
    /// pure scheduler tests keep the no-op port unless they need to assert
    /// ordering explicitly.
    pub fn with_events(mut self, events: Box<dyn ScheduleEvents>) -> Self {
        self.events = events;
        self
    }

    pub fn maintain_events(&mut self) -> PortResult<ScheduleEventMaintenance> {
        self.events.maintain()
    }

    pub fn reconcile_prepared_events(
        &mut self,
        limit: usize,
    ) -> PortResult<ScheduleEventMaintenance> {
        self.events.reconcile_prepared(limit)
    }

    pub fn reconcile_created_event(&mut self, schedule: ScheduleId) -> PortResult<()> {
        self.events.reconcile_created(schedule)
    }

    #[cfg(test)]
    pub(crate) fn has_claimed_scope(&self, run: RunId) -> bool {
        self.claimed_scopes.contains_key(&run)
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn fire_due_for_test(
        &mut self,
        effects: &mut dyn ScheduleEffects,
        handle: &mut dyn ScheduleDocumentHandle,
        workspace: koru_delegate_core::WorkspaceId,
        document: DocumentId,
        occurrence_ms: i64,
        evaluated_at_ms: i64,
        report: &mut ScheduleEvaluation,
    ) -> PortResult<bool> {
        self.fire_due(
            effects,
            handle,
            DueFiring {
                workspace,
                document,
                occurrence_ms,
                backlog_missed: None,
                evaluated_at_ms,
            },
            report,
        )
    }

    pub fn schedules_mut(&mut self) -> &mut dyn ScheduleDocuments {
        self.schedules.as_mut()
    }

    /// Create through the same scheduler-owned Store adapter used by
    /// evaluation. A replay of the client-minted id reads back the canonical
    /// stored document rather than echoing possibly different retry input.
    pub fn create_schedule<R: ZoneRules + ?Sized>(
        &mut self,
        workspace: WorkspaceId,
        schedule: ScheduleId,
        template: DelegationTemplate,
        recurrence: RecurrenceRule,
        now_ms: i64,
        floating_zone: &R,
    ) -> PortResult<ScheduleListing> {
        let name = ScheduleName::from_objective(&template.objective);
        self.create_named_schedule(
            workspace,
            schedule,
            name,
            template,
            recurrence,
            now_ms,
            floating_zone,
        )
    }

    pub fn create_named_schedule<R: ZoneRules + ?Sized>(
        &mut self,
        workspace: WorkspaceId,
        schedule: ScheduleId,
        name: ScheduleName,
        template: DelegationTemplate,
        recurrence: RecurrenceRule,
        now_ms: i64,
        floating_zone: &R,
    ) -> PortResult<ScheduleListing> {
        let active = self.schedules.discover_active()?;
        if !active.workspaces.contains(&workspace) {
            return Err(PortError::new(
                Port::Schedules,
                "unknown_workspace",
                "the workspace is not active on this device",
            ));
        }
        let identities = self.schedules.discover_identities()?;
        if identities.failures != 0 {
            return Err(PortError::new(
                Port::Schedules,
                "schedule_unavailable",
                "schedule identity cannot be proven from a partial catalog",
            ));
        }
        if let Some(existing) = identities
            .schedules
            .iter()
            .filter(|listed| listed.header.schedule == schedule)
            .min_by_key(|listed| (listed.workspace, listed.document))
        {
            if existing.workspace != workspace {
                return Err(PortError::new(
                    Port::Schedules,
                    "schedule_conflict",
                    "the schedule id is already bound to another workspace",
                ));
            }
            self.events
                .existing_created(schedule_event_ref(existing), &existing.header)?;
            return self.read_listing(workspace, existing.document, now_ms, floating_zone);
        }
        let header = ScheduleHeader {
            schedule,
            created_at_ms: now_ms,
            initial_owner: self.device,
            definition: ScheduleDefinition {
                routine: None,
                revision: 1,
                effective_at_ms: now_ms,
                template,
                recurrence,
                lifecycle: ScheduleLifecycle::Active,
            },
        };
        let expected = ScheduleEventRef {
            schedule,
            workspace,
            document: DocumentId::from_uuid(schedule.as_uuid()),
        };
        let prepared = self.events.prepare_created(expected, &header)?;
        let document = self.schedules.create_named(workspace, &name, &header)?;
        let listing = self.read_listing(workspace, document, now_ms, floating_zone)?;
        if document == expected.document && listing.header == header {
            self.events.ready(prepared)?;
        } else {
            // A deterministic Store identity race may have returned the
            // schedule another writer committed. The losing request must not
            // leave its envelope owning the ScheduleId.
            self.events.abandon(prepared)?;
            self.events.existing_created(
                ScheduleEventRef {
                    schedule: listing.header.schedule,
                    workspace,
                    document,
                },
                &listing.header,
            )?;
        }
        Ok(listing)
    }

    /// Rename one schedule through its stable identity. The document owns
    /// owner/retired/conflict checks; this coordinator proves the requested
    /// workspace before opening the Store handle.
    pub fn rename_schedule<R: ZoneRules + ?Sized>(
        &mut self,
        workspace: WorkspaceId,
        schedule: ScheduleId,
        expected_name: &ScheduleName,
        name: &ScheduleName,
        now_ms: i64,
        floating_zone: &R,
    ) -> PortResult<ScheduleListing> {
        let document = self.schedule_document(workspace, schedule)?;
        let mut handle = self.schedules.open(workspace, document)?;
        handle.absorb()?;
        let mut outcome = None;
        handle.commit(&mut |document| {
            outcome = Some(document.rename(self.device, expected_name, name)?);
            Ok(())
        })?;
        let _ = outcome.ok_or_else(|| {
            PortError::new(
                Port::Schedules,
                "schedule_rename",
                "the rename operation did not run",
            )
        })?;
        self.read_listing(workspace, document, now_ms, floating_zone)
    }

    /// Replace the editable schedule snapshot through one revision-checked
    /// document commit. Event preparation precedes that commit and readiness
    /// follows it, preserving the durable outbox ordering used by automatic
    /// pauses.
    #[allow(clippy::too_many_arguments)]
    pub fn edit_schedule<R: ZoneRules + ?Sized>(
        &mut self,
        workspace: WorkspaceId,
        schedule: ScheduleId,
        expected_revision: u32,
        expected_name: &ScheduleName,
        name: &ScheduleName,
        template: DelegationTemplate,
        recurrence: RecurrenceRule,
        now_ms: i64,
        floating_zone: &R,
    ) -> PortResult<ScheduleListing> {
        let document = self.schedule_document(workspace, schedule)?;
        let mut handle = self.schedules.open(workspace, document)?;
        handle.absorb()?;
        let current = handle.document().header()?.definition;
        if current.routine.is_some() {
            return Err(PortError::new(
                Port::Schedules,
                "bot_routine_unavailable",
                "Bot routine edits require their Bot execution owner.",
            ));
        }
        let successor = expected_revision
            .checked_add(1)
            .ok_or(ScheduleDocumentError::RevisionExhausted)?;
        let effective_at_ms = if current.revision == successor {
            current.effective_at_ms
        } else {
            now_ms.max(current.effective_at_ms.saturating_add(1))
        };
        let definition = ScheduleDefinition {
            routine: None,
            revision: successor,
            effective_at_ms,
            template,
            recurrence,
            lifecycle: current.lifecycle,
        };
        let reference = ScheduleEventRef {
            schedule,
            workspace,
            document,
        };
        let prepared = self.events.prepare_definition(reference, &definition)?;
        handle.commit(&mut |document| {
            document.edit(
                self.device,
                expected_revision,
                expected_name,
                name,
                &definition,
            )?;
            Ok(())
        })?;
        self.events.ready(prepared)?;
        self.read_listing(workspace, document, now_ms, floating_zone)
    }

    pub fn pause_schedule<R: ZoneRules + ?Sized>(
        &mut self,
        workspace: WorkspaceId,
        schedule: ScheduleId,
        expected_revision: u32,
        now_ms: i64,
        floating_zone: &R,
    ) -> PortResult<ScheduleListing> {
        self.change_lifecycle(
            workspace,
            schedule,
            expected_revision,
            ScheduleLifecycle::Paused {
                reason: SchedulePauseReason::Manual,
            },
            now_ms,
            floating_zone,
        )
    }

    pub fn resume_schedule<R: ZoneRules + ?Sized>(
        &mut self,
        workspace: WorkspaceId,
        schedule: ScheduleId,
        expected_revision: u32,
        now_ms: i64,
        floating_zone: &R,
    ) -> PortResult<ScheduleListing> {
        self.change_lifecycle(
            workspace,
            schedule,
            expected_revision,
            ScheduleLifecycle::Active,
            now_ms,
            floating_zone,
        )
    }

    fn change_lifecycle<R: ZoneRules + ?Sized>(
        &mut self,
        workspace: WorkspaceId,
        schedule: ScheduleId,
        expected_revision: u32,
        lifecycle: ScheduleLifecycle,
        now_ms: i64,
        floating_zone: &R,
    ) -> PortResult<ScheduleListing> {
        let document = self.schedule_document(workspace, schedule)?;
        let mut handle = self.schedules.open(workspace, document)?;
        handle.absorb()?;
        let current = handle.document().header()?.definition;
        if current.routine.is_some() && matches!(lifecycle, ScheduleLifecycle::Active) {
            return Err(PortError::new(
                Port::Schedules,
                "bot_routine_unavailable",
                "Bot routine activation requires its Bot execution owner.",
            ));
        }
        let successor = expected_revision
            .checked_add(1)
            .ok_or(ScheduleDocumentError::RevisionExhausted)?;
        let effective_at_ms = if current.revision == successor && current.lifecycle == lifecycle {
            current.effective_at_ms
        } else {
            now_ms.max(current.effective_at_ms.saturating_add(1))
        };
        let definition = ScheduleDefinition {
            routine: current.routine,
            revision: successor,
            effective_at_ms,
            template: current.template.clone(),
            recurrence: current.recurrence.clone(),
            lifecycle: lifecycle.clone(),
        };
        let prepared = self.events.prepare_definition(
            ScheduleEventRef {
                schedule,
                workspace,
                document,
            },
            &definition,
        )?;
        handle.commit(&mut |document| {
            document.set_lifecycle(
                self.device,
                expected_revision,
                lifecycle.clone(),
                effective_at_ms,
            )?;
            Ok(())
        })?;
        self.events.ready(prepared)?;
        self.read_listing(workspace, document, now_ms, floating_zone)
    }

    /// Permanently stop a schedule without touching its Runs or any workspace
    /// manifest. The hidden retired document remains the replicated authority.
    pub fn retire_schedule<R: ZoneRules + ?Sized>(
        &mut self,
        workspace: WorkspaceId,
        schedule: ScheduleId,
        now_ms: i64,
        floating_zone: &R,
    ) -> PortResult<ScheduleListing> {
        let document = self.schedule_document(workspace, schedule)?;
        let mut handle = self.schedules.open(workspace, document)?;
        handle.absorb()?;
        handle.commit(&mut |document| {
            document.retire(self.device, now_ms)?;
            Ok(())
        })?;
        self.read_listing(workspace, document, now_ms, floating_zone)
    }

    fn schedule_document(
        &mut self,
        workspace: WorkspaceId,
        schedule: ScheduleId,
    ) -> PortResult<DocumentId> {
        let active = self.schedules.discover_active()?;
        if !active.workspaces.contains(&workspace) {
            return Err(PortError::new(
                Port::Schedules,
                "unknown_workspace",
                "the workspace is not active on this device",
            ));
        }
        let identities = self.schedules.discover_identities()?;
        if identities.failures != 0 {
            return Err(PortError::new(
                Port::Schedules,
                "schedule_unavailable",
                "schedule identity cannot be proven from a partial catalog",
            ));
        }
        let existing = identities
            .schedules
            .iter()
            .filter(|listed| listed.header.schedule == schedule)
            .min_by_key(|listed| (listed.workspace, listed.document))
            .ok_or_else(|| {
                PortError::new(
                    Port::Schedules,
                    "schedule_not_found",
                    "the schedule does not exist",
                )
            })?;
        if existing.workspace != workspace {
            return Err(PortError::new(
                Port::Schedules,
                "schedule_wrong_workspace",
                "the schedule belongs to another workspace",
            ));
        }
        Ok(existing.document)
    }

    /// Read one workspace without letting public callers race the evaluator's
    /// document links or local claim ledger.
    pub fn list_schedules<R: ZoneRules + ?Sized>(
        &mut self,
        workspace: WorkspaceId,
        now_ms: i64,
        floating_zone: &R,
    ) -> PortResult<ScheduleList> {
        let catalog = self.schedules.discover_active()?;
        if !catalog.workspaces.contains(&workspace) {
            return Err(PortError::new(
                Port::Schedules,
                "unknown_workspace",
                "the workspace is not active on this device",
            ));
        }
        let mut incomplete = catalog.failed_workspaces.contains(&workspace)
            || catalog
                .unavailable_documents
                .iter()
                .any(|(failed_workspace, _)| *failed_workspace == workspace);
        let canonical = canonical_schedule_locations(&catalog.schedules);
        let mut schedules = Vec::new();
        for listed in catalog.schedules.into_iter().filter(|listed| {
            listed.workspace == workspace
                && listed.trashed_at_ms.is_none()
                && canonical.get(&listed.header.schedule)
                    == Some(&(listed.workspace, listed.document))
        }) {
            match self.read_listing(workspace, listed.document, now_ms, floating_zone) {
                Ok(listed) => schedules.push(listed),
                Err(_) => incomplete = true,
            }
        }
        Ok(ScheduleList {
            schedules,
            incomplete,
        })
    }

    fn read_listing<R: ZoneRules + ?Sized>(
        &mut self,
        workspace: WorkspaceId,
        document: DocumentId,
        now_ms: i64,
        floating_zone: &R,
    ) -> PortResult<ScheduleListing> {
        let mut handle = self.schedules.open(workspace, document)?;
        handle.absorb()?;
        let header = handle.document().header()?;
        let name = handle.document().name()?;
        let state = handle.document().state()?;
        let firings = handle.document().firings()?;
        let next = match &state {
            ScheduleState::Active { owner } => {
                if matches!(
                    header.definition.recurrence,
                    RecurrenceRule::Floating { .. }
                ) && *owner != self.device
                {
                    ScheduleNext::OwnerLocal
                } else {
                    let observed = max_option(
                        handle.document().observed_through_ms()?,
                        self.ledger.observed_through_ms(header.schedule)?,
                    );
                    let plan = plan_schedule(&header.definition, observed, now_ms, floating_zone)
                        .map_err(|_| {
                        PortError::new(Port::Schedules, "recurrence", "evaluation failed")
                    })?;
                    match plan.actions.iter().find_map(|action| match action {
                        PlannedAction::Due { occurrence_ms } => Some(*occurrence_ms),
                        PlannedAction::Missed { .. } => None,
                    }) {
                        Some(at_ms) => ScheduleNext::Exact(at_ms),
                        None => plan
                            .next_deadline_ms
                            .map(ScheduleNext::Exact)
                            .unwrap_or(ScheduleNext::None),
                    }
                }
            }
            ScheduleState::Paused { .. }
            | ScheduleState::HandoffPending { .. }
            | ScheduleState::Retired => ScheduleNext::None,
        };
        Ok(ScheduleListing {
            workspace,
            document,
            name,
            header,
            state,
            next,
            firings,
        })
    }

    /// Reconcile durable claims first, then evaluate current authority. A
    /// claimed launch completes even if its schedule was edited, paused,
    /// retired, or handed off after the claim; those changes govern only
    /// later occurrences.
    pub fn evaluate<R: ZoneRules + ?Sized>(
        &mut self,
        effects: &mut dyn ScheduleEffects,
        now_ms: i64,
        floating_zone: &R,
    ) -> PortResult<ScheduleEvaluation> {
        crate::counters::schedule_evaluation();
        if let Some(routines) = &mut self.routines {
            routines.begin_evaluation(now_ms)?;
        }
        let evaluated = self.evaluate_inner(effects, now_ms, floating_zone);
        if evaluated.is_err() {
            crate::counters::schedule_evaluation_failed(1);
        }
        evaluated
    }

    fn evaluate_inner<R: ZoneRules + ?Sized>(
        &mut self,
        effects: &mut dyn ScheduleEffects,
        now_ms: i64,
        floating_zone: &R,
    ) -> PortResult<ScheduleEvaluation> {
        let mut report = ScheduleEvaluation::default();
        // A durable launch outranks every later authority change. Reconcile
        // outstanding claims before catalog discovery so a retired, moved or
        // temporarily unavailable schedule cannot suppress its claimed run.
        for claim in self.ledger.unsettled()? {
            let reconciled = (|| {
                let authored_scope = self.ledger.intent_scope(claim.run)?.ok_or_else(|| {
                    PortError::new(
                        Port::Store,
                        "missing_intent_scope",
                        "the claimed run has no authored scope",
                    )
                })?;
                let started = match self.claimed_scopes.get(&claim.run).cloned() {
                    Some((scope, cached_scope)) => {
                        let started = effects.finish(claim.run, claim, scope, cached_scope)?;
                        // Finish transfers launch ownership to the Actor. If
                        // history append or settlement fails below, the next
                        // pass must use reconciliation rather than replaying a
                        // consumed admission token.
                        self.claimed_scopes.remove(&claim.run);
                        started
                    }
                    None => effects.reconcile(claim, authored_scope)?,
                };
                self.ledger
                    .settle(claim.schedule, claim.occurrence_ms, SlotOutcome::Fired)?;
                Ok(started)
            })();
            match reconciled {
                Ok(Some(started)) => report.started.push(started),
                Ok(None) => {}
                Err(error) => record_failure(&mut report, now_ms, &error),
            }
        }

        let catalog = self.schedules.discover_active()?;
        if catalog.failures != 0 {
            crate::counters::schedule_evaluation_failed(u64::from(catalog.failures));
            report.failures = report.failures.saturating_add(catalog.failures);
            report.next_deadline_ms =
                min_option(report.next_deadline_ms, Some(now_ms.saturating_add(1_000)));
        }
        if let Some(mut routines) = self.routines.take() {
            let evaluated = self.evaluate_routine_catalog(
                &mut routines,
                catalog,
                effects,
                now_ms,
                floating_zone,
                &mut report,
            );
            self.routines = Some(routines);
            evaluated?;
            return Ok(report);
        }
        let canonical = canonical_schedule_locations(&catalog.schedules);
        for listed in catalog.schedules.into_iter().filter(|listed| {
            canonical.get(&listed.header.schedule) == Some(&(listed.workspace, listed.document))
        }) {
            crate::counters::schedule_document_evaluated();
            if let Err(error) =
                self.evaluate_one(listed, effects, now_ms, floating_zone, &mut report)
            {
                record_failure(&mut report, now_ms, &error);
            }
        }
        Ok(report)
    }

    fn evaluate_one<R: ZoneRules + ?Sized>(
        &mut self,
        listed: ListedSchedule,
        effects: &mut dyn ScheduleEffects,
        now_ms: i64,
        floating_zone: &R,
        report: &mut ScheduleEvaluation,
    ) -> PortResult<()> {
        let mut handle = self.schedules.open(listed.workspace, listed.document)?;
        handle.absorb()?;
        let authoritative_header = handle.document().header()?;
        if authoritative_header.definition.routine.is_some() {
            return Err(PortError::new(
                Port::Schedules,
                "bot_routine_unavailable",
                "Bot routines require their Bot execution owner.",
            ));
        }
        let schedule_ref = ScheduleEventRef {
            schedule: authoritative_header.schedule,
            workspace: listed.workspace,
            document: listed.document,
        };
        self.events
            .existing_created(schedule_ref, &authoritative_header)?;
        let schedule = authoritative_header.schedule;
        let document = handle.document();
        if !document.is_writable() {
            return Ok(());
        }
        document.firings()?;
        let header = document.header()?;
        let document_observed = document.observed_through_ms()?;
        for claim in self.ledger.claims_of(schedule)? {
            let known = handle
                .document()
                .firings()?
                .into_iter()
                .any(|firing| firing.occurrence_ms == claim.occurrence_ms);
            if !known {
                append_firing(handle.as_mut(), &claim, FiringAppend::Repair)?;
            }
        }
        if !handle.document().may_claim(self.device)? {
            return Ok(());
        }
        let observed = max_option(
            document_observed,
            self.ledger.observed_through_ms(schedule)?,
        );
        let plan = plan_schedule(&header.definition, observed, now_ms, floating_zone)
            .map_err(|_| PortError::new(Port::Schedules, "recurrence", "evaluation failed"))?;
        report.evaluated = report.evaluated.saturating_add(1);
        report.next_deadline_ms = min_option(report.next_deadline_ms, plan.next_deadline_ms);

        let backlog_missed = plan.actions.iter().find_map(|action| match action {
            PlannedAction::Missed { occurrence_ms } => Some(*occurrence_ms),
            PlannedAction::Due { .. } => None,
        });
        let has_due = plan
            .actions
            .iter()
            .any(|action| matches!(action, PlannedAction::Due { .. }));
        for action in plan.actions {
            match action {
                PlannedAction::Due { occurrence_ms } => {
                    if !self.fire_due(
                        effects,
                        handle.as_mut(),
                        DueFiring {
                            workspace: listed.workspace,
                            document: listed.document,
                            occurrence_ms,
                            backlog_missed,
                            evaluated_at_ms: now_ms,
                        },
                        report,
                    )? {
                        break;
                    }
                }
                PlannedAction::Missed { occurrence_ms } => {
                    if has_due {
                        continue;
                    }
                    if handle.absorb()? {
                        report.next_deadline_ms = Some(now_ms);
                        break;
                    }
                    let run = RunId::mint();
                    let claimed_at_ms = self.clock.now_ms(now_ms);
                    if let Some(authority) = handle.claim_authority(
                        self.device,
                        ScheduleClaimPlan::Skip {
                            occurrence_ms,
                            run,
                            outcome: ScheduleFiringOutcome::SkippedMissed,
                            busy_holder: None,
                        },
                    )? {
                        let proposed = ScheduleOccurrenceEvent {
                            schedule: schedule_ref,
                            revision: header.definition.revision,
                            occurrence_ms,
                            run,
                            outcome: ScheduleFiringOutcome::SkippedMissed,
                            busy_holder: None,
                            claimed_at_ms,
                        };
                        let prepared = self.events.prepare_occurrence(proposed)?;
                        let skipped = self.ledger.claim_skip(authority, claimed_at_ms)?;
                        let prepared = reconcile_occurrence_preparation(
                            self.events.as_mut(),
                            schedule_ref,
                            prepared,
                            &skipped.claim,
                        )?;
                        append_firing(handle.as_mut(), &skipped.claim, FiringAppend::Live)?;
                        ready_if_some(self.events.as_mut(), prepared)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn record_due_skip(
        &mut self,
        handle: &mut dyn ScheduleDocumentHandle,
        skip: DueSkip,
    ) -> PortResult<()> {
        let DueSkip {
            schedule,
            occurrence_ms,
            run,
            outcome,
            busy_holder,
            backlog_missed,
            claimed_at_ms,
        } = skip;
        let revision = handle.document().header()?.definition.revision;
        let Some(due) = handle.claim_authority(
            self.device,
            ScheduleClaimPlan::Skip {
                occurrence_ms,
                run,
                outcome,
                busy_holder,
            },
        )?
        else {
            return Ok(());
        };
        let due_prepared = self.events.prepare_occurrence(ScheduleOccurrenceEvent {
            schedule,
            revision,
            occurrence_ms,
            run,
            outcome,
            busy_holder,
            claimed_at_ms,
        })?;
        if let Some(missed_occurrence_ms) = backlog_missed {
            let missed_run = RunId::mint();
            let Some(missed) = handle.claim_authority(
                self.device,
                ScheduleClaimPlan::Skip {
                    occurrence_ms: missed_occurrence_ms,
                    run: missed_run,
                    outcome: ScheduleFiringOutcome::SkippedMissed,
                    busy_holder: None,
                },
            )?
            else {
                if let Some(prepared) = due_prepared {
                    self.events.abandon(prepared)?;
                }
                return Ok(());
            };
            let missed_prepared = self.events.prepare_occurrence(ScheduleOccurrenceEvent {
                schedule,
                revision,
                occurrence_ms: missed_occurrence_ms,
                run: missed_run,
                outcome: ScheduleFiringOutcome::SkippedMissed,
                busy_holder: None,
                claimed_at_ms,
            })?;
            let (due, missed) = self
                .ledger
                .claim_skip_with_missed(due, missed, claimed_at_ms)?;
            let missed_prepared = reconcile_occurrence_preparation(
                self.events.as_mut(),
                schedule,
                missed_prepared,
                &missed.claim,
            )?;
            let due_prepared = reconcile_occurrence_preparation(
                self.events.as_mut(),
                schedule,
                due_prepared,
                &due.claim,
            )?;
            append_firing(handle, &missed.claim, FiringAppend::Live)?;
            ready_if_some(self.events.as_mut(), missed_prepared)?;
            append_firing(handle, &due.claim, FiringAppend::Live)?;
            ready_if_some(self.events.as_mut(), due_prepared)?;
        } else {
            let due = self.ledger.claim_skip(due, claimed_at_ms)?;
            let due_prepared = reconcile_occurrence_preparation(
                self.events.as_mut(),
                schedule,
                due_prepared,
                &due.claim,
            )?;
            append_firing(handle, &due.claim, FiringAppend::Live)?;
            ready_if_some(self.events.as_mut(), due_prepared)?;
        }
        Ok(())
    }

    fn fire_due(
        &mut self,
        effects: &mut dyn ScheduleEffects,
        handle: &mut dyn ScheduleDocumentHandle,
        due: DueFiring,
        report: &mut ScheduleEvaluation,
    ) -> PortResult<bool> {
        let DueFiring {
            workspace,
            document,
            occurrence_ms,
            backlog_missed,
            evaluated_at_ms: now_ms,
        } = due;
        let header = handle.document().header()?;
        let authored_scope = header.definition.template.scope.clone();
        let profile_ready = self
            .preparation
            .profile_delegatable(&header.definition.template.agent)?;
        if !profile_ready {
            if refresh_before_pause(handle, report, now_ms)? {
                return Ok(false);
            }
            pause(
                handle,
                self.events.as_mut(),
                ScheduleEventRef {
                    schedule: header.schedule,
                    workspace,
                    document,
                },
                self.device,
                now_ms,
                SchedulePauseReason::ProviderNotDelegatable,
            )?;
            return Ok(false);
        }

        let run = RunId::mint();
        let scope = match self.preparation.resolve_scope(run, &authored_scope) {
            Ok(scope) => scope,
            Err(_) => {
                if refresh_before_pause(handle, report, now_ms)? {
                    return Ok(false);
                }
                pause(
                    handle,
                    self.events.as_mut(),
                    ScheduleEventRef {
                        schedule: header.schedule,
                        workspace,
                        document,
                    },
                    self.device,
                    now_ms,
                    SchedulePauseReason::ScopeUnavailable,
                )?;
                return Ok(false);
            }
        };

        let pinned = match self.preparation.resolve_and_pin(
            run,
            &header.definition.template.context,
            workspace,
        ) {
            Ok(pinned) => pinned,
            Err(failure) => {
                self.preparation.discard_candidate(run, &authored_scope)?;
                if refresh_before_pause(handle, report, now_ms)? {
                    return Ok(false);
                }
                pause(
                    handle,
                    self.events.as_mut(),
                    ScheduleEventRef {
                        schedule: header.schedule,
                        workspace,
                        document,
                    },
                    self.device,
                    now_ms,
                    SchedulePauseReason::ContextUnavailable {
                        item: u32::try_from(failure.item).unwrap_or(u32::MAX),
                        stage: context_stage(failure.stage),
                    },
                )?;
                return Ok(false);
            }
        };
        let admission = match effects.begin(run, scope.identity.clone(), authored_scope.clone()) {
            Ok(admission) => admission,
            Err(error) => {
                self.preparation.discard_candidate(run, &authored_scope)?;
                return Err(error);
            }
        };
        // Preparation may have waited on a provider, filesystem and Store.
        // Import everything that arrived during that work immediately before
        // minting the live authority. Any change invalidates the prepared
        // template wholesale; the next evaluation resolves it from scratch.
        match handle.absorb() {
            Ok(true) => {
                effects.abort(run);
                self.preparation.discard_candidate(run, &authored_scope)?;
                report.next_deadline_ms = Some(now_ms);
                return Ok(false);
            }
            Ok(false) => {}
            Err(error) => {
                effects.abort(run);
                self.preparation.discard_candidate(run, &authored_scope)?;
                return Err(error);
            }
        }

        let final_now_ms = self.clock.now_ms(now_ms);
        if final_now_ms < occurrence_ms {
            effects.abort(run);
            self.preparation.discard_candidate(run, &authored_scope)?;
            report.next_deadline_ms = min_option(report.next_deadline_ms, Some(occurrence_ms));
            return Ok(false);
        }
        if final_now_ms >= occurrence_ms.saturating_add(OCCURRENCE_PRECISION_MS) {
            effects.abort(run);
            self.preparation.discard_candidate(run, &authored_scope)?;
            self.record_due_skip(
                handle,
                DueSkip {
                    schedule: ScheduleEventRef {
                        schedule: header.schedule,
                        workspace,
                        document,
                    },
                    occurrence_ms,
                    run,
                    outcome: ScheduleFiringOutcome::SkippedMissed,
                    busy_holder: None,
                    backlog_missed,
                    claimed_at_ms: final_now_ms,
                },
            )?;
            return Ok(true);
        }

        if let ScheduleAdmission::Busy(holder) = admission {
            self.preparation.discard_candidate(run, &authored_scope)?;
            self.record_due_skip(
                handle,
                DueSkip {
                    schedule: ScheduleEventRef {
                        schedule: header.schedule,
                        workspace,
                        document,
                    },
                    occurrence_ms,
                    run,
                    outcome: ScheduleFiringOutcome::SkippedBusy,
                    busy_holder: Some(holder),
                    backlog_missed,
                    claimed_at_ms: final_now_ms,
                },
            )?;
            return Ok(true);
        }

        // End candidate preparation before the atomic claim. Production's
        // planned Temporary identity creates no run folder and this is a
        // no-op; a claimed start creates and verifies the exact path only
        // after the ledger owns the RunId.
        self.preparation.discard_candidate(run, &authored_scope)?;

        let authority = match handle.claim_authority(
            self.device,
            ScheduleClaimPlan::Run {
                occurrence_ms,
                run,
                pinned_context: pinned,
            },
        ) {
            Ok(Some(authority)) => authority,
            Ok(None) => {
                effects.abort(run);
                self.preparation.discard_candidate(run, &authored_scope)?;
                return Ok(false);
            }
            Err(error) => {
                effects.abort(run);
                self.preparation.discard_candidate(run, &authored_scope)?;
                return Err(error);
            }
        };
        let missed_authority = match backlog_missed {
            Some(missed_occurrence_ms) => {
                let missed_run = RunId::mint();
                match handle.claim_authority(
                    self.device,
                    ScheduleClaimPlan::Skip {
                        occurrence_ms: missed_occurrence_ms,
                        run: missed_run,
                        outcome: ScheduleFiringOutcome::SkippedMissed,
                        busy_holder: None,
                    },
                ) {
                    Ok(Some(authority)) => Some((authority, missed_run, missed_occurrence_ms)),
                    Ok(None) => {
                        effects.abort(run);
                        self.preparation.discard_candidate(run, &authored_scope)?;
                        return Ok(false);
                    }
                    Err(error) => {
                        effects.abort(run);
                        self.preparation.discard_candidate(run, &authored_scope)?;
                        return Err(error);
                    }
                }
            }
            None => None,
        };
        let event_ref = ScheduleEventRef {
            schedule: header.schedule,
            workspace,
            document,
        };
        let fired_prepared = match self.events.prepare_occurrence(ScheduleOccurrenceEvent {
            schedule: event_ref,
            revision: header.definition.revision,
            occurrence_ms,
            run,
            outcome: ScheduleFiringOutcome::Fired,
            busy_holder: None,
            claimed_at_ms: final_now_ms,
        }) {
            Ok(prepared) => prepared,
            Err(error) => {
                effects.abort(run);
                self.preparation.discard_candidate(run, &authored_scope)?;
                return Err(error);
            }
        };
        let missed_prepared = match missed_authority.as_ref() {
            Some((_, missed_run, missed_occurrence_ms)) => {
                match self.events.prepare_occurrence(ScheduleOccurrenceEvent {
                    schedule: event_ref,
                    revision: header.definition.revision,
                    occurrence_ms: *missed_occurrence_ms,
                    run: *missed_run,
                    outcome: ScheduleFiringOutcome::SkippedMissed,
                    busy_holder: None,
                    claimed_at_ms: final_now_ms,
                }) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        effects.abort(run);
                        self.preparation.discard_candidate(run, &authored_scope)?;
                        return Err(error);
                    }
                }
            }
            None => None,
        };
        let claim_result = match missed_authority {
            Some((missed, _, _)) => {
                self.ledger
                    .claim_run_with_missed(authority, missed, final_now_ms)
                    .map(|(claimed, missed)| {
                        // The ledger is already crash-safe. This append makes the
                        // representative miss visible immediately; catalog
                        // reconciliation repairs it if the Store write fails.
                        (claimed, Some(missed.claim))
                    })
            }
            None => self
                .ledger
                .claim_run(authority, final_now_ms)
                .map(|claimed| (claimed, None)),
        };
        let claimed = match claim_result {
            Ok(claimed) => claimed,
            Err(error) => {
                effects.abort(run);
                self.preparation.discard_candidate(run, &authored_scope)?;
                return Err(error.into());
            }
        };
        let (claimed, missed_claim) = claimed;
        if claimed.run == run {
            // Once the durable claim belongs to this candidate, the granted
            // admission lease is restart state. Cache it before any fallible
            // projection/event repair so a later command cannot overlap it.
            self.claimed_scopes
                .insert(run, (scope.clone(), authored_scope.clone()));
        } else {
            // A prior durable winner owns the slot. Release the losing
            // candidate before event reconciliation can fail and defer this
            // branch; there must be no unowned admission lease in between.
            effects.abort(run);
        }
        let claimed_record = SlotClaim {
            schedule: claimed.schedule,
            occurrence_ms: claimed.occurrence_ms,
            revision: claimed.revision,
            run: claimed.run,
            state: SlotState::Claimed,
            busy_holder: None,
            claimed_at_ms: claimed.claimed_at_ms,
        };
        let fired_prepared = reconcile_occurrence_preparation(
            self.events.as_mut(),
            event_ref,
            fired_prepared,
            &claimed_record,
        )?;
        let missed_prepared = match (missed_prepared, missed_claim.as_ref()) {
            (prepared, Some(claim)) => {
                reconcile_occurrence_preparation(self.events.as_mut(), event_ref, prepared, claim)?
            }
            (prepared, None) => prepared,
        };
        if let Some(missed_claim) = missed_claim {
            append_firing(handle, &missed_claim, FiringAppend::Live)?;
            ready_if_some(self.events.as_mut(), missed_prepared)?;
        }
        if claimed.run != run {
            // A lost answer or competing preparation already owns this slot.
            // Release this candidate's admission and complete the durable
            // winner; never launch under the candidate's prepared context.
            let claim = self.ledger.by_run(claimed.run)?.ok_or_else(|| {
                PortError::new(Port::Store, "missing_claim", "run claim disappeared")
            })?;
            let authored_scope = self.ledger.intent_scope(claim.run)?.ok_or_else(|| {
                PortError::new(
                    Port::Store,
                    "missing_intent_scope",
                    "the claimed run has no authored scope",
                )
            })?;
            if let Some(started) = effects.reconcile(claim, authored_scope)? {
                report.started.push(started);
            }
            append_firing(handle, &claim, FiringAppend::Repair)?;
            self.ledger
                .settle(claim.schedule, claim.occurrence_ms, SlotOutcome::Fired)?;
            ready_if_some(self.events.as_mut(), fired_prepared)?;
            return Ok(true);
        }
        let claim = match self.ledger.by_run(claimed.run) {
            Ok(Some(claim)) => claim,
            Ok(None) => {
                return Err(PortError::new(
                    Port::Store,
                    "missing_claim",
                    "run claim disappeared",
                ));
            }
            Err(error) => {
                return Err(error.into());
            }
        };
        let started = effects.finish(run, claim, scope, authored_scope)?;
        // The exact admission token is consumed once the Actor accepted the
        // launch. Later Store-history/settlement repair reconciles the durable
        // run instead of asking the Actor to accept Finish twice.
        self.claimed_scopes.remove(&run);
        if let Some(started) = started {
            report.started.push(started);
        }
        append_firing(handle, &claim, FiringAppend::Live)?;
        self.ledger
            .settle(claim.schedule, claim.occurrence_ms, SlotOutcome::Fired)?;
        ready_if_some(self.events.as_mut(), fired_prepared)?;
        Ok(true)
    }
}

fn canonical_schedule_locations(
    schedules: &[ListedSchedule],
) -> HashMap<ScheduleId, (WorkspaceId, DocumentId)> {
    let mut canonical: HashMap<ScheduleId, (WorkspaceId, DocumentId)> = HashMap::new();
    for listed in schedules {
        canonical
            .entry(listed.header.schedule)
            .and_modify(|location| {
                *location = (*location).min((listed.workspace, listed.document));
            })
            .or_insert((listed.workspace, listed.document));
    }
    canonical
}

impl ScheduleEffects for Orchestrator {
    fn begin(
        &mut self,
        _run: RunId,
        scope: ScopeIdentity,
        _authored_scope: koru_delegate_core::WorkspaceScope,
    ) -> PortResult<ScheduleAdmission> {
        Ok(self
            .claims()
            .holders_of(&scope)
            .first()
            .map_or(ScheduleAdmission::Granted, |holder| {
                ScheduleAdmission::Busy(holder.run)
            }))
    }

    fn finish(
        &mut self,
        run: RunId,
        claim: SlotClaim,
        scope: koru_delegate_scope::ResolvedScope,
        _authored_scope: koru_delegate_core::WorkspaceScope,
    ) -> PortResult<Option<Started>> {
        debug_assert_eq!(run, claim.run);
        self.complete_claimed_intent(claim.run)
            .map_err(create_port)?;
        if matches!(
            self.header(claim.run).map_err(run_port)?.status,
            koru_delegate_core::RunStatus::Created
        ) {
            return self
                .start_claimed(
                    claim.run,
                    scope,
                    StartOrigin::ClaimedSchedule {
                        schedule: ScheduleRef::from_uuid(claim.schedule.as_uuid()),
                    },
                )
                .map(Some)
                .map_err(run_port);
        }
        Ok(None)
    }

    fn abort(&mut self, _run: RunId) {}

    fn reconcile(
        &mut self,
        claim: SlotClaim,
        _authored_scope: koru_delegate_core::WorkspaceScope,
    ) -> PortResult<Option<Started>> {
        match claim.state {
            SlotState::Claimed => {
                self.complete_claimed_intent(claim.run)
                    .map_err(create_port)?;
                if matches!(
                    self.header(claim.run).map_err(run_port)?.status,
                    koru_delegate_core::RunStatus::Created
                ) {
                    let header = self.header(claim.run).map_err(run_port)?;
                    let scope = koru_delegate_scope::ResolvedScope::resolve(
                        &header.scope,
                        claim.run,
                        self.ports().roots.as_ref(),
                    )
                    .map_err(|_| {
                        PortError::new(
                            Port::Scope,
                            "claimed_scope_resolve",
                            "the claimed scope could not be resolved",
                        )
                    })?;
                    return self
                        .start_claimed(
                            claim.run,
                            scope,
                            StartOrigin::ClaimedSchedule {
                                schedule: ScheduleRef::from_uuid(claim.schedule.as_uuid()),
                            },
                        )
                        .map(Some)
                        .map_err(run_port);
                }
                Ok(None)
            }
            SlotState::Settled(_) => Ok(None),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FiringAppend {
    /// An outcome decided and written by this live evaluation.
    Live,
    /// History repair or reconciliation of an outcome decided earlier.
    Repair,
}

fn append_firing(
    handle: &mut dyn ScheduleDocumentHandle,
    claim: &SlotClaim,
    origin: FiringAppend,
) -> PortResult<()> {
    let outcome = match claim.state {
        SlotState::Claimed | SlotState::Settled(SlotOutcome::Fired) => ScheduleFiringOutcome::Fired,
        SlotState::Settled(SlotOutcome::SkippedMissed) => ScheduleFiringOutcome::SkippedMissed,
        SlotState::Settled(SlotOutcome::SkippedBusy) => ScheduleFiringOutcome::SkippedBusy,
        SlotState::Settled(SlotOutcome::SkippedRetired) => return Ok(()),
    };
    let firing = ScheduleFiring {
        occurrence_ms: claim.occurrence_ms,
        revision: claim.revision,
        run: claim.run,
        outcome,
        busy_holder: claim.busy_holder,
        at: claim.claimed_at_ms,
    };
    let mut write = ScheduleWrite::AlreadyApplied;
    handle.commit(&mut |document| {
        write = document.append_firing(&firing)?;
        Ok(())
    })?;
    if origin == FiringAppend::Live && write == ScheduleWrite::Applied {
        crate::counters::schedule_firing_applied(outcome);
    }
    Ok(())
}

fn pause(
    handle: &mut dyn crate::ScheduleDocumentHandle,
    events: &mut dyn ScheduleEvents,
    schedule: ScheduleEventRef,
    device: DeviceId,
    now_ms: i64,
    reason: SchedulePauseReason,
) -> PortResult<()> {
    let mut definition = handle.document().header()?.definition;
    definition.revision = definition
        .revision
        .checked_add(1)
        .ok_or(ScheduleDocumentError::RevisionExhausted)?;
    definition.effective_at_ms = now_ms.max(definition.effective_at_ms.saturating_add(1));
    definition.lifecycle = ScheduleLifecycle::Paused { reason };
    let prepared = events.prepare_definition(schedule, &definition)?;
    handle.commit(&mut |document| document.update_definition(device, &definition))?;
    events.ready(prepared)
}

fn schedule_event_ref(listed: &ListedSchedule) -> ScheduleEventRef {
    ScheduleEventRef {
        schedule: listed.header.schedule,
        workspace: listed.workspace,
        document: listed.document,
    }
}

fn reconcile_occurrence_preparation(
    events: &mut dyn ScheduleEvents,
    schedule: ScheduleEventRef,
    prepared: Option<PreparedScheduleEvent>,
    claim: &SlotClaim,
) -> PortResult<Option<PreparedScheduleEvent>> {
    events.reconcile_occurrence(schedule, prepared, claim)
}

fn ready_if_some(
    events: &mut dyn ScheduleEvents,
    prepared: Option<PreparedScheduleEvent>,
) -> PortResult<()> {
    if let Some(prepared) = prepared {
        events.ready(prepared)?;
    }
    Ok(())
}

/// A failure discovered from an old template must never pause a newer
/// definition. Import everything currently buffered and let an immediate
/// evaluation prepare that definition from scratch.
fn refresh_before_pause(
    handle: &mut dyn ScheduleDocumentHandle,
    report: &mut ScheduleEvaluation,
    now_ms: i64,
) -> PortResult<bool> {
    let changed = handle.absorb()?;
    if changed {
        report.next_deadline_ms = Some(now_ms);
    }
    Ok(changed)
}

fn record_failure(report: &mut ScheduleEvaluation, now_ms: i64, error: &PortError) {
    let _ = error;
    crate::counters::schedule_evaluation_failed(1);
    report.failures = report.failures.saturating_add(1);
    report.next_deadline_ms =
        min_option(report.next_deadline_ms, Some(now_ms.saturating_add(1_000)));
}

fn context_stage(stage: CaptureStage) -> ScheduleContextFailureStage {
    match stage {
        CaptureStage::Resolve => ScheduleContextFailureStage::Resolve,
        CaptureStage::Commit => ScheduleContextFailureStage::Commit,
        CaptureStage::Retention => ScheduleContextFailureStage::Retention,
    }
}

fn create_port(error: crate::CreateError) -> PortError {
    match error {
        crate::CreateError::Port(error) => error,
        crate::CreateError::Capture(failure) => PortError::new(
            Port::Capture,
            "capture",
            format!("item {} at {:?}", failure.item, failure.stage),
        ),
    }
}

fn run_port(error: crate::RunError) -> PortError {
    match error {
        crate::RunError::Port(error) => error,
        other => PortError::new(
            Port::Schedules,
            other.code(),
            "claimed run could not advance",
        ),
    }
}

fn min_option(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn max_option(left: Option<i64>, right: Option<i64>) -> Option<i64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use koru_delegate_core::{ActorRef, Objective, ProfileId, SessionProfileKey, WorkspaceScope};
    use koru_delegate_schedule::{DelegationTemplate, ScheduleLifecycle};
    use koru_recurrence::{CronExpression, RecurrenceRule};
    use koru_time_zone::IanaZoneRules;

    use super::*;

    fn definition(effective_at_ms: i64) -> ScheduleDefinition {
        ScheduleDefinition {
            routine: None,
            revision: 1,
            effective_at_ms,
            template: DelegationTemplate {
                requested_by: ActorRef::Person {
                    profile: ProfileId::new(),
                },
                objective: Objective::new("sentinel objective").unwrap(),
                agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
                requested_model: None,
                scope: WorkspaceScope::Temporary,
                grants: BTreeSet::new(),
                context: Vec::new(),
            },
            recurrence: RecurrenceRule::floating(CronExpression::parse("0 * * * *").unwrap()),
            lifecycle: ScheduleLifecycle::Active,
        }
    }

    #[test]
    fn one_month_of_hourly_slots_is_one_miss_plus_the_current_slot() {
        let zone = IanaZoneRules::parse("UTC").unwrap();
        let start = 1_767_225_600_000; // 2026-01-01T00:00:00Z
        let now = start + 31 * 24 * 60 * OCCURRENCE_PRECISION_MS;
        let plan = plan_schedule(&definition(start), None, now, &zone).unwrap();
        assert_eq!(
            plan.actions,
            vec![
                PlannedAction::Due { occurrence_ms: now },
                PlannedAction::Missed {
                    occurrence_ms: start,
                },
            ]
        );
        assert_eq!(
            plan.next_deadline_ms,
            Some(now + 60 * OCCURRENCE_PRECISION_MS)
        );

        // A crash after the runnable action but before the representative
        // miss cannot suppress or duplicate the current occurrence.
        let after_crash = plan_schedule(&definition(start), Some(now), now, &zone).unwrap();
        assert!(after_crash.actions.is_empty());
    }

    #[test]
    fn effective_time_is_inclusive_and_a_cursor_makes_duplicate_wakes_inert() {
        let zone = IanaZoneRules::parse("UTC").unwrap();
        let at = 1_767_225_600_000;
        let definition = definition(at);
        assert_eq!(
            plan_schedule(&definition, None, at + 1, &zone)
                .unwrap()
                .actions,
            vec![PlannedAction::Due { occurrence_ms: at }]
        );
        assert!(
            plan_schedule(&definition, Some(at), at + 1, &zone)
                .unwrap()
                .actions
                .is_empty()
        );
    }

    #[test]
    fn planner_values_and_errors_print_no_template_content() {
        let plan = SchedulePlan {
            actions: vec![PlannedAction::Due { occurrence_ms: 0 }],
            next_deadline_ms: None,
        };
        assert!(!format!("{plan:?}").contains("sentinel objective"));
    }
}
