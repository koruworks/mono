// SPDX-License-Identifier: MPL-2.0

//! The exact Bot reply's work reader owns the whole window while open.
use super::*;

impl MainView {
    pub(super) fn bot_work_surface(&self) -> Option<&ChatSurface> {
        self.tabs()
            .active_tab()
            .view
            .chat_view()
            .map(|view| view.dictation_surface())
    }

    pub(super) fn bot_work_surface_mut(&mut self) -> Option<&mut ChatSurface> {
        self.tabs_mut()
            .active_view()
            .chat_view_mut()
            .map(|view| view.dictation_surface_mut())
    }

    pub(super) fn bot_work_open(&self) -> bool {
        self.bot_work_surface()
            .is_some_and(ChatSurface::bot_work_open)
    }

    pub(super) fn bot_work_owns_input(&self) -> bool {
        self.blockers() == Blockers::Modal(ModalKind::BotWork)
    }

    pub(super) fn bot_work_pointer(&mut self, x: f32, y: f32, press: bool) -> bool {
        if !self.bot_work_owns_input() {
            return false;
        }
        self.sync_viewer_lift();
        self.bot_work_surface_mut()
            .is_some_and(|surface| surface.bot_work_pointer(x, y, press))
    }

    pub(super) fn bot_work_key(&mut self, key: &Key, mods: ModifiersState) -> bool {
        self.bot_work_surface_mut()
            .is_some_and(|surface| surface.bot_work_key(key, mods))
    }

    pub(super) fn draw_bot_work(&mut self, canvas: &mut Scene, root: Affine) {
        if !self.bot_work_open() {
            return;
        }
        self.sync_viewer_lift();
        if let Some(surface) = self.bot_work_surface_mut() {
            surface.draw_bot_work(canvas, root);
        }
    }
}

#[cfg(test)]
mod tests;
