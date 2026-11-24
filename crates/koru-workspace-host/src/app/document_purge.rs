// SPDX-License-Identifier: MPL-2.0
//! The two choices share the workspace's durable reconciliation on every host.
use super::*;

#[cfg(windows)]
mod native;

impl<H: Host> App<H> {
    pub(super) fn reconcile_document_purge_ui(&mut self, event_loop: &ActiveEventLoop) {
        // A completed worker may already have forgotten the source document.
        // Keep its identity until every window has retired the old editor.
        let mut frozen: Vec<_> = self
            .runtime
            .workspace
            .take_document_purge_closed()
            .into_iter()
            .map(|document| (document, None))
            .collect();
        for (source, replacement) in self.runtime.workspace.take_document_purge_replacements() {
            frozen.push((source, None));
            self.document_purge_replacements.push(replacement);
        }
        frozen.extend(self.runtime.workspace.documents().filter_map(|doc| {
            let id = doc.own_document_id()?;
            self.runtime
                .workspace
                .document_purge_frozen(id)
                .then(|| (id, koru_workspace::document_root(doc)))
        }));
        let mut requests = Vec::new();
        let mut quiesced = HashSet::new();
        let mut documents = HashSet::new();
        for (document, root) in frozen {
            if !documents.insert(document) {
                continue;
            }
            for (&id, pane) in &mut self.panes {
                if pane.view.quiesce_purged_document(document, root) {
                    pane.window.request_redraw();
                    quiesced.insert(id);
                    // A container can have an unrelated user window request;
                    // only the retired standalone emits a purge close.
                    if !pane.view.is_container()
                        && let Some(request) = pane.view.take_window_request()
                    {
                        requests.push((id, request));
                    }
                }
            }
        }
        if self.dictation_pane.is_some_and(|id| quiesced.contains(&id)) {
            self.interrupt_dictation(Interruption::WindowClosed);
        }
        for (id, request) in requests {
            if matches!(request, WindowRequest::Close) {
                // This is retirement of a frozen editor, not a user closing
                // Koru: no restore-as-tab or tray choice may reopen that id.
                if self.container_pane().is_none() {
                    // Keep a workspace surface alive, including when the only
                    // document window was hidden in the tray. Replacing its
                    // editor with Welcome neither reveals a window nor exits
                    // over pending durable reconciliation.
                    let mut view = self.host.duplicate(&self.runtime.workspace.ws);
                    view.adopt_preferences(&self.runtime.settings);
                    if let Some(pane) = self.panes.get_mut(&id) {
                        view.seat_accessibility(pane.access.slot(), pane.access.view_slot());
                        view.attach_surface_host(crate::native_surface::SurfaceHost::new(
                            pane.window.clone(),
                            self.wake_proxy.clone(),
                        ));
                        pane.view = view;
                    }
                    self.primary = Some(id);
                    self.sync_pinned(id);
                    self.sync_viewport(id);
                    self.sync_pane_context();
                    self.push_entries();
                } else {
                    self.close_pane(event_loop, id);
                }
            } else {
                self.perform_window_request(event_loop, id, request);
            }
        }
        let prompts = self.runtime.workspace.document_purge_prompts();
        for pane in self.panes.values_mut() {
            if pane.view.take_review_document_deletions() {
                self.document_purge_asked.clear();
            }
            if pane.view.set_pending_document_deletions(prompts.len()) {
                pane.window.request_redraw();
            }
        }
        self.document_purge_asked
            .retain(|id| prompts.iter().any(|p| p.document == *id));
        if self.in_tray || self.panes.is_empty() {
            return;
        }
        // A background Keep completion must not open a hidden editor or make
        // a window visible. The durable new document is ready when we return.
        for replacement in std::mem::take(&mut self.document_purge_replacements) {
            match self.runtime.workspace.open_document(replacement) {
                Ok(()) => {
                    self.opened_document(replacement);
                    self.push_entries();
                    self.show_active_document();
                }
                Err(error) => self.report(ErrorReport::new("Koru", error.to_string())),
            }
        }
        let Some(prompt) = prompts
            .into_iter()
            .find(|p| !self.document_purge_asked.contains(&p.document))
        else {
            return;
        };
        self.document_purge_asked.insert(prompt.document);
        let lang = koru_i18n::current();
        use koru_i18n::{Key, fill, t};
        let keep = t(lang, Key::DocumentDeletedKeep).to_owned();
        let delete = t(lang, Key::DocumentDeletedDelete).to_owned();
        let body = fill(
            t(
                lang,
                if prompt.unknown {
                    Key::DocumentDeletedUnknown
                } else {
                    Key::DocumentDeletedChanged
                },
            ),
            &[&prompt.title],
        );
        #[cfg(windows)]
        let answer = match native::show(
            self.primary
                .and_then(|id| self.panes.get(&id))
                .map(|pane| pane.window.as_ref()),
            t(lang, Key::DocumentDeletedTitle),
            &body,
            [&keep, &delete, t(lang, Key::WorkspacesCancel)],
            lang.is_rtl(),
        ) {
            Ok(answer) => answer,
            Err(()) => {
                tracing::warn!(target: "cloud.document.removal", code = "choice_dialog_unavailable", "retaining the unanswered document");
                self.report(ErrorReport::new(
                    t(lang, Key::DocumentDeletedTitle),
                    t(lang, Key::DocumentDeletePending),
                ));
                return;
            }
        };
        #[cfg(not(windows))]
        let answer = self
            .message_dialog()
            .set_title(t(lang, Key::DocumentDeletedTitle))
            .set_description(body)
            .set_buttons(rfd::MessageButtons::YesNoCancelCustom(
                keep.clone(),
                delete.clone(),
                t(lang, Key::WorkspacesCancel).to_owned(),
            ))
            .show();
        let Some(decision) = decision_for_dialog(answer, &keep, &delete) else {
            return;
        };
        if let Err(error) = self
            .runtime
            .workspace
            .decide_document_purge(prompt.document, decision)
        {
            self.document_purge_asked.remove(&prompt.document);
            self.report(ErrorReport::new(t(lang, Key::DocumentDeletedTitle), error));
        }
        self.invalidate_everywhere();
    }
}

// Some native backends return Yes/No for custom buttons; others return labels.
// Cancellation and unexpected answers must retain the frozen source.
fn decision_for_dialog(
    answer: rfd::MessageDialogResult,
    keep: &str,
    delete: &str,
) -> Option<koru_workspace::DocumentPurgeDecision> {
    use koru_workspace::DocumentPurgeDecision::{Delete, Keep};
    use rfd::MessageDialogResult::{Custom, No, Yes};
    match answer {
        Yes => Some(Keep),
        No => Some(Delete),
        Custom(label) if label == keep => Some(Keep),
        Custom(label) if label == delete => Some(Delete),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
