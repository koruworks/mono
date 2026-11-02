// SPDX-License-Identifier: MPL-2.0

//! **Coding → Agent providers**: which coding agents Koru shows, whether
//! their CLIs are ready, and where a new chat starts.
//!
//! The *Default permission for new chats* card — two segmented controls, the
//! supported agents' *Manual · Auto · Yolo* and the other agents' *Manual ·
//! Yolo*, and a sentence saying plainly what each means. Then one row per
//! agent the host lists — its brand mark and name, its status in plain words,
//! a *Setup guide ↗* link and its switch — under a card that says what a
//! switch does and offers *Refresh status*, and a title that offers *Restore
//! defaults*. The host owns everything here (the agent list, the existing
//! readiness checks, the stored settings), so every press is a
//! [`Request`](crate::Request) and the page only draws what it was given.

use koru_i18n::{Key, Lang, t};
use koru_settings::AgentPermission;

use crate::action::Action;
use crate::page::Builder;
use crate::state::{AgentDefaults, AgentProviderRow};

/// Lay the page's content out under its title.
pub(crate) fn lay_out(
    b: &mut Builder<'_>,
    lang: Lang,
    rows: &[AgentProviderRow],
    defaults: &AgentDefaults,
) {
    permissions(b, lang, defaults);
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

/// **Default permission for new chats**: the supported agents' three
/// choices, the other agents' two, and the sentence that says what they
/// mean — *Yolo* in so many words.
fn permissions(b: &mut Builder<'_>, lang: Lang, defaults: &AgentDefaults) {
    let choice = |other: bool, permission: AgentPermission, current: AgentPermission| {
        let words = match permission {
            AgentPermission::Manual => Key::SettingsPermissionManual,
            AgentPermission::Auto => Key::SettingsPermissionAuto,
            AgentPermission::Yolo => Key::SettingsPermissionYolo,
        };
        (
            Action::AgentPermission(other, permission),
            t(lang, words).to_owned(),
            permission == current,
        )
    };
    b.card(Some(t(lang, Key::SettingsDefaultPermission)));
    b.row_choice(
        t(lang, Key::SettingsSupportedAgents),
        Some(t(lang, Key::SettingsSupportedAgentsHint)),
        AgentPermission::ALL
            .iter()
            .map(|permission| choice(false, *permission, defaults.permission))
            .collect(),
    );
    b.row_choice(
        t(lang, Key::SettingsOtherAgents),
        Some(t(lang, Key::SettingsOtherAgentsHint)),
        [AgentPermission::Manual, AgentPermission::Yolo]
            .into_iter()
            .map(|permission| choice(true, permission, defaults.permission_other))
            .collect(),
    );
    b.note(t(lang, Key::SettingsDefaultPermissionHint));
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
