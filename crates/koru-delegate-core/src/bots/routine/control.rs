// SPDX-License-Identifier: MPL-2.0

//! Neutral owner-control vocabulary (delegate IPC 17). These values describe
//! a request and its answer; decoding one never authorizes a Schedule write
//! or a claim. The coordinator that owns the Schedule document decides.
use super::*;

/// The encoded bound of one owner control or owner Test, command or reply.
/// The frame bound is separate and unchanged.
pub const ROUTINE_CONTROL_WIRE_LIMIT: usize = 4 * 1024;
/// One occurrence slot. A Test names the start of the delegate's current one.
pub const ROUTINE_TEST_MINUTE_MS: i64 = 60_000;
/// A Test candidate is dropped, with no write, once less than this remains of
/// its minute, so the ledger's late branch is not reached by a slow claim.
pub const ROUTINE_TEST_CLAIM_MARGIN_MS: i64 = 2_000;

/// Whether one revision-checked definition commit wrote, or was the exact
/// replay of the commit a lost reply already made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RoutineControlOutcome {
    Applied,
    AlreadyApplied,
}

/// The lifecycle a control left the Routine in. The pause reason is not
/// repeated: an owner control only ever writes a manual pause.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RoutineControlLifecycle {
    Active,
    Paused,
}

/// A Schedule definition revision a caller may name: 1..=u32::MAX.
pub const fn routine_control_revision_valid(expected_revision: u32) -> bool {
    expected_revision > 0
}

/// A Test minute is a whole, non-negative minute. Whether it is the current
/// one is the delegate clock's answer, never the caller's.
pub const fn routine_test_minute_valid(occurrence_ms: i64) -> bool {
    occurrence_ms >= 0 && occurrence_ms % ROUTINE_TEST_MINUTE_MS == 0
}

/// The start of the minute that holds `now_ms`.
pub const fn routine_test_minute_of(now_ms: i64) -> i64 {
    now_ms - now_ms.rem_euclid(ROUTINE_TEST_MINUTE_MS)
}

/// Encode into a counter that stops at the bound, so an oversized value is
/// refused while it is encoded and without buffering it.
pub fn routine_control_within_wire_limit<T: Serialize + ?Sized>(value: &T) -> bool {
    struct Bounded(usize);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| std::io::Error::other("routine control bound"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Bounded(ROUTINE_CONTROL_WIRE_LIMIT), value).is_ok()
}
