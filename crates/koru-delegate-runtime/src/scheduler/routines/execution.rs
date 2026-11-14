// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_delegate_document::RoutineExpiryPlan;
use koru_delegate_schedule::routine_definition_digest;

impl ScheduleCoordinator {
    pub(super) fn run_routine_work(
        &mut self,
        routines: &mut Routines,
        candidate: RoutineCandidate,
        effects: &mut dyn ScheduleEffects,
        deadline: Instant,
        report: &mut ScheduleEvaluation,
    ) -> PortResult<()> {
        routines.live(deadline)?;
        match candidate {
            RoutineCandidate::Recover { route, record } => {
                self.recover_routine(routines, &route, record, deadline, report)
            }
            RoutineCandidate::Due {
                route,
                location,
                header,
                owner_handoff,
                slot,
                semantic_hash,
                representative_miss,
            } => {
                routines.route_current(&route)?;
                // A stored winner requires neither a live due window nor a new
                // preparation/admission proof. Never mix its IDs with a loser.
                let observed = routines
                    .ledger
                    .read_routine_claim(&slot)
                    .map_err(ledger_error)?;
                if !matches!(observed, RoutineClaimObservation::Absent { .. }) {
                    if routines.attempted_slot == Some(slot) {
                        routines.attempted_slot = None;
                    }
                    return self.routine_observation(routines, &route, observed, deadline, report);
                }
                let now = self.clock.now_ms(routines.evaluated_at_ms);
                if now >= slot.occurrence_ms.saturating_add(OCCURRENCE_PRECISION_MS) {
                    return self.expire_routine(
                        routines,
                        &route,
                        location,
                        &header,
                        owner_handoff,
                        slot,
                        representative_miss,
                        None,
                        deadline,
                        report,
                    );
                }
                if now < slot.occurrence_ms {
                    continue_later(report, routines.evaluated_at_ms);
                    return Ok(());
                }
                routines.attempted_slot.get_or_insert(slot);
                let preparation = routines
                    .bridge
                    .read_preparation(&slot, semantic_hash, deadline);
                routines.live(deadline)?;
                let prepared = match preparation.map_err(bridge_error)? {
                    RoutinePreparationObservation::Prepared(prepared) => prepared,
                    RoutinePreparationObservation::Missing => {
                        let reply = routines.bridge.prepare(
                            &slot,
                            route.workspace,
                            &header.definition,
                            owner_handoff,
                            deadline,
                        );
                        routines.live(deadline)?;
                        match reply.map_err(bridge_error)? {
                            RoutinePreparedReply::Prepared(prepared) => prepared,
                            RoutinePreparedReply::Closed { .. } => {
                                return Err(refused("routine_preparation_closed"));
                            }
                        }
                    }
                    RoutinePreparationObservation::Linked { .. } => {
                        let observed = routines
                            .ledger
                            .read_routine_claim(&slot)
                            .map_err(ledger_error)?;
                        if matches!(observed, RoutineClaimObservation::Absent { .. }) {
                            return Err(refused("routine_link_without_claim"));
                        }
                        return self
                            .routine_observation(routines, &route, observed, deadline, report);
                    }
                    RoutinePreparationObservation::Closed { .. } => {
                        return Err(refused("routine_preparation_closed"));
                    }
                };
                let definition = RoutineDefinitionIdentity {
                    revision: header.definition.revision,
                    digest: routine_definition_digest(
                        &header.definition,
                        route.workspace,
                        owner_handoff,
                    )
                    .map_err(|_| refused("routine_definition_identity"))?,
                    owner_handoff,
                };
                if !prepared.valid()
                    || prepared.slot != slot
                    || prepared.definition != definition
                    || prepared.semantic_hash != semantic_hash
                    || prepared.submission.workspace != route.workspace
                    || !matches!(
                        prepared.submission.request.scope,
                        WorkspaceScope::BotFolder { .. }
                    )
                {
                    return Err(refused("routine_prepared_mismatch"));
                }
                let proof = routines.bridge.synchronize_preclaim(&prepared, deadline);
                routines.live(deadline)?;
                let (admission, proof) = proof.map_err(bridge_error)?;
                let run = prepared.ids.run;
                let authored = prepared.submission.request.scope.clone();
                let scope = self
                    .preparation
                    .resolve_scope(run, &authored)
                    .map_err(|_| refused("routine_scope_unavailable"))?;
                // BotFolder resolution only checks the existing directory; it
                // allocates no temporary candidate. Still honor the port's
                // cleanup hook on every failure after successful resolution.
                let opened = (|| {
                    routines.live(deadline)?;
                    // Store's inherited join bound is not the HTTP slice.
                    let handle = self.schedules.open(location.workspace, location.document)?;
                    routines.live(deadline)?;
                    let guard = routines.phases.enter_until(deadline)?;
                    routines.live(deadline)?;
                    Ok((handle, guard))
                })();
                let (mut handle, guard) = match opened {
                    Ok(opened) => opened,
                    Err(error) => {
                        self.preparation.discard_candidate(run, &authored)?;
                        return Err(error);
                    }
                };
                let admission_result = effects.begin_routine(
                    &prepared,
                    &admission,
                    &proof,
                    scope.identity.clone(),
                    authored.clone(),
                    deadline,
                );
                let result = match admission_result {
                    Ok(ScheduleAdmission::Busy(_)) => {
                        drop(handle);
                        drop(guard);
                        self.preparation.discard_candidate(run, &authored)?;
                        continue_later(report, routines.evaluated_at_ms);
                        return Ok(());
                    }
                    Ok(ScheduleAdmission::Granted) => (|| {
                        routines.live(deadline)?;
                        handle.absorb()?;
                        routines.live(deadline)?;
                        let live = handle.document().header()?;
                        if !route_header(&route, location, &live) {
                            return Err(refused("routine_live_route"));
                        }
                        let authority = handle
                            .claim_authority(
                                self.device,
                                ScheduleClaimPlan::BotRoutine {
                                    prepared,
                                    representative_miss: representative_miss.map(|occurrence_ms| {
                                        RoutineRepresentativeMiss {
                                            occurrence_ms,
                                            planned_run: RunId::mint(),
                                        }
                                    }),
                                },
                            )?
                            .ok_or_else(|| refused("routine_live_authority"))?;
                        routines.live(deadline)?;
                        routines
                            .ledger
                            .claim_bot_routine(authority, &admission, &proof)
                            .map_err(ledger_error)
                    })(),
                    Err(error) => Err(error),
                };
                drop(handle);
                drop(guard);
                drop(scope);
                // Cleanup has its own finite deadline. It cannot extend Ready
                // authority, and a missing acknowledgement prevents all HTTP.
                effects.release_routine(run, Instant::now() + CLEANUP_BUDGET)?;
                self.preparation.discard_candidate(run, &authored)?;
                let (observed, representative) = result?;
                if routines.attempted_slot == Some(slot) {
                    routines.attempted_slot = None;
                }
                routines.repair.wake();
                routines.live(deadline)?;
                if let Some(record) = representative {
                    self.publish_routine_skip(routines, &route, &record, deadline)?;
                }
                self.routine_observation(routines, &route, observed, deadline, report)
            }
        }
    }

    fn expire_routine(
        &mut self,
        routines: &mut Routines,
        route: &RoutineRouteIdentity,
        location: ScheduleEventRef,
        header: &ScheduleHeader,
        owner_handoff: Option<HandoffId>,
        slot: RoutineSlot,
        representative: Option<i64>,
        run: Option<RunId>,
        deadline: Instant,
        report: &mut ScheduleEvaluation,
    ) -> PortResult<()> {
        let mut handle = self.schedules.open(location.workspace, location.document)?;
        routines.live(deadline)?;
        let guard = routines.phases.enter_until(deadline)?;
        handle.absorb()?;
        routines.live(deadline)?;
        let live = handle.document().header()?;
        if !route_header(route, location, &live)
            || live.definition != header.definition
            || handle.document().handoff_state()?.epoch != owner_handoff
        {
            return Err(refused("routine_expiry_changed"));
        }
        let authority = handle
            .expired_routine_authority(
                self.device,
                RoutineExpiryPlan {
                    occurrence_ms: slot.occurrence_ms,
                    planned_run: run.unwrap_or_else(RunId::mint),
                    representative_miss: representative.map(|occurrence_ms| {
                        RoutineRepresentativeMiss {
                            occurrence_ms,
                            planned_run: RunId::mint(),
                        }
                    }),
                },
            )?
            .ok_or_else(|| refused("routine_expiry_authority"))?;
        routines.live(deadline)?;
        let (observed, representative) = routines
            .ledger
            .record_expired_routine(authority)
            .map_err(ledger_error)?;
        if routines.attempted_slot == Some(slot) {
            routines.attempted_slot = None;
        }
        drop(handle);
        drop(guard);
        routines.repair.wake();
        routines.live(deadline)?;
        if let Some(record) = representative {
            self.publish_routine_skip(routines, route, &record, deadline)?;
        }
        self.routine_observation(routines, route, observed, deadline, report)
    }

    fn routine_observation(
        &mut self,
        routines: &mut Routines,
        route: &RoutineRouteIdentity,
        observed: RoutineClaimObservation,
        deadline: Instant,
        report: &mut ScheduleEvaluation,
    ) -> PortResult<()> {
        if !observed.valid() || !route.matches_slot(observed.slot()) {
            return Err(refused("routine_observation_identity"));
        }
        match observed {
            RoutineClaimObservation::Claimed { record } => {
                self.recover_routine(routines, route, record, deadline, report)
            }
            RoutineClaimObservation::Skipped { record } => {
                self.publish_routine_skip(routines, route, &record, deadline)
            }
            RoutineClaimObservation::Absent { .. } => Err(refused("routine_claim_absent")),
        }
    }

    fn recover_routine(
        &mut self,
        routines: &mut Routines,
        route: &RoutineRouteIdentity,
        record: RoutineClaimRecord,
        deadline: Instant,
        report: &mut ScheduleEvaluation,
    ) -> PortResult<()> {
        routines.live(deadline)?;
        routines.route_current(route)?;
        if !record.valid() || !route.matches_slot(&record.claim.slot) {
            return Err(refused("routine_recovery_identity"));
        }
        let read_current = |routines: &Routines| -> PortResult<RoutineClaimRecord> {
            match routines
                .ledger
                .read_routine_claim(&record.claim.slot)
                .map_err(ledger_error)?
            {
                RoutineClaimObservation::Claimed { record: current }
                    if current.claim == record.claim
                        && current.submission == record.submission
                        && record.link_state.permits(current.link_state) =>
                {
                    Ok(current)
                }
                _ => Err(refused("routine_recovery_changed")),
            }
        };
        let current = read_current(routines)?;
        if matches!(current.link_state, RoutineLinkState::Terminal { .. }) {
            return Ok(());
        }
        let read = routines.bridge.read_link(&record.claim, deadline);
        routines.live(deadline)?;
        let linked = read.map_err(bridge_error)?;
        let current = read_current(routines)?;
        if matches!(current.link_state, RoutineLinkState::Terminal { .. }) {
            return Ok(());
        }
        if matches!(current.link_state, RoutineLinkState::Linked {})
            && linked == Some(RoutineLinkOutcome::Linked {})
        {
            return Ok(());
        }
        // Positive AI state is not a native acknowledgement. The AI owner must
        // replay its exact Finalize; the scheduler never constructs an ack.
        let finalized = routines.bridge.finalize(&record.claim, deadline);
        routines.live(deadline)?;
        let outcome = finalized.map_err(bridge_error)?;
        let current = read_current(routines)?;
        if current.link_state != outcome.state() {
            continue_later(report, routines.evaluated_at_ms);
            return Err(refused("routine_ack_pending"));
        }
        Ok(())
    }
}
