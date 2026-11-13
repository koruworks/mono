// SPDX-License-Identifier: MPL-2.0

//! The active tab supplies Library data. No document/history target is used
//! as Bot authority, and no second reader or editor lives in this host.
use super::{MainView, tab::TabView};

#[cfg(test)]
mod tests;
use crate::bots::view::context_library::{Action, Data};
use crate::panes::context::Section;
use koru_draw::el::Key;
use koru_draw::geometry::Rect;
use koru_draw::{scene, theme};
use vello::{Scene, kurbo::Affine};
use winit::keyboard::{Key as InputKey, ModifiersState};

impl MainView {
    fn active_context_library(&self) -> Option<Data> {
        let tab = self.tabs().active_tab();
        match &tab.view {
            TabView::Bot(view) => view.context_library_data(tab.id),
            _ => None,
        }
    }
    pub(super) fn context_sections(&self) -> Vec<Section> {
        if self.active_context_library().is_some() {
            vec![Section::Library]
        } else if self.snapshot_target_agrees() {
            Section::visible()
        } else {
            Vec::new()
        }
    }
    pub(super) fn context_has_sections(&self) -> bool {
        !self.context_sections().is_empty()
    }
    pub(super) fn bot_context_stands(&self) -> bool {
        self.context.is_open() && self.active_context_library().is_some()
    }
    pub(super) fn bot_context_unobstructed(&self) -> bool {
        !self.blockers().is_blocking()
            && self.file_preview.is_none()
            && !self.explore.popover_open()
    }
    pub(super) fn bot_context_current(&self) -> bool {
        self.bot_context_stands()
            && self
                .active_context_library()
                .as_ref()
                .map(|d| (d.tab, d.bot, d.instance, d.revision))
                == self
                    .bot_context
                    .data
                    .as_ref()
                    .map(|d| (d.tab, d.bot, d.instance, d.revision))
    }
    pub(super) fn sync_bot_context(&mut self) -> bool {
        let data = self.active_context_library();
        if data.is_some() {
            if self.context_view.section() != Section::Library {
                self.context_document_section = self.context_view.section();
                self.context_view.select(Section::Library);
            }
        } else if self.context_view.section() == Section::Library {
            self.context_view.select(self.context_document_section);
        }
        let visible = self.context.is_open();
        let active = data
            .as_ref()
            .filter(|_| visible)
            .map(|d| (d.tab, d.instance));
        if self
            .bot_context
            .data
            .as_ref()
            .map(|data| (data.tab, data.instance))
            != active
        {
            self.each_tab_mut(|tab| match &mut tab.view {
                TabView::Bot(view) => {
                    let visible = active.is_some()
                        && view
                            .context_library_data(tab.id)
                            .map(|d| (d.tab, d.instance))
                            == active;
                    view.show_context_library(visible)
                }
                _ => false,
            });
        }
        if self
            .context_return_tab
            .as_ref()
            .is_some_and(|(tab, instance, _)| Some((*tab, *instance)) != active)
        {
            self.context_return_tab = None;
        }
        let floating = self.regions.context.w < theme::CONTEXT_W;
        let area = if floating {
            let gutter = theme::NAV_PAD;
            let width = theme::CONTEXT_W.min((self.viewport.0 - gutter * 2.0).max(0.0));
            Rect::new(
                (self.viewport.0 - gutter - width).max(0.0),
                self.regions.context.y + gutter,
                width,
                (self.regions.context.h - gutter * 2.0).max(0.0),
            )
        } else {
            self.context_rect()
        };
        let changed = self
            .bot_context
            .update(data.filter(|_| visible), area, floating);
        if self.bot_context.data.is_some()
            && floating
            && self.bot_context.focus.is_none()
            && self.bot_context_unobstructed()
        {
            self.take_bot_context_focus();
        }
        changed
    }
    pub(super) fn draw_bot_context(&mut self, scene_out: &mut Scene, root: Affine) {
        self.sync_bot_context();
        if !self.bot_context_current() {
            return;
        }
        let area = self.bot_context.area;
        let _scope = koru_draw::inspect::scope(
            "Bot Library panel",
            "container",
            root,
            scene::rect(area.x, area.y, area.w, area.h),
            |_| {},
        );
        if self.bot_context.floating {
            scene::floating_panel(
                scene_out,
                root,
                area.x,
                area.y,
                area.w,
                area.h,
                &scene::Panel::popover(theme::NAV_PAD),
            );
        } else {
            scene::fill(
                scene_out,
                root,
                theme::p().pane_bg,
                &scene::rect(area.x, area.y, area.w, area.h),
            );
        }
        if let Some((tree, frame)) = self.bot_context.layout(&mut self.engine) {
            frame.paint(&tree, scene_out, root, &mut self.engine);
        }
    }
    pub(super) fn focus_bot_context(&mut self) -> bool {
        self.sync_bot_context();
        self.take_bot_context_focus()
    }
    fn take_bot_context_focus(&mut self) -> bool {
        if !self.bot_context_current() || !self.bot_context_unobstructed() {
            return false;
        }
        let tab = self.tabs().active_tab();
        if self.context_return_tab.is_none() {
            let focus = match &tab.view {
                TabView::Bot(view) => view.focus(),
                _ => None,
            };
            let instance = self
                .bot_context
                .data
                .as_ref()
                .expect("current Bot context")
                .instance;
            self.context_return_tab = Some((tab.id, instance, focus));
        }
        self.release_tab_keyboard();
        self.chrome_focus = None;
        self.reminder_shell.blur_bell();
        self.reminder_creation.blur_pill();
        self.explore.clear_focus();
        self.ai_focused = false;
        self.bot_context.focus_first()
    }
    pub(super) fn blur_bot_context(&mut self) -> bool {
        self.bot_context.focus.take().is_some()
    }
    pub(super) fn return_bot_context_focus(&mut self) {
        let back = self.context_return_tab.take();
        self.bot_context.focus = None;
        self.bot_context.pressed = None;
        self.bot_context.drag = None;
        if let Some((tab, instance, Some(key))) = back {
            if self
                .active_context_library()
                .is_some_and(|data| data.tab == tab && data.instance == instance)
            {
                if let TabView::Bot(view) = self.active_view() {
                    view.accessibility_action(key, false);
                }
            }
        }
    }
    fn dismiss_bot_context(&mut self) {
        self.return_bot_context_focus();
        if self.bot_context.floating {
            self.context.set_immediately(false);
            self.relayout();
        }
    }
    pub(super) fn bot_context_action(&mut self, key: Key) -> bool {
        self.sync_bot_context();
        if !self.bot_context_current() {
            return false;
        }
        let Some(action) = self.bot_context.action(key) else {
            return false;
        };
        self.apply_bot_context_action(action)
    }
    fn apply_bot_context_action(&mut self, action: Action) -> bool {
        if action == Action::Escape {
            self.dismiss_bot_context();
            return true;
        }
        let dismiss = matches!(action, Action::Open(_) | Action::Manage);
        let changed = match self.active_view() {
            TabView::Bot(view) => view.perform_context_library(action),
            _ => false,
        };
        if changed && dismiss {
            self.context_return_tab = None;
            self.bot_context.focus = None;
            if self.bot_context.floating {
                self.context.set_immediately(false);
                self.relayout();
            }
        }
        changed
    }
    pub(super) fn bot_context_key(
        &mut self,
        key: &InputKey,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        self.sync_bot_context();
        if !self.bot_context_current() || !self.bot_context_unobstructed() {
            return false;
        }
        if self.bot_context.floating {
            use super::keys::Chord;
            if matches!(
                super::keys::resolve(key, text, mods),
                Some(
                    Chord::ToggleContext
                        | Chord::NextContextSection
                        | Chord::FocusChrome
                        | Chord::FocusChromeBack
                )
            ) {
                return false;
            }
            if self.bot_context.focus.is_none() {
                self.focus_bot_context();
            }
        }
        let Some(action) = self.bot_context.key(key, mods) else {
            return self.bot_context.floating;
        };
        if let Some(action) = action {
            self.apply_bot_context_action(action);
        }
        true
    }
    pub(super) fn bot_context_press(&mut self, x: f32, y: f32, ctrl: bool) -> bool {
        self.sync_bot_context();
        if !self.bot_context_current() || !self.bot_context_unobstructed() {
            return false;
        }
        if !self.bot_context.area.contains(x, y) {
            self.blur_bot_context();
            if self.bot_context.floating {
                self.dismiss_bot_context();
                self.bot_context_release_owed = true;
                return true;
            }
            return false;
        }
        self.focus_bot_context();
        self.bot_context_release_owed = true;
        self.bot_context.pressed = None;
        self.bot_context.drag = None;
        if ctrl || self.mods.control_key() || self.mods.alt_key() || self.mods.super_key() {
            return true;
        }
        if self.bot_context.press_scrollbar(x, y) {
            return true;
        }
        self.bot_context.pressed = self
            .bot_context
            .hit(x, y)
            .filter(|key| self.bot_context.action(*key).is_some());
        self.bot_context.focus = self.bot_context.pressed;
        true
    }
    pub(super) fn bot_context_release(&mut self) -> bool {
        if !std::mem::take(&mut self.bot_context_release_owed) {
            return false;
        }
        self.sync_bot_context();
        self.bot_context.drag = None;
        let pressed = self.bot_context.pressed.take();
        if self.bot_context_current() {
            let (x, y) = self.pointer;
            if let Some(key) = pressed.filter(|key| self.bot_context.hit(x, y) == Some(*key)) {
                self.bot_context_action(key);
            }
        }
        true
    }

    pub(super) fn bot_context_hover(&mut self, x: f32, y: f32) -> Option<bool> {
        self.sync_bot_context();
        if !self.bot_context_current() || !self.bot_context_unobstructed() {
            return None;
        }
        if self.bot_context.drag_to(y) {
            self.tooltip.clear();
            return Some(true);
        }
        let hot = self
            .bot_context
            .hit(x, y)
            .filter(|key| self.bot_context.action(*key).is_some());
        let changed = self.bot_context.hot != hot;
        self.bot_context.hot = hot;
        if !self.bot_context.area.contains(x, y) {
            return None;
        }
        self.explore.clear_hover();
        self.context_view.clear_hover();
        let tooltip = hot.and_then(|key| {
            self.bot_context
                .with_frame(|frame| {
                    let node = frame.node(key)?;
                    let words = match node.label.as_ref()? {
                        koru_draw::el::Name::Text(words) => words.clone(),
                        koru_draw::el::Name::Key(words) => {
                            koru_i18n::t(crate::format::lang(), *words).into()
                        }
                    };
                    Some(koru_ui::tooltip::TooltipContent::new(
                        self.bot_context.occurrence(key).unwrap_or(0),
                        node.rect,
                        words,
                    ))
                })
                .flatten()
        });
        // The ordinary tooltip owner still schedules and draws this content.
        let tip_changed = self.tooltip.set_hover(tooltip, std::time::Instant::now());
        Some(changed || tip_changed)
    }

    fn context_occurrence(
        &self,
        cx: &super::accessibility::Shell,
        key: Key,
    ) -> Option<koru_workspace_host::accessibility::Occurrence> {
        use koru_workspace_host::accessibility::ControlPart;
        Some(cx.instance(
            crate::bots::view::context_library::SURFACE,
            self.bot_context.occurrence(key)?,
            ControlPart::ROOT,
        ))
    }
    pub(super) fn bot_context_semantics(
        &self,
        cx: &super::accessibility::Shell,
    ) -> Option<koru_workspace_host::accessibility::SemanticNode> {
        use crate::bots::view::context_library::BODY;
        use accesskit::{Action as A, Role};
        use koru_workspace_host::accessibility::SemanticNode;
        if !self.bot_context_current() || !self.bot_context_unobstructed() {
            return None;
        }
        let data = self.bot_context.data.as_ref()?;
        let children = self
            .bot_context
            .with_frame(|frame| {
                frame
                    .nodes
                    .iter()
                    .filter(|node| node.focusable)
                    .filter_map(|node| {
                        let key = node.key?;
                        let label = match node.label.as_ref()? {
                            koru_draw::el::Name::Text(words) => words.clone(),
                            koru_draw::el::Name::Key(words) => {
                                koru_i18n::t(data.lang, *words).into()
                            }
                        };
                        let mut control =
                            SemanticNode::new(self.context_occurrence(cx, key)?, Role::Button)
                                .with_label(label)
                                .disabled(node.disabled);
                        if let Some(bounds) = node
                            .hit_region()
                            .filter(|bounds| bounds.w > 0.0 && bounds.h > 0.0)
                        {
                            control = control.with_bounds(bounds);
                        }
                        if !node.disabled {
                            control = control.with_actions([A::Click, A::Focus]);
                        }
                        Some(control)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut label = koru_i18n::t(data.lang, koru_i18n::Key::BotsLibrary).to_owned();
        for notice in &data.notices {
            label.push_str(". ");
            label.push_str(notice);
        }
        Some(
            SemanticNode::new(self.context_occurrence(cx, BODY)?, Role::Group)
                .with_label(label)
                .with_bounds(self.bot_context.area)
                .with_children(children),
        )
    }
    pub(super) fn bot_context_accessible_focus(
        &self,
        cx: &super::accessibility::Shell,
    ) -> Option<koru_workspace_host::accessibility::Occurrence> {
        if !self.bot_context_current() || !self.bot_context_unobstructed() {
            return None;
        }
        self.context_occurrence(cx, self.bot_context.focus?)
    }
    pub(super) fn bot_context_accessible_action(
        &mut self,
        target: koru_workspace_host::accessibility::Occurrence,
        action: accesskit::Action,
    ) -> bool {
        use koru_workspace_host::accessibility::{ControlPart, ObjectKey};
        self.sync_bot_context();
        if !self.bot_context_current() || !self.bot_context_unobstructed() {
            return false;
        }
        let ObjectKey::Instance { kind, id } = target.object else {
            return false;
        };
        if kind != crate::bots::view::context_library::SURFACE || target.part != ControlPart::ROOT {
            return false;
        }
        let Some(key) = self.bot_context.key_for_occurrence(id) else {
            return false;
        };
        if self.bot_context.action(key).is_none() {
            return false;
        }
        match action {
            accesskit::Action::Click => self.bot_context_action(key),
            accesskit::Action::Focus => {
                self.focus_bot_context();
                self.bot_context.focus = Some(key);
                self.bot_context.reveal();
                true
            }
            _ => false,
        }
    }
    pub(super) fn revise_bot_context(
        &self,
        rev: &mut koru_workspace_host::accessibility::Revision,
    ) {
        rev.bool(self.bot_context_current());
        if !self.bot_context_current() {
            return;
        }
        let area = self.bot_context.area;
        rev.f32(area.x)
            .f32(area.y)
            .f32(area.w)
            .f32(area.h)
            .f32(self.bot_context.scroll);
        if let Some(data) = &self.bot_context.data {
            rev.u64(data.tab)
                .u64(data.instance)
                .u64(data.revision)
                .bool(data.loading);
            for page in &data.pages {
                rev.str(&page.label);
            }
            for notice in &data.notices {
                rev.str(notice);
            }
            for key in self.bot_context.keys() {
                rev.u64(self.bot_context.occurrence(key).unwrap_or(0));
            }
        }
        rev.u64(
            self.bot_context
                .focus
                .and_then(|key| self.bot_context.occurrence(key))
                .unwrap_or(0),
        );
    }
}
