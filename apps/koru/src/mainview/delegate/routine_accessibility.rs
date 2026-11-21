// SPDX-License-Identifier: MPL-2.0
//! The creation form publishes its measured controls, with a new identity per
//! opening. Accessible actions enter the same modal controller as key presses.
use super::*;
use crate::bots::view::routine_modal::form;
use crate::mainview::accessibility::{Shell, instance_of};
use accesskit::{Action, ActionData, Role};
use koru_workspace_host::accessibility::{ControlPart, Occurrence, Revision, SemanticNode};
use koru_workspace_host::routing::{Blockers, PromptKind};
const SURFACE: &str = "bot-routine-create";
fn control_id(hit: Hit) -> u64 {
    let koru_draw::el::Object::Model(id) = form::control_key(hit).object else {
        unreachable!()
    };
    id as u64
}
impl MainView {
    pub(crate) fn routine_creation_semantics(&self, cx: &Shell) -> Option<SemanticNode> {
        let modal = self.delegation.as_ref()?;
        let host = modal.routine_creation.as_ref()?;
        let mut title = koru_i18n::fill(
            koru_i18n::t(cx.lang, koru_i18n::Key::BotsRoutineCreateTitle),
            &[&host.name],
        );
        let notice = match modal.phase {
            Phase::Editing { problem } => problem,
            Phase::Scheduling => Some(koru_i18n::Key::BotsRoutineSaving),
            _ => None,
        };
        if let Some(words) = notice {
            title.push_str(". ");
            title.push_str(koru_i18n::t(cx.lang, words));
        }
        let (bounds, children) = crate::input_engine::with(|engine| {
            let bounds = paint::panel(engine, modal, cx.viewport);
            let controls = if matches!(modal.phase, Phase::Editing { .. }) {
                form::published(engine, modal, cx.viewport)
            } else {
                paint::layout(engine, modal, cx.viewport)
                    .into_iter()
                    .map(|(hit, rect)| form::Control {
                        hit,
                        label: koru_i18n::t(cx.lang, koru_i18n::Key::DelegateClose).into(),
                        bounds: Some(rect),
                        value: None,
                        enabled: true,
                        selected: None,
                    })
                    .collect()
            };
            let children = controls
                .into_iter()
                .map(|control| {
                    let mut node = SemanticNode::new(
                        cx.instance(
                            SURFACE,
                            host.instance + control_id(control.hit),
                            ControlPart::ROOT,
                        ),
                        if control.value.is_some() {
                            Role::TextInput
                        } else {
                            Role::Button
                        },
                    )
                    .with_label(control.label)
                    .disabled(!control.enabled);
                    if let Some(bounds) = control.bounds {
                        node = node.with_bounds(bounds);
                    }
                    if control.enabled {
                        node = node.with_actions(if control.value.is_some() {
                            vec![Action::Focus, Action::SetValue]
                        } else {
                            vec![Action::Focus, Action::Click]
                        });
                    }
                    if let Some(value) = control.value {
                        node = node.with_value(value);
                    }
                    if let Some(selected) = control.selected {
                        node = node.with_selected(selected);
                    }
                    node
                })
                .collect();
            (bounds, children)
        });
        Some(
            SemanticNode::new(
                cx.instance(SURFACE, host.instance, ControlPart::ROOT),
                Role::Dialog,
            )
            .with_label(title)
            .with_bounds(bounds)
            .with_children(children)
            .modal(),
        )
    }
    pub(crate) fn routine_creation_focus(&self, cx: &Shell) -> Option<Occurrence> {
        let modal = self.delegation.as_ref()?;
        let host = modal.routine_creation.as_ref()?;
        Some(cx.instance(
            SURFACE,
            host.instance + modal.focused().map(control_id).unwrap_or(0),
            ControlPart::ROOT,
        ))
    }
    fn routine_creation_target(&self, target: Occurrence) -> Option<Hit> {
        if self.blockers() != Blockers::Prompt(PromptKind::Delegate)
            || target.part != ControlPart::ROOT
        {
            return None;
        }
        let modal = self.delegation.as_ref()?;
        let host = modal.routine_creation.as_ref()?;
        let id = instance_of(target, SURFACE)?;
        modal
            .controls()
            .into_iter()
            .find(|hit| host.instance + control_id(*hit) == id)
    }
    pub(crate) fn routine_creation_is_target(&self, target: Occurrence) -> bool {
        self.routine_creation_target(target).is_some()
    }
    pub(crate) fn routine_creation_action(
        &mut self,
        target: Occurrence,
        action: Action,
        data: Option<&ActionData>,
    ) -> bool {
        let Some(hit) = self.routine_creation_target(target) else {
            return false;
        };
        let modal = self.delegation.as_mut().unwrap();
        modal.focus = modal
            .controls()
            .iter()
            .position(|candidate| *candidate == hit)
            .unwrap();
        let outcome = match (action, data) {
            (Action::Focus, _) => modal::Outcome::Keep,
            (Action::Click, _) => modal.activate_with_engine(&mut self.engine, hit),
            (Action::SetValue, Some(ActionData::Value(value))) => {
                if hit == Hit::Objective {
                    modal.objective.set_text(&mut self.engine, value);
                } else if let Some(field) = modal.recurrence_field_mut(hit) {
                    field.set_text(&mut self.engine, value);
                    if hit == Hit::ScheduleAdvanced {
                        modal.repeat.advanced_changed(&mut self.engine);
                    } else if matches!(hit, Hit::ScheduleTime | Hit::ScheduleDay) {
                        modal.repeat.accept_picker(&mut self.engine);
                    }
                } else {
                    return false;
                }
                modal::Outcome::Keep
            }
            _ => return false,
        };
        paint::reveal_focus(&mut self.engine, modal, self.viewport);
        self.settle_delegation(outcome);
        true
    }
    pub(crate) fn revise_routine_creation(&self, rev: &mut Revision) {
        let Some(modal) = self
            .delegation
            .as_ref()
            .filter(|m| m.routine_creation.is_some())
        else {
            return;
        };
        rev.str(&format!("{:?}:{:?}", modal.phase, modal.focused()));
        rev.str(
            &modal
                .routine_creation
                .as_ref()
                .unwrap()
                .instance
                .to_string(),
        );
        rev.str(&modal.schedule_name.value())
            .str(&modal.objective.value())
            .str(&modal.repeat.time.value())
            .str(&modal.repeat.day.value())
            .str(&modal.repeat.zone.value())
            .str(&modal.repeat.advanced.value())
            .f32(modal.scroll);
    }
}
