// SPDX-License-Identifier: MPL-2.0

//! Retained Routine data and narrow owner creation projections. Execution
//! authority and live Schedule reads belong to their independent owners.
use super::{Bot, BotConfig, BotError, BotLibraryCapture, validate_config};
pub use koru_delegate_core::BotRoutineCloseReason;
use koru_delegate_core::*;
use koru_delegate_schedule::{
    RoutineAssociation, ScheduleDefinition, ScheduleHeader, ScheduleLifecycle, ScheduleName,
};
use serde::{Deserialize, Serialize};

mod creation;
mod owner;
mod preparation;
pub use creation::*;
use koru_delegate_schedule::routine_wire as strict;
pub use owner::*;
pub use preparation::*;

pub const BOT_ROUTINE_SCOPE: &str = "koru.bot.routine.v1";
pub const BOT_ROUTINE_SEED_BYTES: usize = 4 * 1024 * 1024;
pub const BOT_ROUTINE_SUBMISSION_BYTES: usize = 4 * 1024 * 1024;
pub const BOT_ROUTINE_CAPTURE_BYTES: usize = 1024 * 1024;
pub const BOT_ROUTINE_GRANT_BYTES: usize = 65536;
pub const BOT_ROUTINE_RECEIPT_BYTES: usize = 16384;
pub const BOT_ROUTINE_VV_BYTES: usize = 65536;

fn revision(value: u64) -> bool {
    value > 0 && value <= i64::MAX as u64
}
fn v4(value: uuid::Uuid) -> bool {
    value.get_version() == Some(uuid::Version::Random)
        && value.get_variant() == uuid::Variant::RFC4122
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
