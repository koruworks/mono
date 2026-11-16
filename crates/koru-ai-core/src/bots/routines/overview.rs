// SPDX-License-Identifier: MPL-2.0
//! Trusted private read port; deliberately not a serialized/public list DTO.
use koru_delegate_schedule::{RecurrenceRule, ScheduleName, ScheduleState};

#[derive(Clone, PartialEq, Eq)]
pub enum RoutineOverviewObservation {
    Current {
        name: ScheduleName,
        definition_revision: u32,
        state: ScheduleState,
        /// Timing, so a row can say when the Routine is next due.
        effective_at_ms: i64,
        recurrence: RecurrenceRule,
    },
    /// Authorized live visibility returned absent or direct/inherited Trash.
    NotLive,
    /// Required visible-document evidence could not be projected faithfully.
    Unavailable,
}
impl std::fmt::Debug for RoutineOverviewObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RoutineOverviewObservation([REDACTED])")
    }
}
