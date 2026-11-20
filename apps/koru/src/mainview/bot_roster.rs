// SPDX-License-Identifier: MPL-2.0

//! Navigator actions use the same settings and archive confirmation as a Bot tab.
use super::*;
use koru_ai_core::bots::BotId;

impl MainView {
    pub(super) fn choose_bot_roster_action(
        &mut self,
        bot: Option<BotId>,
        action: MenuAction,
    ) -> bool {
        let Some(bot) = bot else {
            return match action {
                MenuAction::BotTeamPause(_) => self.explore.bots_mut().press_team(),
                MenuAction::BotArchived(_) => self.explore.bots_mut().toggle_archived(),
                _ => false,
            };
        };
        if !matches!(
            action,
            MenuAction::EditBotIdentity
                | MenuAction::BotSettings
                | MenuAction::BotNewConversation
                | MenuAction::ArchiveBot
                | MenuAction::TrashBot
                | MenuAction::UnarchiveBot
        ) {
            return false;
        }
        let Some(row) = self.explore.bots().menu_row(bot) else {
            return false;
        };
        self.open_bot_tab(row);
        let tab = self.tabs().active_tab();
        if !matches!(&tab.view, TabView::Bot(view) if view.bot() == Some(bot)) {
            return false;
        }
        self.bot_roster_action = Some((bot, tab.id, action));
        true
    }

    pub(super) fn maintain_bot_roster_actions(&mut self) -> bool {
        let mut changed = false;
        if let Some((bot, tab, action)) = self.bot_roster_action {
            match self.asking_bot_tab_mut(tab, bot) {
                None => self.bot_roster_action = None,
                Some(view) if action == MenuAction::EditBotIdentity => {
                    view.edit_identity();
                    self.bot_roster_action = None;
                    changed = true;
                }
                Some(view) if action == MenuAction::BotSettings => {
                    view.set_settings_open(true);
                    self.bot_roster_action = None;
                    changed = true;
                }
                Some(view) if view.identity_loaded() => {
                    if action == MenuAction::BotNewConversation {
                        view.set_settings_open(false);
                        changed |= view.new_conversation();
                        self.bot_roster_action = None;
                        return changed;
                    }
                    self.bot_roster_action = None;
                    changed |= self.choose_bot_lifecycle(bot, tab, action);
                }
                _ => {}
            }
        }
        if let Some((bot, at)) = self.explore.bots_mut().take_menu() {
            let items = if let Some(bot) = bot {
                let archived = self
                    .explore
                    .bots()
                    .menu_row(bot)
                    .is_some_and(|row| row.archived);
                vec![
                    MenuAction::BotNewConversation,
                    MenuAction::EditBotIdentity,
                    MenuAction::BotSettings,
                    MenuAction::Divider,
                    if archived {
                        MenuAction::UnarchiveBot
                    } else {
                        MenuAction::ArchiveBot
                    },
                    MenuAction::TrashBot,
                ]
            } else {
                let mut items = Vec::new();
                if let Some(control) = self.explore.bots().team().control() {
                    items.push(MenuAction::BotTeamPause(control.words));
                }
                items.push(MenuAction::BotArchived(self.explore.bots().archived_open()));
                items
            };
            let at = at.unwrap_or_else(|| self.explore.bots_menu_anchor(self.explore_area(), bot));
            changed |= self.open_subject_menu(MenuSubject::BotRoster { bot }, items, at);
        }
        changed
    }
}
