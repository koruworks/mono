// SPDX-License-Identifier: MPL-2.0

use super::*;

enum RecoveryCursor {
    Held(RoutineHeldCursor),
    Queued(RoutineQueuedCursor),
}
struct RecoverySelection {
    candidate: RoutineCandidate,
    cursor: RecoveryCursor,
    alternate: bool,
}

impl ScheduleCoordinator {
    pub(in crate::scheduler) fn evaluate_routine_catalog<R: ZoneRules + ?Sized>(
        &mut self,
        routines: &mut Routines,
        catalog: ScheduleCatalog,
        effects: &mut dyn ScheduleEffects,
        now_ms: i64,
        zone: &R,
        report: &mut ScheduleEvaluation,
    ) -> PortResult<()> {
        let mut ordinary = Vec::new();
        let mut candidates = Vec::with_capacity(PAGE);
        let mut due_rows = 0usize;
        let catalog_complete = catalog.failures == 0
            && catalog.failed_workspaces.is_empty()
            && catalog.unavailable_documents.is_empty();
        let mut attempted_seen = false;
        let mut repair_next: Option<(RoutineRouteIdentity, ScheduleEventRef)> = None;
        let mut repair_highest = None;
        // Route classification precedes ordinary duplicate-header selection.
        // Only positive, fully decoded absence can enter that old path.
        for listed in catalog.schedules {
            let location = schedule_event_ref(&listed);
            match routines.route_at(location) {
                Ok(None) if listed.header.definition.routine.is_none() => ordinary.push(listed),
                Ok(Some(route)) if route.valid() && location == route_location(&route) => {
                    if routines
                        .attempted_slot
                        .is_some_and(|slot| route.matches_slot(&slot))
                    {
                        attempted_seen = true;
                        if route.owner.device != self.device
                            || !route_header(&route, location, &listed.header)
                            || !matches!(
                                listed.header.definition.lifecycle,
                                ScheduleLifecycle::Active
                            )
                        {
                            routines.attempted_slot = None;
                        }
                    }
                    if route.owner.device != self.device {
                        continue;
                    }
                    repair_highest = Some(
                        repair_highest
                            .map_or(route.schedule, |id: ScheduleId| id.max(route.schedule)),
                    );
                    let within_repair_pass = routines
                        .repair
                        .anchor
                        .is_none_or(|anchor| route.schedule <= anchor);
                    if routines.repair.active && !within_repair_pass {
                        routines.repair.again = true;
                    }
                    if routines.repair.active
                        && within_repair_pass
                        && routines.repair.current.is_none()
                        && routines
                            .repair
                            .last
                            .is_none_or(|last| route.schedule > last)
                        && repair_next
                            .as_ref()
                            .is_none_or(|(next, _)| route.schedule < next.schedule)
                    {
                        repair_next = Some((route.clone(), location));
                    }
                    if !route_header(&route, location, &listed.header) {
                        record_failure(report, now_ms, &refused("routine_catalog_identity"));
                        continue;
                    }
                    if routines
                        .due_after
                        .is_some_and(|after| route.schedule <= after)
                    {
                        continue;
                    }
                    due_rows = due_rows.saturating_add(1);
                    let position = candidates.partition_point(
                        |(r, _): &(RoutineRouteIdentity, ListedSchedule)| {
                            r.schedule < route.schedule
                        },
                    );
                    if position < PAGE {
                        candidates.insert(position, (route, listed));
                        candidates.truncate(PAGE);
                    }
                }
                Ok(_) => record_failure(report, now_ms, &refused("routine_catalog_identity")),
                Err(error) => {
                    if routines
                        .attempted_slot
                        .is_some_and(|slot| slot.schedule == listed.header.schedule)
                    {
                        attempted_seen = true; // Unavailable is not positive absence.
                    }
                    record_failure(report, now_ms, &error);
                }
            }
        }
        if catalog_complete && !attempted_seen {
            routines.attempted_slot = None;
        }
        if routines.repair.active && routines.repair.anchor.is_none() {
            // Freeze positive canonical knowledge, even if unrelated catalog
            // entries are unavailable. This bounds a live keyset pass, not a
            // snapshot: concurrent lower UUIDs may still appear in this pass.
            routines.repair.anchor = repair_highest;
        }
        let canonical = canonical_schedule_locations(&ordinary);
        for listed in ordinary.into_iter().filter(|listed| {
            canonical.get(&listed.header.schedule) == Some(&(listed.workspace, listed.document))
        }) {
            crate::counters::schedule_document_evaluated();
            if let Err(error) = self.evaluate_one(listed, effects, now_ms, zone, report) {
                record_failure(report, now_ms, &error);
            }
        }
        self.repair_routine_page(routines, repair_next, report, now_ms);
        let recovery = self.select_routine_recovery(routines, report, now_ms);
        let mut due = None;
        let mut inspected = 0usize;
        for (route, listed) in candidates {
            inspected += 1;
            let inspected_schedule = route.schedule;
            match self.select_routine_due(routines, route, listed, now_ms, zone, report) {
                Ok(Some(work)) => {
                    due = Some(work);
                    break;
                }
                Ok(None) => {}
                Err(error) => record_failure(report, now_ms, &error),
            }
            routines.due_after = Some(inspected_schedule);
        }
        report.next_deadline_ms = min_option(report.next_deadline_ms, routines.due_deadline_ms);
        // The owner Test is not a due candidate. It comes only from the slot
        // an accepted TestRoutine left in memory, and it is forgotten, with no
        // write, once its minute can no longer be claimed.
        if routines
            .test
            .as_ref()
            .is_some_and(|test| control::test_window_closed(now_ms, test.slot.occurrence_ms))
        {
            routines.test = None;
        }
        let other_work = recovery.is_some() || due.is_some();
        let use_test = routines.test.is_some() && (!other_work || !routines.test_yielded);
        if routines.test.is_some() {
            // Alternate with due and recovery work. Whatever was not chosen
            // keeps its cursor and is selected again by the next pass.
            routines.test_yielded = use_test && other_work;
            continue_later(report, now_ms);
        }
        let had_both = !use_test && recovery.is_some() && due.is_some();
        let use_recovery =
            !use_test && recovery.is_some() && (due.is_none() || routines.prefer_recovery);
        if inspected < due_rows || ((use_recovery || use_test) && due.is_some()) {
            continue_later(report, now_ms);
        } else {
            routines.due_scanning = false;
        }
        let chosen = if use_test {
            routines.test.as_ref().map(|test| test.candidate())
        } else if use_recovery {
            recovery.map(|selected| {
                match selected.cursor {
                    RecoveryCursor::Held(cursor) => routines.held_after = Some(cursor),
                    RecoveryCursor::Queued(cursor) => routines.queued_after = Some(cursor),
                }
                if selected.alternate {
                    routines.prefer_queued = !routines.prefer_queued;
                }
                selected.candidate
            })
        } else {
            due.map(|candidate| {
                if let RoutineCandidate::Due { slot, .. } = &candidate {
                    routines.due_after = Some(slot.schedule);
                }
                candidate
            })
        };
        if had_both {
            routines.prefer_recovery = !routines.prefer_recovery;
        }
        if let Some(candidate) = chosen {
            routines.outstanding = Some(routines.serial);
            report.routine_work = Some(RoutineWork {
                evaluation_serial: routines.serial,
                candidate,
            });
            continue_later(report, now_ms);
        }
        Ok(())
    }

    fn select_routine_due<R: ZoneRules + ?Sized>(
        &mut self,
        routines: &mut Routines,
        route: RoutineRouteIdentity,
        listed: ListedSchedule,
        now_ms: i64,
        zone: &R,
        report: &mut ScheduleEvaluation,
    ) -> PortResult<Option<RoutineCandidate>> {
        if routines.bridge.stopped() {
            return Ok(None);
        }
        let location = schedule_event_ref(&listed);
        let mut handle = self.schedules.open(location.workspace, location.document)?;
        handle.absorb()?;
        let header = handle.document().header()?;
        if !route_header(&route, location, &header) {
            return Err(refused("routine_live_route"));
        }
        if !handle.document().may_claim(self.device)? {
            if routines
                .attempted_slot
                .is_some_and(|slot| route.matches_slot(&slot))
            {
                routines.attempted_slot = None;
            }
            return Ok(None);
        }
        let owner_handoff = handle.document().handoff_state()?.epoch;
        let observed = max_option(
            handle.document().observed_through_ms()?,
            self.ledger.observed_through_ms(route.schedule)?,
        );
        let plan = plan_schedule(&header.definition, observed, now_ms, zone)
            .map_err(|_| refused("routine_recurrence"))?;
        report.evaluated = report.evaluated.saturating_add(1);
        routines.due_deadline_ms = min_option(routines.due_deadline_ms, plan.next_deadline_ms);
        report.next_deadline_ms = min_option(report.next_deadline_ms, plan.next_deadline_ms);
        let missed = plan.actions.iter().find_map(|a| match a {
            PlannedAction::Missed { occurrence_ms } => Some(*occurrence_ms),
            _ => None,
        });
        let due = plan.actions.iter().find_map(|a| match a {
            PlannedAction::Due { occurrence_ms } => Some(*occurrence_ms),
            _ => None,
        });
        let mut attempted_expiry = None;
        if let Some(attempted) = routines
            .attempted_slot
            .filter(|slot| route.matches_slot(slot))
        {
            let still_belongs = attempted.occurrence_ms >= header.definition.effective_at_ms
                && header
                    .definition
                    .recurrence
                    .next_after(attempted.occurrence_ms.saturating_sub(1), zone)
                    .map_err(|_| refused("routine_recurrence"))?
                    .is_some_and(|occurrence| occurrence.instant_ms() == attempted.occurrence_ms);
            if !still_belongs {
                // Positive recurrence/effective-time proof, not a newer
                // representative Missed, retires the identity cursor.
                routines.attempted_slot = None;
            } else if now_ms
                >= attempted
                    .occurrence_ms
                    .saturating_add(OCCURRENCE_PRECISION_MS)
            {
                attempted_expiry = Some(attempted.occurrence_ms);
            }
        }
        let Some(occurrence_ms) = attempted_expiry.or(due).or(missed) else {
            return Ok(None);
        };
        let slot = RoutineSlot {
            owner: route.owner,
            bot: route.bot,
            schedule: route.schedule,
            occurrence_ms,
        };
        match routines
            .ledger
            .read_routine_claim(&slot)
            .map_err(ledger_error)?
        {
            RoutineClaimObservation::Claimed { record }
                if !matches!(record.link_state, RoutineLinkState::Held {}) =>
            {
                if routines.attempted_slot == Some(slot) {
                    routines.attempted_slot = None;
                }
                return Ok(None);
            }
            RoutineClaimObservation::Skipped { record } => {
                if routines.attempted_slot == Some(slot) {
                    routines.attempted_slot = None;
                }
                self.project_routine_skip(&route, handle.as_mut(), &record)?;
                return Ok(None);
            }
            _ => {}
        }
        let semantic_hash = koru_delegate_schedule::routine_semantic_digest(
            &slot,
            &header.definition,
            route.workspace,
            owner_handoff,
        )
        .map_err(|_| refused("routine_semantic_identity"))?;
        Ok(Some(RoutineCandidate::Due {
            route,
            location,
            header,
            owner_handoff,
            slot,
            semantic_hash,
            representative_miss: missed.filter(|missed| *missed < occurrence_ms),
        }))
    }

    fn select_routine_recovery(
        &self,
        routines: &mut Routines,
        report: &mut ScheduleEvaluation,
        now_ms: i64,
    ) -> Option<RecoverySelection> {
        let mut held = None;
        match routines.ledger.held_routine_page(routines.held_after) {
            Ok(page) => {
                let mut selected = false;
                for (cursor, entry) in page.entries {
                    match entry {
                        Ok(entry) if entry.route.owner.device == self.device => {
                            held = Some(RecoverySelection {
                                candidate: RoutineCandidate::Recover {
                                    route: entry.route,
                                    record: entry.record,
                                },
                                cursor: RecoveryCursor::Held(cursor),
                                alternate: false,
                            });
                            selected = true;
                            break;
                        }
                        Ok(_) => {}
                        Err(error) => record_failure(report, now_ms, &ledger_error(error)),
                    }
                    routines.held_after = Some(cursor);
                }
                if !selected {
                    routines.held_after = page.next;
                }
                if page.next.is_some() || selected {
                    continue_later(report, now_ms);
                }
            }
            Err(error) => {
                routines.held_after = None;
                record_failure(report, now_ms, &ledger_error(error));
            }
        }
        let mut queued = None;
        match routines.ledger.queued_routine_page(routines.queued_after) {
            Ok(page) => {
                let mut selected = false;
                for (cursor, entry) in page.entries {
                    match entry {
                        Ok(Some(entry))
                            if entry.route.owner.device == self.device
                                && matches!(entry.record.link_state, RoutineLinkState::Held {}) =>
                        {
                            queued = Some(RecoverySelection {
                                candidate: RoutineCandidate::Recover {
                                    route: entry.route,
                                    record: entry.record,
                                },
                                cursor: RecoveryCursor::Queued(cursor),
                                alternate: false,
                            });
                            selected = true;
                            break;
                        }
                        Ok(_) => {}
                        Err(error) => record_failure(report, now_ms, &ledger_error(error)),
                    }
                    routines.queued_after = Some(cursor);
                }
                if !selected {
                    routines.queued_after = page.next;
                }
                if page.next.is_some() || selected {
                    continue_later(report, now_ms);
                }
            }
            Err(error) => {
                routines.queued_after = None;
                record_failure(report, now_ms, &ledger_error(error));
            }
        }
        let both = held.is_some() && queued.is_some();
        let mut chosen = if routines.prefer_queued {
            queued.or(held)
        } else {
            held.or(queued)
        };
        if let Some(selected) = &mut chosen {
            selected.alternate = both;
        }
        chosen
    }
}
