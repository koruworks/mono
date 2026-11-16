// SPDX-License-Identifier: MPL-2.0

//! The owner Test candidate. It follows the due path's order after selection
//! (preparation, preclaim, the phase gate, the live authority, the claim) but
//! shares none of its entry conditions:
//!
//! - it exists only as the one slot an accepted `TestRoutine` left in memory;
//! - it mints a [`koru_delegate_document::RoutineTestAuthority`] and claims
//!   through `claim_bot_routine_test`, never the ordinary authority or method;
//! - it never expires a slot and never reads or writes `attempted_slot`;
//! - once fewer than two seconds of its minute remain, or the live definition
//!   is no longer the one it was accepted under, it is dropped with no ledger
//!   write and no document write.
use super::control::test_window_closed;
use super::*;

impl Routines {
    /// A refusal the AI owner actually answered ends the Test. A request that
    /// was not sent, or whose answer is unknown, leaves it for the next pass.
    fn test_bridge<T>(&mut self, result: crate::RoutineBridgeResult<T>) -> PortResult<T> {
        result.map_err(|failure| {
            if matches!(failure, RoutineBridgeFailure::Refused(_)) {
                self.test = None;
            }
            bridge_error(failure)
        })
    }
}

impl ScheduleCoordinator {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_routine_test(
        &mut self,
        routines: &mut Routines,
        route: RoutineRouteIdentity,
        location: ScheduleEventRef,
        header: ScheduleHeader,
        owner_handoff: Option<HandoffId>,
        slot: RoutineSlot,
        semantic_hash: RoutineDigest32,
        effects: &mut dyn ScheduleEffects,
        deadline: Instant,
        report: &mut ScheduleEvaluation,
    ) -> PortResult<()> {
        // Work that no longer names the outstanding slot runs nothing. This is
        // also what makes a Test single-shot per acceptance.
        if !routines.test.as_ref().is_some_and(|test| {
            test.route == route
                && test.location == location
                && test.slot == slot
                && test.header == header
                && test.owner_handoff == owner_handoff
                && test.semantic_hash == semantic_hash
        }) {
            return Err(refused("routine_test_not_outstanding"));
        }
        if let Err(error) = routines.route_current(&route) {
            routines.test = None;
            return Err(error);
        }
        // A stored winner or skip needs no live window and no new proof.
        let observed = routines
            .ledger
            .read_routine_claim(&slot)
            .map_err(ledger_error)?;
        if !matches!(observed, RoutineClaimObservation::Absent { .. }) {
            routines.test = None;
            return self.routine_observation(routines, &route, observed, deadline, report);
        }
        let now = self.clock.now_ms(routines.evaluated_at_ms);
        if test_window_closed(now, slot.occurrence_ms) {
            // Dropped: nothing was claimed and nothing is recorded.
            routines.test = None;
            return Ok(());
        }
        if now < slot.occurrence_ms {
            continue_later(report, routines.evaluated_at_ms);
            return Ok(());
        }
        let preparation = routines
            .bridge
            .read_preparation(&slot, semantic_hash, deadline);
        routines.live(deadline)?;
        let prepared = match routines.test_bridge(preparation)? {
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
                match routines.test_bridge(reply)? {
                    RoutinePreparedReply::Prepared(prepared) => prepared,
                    RoutinePreparedReply::Closed { .. } => {
                        routines.test = None;
                        return Err(refused("routine_preparation_closed"));
                    }
                }
            }
            RoutinePreparationObservation::Linked { .. } => {
                routines.test = None;
                let observed = routines
                    .ledger
                    .read_routine_claim(&slot)
                    .map_err(ledger_error)?;
                if matches!(observed, RoutineClaimObservation::Absent { .. }) {
                    return Err(refused("routine_link_without_claim"));
                }
                return self.routine_observation(routines, &route, observed, deadline, report);
            }
            RoutinePreparationObservation::Closed { .. } => {
                routines.test = None;
                return Err(refused("routine_preparation_closed"));
            }
        };
        let definition = definition_identity(&header.definition, route.workspace, owner_handoff)?;
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
            routines.test = None;
            return Err(refused("routine_prepared_mismatch"));
        }
        let proof = routines.bridge.synchronize_preclaim(&prepared, deadline);
        routines.live(deadline)?;
        let (admission, proof) = routines.test_bridge(proof)?;
        let run = prepared.ids.run;
        let authored = prepared.submission.request.scope.clone();
        let scope = match self.preparation.resolve_scope(run, &authored) {
            Ok(scope) => scope,
            Err(_) => {
                routines.test = None;
                return Err(refused("routine_scope_unavailable"));
            }
        };
        let opened = (|| {
            routines.live(deadline)?;
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
        let device = self.device;
        let evaluated_at_ms = routines.evaluated_at_ms;
        // `Ok(None)` is a clean drop: the Test's conditions no longer hold and
        // nothing was written.
        let result = match admission_result {
            Ok(ScheduleAdmission::Busy(_)) => {
                drop(handle);
                drop(guard);
                self.preparation.discard_candidate(run, &authored)?;
                // The scope is held by other work. The Test stays outstanding
                // until its minute's margin; it is never carried past it.
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
                if live.definition != header.definition
                    || handle.document().handoff_state()?.epoch != owner_handoff
                    || test_window_closed(self.clock.now_ms(evaluated_at_ms), slot.occurrence_ms)
                {
                    return Ok(None);
                }
                let authority = handle.test_claim_authority(device, prepared)?;
                routines.live(deadline)?;
                routines
                    .ledger
                    .claim_bot_routine_test(authority, &admission, &proof)
                    .map_err(ledger_error)
                    .map(Some)
            })(),
            Err(error) => Err(error),
        };
        drop(handle);
        drop(guard);
        drop(scope);
        effects.release_routine(run, Instant::now() + CLEANUP_BUDGET)?;
        self.preparation.discard_candidate(run, &authored)?;
        // Past this point the Test is resolved either way: claimed, skipped,
        // cleanly dropped or refused at the claim. It is not attempted again.
        routines.test = None;
        let Some((observed, representative)) = result? else {
            return Ok(());
        };
        routines.repair.wake();
        routines.live(deadline)?;
        if let Some(record) = representative {
            self.publish_routine_skip(routines, &route, &record, deadline)?;
        }
        self.routine_observation(routines, &route, observed, deadline, report)
    }
}
