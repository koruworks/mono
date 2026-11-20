// SPDX-License-Identifier: MPL-2.0

//! Retained Routine data and narrow owner creation projections. Execution
//! authority and live Schedule reads belong to their independent owners.
use super::BotError;
pub use koru_delegate_core::BotRoutineCloseReason;
use koru_delegate_core::*;
use koru_delegate_schedule::ScheduleName;
use serde::{Deserialize, Serialize};

mod control;
mod creation;
mod detail;
mod listing;
mod overview;
mod owner;
pub use control::*;
pub use creation::*;
pub use detail::*;
/// Existing recurrence owner, reused without adding an adapter dependency edge.
pub use koru_delegate_schedule::RecurrenceRule as RoutineRecurrenceRule;
pub use koru_delegate_schedule::ScheduleContextPointer as RoutineScheduleContextPointer;
/// Existing neutral definition, re-exported for trusted adapter signatures.
pub use koru_delegate_schedule::ScheduleDefinition as RoutineScheduleDefinition;
/// Existing name owner, reused by trusted in-process setup arguments.
pub use koru_delegate_schedule::ScheduleName as RoutineScheduleName;
/// Existing state owner, reused by the owner controls' replies.
pub use koru_delegate_schedule::ScheduleState as RoutineScheduleState;
pub use listing::*;
pub use overview::*;
pub use owner::*;

fn revision(value: u64) -> bool {
    value > 0 && value <= i64::MAX as u64
}
/// Count exact serde bytes without allocating the complete encoded body.
fn bounded<T: Serialize>(value: &T, limit: usize) -> Result<(), BotError> {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| std::io::Error::other("Routine value too large"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Count(limit), value).map_err(|_| BotError::Invalid)
}

#[cfg(test)]
mod tests;
