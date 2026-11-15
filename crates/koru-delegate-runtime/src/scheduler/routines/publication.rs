// SPDX-License-Identifier: MPL-2.0

use super::*;

fn skipped_slot(record: &RoutineSkippedClaim) -> SlotClaim {
    let (outcome, busy_holder) = match record.outcome {
        RoutineSkipOutcome::SkippedMissed {} => (SlotOutcome::SkippedMissed, None),
        RoutineSkipOutcome::SkippedBusy { holder } => (SlotOutcome::SkippedBusy, Some(holder)),
    };
    SlotClaim {
        schedule: record.slot.schedule,
        occurrence_ms: record.slot.occurrence_ms,
        revision: record.definition_revision,
        run: record.planned_run,
        state: SlotState::Settled(outcome),
        busy_holder,
        claimed_at_ms: record.recorded_at_ms,
    }
}

impl ScheduleCoordinator {
    pub(super) fn project_routine_skip(
        &mut self,
        route: &RoutineRouteIdentity,
        handle: &mut dyn ScheduleDocumentHandle,
        record: &RoutineSkippedClaim,
    ) -> PortResult<()> {
        if !record.valid()
            || !route.matches_slot(&record.slot)
            || handle.document().header()?.schedule != route.schedule
        {
            return Err(refused("routine_skip_identity"));
        }
        let claim = skipped_slot(record);
        let header = handle.document().header()?;
        self.events
            .existing_created(route_location(route), &header)?;
        // Only durable skips cross the ordinary projection helper. Held, Linked
        // and terminal link records establish no launch fact and never Fired.
        let event = self
            .events
            .reconcile_occurrence(route_location(route), None, &claim)?;
        append_firing(handle, &claim, FiringAppend::Repair)?;
        ready_if_some(self.events.as_mut(), event)
    }

    pub(super) fn publish_routine_skip(
        &mut self,
        routines: &Routines,
        route: &RoutineRouteIdentity,
        record: &RoutineSkippedClaim,
        deadline: Instant,
    ) -> PortResult<()> {
        routines.live(deadline)?;
        let location = route_location(route);
        let mut handle = self.schedules.open(location.workspace, location.document)?;
        routines.live(deadline)?;
        let _guard = routines.phases.enter_until(deadline)?;
        routines.live(deadline)?;
        handle.absorb()?;
        routines.live(deadline)?;
        routines.route_current(route)?;
        match routines
            .ledger
            .read_routine_claim(&record.slot)
            .map_err(ledger_error)?
        {
            RoutineClaimObservation::Skipped { record: current } if &current == record => {
                self.project_routine_skip(route, handle.as_mut(), record)
            }
            _ => Err(refused("routine_skip_changed")),
        }
    }

    pub(super) fn repair_routine_page(
        &mut self,
        routines: &mut Routines,
        next: Option<(RoutineRouteIdentity, ScheduleEventRef)>,
        report: &mut ScheduleEvaluation,
        now_ms: i64,
    ) {
        if !routines.repair.active {
            return;
        }
        if routines.repair.current.is_none() {
            let Some((route, location)) = next else {
                routines.repair.finish_pass();
                return;
            };
            routines.repair.current = Some(RepairRoute {
                route,
                location,
                after: None,
            });
        }
        let mut current = routines
            .repair
            .current
            .take()
            .expect("selected repair route");
        let repaired = (|| {
            routines.route_current(&current.route)?;
            let page = routines
                .ledger
                .routine_slots_page(&current.route, current.after)
                .map_err(ledger_error)?;
            let mut handle = self
                .schedules
                .open(current.location.workspace, current.location.document)?;
            handle.absorb()?;
            if handle.document().header()?.schedule != current.route.schedule {
                return Err(refused("routine_repair_location"));
            }
            for (cursor, entry) in page.entries {
                current.after = Some(cursor);
                let result = match entry {
                    Ok(fact) => match fact.observation {
                        RoutineClaimObservation::Skipped { record } => {
                            self.project_routine_skip(&current.route, handle.as_mut(), &record)
                        }
                        RoutineClaimObservation::Claimed { record } => {
                            if let Some(claim) = routines
                                .ledger
                                .routine_firing(&record.claim.slot)
                                .map_err(ledger_error)?
                            {
                                let header = handle.document().header()?;
                                self.events.existing_created(current.location, &header)?;
                                let event = self.events.reconcile_occurrence(
                                    current.location,
                                    None,
                                    &claim,
                                )?;
                                append_firing(handle.as_mut(), &claim, FiringAppend::Repair)?;
                                ready_if_some(self.events.as_mut(), event)
                            } else {
                                Ok(())
                            }
                        }
                        RoutineClaimObservation::Absent { .. } => {
                            Err(refused("routine_repair_absent"))
                        }
                    },
                    Err(error) => Err(ledger_error(error)),
                };
                if let Err(error) = result {
                    record_failure(report, now_ms, &error);
                    routines.repair.again = true;
                }
            }
            Ok(page.next)
        })();
        match repaired {
            Ok(Some(next)) => {
                current.after = Some(next);
                routines.repair.current = Some(current);
            }
            Ok(None) => routines.repair.last = Some(current.route.schedule),
            Err(error) => {
                // An unavailable route cannot pin the pass ahead of healthy ones.
                routines.repair.last = Some(current.route.schedule);
                routines.repair.again = true;
                record_failure(report, now_ms, &error);
            }
        }
        continue_later(report, now_ms);
    }
}
