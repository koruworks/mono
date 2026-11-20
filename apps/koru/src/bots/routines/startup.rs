// SPDX-License-Identifier: MPL-2.0
use super::*;
use koru_api_client::bot_routines as wire;
use koru_delegate_schedule::ScheduleState;
use uuid::Uuid;

#[derive(Default)]
pub(super) struct Startup {
    request: Option<Uuid>,
    sent: bool,
    pub(super) uncertain: bool,
    checked: bool,
}
impl Startup {
    pub(super) fn reopen(&mut self) {
        if self.request.is_none() {
            self.checked = false;
        }
    }
}
impl Routines {
    /// One claim when the native pane actually shows an enabled Routine.
    /// It does not write startup settings or run again on an idle deadline.
    pub(crate) fn startup_read(&mut self) -> Option<Command> {
        if !self.visible
            || self.loading()
            || self.stale
            || self.startup.checked
            || self.startup.request.is_some()
            || !self.held.as_ref()?.items.iter().any(|item| {
                matches!(
                    &item.live,
                    BotRoutineLiveObservation::Current {
                        state: ScheduleState::Active { .. },
                        ..
                    }
                )
            })
        {
            return None;
        }
        self.startup.request = Some(Uuid::new_v4());
        self.startup.sent = true;
        Some(Command::RoutineStartup {
            bot: self.bot?,
            request: self.startup.request?,
        })
    }
    pub(crate) fn startup_unsent(&mut self) {
        self.startup.sent = false;
        self.startup.uncertain = true;
        self.changed();
    }
    pub(crate) fn startup_retry(&mut self) -> Option<Command> {
        if !self.visible || !self.startup.uncertain || self.startup.sent {
            return None;
        }
        self.startup.sent = true;
        self.startup.uncertain = false;
        self.changed();
        Some(Command::RoutineStartup {
            bot: self.bot?,
            request: self.startup.request?,
        })
    }
    pub(crate) fn startup_answer(
        &mut self,
        bot: BotId,
        request: Uuid,
        result: Result<wire::Response, control::Refusal>,
    ) -> bool {
        if self.bot != Some(bot) || self.startup.request != Some(request) {
            return false;
        }
        match result {
            Err(control::Refusal {
                failure: Failure::Refused(code),
                ..
            }) if code == "expired" => {
                // The exact response is no longer retained. Do not mint a new
                // claim or imply consent; startup remains available in Settings.
                self.startup = Startup {
                    checked: true,
                    ..Default::default()
                };
            }
            Ok(wire::Response::Startup {
                request: echoed,
                first_enabled,
                ..
            }) if echoed == request => {
                self.startup_offer |= first_enabled;
                self.startup = Startup {
                    checked: true,
                    ..Default::default()
                };
            }
            _ => {
                self.startup.sent = false;
                self.startup.uncertain = true;
            }
        }
        self.changed();
        true
    }
}
