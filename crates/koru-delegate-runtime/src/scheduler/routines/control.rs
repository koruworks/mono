// SPDX-License-Identifier: MPL-2.0

//! The Bot Routine owner path: Enable, Pause and Edit as one synchronous,
//! revision-checked definition commit, and the acceptance of an owner Test.
//!
//! The coordinator serves one command at a time on the scheduler thread, so
//! these never interleave with an evaluation or with each other. Nothing here
//! claims an occurrence: an accepted Test is one in-memory slot that the next
//! pass may run through its own candidate, and a restart forgets it.
use super::*;
use koru_delegate_schedule::routine_semantic_digest;

/// What an owner control changes. The name and recurrence of an `Edit` are the
/// only content a caller supplies; everything else is copied from the live
/// definition.
#[derive(Clone, PartialEq, Eq)]
pub enum RoutineControl {
    Enable,
    Pause,
    Edit {
        expected_name: ScheduleName,
        name: ScheduleName,
        recurrence: RecurrenceRule,
    },
}
impl std::fmt::Debug for RoutineControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Enable => "RoutineControl::Enable",
            Self::Pause => "RoutineControl::Pause",
            Self::Edit { .. } => "RoutineControl::Edit([REDACTED])",
        })
    }
}

/// The Routine as the document holds it after one control.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RoutineControlled {
    pub route: RoutineRouteIdentity,
    pub outcome: RoutineControlOutcome,
    pub name: ScheduleName,
    pub definition: RoutineDefinitionIdentity,
    pub effective_at_ms: i64,
    pub lifecycle: RoutineControlLifecycle,
}

/// An accepted owner Test: the slot it may claim and the paused definition it
/// was accepted under. Not a claim.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RoutineTestAccepted {
    pub slot: RoutineSlot,
    pub definition: RoutineDefinitionIdentity,
}

/// The one outstanding owner Test of this delegate. Process memory only.
pub(super) struct OutstandingTest {
    pub(super) route: RoutineRouteIdentity,
    pub(super) location: ScheduleEventRef,
    pub(super) header: ScheduleHeader,
    pub(super) owner_handoff: Option<HandoffId>,
    pub(super) slot: RoutineSlot,
    pub(super) definition: RoutineDefinitionIdentity,
    pub(super) semantic_hash: RoutineDigest32,
}
impl OutstandingTest {
    pub(super) fn candidate(&self) -> RoutineCandidate {
        RoutineCandidate::Test {
            route: self.route.clone(),
            location: self.location,
            header: self.header.clone(),
            owner_handoff: self.owner_handoff,
            slot: self.slot,
            semantic_hash: self.semantic_hash,
        }
    }
}

/// A Test may no longer claim once less than the margin remains of its minute.
pub(super) fn test_window_closed(now_ms: i64, occurrence_ms: i64) -> bool {
    now_ms
        >= occurrence_ms
            .saturating_add(ROUTINE_TEST_MINUTE_MS)
            .saturating_sub(ROUTINE_TEST_CLAIM_MARGIN_MS)
}

fn schedule_error(code: &'static str, detail: &'static str) -> PortError {
    PortError::new(Port::Schedules, code, detail)
}

struct OwnedRoutine {
    location: ScheduleEventRef,
    /// The catalog's header for the exact location, as the immutable identity
    /// every later read of the live document must still carry.
    listed: ScheduleHeader,
    handle: Box<dyn ScheduleDocumentHandle>,
    header: ScheduleHeader,
}

impl Routines {
    /// The live header must still be the listed subject, and the route the
    /// ledger retains for its exact physical location must be the command's.
    fn owner_route(
        &self,
        route: &RoutineRouteIdentity,
        location: ScheduleEventRef,
        listed: &ScheduleHeader,
        current: &ScheduleHeader,
    ) -> PortResult<()> {
        if current.schedule != listed.schedule
            || current.created_at_ms != listed.created_at_ms
            || current.initial_owner != listed.initial_owner
            || current.definition.routine != listed.definition.routine
            || self.mutation_route(location, current)?.as_ref() != Some(route)
        {
            return Err(refused("routine_route_mismatch"));
        }
        Ok(())
    }

    /// Enable and Test only. The barrier is read now, inside the command; a
    /// Trash or Archive installs its pause before it settles anything, so an
    /// Enable handled after that cannot turn a Routine on inside it.
    fn bot_admits(&self, route: &RoutineRouteIdentity) -> PortResult<()> {
        match self
            .ledger
            .bot_admission_barrier(route.owner, route.bot)
            .map_err(ledger_error)?
        {
            None => Err(schedule_error(
                "routine_barrier_absent",
                "no admission barrier is recorded for the Bot yet",
            )),
            Some(barrier) if barrier.owner != route.owner || barrier.bot != route.bot => {
                Err(refused("routine_barrier_identity"))
            }
            Some(barrier) if barrier.paused => Err(schedule_error(
                "routine_bot_paused",
                "the Bot's admission barrier is paused",
            )),
            Some(_) => Ok(()),
        }
    }
}

impl ScheduleCoordinator {
    /// Authority chain step 3: the command's route, the route retained in
    /// the ledger and the live header at `DocumentId(schedule uuid)` must be
    /// one subject. The exact location is opened; the lowest listed header
    /// that happens to carry the Schedule id is never consulted.
    fn owned_routine(
        &mut self,
        routines: &Routines,
        route: &RoutineRouteIdentity,
    ) -> PortResult<OwnedRoutine> {
        if !route.valid() {
            return Err(refused("routine_control_invalid"));
        }
        if route.owner.device != self.device {
            return Err(ScheduleDocumentError::NotOwner.into());
        }
        routines.route_current(route)?;
        let location = route_location(route);
        let catalog = self.schedules.discover_active()?;
        if catalog.failures != 0
            || !catalog.failed_workspaces.is_empty()
            || !catalog.unavailable_documents.is_empty()
        {
            return Err(schedule_error(
                "schedule_unavailable",
                "Routine identity cannot be proven from a partial catalog",
            ));
        }
        let listed = catalog
            .schedules
            .into_iter()
            .find(|listed| {
                listed.workspace == location.workspace && listed.document == location.document
            })
            .ok_or_else(|| {
                schedule_error(
                    "schedule_not_found",
                    "no Schedule document is at the Routine's location",
                )
            })?
            .header;
        let mut handle = self.schedules.open(location.workspace, location.document)?;
        handle.absorb()?;
        let header = handle.document().header()?;
        routines.owner_route(route, location, &listed, &header)?;
        Ok(OwnedRoutine {
            location,
            listed,
            handle,
            header,
        })
    }

    /// Enable, Pause or Edit one Bot Routine as a single revision-checked
    /// commit. The answer is `Applied` only after the Store acknowledged the
    /// commit and its prepared event was made Ready; a retry with the same
    /// revision and content answers `AlreadyApplied` and writes nothing.
    pub fn control_routine<R: ZoneRules + ?Sized>(
        &mut self,
        route: &RoutineRouteIdentity,
        expected_revision: u32,
        control: RoutineControl,
        now_ms: i64,
        floating_zone: &R,
    ) -> PortResult<RoutineControlled> {
        let routines = self
            .routines
            .take()
            .ok_or_else(|| refused("routine_owner_unavailable"))?;
        let result = self.control_owned(
            &routines,
            route,
            expected_revision,
            control,
            now_ms,
            floating_zone,
        );
        self.routines = Some(routines);
        result
    }

    fn control_owned<R: ZoneRules + ?Sized>(
        &mut self,
        routines: &Routines,
        route: &RoutineRouteIdentity,
        expected_revision: u32,
        control: RoutineControl,
        now_ms: i64,
        floating_zone: &R,
    ) -> PortResult<RoutineControlled> {
        if !routine_control_revision_valid(expected_revision) {
            return Err(refused("routine_control_invalid"));
        }
        let OwnedRoutine {
            location,
            listed,
            mut handle,
            header,
        } = self.owned_routine(routines, route)?;
        if matches!(control, RoutineControl::Enable) {
            // Before the commit, and whether or not this is a replay: a
            // paused or unsynchronized Bot never has a Routine turned on.
            routines.bot_admits(route)?;
        }
        let current = header.definition;
        let successor = expected_revision
            .checked_add(1)
            .ok_or(ScheduleDocumentError::RevisionExhausted)?;
        let (lifecycle, recurrence) = match &control {
            RoutineControl::Enable => (ScheduleLifecycle::Active, current.recurrence.clone()),
            RoutineControl::Pause => (
                ScheduleLifecycle::Paused {
                    reason: SchedulePauseReason::Manual,
                },
                current.recurrence.clone(),
            ),
            RoutineControl::Edit { recurrence, .. } => {
                (current.lifecycle.clone(), recurrence.clone())
            }
        };
        // The exact successor keeps its stored effective time, so a replay
        // compares equal instead of proposing a later one.
        let replay = current.revision == successor
            && (matches!(control, RoutineControl::Edit { .. }) || current.lifecycle == lifecycle);
        let effective_at_ms = if replay {
            current.effective_at_ms
        } else {
            now_ms.max(current.effective_at_ms.saturating_add(1))
        };
        if matches!(control, RoutineControl::Edit { .. }) {
            // The rule must be evaluable before anything is prepared or
            // written; a rule that cannot be evaluated could never fire.
            recurrence
                .next_after(effective_at_ms.saturating_sub(1), floating_zone)
                .map_err(|_| schedule_error("recurrence", "evaluation failed"))?;
        }
        let definition = ScheduleDefinition {
            routine: current.routine.clone(),
            revision: successor,
            effective_at_ms,
            template: current.template.clone(),
            recurrence,
            lifecycle: lifecycle.clone(),
        };
        let device = self.device;
        let prepared = self.events.prepare_definition(location, &definition)?;
        let mut written = None;
        let mut route_changed = false;
        let committed = handle.commit(&mut |document| {
            if routines
                .owner_route(route, location, &listed, &document.header()?)
                .is_err()
            {
                route_changed = true;
                return Err(ScheduleDocumentError::RoutineAssociationChanged);
            }
            written = Some(match &control {
                RoutineControl::Enable | RoutineControl::Pause => document.set_lifecycle(
                    device,
                    expected_revision,
                    lifecycle.clone(),
                    effective_at_ms,
                )?,
                RoutineControl::Edit {
                    expected_name,
                    name,
                    ..
                } => document.edit(device, expected_revision, expected_name, name, &definition)?,
            });
            Ok(())
        });
        if route_changed {
            return Err(refused("routine_route_mismatch"));
        }
        committed?;
        let written = written
            .ok_or_else(|| schedule_error("schedule_unavailable", "the control did not run"))?;
        self.events.ready(prepared)?;
        let document = handle.document();
        let after = document.header()?.definition;
        let owner_handoff = document.handoff_state()?.epoch;
        Ok(RoutineControlled {
            route: route.clone(),
            outcome: match written {
                ScheduleWrite::Applied => RoutineControlOutcome::Applied,
                ScheduleWrite::AlreadyApplied => RoutineControlOutcome::AlreadyApplied,
            },
            name: document.name()?,
            definition: definition_identity(&after, route.workspace, owner_handoff)?,
            effective_at_ms: after.effective_at_ms,
            lifecycle: match after.lifecycle {
                ScheduleLifecycle::Active => RoutineControlLifecycle::Active,
                ScheduleLifecycle::Paused { .. } => RoutineControlLifecycle::Paused,
            },
        })
    }

    /// Accept one owner Test of a Routine that is exactly Paused(Manual), for
    /// the delegate's current minute. Acceptance writes nothing to the ledger
    /// or the document: it leaves one slot in memory for the next pass.
    pub fn accept_routine_test(
        &mut self,
        route: &RoutineRouteIdentity,
        expected_revision: u32,
        occurrence_ms: i64,
        now_ms: i64,
    ) -> PortResult<RoutineTestAccepted> {
        let mut routines = self
            .routines
            .take()
            .ok_or_else(|| refused("routine_owner_unavailable"))?;
        let result = self.accept_test_owned(
            &mut routines,
            route,
            expected_revision,
            occurrence_ms,
            now_ms,
        );
        self.routines = Some(routines);
        result
    }

    fn accept_test_owned(
        &mut self,
        routines: &mut Routines,
        route: &RoutineRouteIdentity,
        expected_revision: u32,
        occurrence_ms: i64,
        now_ms: i64,
    ) -> PortResult<RoutineTestAccepted> {
        if !routine_control_revision_valid(expected_revision)
            || !routine_test_minute_valid(occurrence_ms)
        {
            return Err(refused("routine_control_invalid"));
        }
        let OwnedRoutine {
            location,
            handle,
            header,
            ..
        } = self.owned_routine(routines, route)?;
        routines.bot_admits(route)?;
        let document = handle.document();
        if !document.is_writable() {
            // A newer format already failed the header read above; this is
            // the same answer for any other reason the document is read-only.
            return Err(schedule_error(
                "schedule_read_only",
                "the Schedule document is a newer format",
            ));
        }
        if matches!(document.state()?, ScheduleState::Retired) {
            return Err(ScheduleDocumentError::Retired.into());
        }
        let handoff = document.handoff_state()?;
        if !handoff.pending.is_empty() {
            return Err(ScheduleDocumentError::HandoffPending.into());
        }
        if handoff.owner != self.device {
            return Err(ScheduleDocumentError::NotOwner.into());
        }
        if header.definition.revision != expected_revision {
            return Err(ScheduleDocumentError::RevisionConflict {
                current: header.definition.revision,
                offered: expected_revision,
            }
            .into());
        }
        if header.definition.lifecycle
            != (ScheduleLifecycle::Paused {
                reason: SchedulePauseReason::Manual,
            })
        {
            return Err(schedule_error(
                "routine_test_requires_paused",
                "a Test needs a Routine its owner paused",
            ));
        }
        let now = self.clock.now_ms(now_ms);
        if occurrence_ms != routine_test_minute_of(now) {
            return Err(schedule_error(
                "routine_test_minute_changed",
                "the Test minute is not the delegate's current minute",
            ));
        }
        if occurrence_ms < header.definition.effective_at_ms {
            return Err(schedule_error(
                "routine_test_too_soon",
                "the current minute starts before the definition took effect",
            ));
        }
        let slot = RoutineSlot {
            owner: route.owner,
            bot: route.bot,
            schedule: route.schedule,
            occurrence_ms,
        };
        let definition = definition_identity(&header.definition, route.workspace, handoff.epoch)?;
        let accepted = RoutineTestAccepted { slot, definition };
        match routines
            .ledger
            .read_routine_claim(&slot)
            .map_err(ledger_error)?
        {
            RoutineClaimObservation::Absent { .. } => {}
            // A claim under the current paused definition can only be this
            // Test's own: the replay of an acceptance whose reply was lost.
            RoutineClaimObservation::Claimed { record }
                if record.valid() && record.claim.definition == definition =>
            {
                return Ok(accepted);
            }
            _ => {
                return Err(schedule_error(
                    "routine_test_minute_taken",
                    "the minute already has a claim or a skip that is not this Test",
                ));
            }
        }
        if let Some(outstanding) = &routines.test {
            if outstanding.route == *route
                && outstanding.slot == slot
                && outstanding.definition == definition
            {
                return Ok(accepted);
            }
            // A Test whose minute has ended can no longer claim; it is not
            // "still outstanding" and does not block the next one.
            if now
                < outstanding
                    .slot
                    .occurrence_ms
                    .saturating_add(ROUTINE_TEST_MINUTE_MS)
            {
                return Err(schedule_error(
                    "routine_test_busy",
                    "another Test is still outstanding on this delegate",
                ));
            }
        }
        let semantic_hash =
            routine_semantic_digest(&slot, &header.definition, route.workspace, handoff.epoch)
                .map_err(|_| refused("routine_semantic_identity"))?;
        routines.test = Some(OutstandingTest {
            route: route.clone(),
            location,
            header,
            owner_handoff: handoff.epoch,
            slot,
            definition,
            semantic_hash,
        });
        routines.test_yielded = false;
        Ok(accepted)
    }

    /// The slot of the one outstanding owner Test, if any. Read-only.
    pub fn routine_test_outstanding(&self) -> Option<RoutineSlot> {
        self.routines
            .as_ref()
            .and_then(|routines| routines.test.as_ref())
            .map(|test| test.slot)
    }

    /// The due path's attempted-slot cursor, so a test can prove an owner
    /// Test never reads or writes it.
    #[cfg(test)]
    pub(crate) fn routine_attempted_slot(&self) -> Option<RoutineSlot> {
        self.routines
            .as_ref()
            .and_then(|routines| routines.attempted_slot)
    }
}
