// SPDX-License-Identifier: MPL-2.0
//! The window hosts the Bot's creation form; its tab owns the command/receipt.
use super::*;
use crate::bots::view::routine_modal::form::Host;
use koru_ai_core::bots::BotId;
use koru_api_client::bot_routines as wire;

impl MainView {
    pub(crate) fn begin_routine_creation(&mut self, bot: BotId, tab: u64) -> bool {
        if self.delegation.is_some() {
            return false;
        }
        let Some(world) = self.routine_world() else {
            return false;
        };
        let Some(view) = self.asking_bot_tab_mut(tab, bot) else {
            return false;
        };
        if !view.can_create_routine() && view.routine_creation.draft().is_none() {
            return false;
        }
        let name = view.label();
        let draft = view.routine_creation.draft().cloned();
        let busy = view.routine_creation.busy;
        view.routine_creation.answer = None;
        let mut modal = Delegation::open_with_environment(
            &mut self.engine,
            world,
            self.reminder_environment.locale.clone(),
            self.reminder_environment.resolution.current_zone.as_str(),
        );
        modal.kind = modal::repeat::Kind::Repeat;
        modal.routine_creation = Some(Host::new(bot, tab, name, draft.is_some()));
        modal.focus = 0;
        if let Some(wire::Command::Create {
            name,
            objective,
            recurrence,
            ..
        }) = draft
        {
            modal.schedule_name.set_text(&mut self.engine, &name);
            modal.objective.set_text(&mut self.engine, &objective);
            modal.repeat.set_rule(&mut self.engine, &recurrence);
            modal.phase = if busy {
                Phase::Scheduling
            } else {
                Phase::Editing {
                    problem: Some(koru_i18n::Key::BotsRoutineChecking),
                }
            };
        }
        self.delegation = Some(modal);
        true
    }

    pub(super) fn submit_routine_creation(
        &mut self,
        name: String,
        objective: String,
        recurrence: koru_recurrence::RecurrenceRule,
    ) -> bool {
        let Some((bot, tab)) = self
            .delegation
            .as_ref()
            .and_then(|m| m.routine_creation.as_ref())
            .map(|c| (c.bot, c.tab))
        else {
            return false;
        };
        let Some(view) = self.asking_bot_tab_mut(tab, bot) else {
            self.delegation = None;
            return self.say_asking_tab_gone();
        };
        let answer = view.create_routine(name, objective, recurrence);
        if let (Err(words), Some(modal)) = (answer, self.delegation.as_mut()) {
            modal.routine_answered(words);
        }
        true
    }

    pub(super) fn maintain_routine_creation(&mut self) -> bool {
        let Some((bot, tab, saving)) = self.delegation.as_ref().and_then(|m| {
            m.routine_creation
                .as_ref()
                .map(|c| (c.bot, c.tab, matches!(m.phase, Phase::Scheduling)))
        }) else {
            return false;
        };
        let Some(view) = self.asking_bot_tab_mut(tab, bot) else {
            self.delegation = None;
            return self.say_asking_tab_gone();
        };
        if !saving {
            return false;
        }
        let answer = view.routine_creation.answer.take();
        let uncertain = view.routine_creation.draft().is_some();
        match answer {
            Some(Ok(())) => {
                self.delegation = None;
                true
            }
            Some(Err(words)) => {
                let modal = self.delegation.as_mut().unwrap();
                modal.routine_creation.as_mut().unwrap().uncertain = uncertain;
                modal.routine_answered(words);
                true
            }
            None => false,
        }
    }

    pub(super) fn routine_world(&self) -> Option<world::World> {
        let (_, profile, _, Some(workspace)) = self.pane_context.clone()? else {
            return None;
        };
        Some(world::World {
            workspace: koru_delegate_core::WorkspaceId::from_uuid(workspace.as_uuid()),
            workspace_name: String::new(),
            profile: koru_store_core::ProfileId::from_uuid(profile.as_uuid()),
            agents: vec![],
            scopes: vec![],
            context: vec![],
            attachable: None,
            objective: String::new(),
            pill_anchor: None,
            temporary_base: String::new(),
            base_problem: None,
            dropped_context: false,
            chosen_model: None,
            chosen_agent: None,
            chosen_scope: None,
            grant: false,
        })
    }
}
