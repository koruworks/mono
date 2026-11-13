// SPDX-License-Identifier: MPL-2.0

//! Child-local landing, with no shell navigation reservation.

use super::*;
use koru_workspace_host::ChildDocumentRequest;

impl MainView {
    pub(super) fn sync_mini_app_refinement_target(&mut self) {
        let target = if self.ai_panel.is_open() {
            match &self.tabs().active_tab().view {
                TabView::MiniApp(app) => Some(app.refinement_target()),
                _ => None,
            }
        } else {
            None
        };
        self.ai_chat.set_mini_app_refinement_target(target);
    }

    pub(super) fn open_pending_mini_apps(&mut self) -> bool {
        self.sync_mini_app_refinement_target();
        let mut ready = Vec::new();
        self.ai_chat.offer_mini_apps(self.ai_panel.is_open());
        if let Some(creation) = self.ai_chat.take_mini_app_creation() {
            ready.push((creation, self.ai_panel.is_open()));
        }
        let focused = self.panes.focused_path().clone();
        for (path, group) in self.panes.iter_groups_mut() {
            let active = group.tabs().active_tab().id;
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::AiChat(view) = &mut tab.view {
                    view.offer_mini_apps(path == focused && active == tab.id);
                    if let Some(creation) = view.take_mini_app_creation() {
                        ready.push((creation, path == focused && active == tab.id));
                    }
                }
            }
        }
        let refinement = self.ai_chat.take_mini_app_refinement();
        let changed = !ready.is_empty() || refinement.is_some();
        if let Some(refinement) = refinement {
            let applied = self.apply_mini_app_refinement(&refinement);
            self.ai_chat
                .mini_app_refined(refinement.host.request_id, applied);
        }
        for (creation, visible) in ready {
            let renderer = creation.host.renderer_id;
            let request = creation.host.request_id;
            let opened = visible && self.open_mini_app_creation(&creation);
            if self.ai_chat.mini_app_renderer_id() == renderer {
                self.ai_chat.mini_app_opened(request, opened);
            }
            for (_, group) in self.panes.iter_groups_mut() {
                for tab in group.tabs_mut().iter_mut() {
                    if let TabView::AiChat(view) = &mut tab.view
                        && view.mini_app_renderer_id() == renderer
                    {
                        view.mini_app_opened(request, opened);
                    }
                }
            }
        }
        changed
    }

    fn apply_mini_app_refinement(&mut self, refinement: &koru_ai_ui::MiniAppRefinement) -> bool {
        if !self.ai_panel.is_open()
            || refinement.host.renderer_id != self.ai_chat.mini_app_renderer_id()
        {
            return false;
        }
        let Some((_, profile, _, Some(workspace))) = &self.pane_context else {
            return false;
        };
        if profile.as_uuid() != refinement.profile.as_uuid()
            || workspace.as_uuid() != refinement.host.workspace_id.as_uuid()
        {
            return false;
        }
        let Some(target) = refinement.host.refinement else {
            return false;
        };
        let TabView::MiniApp(app) = &mut self.tabs_mut().active_mut().view else {
            return false;
        };
        if app.workspace.as_uuid() != refinement.host.workspace_id.as_uuid() {
            return false;
        }
        app.refine(target, &refinement.envelope).is_ok()
    }
    fn open_mini_app_creation(&mut self, creation: &koru_ai_ui::MiniAppCreation) -> bool {
        let Some((_, profile, _, Some(workspace))) = &self.pane_context else {
            return false;
        };
        if profile.as_uuid() != creation.profile.as_uuid()
            || workspace.as_uuid() != creation.host.workspace_id.as_uuid()
        {
            return false;
        }
        let workspace = *workspace;
        let Ok(declaration) =
            koru_ai_core::mini_apps::Declaration::validate(creation.envelope.clone())
        else {
            return false;
        };
        if let koru_ai_core::mini_apps::TaskSelection::List(id) = declaration.selection
            && !self
                .list_table
                .iter()
                .any(|list| list.id.as_uuid() == id && list.deleted_ms.is_none())
        {
            return false;
        }
        let mut tab = Tab::welcome();
        let Ok(mut app) =
            crate::mini_app::MiniApp::from_declaration(workspace, tab.id, declaration)
        else {
            return false;
        };
        // Creation already minted an invocation-qualified live surface. Saved
        // artifact opening uses the constructor's fresh lifetime instead.
        if !app.adopt_created_surface(&creation.envelope.surface.surface_id) {
            return false;
        }
        tab.home = Some(workspace);
        tab.label = koru_i18n::t(crate::format::lang(), koru_i18n::Key::MiniAppTitle).into();
        app.artifact.owner = Some(*profile);
        app.artifact.title = tab.label.clone();
        tab.view = TabView::MiniApp(Box::new(app));
        self.tabs_mut().push(tab);
        self.slides_mut().clear();
        self.relayout();
        self.size_visible_groups();
        true
    }

    pub(super) fn open_mini_app_fixture(&mut self) -> bool {
        let Some(workspace) = self.current_workspace() else {
            return false;
        };
        let mut tab = Tab::welcome();
        tab.home = Some(workspace);
        tab.label = koru_i18n::t(crate::format::lang(), koru_i18n::Key::MiniAppTitle).to_owned();
        let Ok(mut app) = crate::mini_app::MiniApp::fixture(workspace, tab.id) else {
            return false;
        };
        app.artifact.owner = self
            .pane_context
            .as_ref()
            .map(|(_, profile, _, _)| *profile);
        app.artifact.title = tab.label.clone();
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
