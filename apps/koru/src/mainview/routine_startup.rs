// SPDX-License-Identifier: MPL-2.0

//! The first explicitly enabled Routine offers the existing device startup
//! preference. The engine owns the first-enable fact; this window owns only
//! the pending offer and the correlated settings reply. Declining writes nothing.

use koru_api_client::{ConfirmedOutcome, ConfirmedWrite};
use koru_i18n::{Key, Lang, t};
use koru_settings::{GlobalPatch, GlobalSettings};

use super::{MainView, tab::TabView, trash::ConfirmSubject};

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    #[default]
    Idle,
    Waiting,
    Asking,
    Saving(u64),
    Done,
}

#[derive(Default)]
pub(super) struct StartupOffer {
    stage: Stage,
    /// None until the shell has read the device preference successfully.
    enabled: Option<bool>,
    retry: bool,
    generation: u64,
    receipt: Option<(u64, ConfirmedWrite<GlobalSettings>)>,
}

impl StartupOffer {
    fn first_enabled(&mut self) {
        if self.stage == Stage::Idle {
            self.stage = Stage::Waiting;
        }
    }

    pub(super) fn observe(&mut self, enabled: bool) -> bool {
        let changed = self.enabled != Some(enabled);
        self.enabled = Some(enabled);
        changed
    }

    fn offer(&mut self, blocked: bool) -> bool {
        if self.stage != Stage::Waiting {
            return false;
        }
        if self.enabled == Some(true) {
            self.stage = Stage::Done;
            return false;
        }
        if blocked || self.enabled.is_none() && !self.retry {
            return false;
        }
        self.stage = Stage::Asking;
        true
    }

    fn answer(&mut self, accepted: bool) -> Option<(u64, GlobalPatch)> {
        if self.stage != Stage::Asking {
            return None;
        }
        if !accepted || self.enabled == Some(true) {
            self.stage = Stage::Done;
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        self.stage = Stage::Saving(self.generation);
        Some((
            self.generation,
            GlobalPatch {
                start_on_login: Some(true),
                ..GlobalPatch::default()
            },
        ))
    }

    fn finish(&mut self, generation: u64, outcome: ConfirmedOutcome<GlobalSettings>) -> bool {
        if self.stage != Stage::Saving(generation) {
            return false;
        }
        if let ConfirmedOutcome::Accepted(section) = &outcome
            && section.start_on_login
        {
            self.enabled = Some(true);
            self.stage = Stage::Done;
        } else {
            self.enabled = match outcome {
                ConfirmedOutcome::Accepted(section)
                | ConfirmedOutcome::Reconciled {
                    authoritative: section,
                    ..
                }
                | ConfirmedOutcome::ContractMismatch {
                    authoritative: section,
                } => Some(section.start_on_login),
                _ => None,
            };
            self.retry = true;
            self.stage = Stage::Waiting;
        }
        true
    }

    pub(super) fn deadline(&self) -> Option<std::time::Instant> {
        self.receipt
            .as_ref()
            .and_then(|(_, receipt)| receipt.deadline())
    }
}

fn dialog(lang: Lang, retry: bool) -> koru_ui::Confirm {
    let body = if retry {
        format!(
            "{}\n\n{}",
            t(lang, Key::BotsRoutineStartupFailed),
            t(lang, Key::BotsRoutineStartupBody)
        )
    } else {
        t(lang, Key::BotsRoutineStartupBody).to_owned()
    };
    koru_ui::Confirm::asking(
        t(lang, Key::SettingsStartOnLogin),
        body,
        t(lang, Key::ReminderStartupEnable),
        t(lang, Key::ReminderStartupDecline),
    )
}

impl MainView {
    pub(super) fn maintain_routine_startup(&mut self) -> bool {
        let mut first_enabled = false;
        self.each_tab_mut(|tab| {
            if let TabView::Bot(view) = &mut tab.view {
                first_enabled |= view.take_routine_startup_offer();
            }
            false
        });
        if first_enabled {
            self.routine_startup.first_enabled();
        }
        let completed = self
            .routine_startup
            .receipt
            .as_mut()
            .and_then(|(generation, receipt)| receipt.poll().map(|outcome| (*generation, outcome)));
        let mut changed = false;
        if let Some((generation, outcome)) = completed {
            self.routine_startup.receipt = None;
            changed |= self.routine_startup.finish(generation, outcome);
        }
        // Do not replace a dialog the person is answering or compete with
        // another modal. The first-enable fact remains queued until it can show.
        let blocked = self.blockers().is_blocking();
        if self.routine_startup.offer(blocked) {
            self.confirm = Some((
                dialog(crate::format::lang(), self.routine_startup.retry),
                ConfirmSubject::RoutineStartup,
            ));
            changed = true;
        }
        changed
    }

    pub(super) fn answer_routine_startup(&mut self, accepted: bool) {
        if let Some((generation, patch)) = self.routine_startup.answer(accepted) {
            let receipt = self.delegate_folder.save_confirmed(patch);
            self.routine_startup.receipt = Some((generation, receipt));
        }
    }
}

#[cfg(test)]
mod tests;
