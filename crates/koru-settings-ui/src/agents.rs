// SPDX-License-Identifier: MPL-2.0

//! **Coding → Agent providers**: which coding agents Koru shows, and
//! whether their CLIs are ready.
//!
//! One row per agent the host lists — its brand mark and name, its status in
//! plain words, a *Setup guide ↗* link and its switch — under a card that
//! says what a switch does and offers *Refresh status*, and a title that
//! offers *Restore defaults*. The host owns everything here (the agent list,
//! the existing readiness checks, the stored off list), so every press is a
//! [`Request`](crate::Request) and the page only draws what it was given.

use koru_i18n::{Key, Lang, t};

use crate::action::Action;
use crate::page::Builder;
use crate::state::AgentProviderRow;

/// Lay the page's content out under its title.
pub(crate) fn lay_out(b: &mut Builder<'_>, lang: Lang, rows: &[AgentProviderRow]) {
    let on = rows.iter().filter(|row| row.on).count();
    b.caption_with_link(
        t(lang, Key::SettingsEnabledAgents),
        t(lang, Key::SettingsRefreshAgentStatus),
        Action::RefreshAgentStatus,
    );
    b.card(None);
    b.row_text(
        &koru_i18n::fill(
            t(lang, Key::SettingsAgentsEnabledCount),
            &[&on.to_string(), &rows.len().to_string()],
        ),
        Some(t(lang, Key::SettingsEnabledAgentsHint)),
    );
    for row in rows {
        b.next_row_mark(&row.key);
        if let Some(guide) = row.guide {
            b.next_row_link(
                t(lang, Key::SettingsAgentSetupGuide),
                Action::AgentGuide(guide),
            );
        }
        b.row_toggle(
            &row.name,
            Some(&row.status.words(lang)),
            row.on,
            Action::AgentEnabled(row.key.clone(), !row.on),
        );
    }
}

/// The title's page-wide action.
pub(crate) fn title_button(lang: Lang) -> (&'static str, Action) {
    (
        t(lang, Key::SettingsRestoreDefaults),
        Action::RestoreAgentDefaults,
    )
}

#[cfg(test)]
mod tests;
