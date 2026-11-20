// SPDX-License-Identifier: MPL-2.0
//! Bounded deadline planning shared by document schedules and engine Routines.
use crate::{EvaluationError, OCCURRENCE_PRECISION_MS, OccurrenceTiming, RecurrenceRule};
use koru_core::time::ZoneRules;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlannedAction {
    Missed { occurrence_ms: i64 },
    Due { occurrence_ms: i64 },
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SchedulePlan {
    /// At most one representative miss and one due firing, even after months away.
    pub actions: Vec<PlannedAction>,
    pub next_deadline_ms: Option<i64>,
}

/// The former delegate planner, independent of its document/template wrapper.
/// The effective time is inclusive; recurrence exploration is strictly after.
pub fn plan_schedule<R: ZoneRules + ?Sized>(
    recurrence: &RecurrenceRule,
    effective_at_ms: i64,
    observed_through_ms: Option<i64>,
    now_ms: i64,
    floating_zone: &R,
) -> Result<SchedulePlan, EvaluationError> {
    let floor = effective_at_ms.saturating_sub(1);
    let cursor = observed_through_ms.map(|at| at.max(floor)).unwrap_or(floor);
    let Some(first) = recurrence.next_after(cursor, floating_zone)? else {
        return Ok(SchedulePlan::default());
    };
    match first.timing_at(now_ms) {
        OccurrenceTiming::Upcoming => Ok(SchedulePlan {
            actions: vec![],
            next_deadline_ms: Some(first.instant_ms()),
        }),
        OccurrenceTiming::Due => Ok(SchedulePlan {
            actions: vec![PlannedAction::Due {
                occurrence_ms: first.instant_ms(),
            }],
            next_deadline_ms: recurrence
                .next_after(first.instant_ms(), floating_zone)?
                .map(|at| at.instant_ms()),
        }),
        OccurrenceTiming::Missed => {
            let mut actions = Vec::with_capacity(2);
            let minute = now_ms.saturating_sub(now_ms.rem_euclid(OCCURRENCE_PRECISION_MS));
            if let Some(current) = recurrence.next_after(minute.saturating_sub(1), floating_zone)?
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
                next_deadline_ms: recurrence
                    .next_after(next_reference, floating_zone)?
                    .map(|at| at.instant_ms()),
            })
        }
    }
}
