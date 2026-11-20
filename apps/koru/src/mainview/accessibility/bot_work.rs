// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::mainview::tab_rules::KoruTabs;
use koru_ai_ui::BotWorkControl;
use koru_draw::el::{Key as ElKey, Object};

fn occurrence(cx: &Shell, key: ElKey) -> Occurrence {
    let part = ControlPart::new(key.part);
    match key.object {
        Object::Model(id) => Occurrence::model(cx.window, cx.view, uuid::Uuid::from_u128(id), part),
        Object::Surface(name) => cx.surface(name, part),
    }
}

fn control(cx: &Shell, control: BotWorkControl) -> SemanticNode {
    SemanticNode::new(occurrence(cx, control.key), Role::Button)
        .with_label(control.name)
        .with_bounds(control.bounds)
        .with_actions([Action::Click, Action::Focus])
}

impl MainView {
    pub(super) fn bot_work_semantics(&self, cx: &Shell) -> Option<SemanticNode> {
        let shown = self.bot_work_surface()?.bot_work_semantics()?;
        Some(
            SemanticNode::new(
                cx.surface(SHELL, ControlPart::new("bot-work")),
                Role::Dialog,
            )
            .with_label(shown.name)
            .with_bounds(shown.bounds)
            .modal()
            .with_children(
                std::iter::once(
                    SemanticNode::new(
                        cx.surface("bot-work", ControlPart::new("details")),
                        Role::Label,
                    )
                    .with_label(shown.details),
                )
                .chain(shown.controls.into_iter().map(|item| control(cx, item)))
                .collect(),
            ),
        )
    }

    pub(super) fn bot_work_focus(&self, cx: &Shell) -> Option<Occurrence> {
        let surface = self.bot_work_surface()?;
        surface
            .bot_work_semantics()
            .map(|shown| shown.focus)
            .or_else(|| surface.bot_work_focused_anchor())
            .map(|key| occurrence(cx, key))
    }

    pub(super) fn bot_work_anchor_semantics(&self, cx: &Shell) -> Vec<SemanticNode> {
        let Some(surface) = self.bot_work_surface() else {
            return Vec::new();
        };
        let offset = self
            .focused_pane_rect()
            .map_or((0.0, 0.0), |pane| (pane.x, pane.y));
        surface
            .bot_work_anchors()
            .into_iter()
            .map(|mut item| {
                item.bounds.x += offset.0;
                item.bounds.y += offset.1;
                control(cx, item)
            })
            .collect()
    }

    pub(super) fn bot_work_is_target(&self, target: Occurrence) -> bool {
        let cx = self.shell(target.view);
        self.bot_work_surface()
            .and_then(|surface| surface.bot_work_semantics())
            .is_some_and(|shown| {
                shown
                    .controls
                    .iter()
                    .any(|item| occurrence(&cx, item.key) == target)
            })
    }

    pub(super) fn bot_work_action(&mut self, target: Occurrence, action: Action) -> bool {
        if !matches!(action, Action::Click | Action::Focus) {
            return false;
        }
        let cx = self.shell(target.view);
        let Some(surface) = self.bot_work_surface() else {
            return false;
        };
        let open = surface.bot_work_open();
        let controls = if open {
            surface
                .bot_work_semantics()
                .map_or_else(Vec::new, |shown| shown.controls)
        } else {
            surface.bot_work_anchors()
        };
        let Some(key) = controls
            .iter()
            .find(|item| occurrence(&cx, item.key) == target)
            .map(|item| item.key)
        else {
            return false;
        };
        if let TabView::Bot(view) = self.tabs_mut().active_view() {
            view.release_keyboard();
        }
        let Some(surface) = self.bot_work_surface_mut() else {
            return false;
        };
        if action == Action::Focus {
            return surface.focus_bot_work_control(key);
        }
        if open {
            surface.bot_work_activate(key)
        } else {
            surface.activate_bot_work_anchor(key)
        }
    }
}
