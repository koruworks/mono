// SPDX-License-Identifier: MPL-2.0

//! Window-owned workspace requests and confirmations. Manage projects this
//! controller but does not own its lifetime, answer queue, or modal input.

use super::*;
use crate::manage::workspaces::app_workspace;
use koru_workspaces_ui::Request;

impl MainView {
    pub(super) fn drain_workspace_requests(&mut self) -> bool {
        let requests = self.workspace_controller.borrow_mut().take_requests();
        let mut changed = false;
        for request in requests {
            changed |= match request {
                Request::Open(id) => self.open_workspace(app_workspace(id)),
                Request::OpenInNewWindow(id) => {
                    self.ask_pane(PaneRequest::OpenInNewWindow(app_workspace(id)))
                }
                Request::Create => self.prompt_new_workspace(),
                Request::Rename(id) => self.start_workspace_rename(app_workspace(id)),
                Request::ChangeAppearance(id) => self.start_workspace_icon(app_workspace(id)),
                Request::Import => self.open_import_menu(),
                Request::Export(id) => {
                    self.ask_pane(PaneRequest::ExportWorkspace(app_workspace(id)))
                }
                Request::CopyId(id) => {
                    koru_editor::clipboard::put_system_text(&id.to_storage());
                    true
                }
            };
        }
        changed
    }

    pub(super) fn workspace_confirmation_key(&mut self, key: &Key) -> bool {
        let mut controller = self.workspace_controller.borrow_mut();
        let Some(dialog) = controller.dialog_mut() else {
            return false;
        };
        let outcome = dialog.key(key);
        controller.answer(outcome);
        true
    }

    pub(super) fn workspace_confirmation_mouse_down(&mut self, x: f32, y: f32) -> bool {
        let mut controller = self.workspace_controller.borrow_mut();
        let Some(dialog) = controller.dialog_mut() else {
            return false;
        };
        let outcome = dialog.mouse_down(&mut self.engine, self.viewport, x, y);
        controller.answer(outcome);
        true
    }

    pub(super) fn workspace_confirmation_mouse_move(&mut self, x: f32, y: f32) -> bool {
        let mut controller = self.workspace_controller.borrow_mut();
        let Some(dialog) = controller.dialog_mut() else {
            return false;
        };
        dialog.mouse_move(&mut self.engine, self.viewport, x, y);
        true
    }

    pub(super) fn draw_workspace_confirmation(&mut self, scene: &mut Scene, root: Affine) {
        if let Some(dialog) = self.workspace_controller.borrow().dialog() {
            let _prompt = koru_draw::inspect::scope(
                "Workspace confirmation",
                "part",
                root,
                koru_draw::scene::rect(0.0, 0.0, self.viewport.0, self.viewport.1),
                |record| record.property("title", dialog.title(), "window workspace controller"),
            );
            dialog.draw(scene, root, &mut self.engine, self.viewport);
        }
    }
}
