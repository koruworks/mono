// SPDX-License-Identifier: MPL-2.0

//! Delegate IPC 17: the owner control and the owner Test of one Bot Routine.
//!
//! Both ride the authenticated private Bot channel and are served by the
//! scheduler coordinator, which owns the Schedule document write and the
//! claim. Neither is a public schedule frame or a model tool. A caller cannot
//! supply a template, a document address, an effective time, an owner or an
//! association: the action names only what an owner may change.
use koru_delegate_core::{ROUTINE_CONTROL_WIRE_LIMIT, routine_control_within_wire_limit};
use koru_delegate_schedule::{RecurrenceRule, ScheduleName};
use serde::{Deserialize, Serialize};

/// What one `ControlRoutine` changes. `Edit` replaces the name and the
/// recurrence in one revision; the task text, context, access and lifecycle
/// are copied from the current definition by the coordinator.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoutineControlAction {
    Enable {},
    Pause {},
    Edit {
        expected_name: ScheduleName,
        name: ScheduleName,
        recurrence: RecurrenceRule,
    },
}

impl std::fmt::Debug for RoutineControlAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A name is person-authored; only the kind is diagnostic.
        f.write_str(match self {
            Self::Enable {} => "RoutineControlAction::Enable",
            Self::Pause {} => "RoutineControlAction::Pause",
            Self::Edit { .. } => "RoutineControlAction::Edit([REDACTED])",
        })
    }
}

/// The largest request frame body that may carry an owner control: the 4 KiB
/// command, the frame tag and the bootstrap authority at its longest escaped
/// spelling.
pub const ROUTINE_CONTROL_FRAME_LIMIT: usize = ROUTINE_CONTROL_WIRE_LIMIT + 2048;

/// Whether a raw request body names an owner control and is over its bound.
///
/// This runs before the frame is decoded. The probe borrows the one tag it
/// reads and skips everything else, so an oversized control is refused without
/// allocating its content. A body the probe cannot classify is not an oversized
/// control here; it is decoded and checked again by
/// [`super::BotCommand::routine_control_valid`].
pub fn routine_control_frame_oversized(body: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct Frame<'a> {
        #[serde(borrow, default)]
        command: Option<Command<'a>>,
    }
    #[derive(Deserialize)]
    struct Command<'a> {
        #[serde(borrow, default)]
        command: Option<&'a str>,
    }
    body.len() > ROUTINE_CONTROL_FRAME_LIMIT
        && serde_json::from_slice::<Frame<'_>>(body).is_ok_and(|frame| {
            matches!(
                frame.command.and_then(|command| command.command),
                Some("control_routine" | "test_routine")
            )
        })
}

pub(super) fn within_wire_limit<T: Serialize + ?Sized>(value: &T) -> bool {
    routine_control_within_wire_limit(value)
}
