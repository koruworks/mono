// SPDX-License-Identifier: MPL-2.0

//! Child-local landing, with no shell navigation reservation.

use super::*;
use koru_workspace_host::ChildDocumentRequest;

impl MainView {
    pub(super) fn open_mini_app_fixture(&mut self) -> bool {
        let Some(workspace) = self.current_workspace() else {
            return false;
        };
        let mut tab = Tab::welcome();
        tab.home = Some(workspace);
        tab.label = koru_i18n::t(crate::format::lang(), koru_i18n::Key::MiniAppTitle).to_owned();
        let Ok(app) = crate::mini_app::MiniApp::fixture(workspace, tab.id) else {
            return false;
        };
        tab.view = TabView::MiniApp(Box::new(app));
        self.tabs_mut().push(tab);
        self.slides_mut().clear();
        self.relayout();
        self.size_visible_groups();
        true
    }

    pub(super) fn accept_mini_app_document(
        &mut self,
        request: ChildDocumentRequest,
        doc: &WorkspaceDoc,
    ) -> bool {
        if View::child_document_request(self) != Some(request) {
            return false;
        }
        if !doc.compatibility().writable()
            || doc.id() != Some(request.workspace)
            || doc.own_document_id() != Some(request.document)
            || koru_workspace::document_root(doc).is_none()
        {
            self.fail_mini_app_document(request, koru_i18n::Key::MiniAppOpenFailed);
            return false;
        }
        let node = match &self.tabs().active_tab().view {
            TabView::MiniApp(app) => app.pending.and_then(|(_, node)| node),
            _ => None,
        };
        if let Some(node) = node {
            let reference = koru_core::EntityRef::task(request.workspace, request.document, node);
            if koru_core::tasks::refs::resolve_task_ref(doc, &reference)
                == koru_core::tasks::refs::TaskRefResolution::NotFound
            {
                self.fail_mini_app_document(request, koru_i18n::Key::MiniAppTaskGone);
                if let TabView::MiniApp(app) = &mut self.tabs_mut().active_mut().view {
                    app.tasks.panel_mut().invalidate();
                    app.tasks.mark_stale();
                }
                return false;
            }
        }
        true
    }

    pub(super) fn land_mini_app_document(
        &mut self,
        request: ChildDocumentRequest,
        doc: &WorkspaceDoc,
    ) -> bool {
        if !self.accept_mini_app_document(request, doc) {
            return false;
        }
        let before = self.shown_documents();
        let live = self.entity_live();
        let titles = self.entity_titles.clone();
        let trashed = self.entity_trashed.clone();
        let schedules = self.schedule_pill_presentations();
        let tags = self.tag_table.clone();
        let lists = self.list_table.clone();
        let offline = self.store_trouble.is_some();
        let TabView::MiniApp(app) = &mut self.tabs_mut().active_mut().view else {
            return false;
        };
        let node = app.pending.and_then(|(_, node)| node);
        if !app.land(doc, request.document) {
            return false;
        }
        if let Some(leaf) = app.leaf_mut() {
            View::set_workspace(leaf, Some(request.workspace));
            View::set_tag_table(leaf, &tags);
            View::set_list_table(leaf, &lists);
            leaf.set_entity_titles(&titles, &trashed, &live);
            leaf.set_schedule_pills(&schedules);
            leaf.set_images_offline(offline);
        }
        app.pending = None;
        if let Some(node) = node
            && let Some(leaf) = app.leaf_mut()
        {
            leaf.reveal_centered(node);
        }
        self.active_document = Some(request.document);
        self.released(before);
        true
    }

    pub(super) fn fail_mini_app_document(
        &mut self,
        request: ChildDocumentRequest,
        key: koru_i18n::Key,
    ) {
        if View::child_document_request(self) != Some(request) {
            return;
        }
        if let TabView::MiniApp(app) = &mut self.tabs_mut().active_mut().view {
            app.pending = None;
        }
        self.toast = Some((
            koru_ui::Toast::new(
                koru_i18n::t(crate::format::lang(), key).to_owned(),
                None,
                Instant::now(),
            ),
            super::ToastPayload::None,
        ));
    }
}

#[cfg(test)]
mod tests;
