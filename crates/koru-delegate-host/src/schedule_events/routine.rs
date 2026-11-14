// SPDX-License-Identifier: MPL-2.0

//! One publication boundary for live preparation, repair, ready and delivery.
use super::*;
use koru_delegate_core::{RoutineClaimObservation, RoutineSkipOutcome, RoutineSlot};
use koru_delegate_runtime::RoutineLedger;

impl BusScheduleEvents {
    fn occurrence_route(
        &self,
        schedule: ScheduleEventRef,
    ) -> PortResult<Option<koru_delegate_core::RoutineRouteIdentity>> {
        let authored = self
            .ledger
            .routine_route(schedule.schedule)
            .map_err(|error| Self::error("schedule_event_route", error))?;
        let physical_id = ScheduleId::from_uuid(schedule.document.as_uuid());
        let physical = if physical_id == schedule.schedule {
            authored.clone()
        } else {
            self.ledger
                .routine_route(physical_id)
                .map_err(|error| Self::error("schedule_event_route", error))?
        };
        match (authored, physical) {
            (None, None) => Ok(None),
            (Some(authored), Some(physical))
                if authored == physical
                    && authored.valid()
                    && authored.workspace == schedule.workspace
                    && authored.schedule == schedule.schedule
                    && authored.schedule.as_uuid() == schedule.document.as_uuid() =>
            {
                Ok(Some(authored))
            }
            _ => Err(Self::error(
                "schedule_event_route",
                "Physical and authored Routine identities differ",
            )),
        }
    }
    pub(super) fn validate_known_header(
        &self,
        schedule: ScheduleEventRef,
        header: &ScheduleHeader,
    ) -> PortResult<()> {
        let route = self.occurrence_route(schedule)?;
        if header.schedule != schedule.schedule
            || header.definition.routine.is_some() && route.is_none()
            || route.as_ref().is_some_and(|route| {
                route.workspace != schedule.workspace
                    || schedule.document.as_uuid() != route.schedule.as_uuid()
            })
        {
            return Err(Self::error(
                "schedule_event_route",
                "Schedule association is not an ordinary negative",
            ));
        }
        Ok(())
    }

    pub(super) fn validate_occurrence_authority(
        &self,
        fact: ScheduleOccurrenceEvent,
    ) -> PortResult<()> {
        let route = self.occurrence_route(fact.schedule)?;
        let Some(route) = route else {
            // Absence alone cannot erase retained link/admission/origin markers.
            return self
                .ledger
                .validate_ordinary_schedule_run(fact.run)
                .map_err(|error| Self::error("schedule_event_run", error));
        };
        if route.workspace != fact.schedule.workspace
            || route.schedule.as_uuid() != fact.schedule.document.as_uuid()
            || fact.outcome == ScheduleFiringOutcome::Fired
        {
            return Err(Self::error(
                "schedule_event_routine",
                "Routine has no durable launch publication authority",
            ));
        }
        let slot = RoutineSlot {
            owner: route.owner,
            bot: route.bot,
            schedule: route.schedule,
            occurrence_ms: fact.occurrence_ms,
        };
        let observed = self
            .ledger
            .read_routine_claim(&slot)
            .map_err(|error| Self::error("schedule_event_routine", error))?;
        let RoutineClaimObservation::Skipped { record } = observed else {
            return Err(Self::error(
                "schedule_event_routine",
                "Routine occurrence is not a retained skip",
            ));
        };
        let exact = record.slot == slot
            && record.definition_revision == fact.revision
            && record.planned_run == fact.run
            && record.recorded_at_ms == fact.claimed_at_ms
            && match record.outcome {
                RoutineSkipOutcome::SkippedMissed {} => {
                    fact.outcome == ScheduleFiringOutcome::SkippedMissed
                        && fact.busy_holder.is_none()
                }
                RoutineSkipOutcome::SkippedBusy { holder } => {
                    fact.outcome == ScheduleFiringOutcome::SkippedBusy
                        && fact.busy_holder == Some(holder)
                }
            };
        if exact {
            Ok(())
        } else {
            Err(Self::error(
                "schedule_event_routine",
                "Routine skip differs from its retained authority",
            ))
        }
    }

    pub(super) fn validate_publication_authority(
        &self,
        record: &SchedulePublicationRecord,
    ) -> PortResult<()> {
        let occurrence_ms = match record.fact {
            SchedulePublicationFact::Fired { occurrence_ms, .. }
            | SchedulePublicationFact::Skipped { occurrence_ms, .. } => occurrence_ms,
            _ => return Ok(()),
        };
        let claim = self
            .ledger
            .schedule_slot(record.schedule.schedule, occurrence_ms)
            .map_err(|error| Self::error("schedule_event_ledger", error))?
            .ok_or_else(|| {
                Self::error(
                    "schedule_event_authority",
                    "Occurrence has no retained slot",
                )
            })?;
        let schedule = Self::event_ref(record.schedule);
        let actual = Self::occurrence_from_claim(schedule, &claim)
            .or_else(|| Self::pending_fired_from_claim(schedule, &claim))
            .ok_or_else(|| {
                Self::error(
                    "schedule_event_authority",
                    "Occurrence has no publication fact",
                )
            })?;
        self.validate_occurrence_authority(actual)?;
        let root = self
            .root_record(schedule.schedule)?
            .ok_or_else(|| Self::error("schedule_event_root", "Occurrence has no created root"))?;
        if Self::stored_matches_occurrence(record, root.event, actual)? {
            Ok(())
        } else {
            Err(Self::error(
                "schedule_event_authority",
                "Stored occurrence differs from retained slot",
            ))
        }
    }
}
