// SPDX-License-Identifier: MPL-2.0
//! Routine creation retains one exact command across uncertain delivery. The
//! existing delegation host supplies the modal/input and recurrence editor;
//! this module owns the Bot form and its native creation lifecycle.
use super::BotView;
use crate::bots::link::{Command, Failure};
use crate::bots::routines::control::{Refusal, refusal_words};
use koru_ai_core::bots::BotId;
use koru_api_client::bot_routines as wire;
use koru_i18n::Key as Words;
use uuid::Uuid;

pub(crate) mod form;

#[derive(Default)]
pub(crate) struct Creation {
    pending: Option<wire::Command>,
    pub(crate) busy: bool,
    pub(crate) answer: Option<Result<(), Words>>,
}
impl Creation {
    pub(crate) fn draft(&self) -> Option<&wire::Command> {
        self.pending.as_ref()
    }
    fn request(&self) -> Option<Uuid> {
        match &self.pending {
            Some(wire::Command::Create { id, .. }) => Some(*id),
            _ => None,
        }
    }
    fn begin(&mut self, command: wire::Command) -> Option<(Uuid, wire::Command)> {
        if self.busy {
            return None;
        }
        // After uncertainty a changed form cannot replace the accepted request.
        // Its exact receipt is resolved first; the modal keeps those fields locked.
        self.pending.get_or_insert(command);
        self.answer = None;
        self.busy = true;
        Some((self.request()?, self.pending.as_ref()?.clone()))
    }
    fn received(&mut self, request: Uuid, result: Result<wire::Response, Refusal>) -> bool {
        if self.request() != Some(request) || !self.busy {
            return false;
        }
        self.busy = false;
        match result {
            Ok(wire::Response::Created {
                request: echoed,
                item,
                ..
            }) if echoed == request && item.creation.request_id.as_uuid() == request => {
                self.pending = None;
                self.answer = Some(Ok(()));
            }
            Err(Refusal {
                failure: Failure::Refused(code),
                ..
            }) if code == "expired" || code == "conflict" => {
                // An expired receipt or identity conflict does not prove the
                // original create failed. Never mint a replacement implicitly.
                self.answer = Some(Err(refusal_words(&code)));
            }
            Err(Refusal {
                failure: Failure::Refused(code),
                ..
            }) => {
                self.pending = None;
                self.answer = Some(Err(refusal_words(&code)));
            }
            _ => self.answer = Some(Err(Words::BotsRoutineChecking)),
        }
        true
    }
}

impl BotView {
    pub(crate) fn can_create_routine(&self) -> bool {
        self.settings.bot().is_some_and(|b| {
            b.lifecycle == koru_ai_core::bots::BotLifecycle::Active && b.open_operation.is_none()
        })
    }
    pub(crate) fn take_routine_create_request(&mut self) -> bool {
        std::mem::take(&mut self.routine_create_request)
    }
    pub(crate) fn create_routine(
        &mut self,
        name: String,
        objective: String,
        recurrence: koru_recurrence::RecurrenceRule,
    ) -> Result<(), Words> {
        let bot = self.settings.bot().ok_or(Words::BotsUnavailable)?;
        if !self.can_create_routine() && self.routine_creation.draft().is_none() {
            return Err(Words::BotsErrPending);
        }
        let command = wire::Command::Create {
            id: Uuid::new_v4(),
            bot: bot.id,
            expected_bot_revision: bot.revision,
            name,
            objective,
            recurrence,
        };
        let bot = bot.id;
        let Some((request, command)) = self.routine_creation.begin(command) else {
            return Err(Words::BotsRoutineChecking);
        };
        if !self.link.as_ref().is_some_and(|link| {
            link.send(Command::CreateRoutine {
                bot,
                request,
                command: Box::new(command),
            })
        }) {
            self.routine_creation
                .received(request, Err(Refusal::unanswered()));
        }
        Ok(())
    }
    pub(super) fn routine_created(
        &mut self,
        bot: BotId,
        request: Uuid,
        result: Result<wire::Response, Refusal>,
    ) -> bool {
        if self.bot() != Some(bot) || !self.routine_creation.received(request, result) {
            return false;
        }
        if self.routine_creation.answer == Some(Ok(())) {
            // Re-read confirmed data, then use the existing first-enabled
            // startup claim. A created routine never creates a fake Run.
            self.routines.creation_accepted();
        } else {
            // A revision/lifecycle refusal refreshes the known Bot before retry.
            self.asked = false;
        }
        true
    }
}

#[cfg(test)]
mod tests;
