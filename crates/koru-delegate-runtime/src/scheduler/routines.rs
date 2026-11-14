// SPDX-License-Identifier: MPL-2.0

//! Bounded scheduler composition. Retained claims never imply provider launch.
use super::*;
use crate::{
    RoutineBridge, RoutineBridgeFailure, RoutineCandidate, RoutineLedger,
    RoutinePreparationObservation, RoutinePreparedReply, RoutineWork, ScheduleCatalog,
    SchedulePhaseGate,
};
use koru_delegate_core::*;
use koru_delegate_sqlite::{RoutineHeldCursor, RoutineQueuedCursor, RoutineSlotCursor};
use std::time::{Duration, Instant};

mod execution;
mod publication;
mod selection;

const PAGE: usize = 16;
const CLEANUP_BUDGET: Duration = Duration::from_secs(5);

pub(super) struct Routines {
    ledger: Box<dyn RoutineLedger>,
    bridge: Box<dyn RoutineBridge>,
    phases: Box<dyn SchedulePhaseGate>,
    serial: u64,
    outstanding: Option<u64>,
    evaluated_at_ms: i64,
    prefer_recovery: bool,
    prefer_queued: bool,
    due_after: Option<ScheduleId>,
    due_scanning: bool,
    due_deadline_ms: Option<i64>,
    // One attempted slot, not a draft/retry queue. It only lets the owning
    // recurrence planner revisit exact expiry after unknown preparation.
    attempted_slot: Option<RoutineSlot>,
    held_after: Option<RoutineHeldCursor>,
    queued_after: Option<RoutineQueuedCursor>,
    repair: Repair,
}

struct RepairRoute {
    route: RoutineRouteIdentity,
    location: ScheduleEventRef,
    after: Option<RoutineSlotCursor>,
}
struct Repair {
    active: bool,
    again: bool,
    anchor: Option<ScheduleId>,
    last: Option<ScheduleId>,
    current: Option<RepairRoute>,
}
impl Default for Repair {
    fn default() -> Self {
        Self {
            active: true,
            again: false,
            anchor: None,
            last: None,
            current: None,
        }
    }
}
impl Repair {
    fn wake(&mut self) {
        if self.active {
            self.again = true;
        } else {
            *self = Self::default();
        }
    }
    fn finish_pass(&mut self) {
        let again = self.again;
        *self = Self::default();
        self.active = again;
    }
}

fn refused(code: &'static str) -> PortError {
    PortError::new(
        Port::Schedules,
        code,
        "Routine scheduling requires exact retained authority",
    )
}
fn bridge_error(failure: RoutineBridgeFailure) -> PortError {
    match failure {
        RoutineBridgeFailure::NotSent(error)
        | RoutineBridgeFailure::Unknown(error)
        | RoutineBridgeFailure::Refused(error) => error,
    }
}
fn ledger_error(error: koru_delegate_sqlite::BotLedgerError) -> PortError {
    use koru_delegate_sqlite::BotLedgerError;
    let code = match error {
        BotLedgerError::Ledger(error) => return error.into(),
        BotLedgerError::Invalid => "routine_invalid",
        BotLedgerError::Conflict => "routine_conflict",
        BotLedgerError::Capacity => "routine_capacity",
        BotLedgerError::Stale => "routine_stale",
        BotLedgerError::Fenced => "routine_fenced",
    };
    PortError::new(Port::Store, code, "Routine ledger authority is unavailable")
}
fn continue_later(report: &mut ScheduleEvaluation, now_ms: i64) {
    report.next_deadline_ms =
        min_option(report.next_deadline_ms, Some(now_ms.saturating_add(1_000)));
}
fn route_location(route: &RoutineRouteIdentity) -> ScheduleEventRef {
    ScheduleEventRef {
        schedule: route.schedule,
        workspace: route.workspace,
        document: DocumentId::from_uuid(route.schedule.as_uuid()),
    }
}
fn route_header(
    route: &RoutineRouteIdentity,
    location: ScheduleEventRef,
    header: &ScheduleHeader,
) -> bool {
    route.valid()
        && location == route_location(route)
        && header.schedule == route.schedule
        && header.definition.routine.as_ref().is_some_and(|a| {
            a.owner == route.owner
                && a.bot == route.bot
                && a.home == route.workspace
                && a.creation_request == route.creation_request
        })
}

impl Routines {
    fn route_at(&self, location: ScheduleEventRef) -> PortResult<Option<RoutineRouteIdentity>> {
        let authored = self
            .ledger
            .routine_route(location.schedule)
            .map_err(ledger_error)?;
        let physical_id = ScheduleId::from_uuid(location.document.as_uuid());
        let physical = if physical_id == location.schedule {
            authored.clone()
        } else {
            self.ledger
                .routine_route(physical_id)
                .map_err(ledger_error)?
        };
        match (authored, physical) {
            (None, None) => Ok(None),
            (Some(authored), Some(physical))
                if authored == physical
                    && authored.valid()
                    && route_location(&authored) == location =>
            {
                Ok(Some(authored))
            }
            _ => Err(refused("routine_physical_route_mismatch")),
        }
    }
    fn new(
        ledger: Box<dyn RoutineLedger>,
        bridge: Box<dyn RoutineBridge>,
        phases: Box<dyn SchedulePhaseGate>,
    ) -> Self {
        Self {
            ledger,
            bridge,
            phases,
            serial: 0,
            outstanding: None,
            evaluated_at_ms: 0,
            prefer_recovery: false,
            prefer_queued: false,
            due_after: None,
            due_scanning: false,
            due_deadline_ms: None,
            attempted_slot: None,
            held_after: None,
            queued_after: None,
            repair: Repair::default(),
        }
    }
    pub(super) fn begin_evaluation(&mut self, now_ms: i64) -> PortResult<()> {
        if !self.due_scanning {
            self.due_after = None;
            self.due_deadline_ms = None;
            self.due_scanning = true;
        }
        self.outstanding = None;
        self.serial = self
            .serial
            .checked_add(1)
            .ok_or_else(|| refused("routine_serial_exhausted"))?;
        self.evaluated_at_ms = now_ms;
        Ok(())
    }
    fn live(&self, deadline: Instant) -> PortResult<()> {
        if self.bridge.stopped() || Instant::now() >= deadline {
            Err(refused("routine_phase_expired"))
        } else {
            Ok(())
        }
    }
    fn route_current(&self, route: &RoutineRouteIdentity) -> PortResult<()> {
        if !route.valid()
            || self
                .ledger
                .routine_route(route.schedule)
                .map_err(ledger_error)?
                .as_ref()
                != Some(route)
        {
            return Err(refused("routine_route_mismatch"));
        }
        Ok(())
    }
}

impl ScheduleCoordinator {
    pub fn with_routines(
        mut self,
        ledger: Box<dyn RoutineLedger>,
        bridge: Box<dyn RoutineBridge>,
        phases: Box<dyn SchedulePhaseGate>,
    ) -> Self {
        self.routines = Some(Routines::new(ledger, bridge, phases));
        self
    }

    /// A real/coalesced wake invalidates only bounded traversal state. No work
    /// body or timer is allocated; a wake during a pass requests one more pass.
    pub fn wake_routine_repair(&mut self) {
        if let Some(routines) = &mut self.routines {
            routines.repair.wake();
        }
    }

    pub fn evaluate_routine_work(
        &mut self,
        work: RoutineWork,
        effects: &mut dyn ScheduleEffects,
        deadline: Instant,
    ) -> PortResult<ScheduleEvaluation> {
        let mut routines = self
            .routines
            .take()
            .ok_or_else(|| refused("routine_owner_unavailable"))?;
        let result = if routines.outstanding.take() != Some(work.evaluation_serial)
            || routines.serial != work.evaluation_serial
        {
            Err(refused("routine_stale_work"))
        } else {
            let mut report = ScheduleEvaluation::default();
            self.run_routine_work(
                &mut routines,
                work.candidate,
                effects,
                deadline,
                &mut report,
            )
            .map(|()| report)
        };
        self.routines = Some(routines);
        result
    }
}
