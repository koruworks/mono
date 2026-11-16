// SPDX-License-Identifier: MPL-2.0

//! Delegate IPC 17 at the scheduler thread: the wire shapes of the owner
//! control and the owner Test, mapped onto the coordinator that owns the
//! Schedule document write and the claim. Nothing here decides anything; the
//! refusal codes are the coordinator's, spelled once for the wire.
use super::*;
use koru_delegate_api::bots::{BotReply, RoutineControlAction};
use koru_delegate_runtime::{RoutineControl, RoutineControlled, RoutineTestAccepted};

pub(super) type RoutineReply = mpsc::SyncSender<Result<BotReply, Failure>>;

pub(super) fn request(action: RoutineControlAction) -> RoutineControl {
    match action {
        RoutineControlAction::Enable {} => RoutineControl::Enable,
        RoutineControlAction::Pause {} => RoutineControl::Pause,
        RoutineControlAction::Edit {
            expected_name,
            name,
            recurrence,
        } => RoutineControl::Edit {
            expected_name,
            name,
            recurrence,
        },
    }
}

/// A reply over its encoded bound is never written. It cannot be reached by
/// construction (a name is at most 128 characters); the check keeps the bound
/// a property of the wire rather than of today's field sizes.
fn bounded(reply: BotReply) -> Result<BotReply, Failure> {
    if reply.routine_control_valid() == Some(true) {
        Ok(reply)
    } else {
        Err(Failure::new(
            "schedule_unavailable",
            "The Routine operation could not be completed.",
            true,
        ))
    }
}

pub(super) fn controlled(result: PortResult<RoutineControlled>) -> Result<BotReply, Failure> {
    let controlled = result.map_err(routine_failure)?;
    bounded(BotReply::RoutineControlled {
        route: controlled.route,
        outcome: controlled.outcome,
        name: controlled.name,
        definition: controlled.definition,
        effective_at_ms: controlled.effective_at_ms,
        lifecycle: controlled.lifecycle,
    })
}

pub(super) fn accepted(result: PortResult<RoutineTestAccepted>) -> Result<BotReply, Failure> {
    let accepted = result.map_err(routine_failure)?;
    bounded(BotReply::RoutineTestAccepted {
        slot: accepted.slot,
        definition: accepted.definition,
    })
}

/// The owner path's refusal table. `schedule_failure` folds every code it
/// does not know into `schedule_unavailable`; the Routine codes must survive,
/// so they are spelled here. The message is an English literal for logs: the
/// pane chooses its own localized string by code and never shows this one.
pub(super) fn routine_failure(error: PortError) -> Failure {
    let (code, retryable) = match error.code.as_str() {
        "routine_control_invalid" | "unknown_workspace" => ("invalid_request", false),
        "schedule_document" | "recurrence" => ("invalid_schedule", false),
        "routine_route_mismatch"
        | "routine_catalog_identity"
        | "routine_physical_route_mismatch"
        | "routine_live_route" => ("routine_route_mismatch", false),
        "schedule_not_found" => ("schedule_not_found", false),
        "schedule_not_owner" => ("schedule_not_owner", false),
        "schedule_handoff_pending" => ("schedule_handoff_pending", false),
        "schedule_retired" => ("schedule_retired", false),
        "schedule_read_only" => ("schedule_read_only", false),
        "schedule_revision_conflict" => ("schedule_revision_conflict", false),
        "schedule_state_conflict" => ("schedule_state_conflict", false),
        "schedule_name_conflict" => ("schedule_name_conflict", false),
        "routine_bot_paused" => ("routine_bot_paused", false),
        "routine_test_requires_paused" => ("routine_test_requires_paused", false),
        "routine_test_minute_changed" => ("routine_test_minute_changed", true),
        "routine_test_too_soon" => ("routine_test_too_soon", true),
        "routine_test_minute_taken" => ("routine_test_minute_taken", false),
        "routine_test_busy" => ("routine_test_busy", true),
        // A partial catalog, an unavailable Store or ledger, a Routine owner
        // that is not composed, no admission barrier recorded yet, or failed
        // event preparation: nothing was decided, so the caller may retry.
        _ => ("schedule_unavailable", true),
    };
    Failure::new(
        code,
        "The Routine operation could not be completed.",
        retryable,
    )
    .with_detail("port", error.port.name())
}

impl Control {
    /// One owner control, served by the scheduler thread. The connection
    /// thread waits here, bounded; the Run actor is never involved.
    pub fn control_routine(
        &self,
        route: koru_delegate_core::RoutineRouteIdentity,
        expected_revision: u32,
        action: RoutineControlAction,
    ) -> Result<BotReply, Failure> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.commands
            .send(Command::ControlRoutine {
                route,
                expected_revision,
                action,
                reply,
            })
            .map_err(|_| Failure::unavailable())?;
        answer
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| Failure::unavailable())?
    }

    /// One owner Test acceptance. The claim happens on a later pass.
    pub fn test_routine(
        &self,
        route: koru_delegate_core::RoutineRouteIdentity,
        expected_revision: u32,
        occurrence_ms: i64,
    ) -> Result<BotReply, Failure> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.commands
            .send(Command::TestRoutine {
                route,
                expected_revision,
                occurrence_ms,
                reply,
            })
            .map_err(|_| Failure::unavailable())?;
        answer
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| Failure::unavailable())?
    }
}

#[cfg(test)]
mod tests;
