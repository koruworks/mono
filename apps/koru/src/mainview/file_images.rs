// SPDX-License-Identifier: MPL-2.0

//! Workspace files use the same modal media viewer as chat pictures.
use super::{MainView, PaneRequest};
use koru_core::{DocumentId, WorkspaceId};
use koru_ui::media_viewer::{
    Action, ActionSet, MediaViewer, Outcome, Stage, ViewerImage, ViewerOptions,
};
use std::sync::Arc;

#[derive(Default)]
pub(super) struct FileImages {
    pub viewer: Option<MediaViewer>,
    file: Option<DocumentId>,
    pending: Option<(DocumentId, Option<WorkspaceId>)>,
}

impl MainView {
    pub(super) fn begin_file_image(&mut self, file: DocumentId) -> bool {
        self.file_images.pending = Some((file, self.current_workspace()));
        true
    }

    pub(super) fn receive_file_image(
        &mut self,
        file: DocumentId,
        name: String,
        image: Arc<ViewerImage>,
    ) -> bool {
        if self.file_images.pending != Some((file, self.current_workspace())) {
            return false;
        }
        self.file_images.pending = None;
        self.menu = None;
        self.reveal_file_in_explorer(file);
        self.file_images.file = Some(file);
        self.file_images.viewer = Some(MediaViewer::open(
            Stage::Bitmap { image },
            ViewerOptions {
                actions: ActionSet::PICTURE.with(Action::SaveAs),
                title: name,
            },
            self.viewport,
        ));
        true
    }

    pub(super) fn file_image_input(
        &mut self,
        input: impl FnOnce(&mut MediaViewer) -> Outcome,
    ) -> bool {
        let Some(viewer) = self.file_images.viewer.as_mut() else {
            return false;
        };
        let outcome = input(viewer);
        match outcome {
            Outcome::Close => {
                self.file_images.viewer = None;
                self.file_images.file = None;
                self.file_images.pending = None;
            }
            Outcome::SaveAs | Outcome::OpenExternally => {
                if let Some(file) = self.file_images.file {
                    self.ask_pane(if outcome == Outcome::SaveAs {
                        PaneRequest::SaveFile(file)
                    } else {
                        PaneRequest::OpenFileExternally(file)
                    });
                }
            }
            Outcome::Handled | Outcome::Copy => {}
        }
        true
    }
}

#[cfg(test)]
mod tests;
