// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::panes::explore::help::{self, Action, Model, OfferAction};
use koru_agent_wire::{UpdateChoice, UpdateStage};

#[cfg(test)]
mod tests;

impl MainView {
    /// Adopt whole reports before layout/input. Opening the menu does not mark
    /// release notes read; only opening their page does that.
    pub(super) fn refresh_help(&mut self) {
        if let Some(rx) = self.help_headlines_rx.as_ref()
            && let Ok(answer) = rx.try_recv()
        {
            self.help_headlines = answer;
            self.help_headlines_rx = None;
        }
        let report = self.updates.report();
        self.update_was_offered |= self.updates.was_offered()
            || report.as_ref().is_some_and(|r| {
                !r.version.trim().is_empty()
                    && matches!(
                        r.stage,
                        UpdateStage::Available
                            | UpdateStage::Downloading
                            | UpdateStage::Verifying
                            | UpdateStage::Ready
                            | UpdateStage::Installing
                            | UpdateStage::Deferred
                    )
            });
        self.explore.set_help(Model {
            report,
            was_offered: self.update_was_offered,
            notes_unread: !self.help_notes_read
                && self.help_headlines.as_ref().is_some_and(|h| !h.is_empty()),
            headline: self
                .help_headlines
                .as_ref()
                .and_then(|h| h.first().cloned()),
        });
    }

    pub(super) fn help_action(&mut self, action: Action) -> bool {
        self.refresh_help();
        match action {
            Action::Offer => self.help_offer_action(),
            Action::Current => self.open_whats_new(koru_channel::build_info().version),
            Action::Releases => {
                self.ask_pane(PaneRequest::OpenUri(crate::whatsnew::RELEASES_URL));
                true
            }
            Action::Docs => {
                self.ask_pane(PaneRequest::OpenUri(
                    "https://www.koru.do/en/support/desktop",
                ));
                true
            }
            Action::Feedback => {
                self.run_menu_action(MenuSubject::Help, MenuAction::HelpSendFeedback)
            }
            Action::GitHub => self.run_menu_action(MenuSubject::Help, MenuAction::HelpStarOnGitHub),
            Action::X => self.run_menu_action(
                MenuSubject::Help,
                MenuAction::HelpSocial(context_menu::Social::X),
            ),
        }
    }

    fn help_offer_action(&mut self) -> bool {
        let Some(report) = self.updates.report() else {
            return false;
        };
        let projection = help::project(Some(&report), self.update_was_offered, false);
        let Some(action) = projection.action else {
            return false;
        };
        if self.update_action_in_flight(report.stage) {
            return true;
        }
        match action {
            OfferAction::Download => self.update_consent(UpdateChoice::Download, report.stage),
            OfferAction::Manual => {
                if let Some(url) = report.download_url.as_deref() {
                    let _ = koru_editor::browser::open(url);
                }
            }
            OfferAction::Restart => {
                if self.updates.is_test_upgrade() {
                    self.window_request = Some(koru_window::WindowRequest::TestUpgrade);
                    self.update_action_pending = Some((report.stage, std::time::Instant::now()));
                } else {
                    self.update_consent(
                        UpdateChoice::Install {
                            session_generation: report.session_generation,
                        },
                        report.stage,
                    );
                }
            }
            OfferAction::Retry => self.update_consent(UpdateChoice::Retry, report.stage),
        }
        true
    }
}
