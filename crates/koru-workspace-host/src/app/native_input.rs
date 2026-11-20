// SPDX-License-Identifier: MPL-2.0

//! Native composition belongs to a retained editable occurrence. Geometry
//! updates never change ownership; a new owner must start a new native stream.

use super::*;
use crate::ImeTargetId;
use winit::event::Ime;

#[derive(Default)]
pub(super) struct State {
    allowed: Option<ImeTargetId>,
    owner: Option<ImeTargetId>,
    enabled: bool,
    document: Option<koru_core::DocumentId>,
}

fn cancel_owner(
    view: &mut dyn crate::View,
    workspace: &mut koru_workspace::Workspace,
    document: Option<koru_core::DocumentId>,
    owner: ImeTargetId,
) {
    if document.is_some_and(|id| workspace.document_retiring(id)) {
        return;
    }
    let doc = match document {
        Some(id) if workspace.ws.stamped_document_id() != Some(id) => {
            workspace.resident_document_mut(id)
        }
        _ => Some(&mut workspace.ws),
    };
    if let Some(doc) = doc {
        view.ime(doc, owner, &Ime::Disabled);
    }
}

impl State {
    fn route_after_sync(&mut self, event: &Ime, retargeted: bool) -> Option<ImeTargetId> {
        // The backend checked this event before entering the app callback.
        // A reset inside that callback cannot retroactively filter it.
        if retargeted { None } else { self.route(event) }
    }
    fn route(&mut self, event: &Ime) -> Option<ImeTargetId> {
        match event {
            Ime::Enabled => {
                self.enabled = self.allowed.is_some();
                self.owner = None;
                None
            }
            Ime::Disabled => {
                self.enabled = false;
                self.owner.take()
            }
            Ime::Preedit(_, _) | Ime::Commit(_) if self.enabled => {
                let owner = *self.owner.get_or_insert(self.allowed?);
                (Some(owner) == self.allowed).then_some(owner)
            }
            _ => None,
        }
    }
}

impl<H: Host> App<H> {
    pub(super) fn native_key_in_flight(&self, id: WindowId) -> bool {
        self.panes
            .get(&id)
            .is_some_and(|pane| pane.native_input.owner.is_some())
    }
    pub(super) fn interrupt_native_input(&mut self, id: WindowId) {
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        if pane.native_input.owner.is_none() {
            return;
        }
        if let Some(owner) = pane.native_input.owner.take().or(pane.native_input.allowed) {
            cancel_owner(
                &mut pane.view,
                &mut self.runtime.workspace,
                pane.native_input.document,
                owner,
            );
        }
        pane.native_input.enabled = false;
        pane.window.set_ime_allowed(false);
        if pane.native_input.allowed.is_some() {
            pane.window.set_ime_allowed(true);
        }
    }

    pub(super) fn sync_native_input(&mut self, id: WindowId) -> bool {
        let Some(pane) = self.panes.get_mut(&id) else {
            return false;
        };
        let target = (!self.in_tray && pane.focused)
            .then(|| pane.view.ime_target())
            .flatten();
        let next = target.map(|target| target.id);
        let retargeted = next != pane.native_input.allowed;
        if retargeted {
            if let Some(owner) = pane.native_input.owner.take().or(pane.native_input.allowed) {
                cancel_owner(
                    &mut pane.view,
                    &mut self.runtime.workspace,
                    pane.native_input.document,
                    owner,
                );
            }
            // The pinned winit producer epochs retire queued events and reset
            // native composition before reuse. No Disabled acknowledgement is
            // promised by an idle native context; never wait for one.
            pane.window.set_ime_allowed(false);
            pane.native_input.enabled = false;
            pane.native_input.allowed = next;
            pane.native_input.document = pane.view.editing_document().map(|(_, document)| document);
            if next.is_some() {
                pane.window.set_ime_allowed(true);
            }
        }
        if let Some(rect) = target.and_then(|target| target.cursor_area) {
            let zoom = self.zoom as f64;
            pane.window.set_ime_cursor_area(
                winit::dpi::LogicalPosition::new(rect.x as f64 * zoom, rect.y as f64 * zoom),
                winit::dpi::LogicalSize::new(
                    rect.w.max(1.0) as f64 * zoom,
                    rect.h.max(1.0) as f64 * zoom,
                ),
            );
        }
        retargeted
    }

    pub(super) fn native_input(&mut self, id: WindowId, event: &Ime) {
        let retargeted = self.sync_native_input(id);
        let before = self.runtime.workspace.ws.version();
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        let target = pane.native_input.route_after_sync(event, retargeted);
        let Some(target) = target else {
            return;
        };
        let handled = pane.view.ime(&mut self.runtime.workspace.ws, target, event);
        if matches!(event, Ime::Commit(_)) {
            pane.native_input.owner = None;
        }
        if self.runtime.workspace.ws.version() != before {
            self.runtime.workspace.touched();
            self.adopt_others(id);
        }
        if handled {
            self.wake(id);
        }
    }
}

#[cfg(test)]
mod tests;
