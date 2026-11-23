// SPDX-License-Identifier: MPL-2.0

//! At most one load per window, coalesced to the view's newest request. The
//! completion is delivered only to its originating child, never broadcast as
//! shell navigation. Closing the destination cancels its speculative session.

use super::*;
use crate::ChildDocumentRequest;
use koru_core::DocumentId;

fn resident_restore_target(
    current: WorkspaceId,
    active: Option<DocumentId>,
    target: Option<(WorkspaceId, DocumentId)>,
    resident: impl FnOnce(DocumentId) -> Option<(Option<WorkspaceId>, Option<DocumentId>)>,
) -> Option<(WorkspaceId, DocumentId)> {
    let (workspace, document) = target?;
    if workspace != current || active == Some(document) {
        return None;
    }
    if resident(document)? != (Some(workspace), Some(document)) {
        return None;
    }
    Some((workspace, document))
}

pub(super) struct Pending {
    profile: koru_core::ProfileId,
    request: ChildDocumentRequest,
    load: koru_workspace::document_load::DocumentLoad,
}

impl<H: Host> App<H> {
    pub(super) fn restore_window_document(&mut self, source: WindowId) {
        let target = self
            .panes
            .get(&source)
            .and_then(|pane| pane.view.editing_document());
        let Some((workspace, document)) = resident_restore_target(
            self.runtime.current,
            self.runtime.workspace.active(),
            target,
            |id| {
                self.runtime
                    .workspace
                    .document(id)
                    .map(|doc| (doc.id(), doc.own_document_id()))
            },
        ) else {
            return;
        };
        // Warm-only: retain each window's editor and focus, and never notify
        // another window as if this were user navigation.
        if self
            .runtime
            .workspace
            .open_document_in(workspace, document)
            .is_ok()
        {
            self.opened_document(document);
        }
    }
    pub(super) fn service_child_documents(&mut self) {
        self.runtime.workspace.poll_document_retirements();
        self.retry_document_releases();
        self.collect_task_commands();
        if self.service_task_commands() {
            self.runtime.workspace.touched();
            if !self.in_tray {
                self.adopt_everywhere();
            }
        }
        // Cancel dead ownership before waiting for readiness or native focus.
        // Superseded generations are canceled even while a window is unfocused.
        let profile = self.runtime.profile_id();
        let workspace = self.runtime.current;
        self.child_loads.retain(|source, pending| {
            pending.profile == profile
                && pending.request.workspace == workspace
                && self
                    .panes
                    .get(source)
                    .is_some_and(|pane| pane.view.child_document_request() == Some(pending.request))
        });
        // Native focus loss follows hide asynchronously. Speculative children
        // cannot use that stale focus bit to land while every window is hidden.
        if self.in_tray {
            return;
        }
        let pending: Vec<_> = self.child_loads.keys().copied().collect();
        for source in pending {
            let Some(pending) = self.child_loads.get(&source) else {
                continue;
            };
            if !pending.load.ready() {
                continue;
            }
            // Workspace::ws is process-global. A completion in a background
            // native window may not swap the document under another window's
            // keyboard. Keep its prepared session worker-owned until focus.
            if self.panes.get(&source).is_some_and(|pane| !pane.focused) {
                continue;
            }
            let pending = self.child_loads.remove(&source).expect("present above");
            let request = pending.request;
            let wanted = pending.profile == self.runtime.profile_id()
                && request.workspace == self.runtime.current
                && self
                    .panes
                    .get(&source)
                    .is_some_and(|pane| pane.view.child_document_request() == Some(request));
            if !wanted {
                continue;
            }
            if !supported_kind(
                self.runtime
                    .workspace
                    .document_kind(request.document)
                    .as_deref(),
            ) {
                if let Some(pane) = self.panes.get_mut(&source) {
                    pane.view.child_document_failed(request);
                }
                self.request_redraw(source);
                continue;
            }
            // A warm opening wins over speculation, including its undo state.
            let accepted = if let Some(doc) = self.runtime.workspace.document(request.document) {
                self.panes
                    .get_mut(&source)
                    .is_some_and(|pane| pane.view.accept_child_document(request, doc))
            } else {
                pending
                    .load
                    .inspect(|result| match result {
                        Ok(doc) => self
                            .panes
                            .get_mut(&source)
                            .is_some_and(|pane| pane.view.accept_child_document(request, doc)),
                        Err(_) => {
                            if let Some(pane) = self.panes.get_mut(&source) {
                                pane.view.child_document_failed(request);
                            }
                            false
                        }
                    })
                    .unwrap_or(false)
            };
            if !accepted {
                self.request_redraw(source);
                continue;
            }
            if self.runtime.workspace.document(request.document).is_none() {
                match self
                    .runtime
                    .workspace
                    .install_prepared_document(pending.load)
                {
                    Ok(_) => {}
                    Err(_) => {
                        if let Some(pane) = self.panes.get_mut(&source) {
                            pane.view.child_document_failed(request);
                        }
                        continue;
                    }
                }
            }
            self.finish_child_document(source, request);
        }
        // The view retains only its latest intent. A request arriving during
        // I/O does not spawn a second worker or allocate another result queue.
        let requests: Vec<_> = self
            .panes
            .iter()
            .filter_map(|(&source, pane)| {
                pane.view
                    .child_document_request()
                    .map(|request| (source, request))
            })
            .collect();
        for (source, request) in requests {
            if self.panes.get(&source).is_some_and(|pane| !pane.focused) {
                continue;
            }
            if self.child_loads.contains_key(&source) || self.child_loads.len() >= 8 {
                continue;
            }
            if request.workspace != self.runtime.current {
                if let Some(pane) = self.panes.get_mut(&source) {
                    pane.view.child_document_failed(request);
                }
                continue;
            }
            if !supported_kind(
                self.runtime
                    .workspace
                    .document_kind(request.document)
                    .as_deref(),
            ) {
                if let Some(pane) = self.panes.get_mut(&source) {
                    pane.view.child_document_failed(request);
                }
                self.request_redraw(source);
                continue;
            }
            if let Some(doc) = self.runtime.workspace.document(request.document) {
                if self
                    .panes
                    .get_mut(&source)
                    .is_some_and(|pane| pane.view.accept_child_document(request, doc))
                {
                    self.finish_child_document(source, request);
                }
                continue;
            }
            if self.runtime.workspace.document_retiring(request.document)
                || !koru_workspace::document_load::capacity_available()
            {
                continue;
            }
            match self
                .runtime
                .workspace
                .prepare_document(request.workspace, request.document)
            {
                Ok(load) => {
                    self.child_loads.insert(
                        source,
                        Pending {
                            profile: self.runtime.profile_id(),
                            request,
                            load,
                        },
                    );
                }
                Err(_) => {
                    if let Some(pane) = self.panes.get_mut(&source) {
                        pane.view.child_document_failed(request);
                    }
                    self.request_redraw(source);
                }
            }
        }
        // A removed native window cannot retain a speculative join. Dropping
        // the handle never joins its worker; the worker disposes its session.
        self.child_loads
            .retain(|source, _| self.panes.contains_key(source));
    }

    fn finish_child_document(&mut self, source: WindowId, request: ChildDocumentRequest) {
        // This is the warm path only: the preceding check/installation made
        // the document resident, so activation performs no locate or join.
        if self
            .runtime
            .workspace
            .open_document_in(request.workspace, request.document)
            .is_err()
        {
            if let Some(pane) = self.panes.get_mut(&source) {
                pane.view.child_document_failed(request);
            }
            return;
        }
        self.opened_document(request.document);
        if let Some(pane) = self.panes.get_mut(&source) {
            pane.view
                .child_document_loaded(request, &self.runtime.workspace.ws);
        }
        self.request_redraw(source);
    }
}

fn supported_kind(kind: Option<&str>) -> bool {
    matches!(kind, Some("page" | "document" | "note" | "task-list"))
}

struct QueuedCommand {
    source: Option<WindowId>,
    profile: koru_core::ProfileId,
    workspace: koru_core::WorkspaceId,
    command: crate::tasks::TaskCommand,
    load: Option<koru_workspace::document_load::DocumentLoad>,
}

/// Ordered accepted intents. Capacity refusal uses the ordinary command error
/// return channel; no accepted command is evicted or coalesced.
#[derive(Default)]
pub(super) struct TaskQueue(std::collections::VecDeque<QueuedCommand>);

impl TaskQueue {
    pub(super) fn has_pending(&self) -> bool {
        !self.0.is_empty()
    }
    pub(super) fn source(&self) -> Option<WindowId> {
        self.0.front().and_then(|head| head.source)
    }
    pub(super) fn in_scope(
        &self,
        profile: koru_core::ProfileId,
        workspace: koru_core::WorkspaceId,
    ) -> bool {
        self.0
            .front()
            .is_some_and(|head| head.profile == profile && head.workspace == workspace)
    }
    #[cfg(test)]
    pub(super) fn push(
        &mut self,
        profile: koru_core::ProfileId,
        workspace: koru_core::WorkspaceId,
        command: crate::tasks::TaskCommand,
    ) -> Result<(), crate::tasks::TaskCommand> {
        self.push_from(None, profile, workspace, command)
    }

    pub(super) fn push_from(
        &mut self,
        source: Option<WindowId>,
        profile: koru_core::ProfileId,
        workspace: koru_core::WorkspaceId,
        command: crate::tasks::TaskCommand,
    ) -> Result<(), crate::tasks::TaskCommand> {
        if self.0.len() >= 64 {
            return Err(command);
        }
        self.0.push_back(QueuedCommand {
            source,
            profile,
            workspace,
            command,
            load: None,
        });
        Ok(())
    }

    pub(super) fn poll(
        &mut self,
        profile: koru_core::ProfileId,
        current: koru_core::WorkspaceId,
        workspace: &mut koru_workspace::Workspace,
    ) -> Option<(crate::tasks::TaskCommand, Result<(), String>)> {
        let head = self.0.front_mut()?;
        let outcome = if head.profile != profile || head.workspace != current {
            // Check scope before readiness. Dropping the worker never joins.
            Err("workspace changed before the task command ran".into())
        } else if let Some(document) = command_document(&head.command).or_else(|| {
            matches!(head.command, crate::tasks::TaskCommand::Create { .. })
                .then(|| koru_workspace::tasks::pick_task_list(&workspace.task_list_documents()))
                .flatten()
        }) {
            if workspace.document(document).is_some() {
                Ok(())
            } else if workspace.document_retiring(document) {
                return None;
            } else if let Some(load) = &head.load {
                if !load.ready() {
                    return None;
                }
                workspace
                    .install_prepared_document(head.load.take().unwrap())
                    .map(|_| ())
            } else {
                if !koru_workspace::document_load::capacity_available() {
                    return None;
                }
                match workspace.prepare_document(current, document) {
                    Ok(load) => {
                        head.load = Some(load);
                        return None;
                    }
                    Err(error) => Err(error),
                }
            }
        } else {
            Ok(())
        };
        Some((self.0.pop_front().unwrap().command, outcome))
    }
}

fn command_document(command: &crate::tasks::TaskCommand) -> Option<koru_core::DocumentId> {
    use crate::tasks::TaskCommand::*;
    match command {
        Complete { document, .. }
        | PinToday { document, .. }
        | Remove { document, .. }
        | SetList { document, .. }
        | SetDue { document, .. }
        | SetPriority { document, .. }
        | SetColor { document, .. }
        | SetRank { document, .. }
        | Retitle { document, .. }
        | CreateHome { document, .. } => Some(*document),
        Create { .. } => None,
    }
}

#[cfg(test)]
mod tests;
