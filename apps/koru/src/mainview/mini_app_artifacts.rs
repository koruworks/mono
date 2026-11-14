// SPDX-License-Identifier: MPL-2.0

//! Artifact routing and save receipts. All Store calls belong to the bounded
//! worker; this module rechecks current scope before touching a tab.
use super::*;
use crate::mini_app::artifact::{self, Answer, Command, Opened, Saved};

impl MainView {
    pub(super) fn mini_app_save_close_veto(&mut self, selected: Option<&[u64]>) -> bool {
        let pending = self.mini_app_pins.pending_writes>0 || self.panes.iter_groups().flat_map(|(_, group)| group.tabs().iter()).any(|tab| {
            selected.is_none_or(|ids| ids.contains(&tab.id))
                && matches!(&tab.view, TabView::MiniApp(app) if app.artifact.requested || app.artifact.in_flight || app.pin.pending())
        });
        if pending {
            self.artifact_notice(koru_i18n::Key::MiniAppSaveClosingPending);
        }
        pending
    }
    pub(super) fn request_artifact_open(
        &mut self,
        version: koru_delegate_core::ArtifactVersionId,
    ) -> bool {
        if !(koru_features::ai() || koru_features::agent_views()) {
            return false;
        }
        let Some((_, profile, _, Some(workspace))) = &self.pane_context else {
            return false;
        };
        let command = Command::Open {
            epoch: self.mini_app_artifacts.epoch(),
            profile: *profile,
            workspace: *workspace,
            version,
        };
        match self.mini_app_artifacts.send(command) {
            Ok(()) => true,
            Err(_) => self.artifact_notice(koru_i18n::Key::MiniAppArtifactOpenFailed),
        }
    }

    pub(super) fn maintain_mini_app_artifacts(&mut self) -> bool {
        let scope = self
            .pane_context
            .as_ref()
            .and_then(|(_, p, _, w)| w.map(|w| (*p, w)));
        let mut changed = false;
        let mut refresh_artifacts = false;
        let mut notices = Vec::new();
        for answer in self.mini_app_artifacts.drain() {
            match answer {
                Answer::Saved {
                    tab,
                    profile,
                    workspace,
                    registration,
                    captured_revision,
                    result,
                } => {
                    for (_, group) in self.panes.iter_groups_mut() {
                        for candidate in group.tabs_mut().iter_mut() {
                            if candidate.id == tab
                                && let TabView::MiniApp(app) = &mut candidate.view
                            {
                                if app.workspace.as_uuid() != workspace.as_uuid()
                                    || !app.artifact.in_flight
                                    || app.artifact.operation.as_ref().is_none_or(|op| {
                                        op.registration.registration_id != registration
                                            || op.captured_revision != captured_revision
                                    })
                                {
                                    continue;
                                }
                                if result.is_err()
                                    && scope.is_some_and(|(p, w)| {
                                        p == profile && w.as_uuid() == workspace.as_uuid()
                                    })
                                {
                                    notices.push(koru_i18n::Key::MiniAppSaveFailed);
                                }
                                refresh_artifacts |= result.is_ok()
                                    && scope.is_some_and(|(p, w)| {
                                        p == profile && w.as_uuid() == workspace.as_uuid()
                                    });
                                let pin_waiting = app.pin.saving == Some(registration);
                                app.artifact.finish(result.clone());
                                if pin_waiting {
                                    if result.is_ok() {
                                        if let Some(saved) = &app.artifact.saved {
                                            app.pin.saved(saved, &app.artifact.title);
                                        }
                                    } else {
                                        app.pin.failed = true;
                                    }
                                }
                                changed = true;
                            }
                        }
                    }
                }
                Answer::Opened {
                    epoch,
                    profile,
                    workspace,
                    version,
                    result,
                } => {
                    if epoch != self.mini_app_artifacts.epoch()
                        || scope != Some((profile, workspace))
                    {
                        continue;
                    }
                    if result
                        .as_ref()
                        .is_ok_and(|opened| opened.version() != version)
                    {
                        changed |= self.artifact_notice(koru_i18n::Key::MiniAppArtifactOpenFailed);
                        continue;
                    }
                    changed |= self.apply_artifact_open(result);
                }
            }
        }
        if refresh_artifacts {
            self.explore.artifacts_mut().refresh();
        }
        let mut saves = Vec::new();
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                let TabView::MiniApp(app) = &mut tab.view else {
                    continue;
                };
                if let Some(notice) = app.artifact.notice.take() {
                    if scope.is_some_and(|(_, workspace)| workspace == app.workspace) {
                        notices.push(notice);
                        changed = true;
                    }
                }
                if !app.artifact.requested || app.artifact.in_flight {
                    continue;
                }
                let Some(profile) = app.artifact.owner else {
                    continue;
                };
                app.artifact.requested = false;
                changed = true;
                let Some(operation) = app.artifact.operation.as_ref().cloned() else {
                    continue;
                };
                app.artifact.in_flight = true;
                app.artifact.failed = false;
                saves.push(Command::Save {
                    tab: tab.id,
                    profile,
                    operation,
                });
            }
        }
        for command in saves {
            let Command::Save {
                tab,
                profile,
                operation,
            } = &command
            else {
                unreachable!()
            };
            let tab = *tab;
            let owner = *profile;
            let workspace = operation.snapshot.workspace();
            if self.mini_app_artifacts.send(command).is_err() {
                for (_, group) in self.panes.iter_groups_mut() {
                    for candidate in group.tabs_mut().iter_mut() {
                        if candidate.id == tab
                            && let TabView::MiniApp(app) = &mut candidate.view
                        {
                            app.artifact.in_flight = false;
                            app.artifact.failed = true;
                            if app.pin.saving.is_some() {
                                app.pin.failed = true;
                            }
                        }
                    }
                }
                if scope.is_some_and(|(profile, current)| {
                    profile == owner && current.as_uuid() == workspace.as_uuid()
                }) {
                    notices.push(koru_i18n::Key::MiniAppSaveFailed);
                }
            }
        }
        for key in notices {
            changed |= self.artifact_notice(key);
        }
        changed
    }

    pub(super) fn apply_artifact_open(&mut self, result: artifact::Result<Opened>) -> bool {
        let opened = match result {
            Ok(opened) => opened,
            Err(_) => return self.artifact_notice(koru_i18n::Key::MiniAppArtifactOpenFailed),
        };
        match opened {
            Opened::Other { version, family } => {
                self.open_markdown_artifact_tab(koru_artifacts_ui::OpenedArtifact {
                    version: version.id,
                    title: family.family.title.as_str().into(),
                    size_bytes: version.size_bytes,
                    produced_by: version.produced_by.map(|p| p.run),
                    held_elsewhere: None,
                })
            }
            Opened::MiniApp {
                version,
                family,
                envelope,
            } => {
                let Some(workspace) = self.current_workspace() else {
                    return false;
                };
                if envelope.workspace().as_uuid() != workspace.as_uuid()
                    || !self.mini_app_documents_available(&envelope.document_references())
                    || envelope.task_list().is_some_and(|id| {
                        !self
                            .list_table
                            .iter()
                            .any(|l| l.id.as_uuid() == id && l.deleted_ms.is_none())
                    })
                {
                    return self.artifact_notice(koru_i18n::Key::MiniAppArtifactOpenFailed);
                }
                // A version is immutable, but every reopen after closure gets
                // a fresh Tab id and fresh runtime identity, never saved IDs.
                if let Some((path,index)) = self.panes.iter_groups().find_map(|(path,g)| g.tabs().iter().enumerate().find_map(|(i,t)| {
                    matches!(&t.view, TabView::MiniApp(app) if app.artifact.saved.as_ref().is_some_and(|s| s.version == version.id && s.snapshot == envelope) && app.saved_arrangement_is_current() && app.artifact.operation.is_none()).then_some((path.clone(),i))
                })) {
                    self.panes.set_focus(path); self.select(index); return true;
                }
                let mut tab = Tab::welcome();
                let declaration = envelope.instantiate(
                    envelope.workspace(),
                    format!("mini-app-{}", uuid::Uuid::new_v4()),
                );
                let Ok(declaration) = declaration else {
                    return self.artifact_notice(koru_i18n::Key::MiniAppArtifactOpenFailed);
                };
                let Ok(mut app) =
                    crate::mini_app::MiniApp::from_declaration(workspace, tab.id, declaration)
                else {
                    return false;
                };
                app.artifact.saved = Some(Saved {
                    family: version.family,
                    version: version.id,
                    snapshot: envelope,
                });
                tab.home = Some(workspace);
                tab.label = family.family.title.as_str().into();
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
        }
    }
    fn artifact_notice(&mut self, key: koru_i18n::Key) -> bool {
        self.show_notice(koru_i18n::t(crate::format::lang(), key).into())
    }
}

#[cfg(test)]
mod tests;
