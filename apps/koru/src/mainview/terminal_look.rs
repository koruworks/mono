// SPDX-License-Identifier: MPL-2.0

//! A terminal tab's own theme and font, chosen from its right-click menu and
//! kept **per agent** ([ADR-475](../../../../docs/decisions/ADR-475-terminallook.md)).
//!
//! The Settings page has one terminal theme and one font. A person who reads
//! Claude in a light terminal and runs Codex in a dark one has no way to say
//! so, and every tab of an agent should look alike, so the choice is filed
//! under the agent's profile key (`claude`, `codex`, `powershell`, …) rather
//! than on the tab or the window. Each half is optional: an agent that has not
//! chosen follows Settings, and *Default (Settings)* puts it back.

use koru_session_protocol::SessionProfileKey;

use super::MainView;
use super::tab::TabView;

impl MainView {
    /// The profile a session tab was started from, for the tab at `tab`.
    /// The session a terminal tab holds, once its launch has named one.
    pub(super) fn session_of_tab(&self, tab: usize) -> Option<koru_session_protocol::SessionId> {
        match &self.tabs().get(tab)?.view {
            TabView::Session(view) => view.session,
            _ => None,
        }
    }

    pub(super) fn session_profile_of_tab(&self, tab: usize) -> Option<SessionProfileKey> {
        match &self.tabs().get(tab)?.view {
            TabView::Session(view) => view.profile_key.clone(),
            _ => None,
        }
    }

    /// Choose the look for the agent the tab at `tab` is running. `theme` and
    /// `font` are each `None` to leave that half alone, `Some(None)` to put it
    /// back on Settings, and `Some(Some(token))` to set it.
    pub(super) fn set_terminal_look(
        &mut self,
        tab: usize,
        theme: Option<Option<&'static str>>,
        font: Option<Option<&'static str>>,
    ) -> bool {
        let Some(profile) = self.session_profile_of_tab(tab) else {
            return false;
        };
        // Every tab of this agent re-reads the look as it draws, so a change
        // shows at once in all of them; the profile file carries it to the
        // next launch at the next capture.
        crate::terminal_prefs::set_agent_look(&profile, theme, font);
        true
    }
}

#[cfg(test)]
mod tests;
