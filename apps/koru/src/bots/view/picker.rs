// SPDX-License-Identifier: MPL-2.0

//! **Adding a selected page** (Bots P1.8 S5): *Add a page…* opens the
//! shared entity picker — the one the `@` list and the Tasks page's *Add
//! existing* use (`koru_editor::components::entity_picker`) — over the
//! access card, searching **only the Bot's home workspace**, because the
//! owner takes no page from another (`normalize_pages`). The window feeds
//! it the way it feeds the Tasks page's: the scope, its recents and the
//! search service's answers (`MainView::sync_bot_page_picker`). A page
//! picked goes into the unsaved access choice; nothing is sent until
//! *Save*.

use koru_ai_core::bots::BotPage;
use koru_draw::geometry::Rect;
use koru_editor::components::Outcome;
use koru_editor::components::entity_picker::{DocumentChoice, EntityPicker, WorkspaceChoice};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};

use super::BotView;
use super::region::ADD_PAGE;

impl BotView {
    /// Open the picker under *Add a page…*.
    pub(super) fn open_page_picker(&mut self) {
        self.picker = Some(Box::new(crate::input_engine::with(EntityPicker::for_link)));
        self.picker_hover = None;
    }

    pub(crate) fn page_picker_open(&self) -> bool {
        self.picker.is_some()
    }

    /// Close it; the keyboard goes back to *Add a page…*.
    fn close_page_picker(&mut self) {
        self.picker = None;
        self.picker_hover = None;
        if self.settings_open {
            self.focus = Some(ADD_PAGE);
        }
    }

    /// The picker's panel: under *Add a page…*, kept inside the tab.
    fn picker_rect(&self) -> Rect {
        let anchor = self
            .with_frame(|_, frame| frame.node(ADD_PAGE).map(|node| node.rect))
            .unwrap_or_else(|| Rect::new(0.0, 0.0, 0.0, 0.0));
        let height = self.picker.as_deref().map_or(0.0, EntityPicker::height);
        let w = koru_draw::theme::PANEL_W;
        let gap = koru_draw::theme::Space::S1.px(koru_draw::theme::Profile::Desktop);
        let x = anchor.x.min(self.size.0 - w - gap).max(gap);
        let y = (anchor.y + anchor.h + gap)
            .min(self.size.1 - height - gap)
            .max(gap);
        Rect::new(x, y, w, height)
    }

    /// **The scope the picker searches**: the Bot's home workspace alone.
    /// `true` when it changed what is owed.
    pub(crate) fn set_picker_context(&mut self, workspaces: Vec<WorkspaceChoice>) -> bool {
        let Some(home) = self.facts.as_ref().map(|facts| facts.home) else {
            return false;
        };
        let home = koru_core::WorkspaceId::from_uuid(home.as_uuid());
        let only: Vec<WorkspaceChoice> = workspaces
            .into_iter()
            .filter(|workspace| workspace.id == home)
            .collect();
        self.picker
            .as_deref_mut()
            .is_some_and(|picker| picker.set_context(home, only))
    }

    /// Hits as the picker may offer them: not a page already selected.
    fn picker_hits(&self, hits: Vec<DocumentChoice>) -> Vec<DocumentChoice> {
        let listed: Vec<koru_core::DocumentId> = self
            .settings
            .pages_shown()
            .iter()
            .map(|page| koru_core::DocumentId::from_uuid(page.document.as_uuid()))
            .collect();
        hits.into_iter()
            .filter(|hit| !listed.contains(&hit.target.document))
            .collect()
    }

    /// The window's recents, while nothing is typed.
    pub(crate) fn feed_picker_recents(&mut self, hits: Vec<DocumentChoice>) -> bool {
        let hits = self.picker_hits(hits);
        self.picker
            .as_deref_mut()
            .is_some_and(|picker| picker.set_recent_results(hits))
    }

    /// The search the picker owes, if any.
    pub(crate) fn take_picker_search(
        &mut self,
    ) -> Option<koru_editor::components::entity_picker::EntitySearchRequest> {
        self.picker
            .as_deref_mut()
            .and_then(EntityPicker::take_request)
    }

    /// The search service's answer for what was typed.
    pub(crate) fn set_picker_results(
        &mut self,
        hits: Vec<DocumentChoice>,
        unavailable: bool,
    ) -> bool {
        let hits = self.picker_hits(hits);
        match self.picker.as_deref_mut() {
            Some(picker) => {
                picker.set_results(hits, unavailable);
                true
            }
            None => false,
        }
    }

    /// What the picker said: a page picked joins the unsaved access choice
    /// and the picker goes; anything else leaves it up.
    fn finish_picker(&mut self, outcome: Outcome) {
        if let Outcome::Commit(spec) = outcome
            && let Some(target) = spec.document
            && let Some(home) = self.facts.as_ref().map(|facts| facts.home)
        {
            self.settings.add_page(BotPage {
                workspace: home,
                document: koru_ai_core::DocumentId::from_uuid(target.document.as_uuid()),
            });
            self.close_page_picker();
        }
    }

    /// A key while the picker is up: all of them are its.
    pub(super) fn picker_key(
        &mut self,
        key: &WinitKey,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        if matches!(key, WinitKey::Named(NamedKey::Escape)) {
            self.close_page_picker();
            return true;
        }
        let Some(picker) = self.picker.as_deref_mut() else {
            return false;
        };
        let outcome = crate::input_engine::with(|engine| picker.key_full(engine, key, text, mods));
        self.finish_picker(outcome);
        true
    }

    /// A press while the picker is up: inside it, the picker's; outside it,
    /// it closes.
    pub(super) fn picker_press(&mut self, x: f32, y: f32) -> bool {
        let panel = self.picker_rect();
        if !panel.contains(x, y) {
            self.close_page_picker();
            return true;
        }
        let (lx, ly) = (x - panel.x, y - panel.y);
        let Some(picker) = self.picker.as_deref_mut() else {
            return false;
        };
        if ly < picker.field_band() {
            crate::input_engine::with(|engine| picker.click_field(engine, lx, panel.w));
            return true;
        }
        if picker.scrolls() && picker.scrollbar_track(panel.w).contains(lx, ly) {
            picker.press_scrollbar(panel.w, ly);
            return true;
        }
        let outcome = picker.click(lx, ly, panel.w);
        self.finish_picker(outcome);
        true
    }

    pub(super) fn picker_move(&mut self, x: f32, y: f32) -> bool {
        let panel = self.picker_rect();
        let Some(picker) = self.picker.as_deref_mut() else {
            return false;
        };
        if picker.is_dragging_scrollbar() {
            picker.drag_scrollbar(panel.w, y - panel.y);
            return true;
        }
        let hover = panel
            .contains(x, y)
            .then(|| picker.item_at(y - panel.y))
            .flatten();
        let changed = hover != self.picker_hover;
        self.picker_hover = hover;
        changed
    }

    pub(super) fn picker_release(&mut self) -> bool {
        self.picker
            .as_deref_mut()
            .is_some_and(EntityPicker::stop_scrollbar_drag)
    }

    pub(super) fn picker_scroll(&mut self, delta: MouseScrollDelta) {
        if let Some(picker) = self.picker.as_deref_mut() {
            let rows = match delta {
                MouseScrollDelta::LineDelta(_, y) => -(y.signum() as isize) * 2,
                MouseScrollDelta::PixelDelta(p) => -(p.y.signum() as isize),
            };
            picker.scroll_rows(rows);
        }
    }

    /// Paint the picker over the region: the shared standalone picker's
    /// own drawing.
    pub(super) fn paint_picker(&mut self, scene: &mut Scene, root: Affine) {
        if self.picker.is_none() {
            return;
        }
        let panel = self.picker_rect();
        let hover = self.picker_hover;
        if let Some(picker) = self.picker.as_deref_mut() {
            crate::input_engine::with(|engine| {
                koru_editor::components::draw::draw_standalone_entity_picker(
                    scene,
                    root,
                    engine,
                    picker,
                    (panel.x, panel.y),
                    hover,
                );
            });
        }
    }
}
