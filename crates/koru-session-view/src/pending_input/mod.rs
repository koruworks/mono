// SPDX-License-Identifier: MPL-2.0

mod interaction;
mod layout;
mod model;
mod render;

use std::path::PathBuf;

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_session_protocol::{
    Attachment, AttachmentId, AttachmentSource, ErrorCode, SendPendingInputResult, SessionId,
};
use vello::Scene;
use vello::kurbo::Affine;

use crate::{SessionSurfaceIntent, SessionSurfaceInteraction};

pub use interaction::PendingInputIntent;
pub use layout::{AttachmentLayout, COMPOSER_HEIGHT, PendingInputHit, PendingInputLayout};
pub use model::{
    MAX_DRAFT_CHARS, PendingAttachment, PendingAttachmentStatus, PendingInputModel, PreviewImage,
};

/// Localized copy is supplied by the host so this reusable renderer stays
/// independent of a process locale and of Koru's translation table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingInputCopy {
    pub heading: String,
    pub placeholder: String,
    pub add_attachment: String,
    pub send: String,
    pub retry: String,
    pub remove: String,
    pub ingesting: String,
    pub rejected: String,
    pub delivering: String,
    pub delivered: String,
    pub delivery_failed: String,
    pub delivery_native: String,
    pub delivery_path: String,
    pub delivery_unsupported: String,
    pub native_short: String,
    pub path_short: String,
    pub unsupported_short: String,
    pub limit_error: String,
    pub invalid_image: String,
    pub host_unavailable: String,
    pub generic_error: String,
}

impl Default for PendingInputCopy {
    fn default() -> Self {
        Self {
            heading: "Pending input".into(),
            placeholder: "Write a message or command".into(),
            add_attachment: "Add image".into(),
            send: "Send".into(),
            retry: "Retry".into(),
            remove: "Remove image".into(),
            ingesting: "Adding…".into(),
            rejected: "Rejected".into(),
            delivering: "Sending…".into(),
            delivered: "Sent".into(),
            delivery_failed: "Delivery failed".into(),
            delivery_native: "Images are sent directly to this provider".into(),
            delivery_path: "Images are sent as safely quoted local paths".into(),
            delivery_unsupported: "This session cannot receive images".into(),
            native_short: "Direct".into(),
            path_short: "Path".into(),
            unsupported_short: "Unsupported".into(),
            limit_error: "The attachment limit was reached".into(),
            invalid_image: "That file is not a supported image".into(),
            host_unavailable: "The session host is unavailable".into(),
            generic_error: "The input could not be sent".into(),
        }
    }
}

/// Reusable pending-input state, geometry, rendering, and command vocabulary.
/// A host owns only window event translation and asynchronous mechanism work.
pub struct PendingInputView<I: SessionSurfaceInteraction> {
    interaction: I,
    model: PendingInputModel,
    copy: PendingInputCopy,
    focused: bool,
    hovered: Option<PendingInputHit>,
    last_layout: Option<PendingInputLayout>,
}

impl<I: SessionSurfaceInteraction> PendingInputView<I> {
    /// Send through another connection from now on.
    pub fn set_interaction(&mut self, interaction: I) {
        self.interaction = interaction;
    }

    pub fn new(interaction: I, copy: PendingInputCopy) -> Self {
        Self {
            interaction,
            model: PendingInputModel::default(),
            copy,
            focused: false,
            hovered: None,
            last_layout: None,
        }
    }

    pub fn interaction_mut(&mut self) -> &mut I {
        &mut self.interaction
    }

    pub fn model(&self) -> &PendingInputModel {
        &self.model
    }

    pub fn copy(&self) -> &PendingInputCopy {
        &self.copy
    }

    pub fn set_copy(&mut self, copy: PendingInputCopy) {
        self.copy = copy;
    }

    pub fn attach(&mut self, session_id: SessionId, attachments: Vec<Attachment>) {
        self.model.attach(session_id, attachments);
    }

    pub fn detach(&mut self) {
        self.model.detach();
        self.focused = false;
        self.hovered = None;
        self.last_layout = None;
    }

    pub fn begin_ingest(&mut self, id: AttachmentId, source: AttachmentSource) {
        self.model.begin_ingest(id, source);
    }

    pub fn ingest_succeeded(&mut self, attachment: Attachment) {
        self.model.ingest_succeeded(attachment);
    }

    pub fn ingest_failed(&mut self, id: AttachmentId, code: ErrorCode) {
        self.model.ingest_failed(id, code);
    }

    pub fn set_preview(&mut self, id: AttachmentId, preview: PreviewImage) {
        self.model.set_preview(id, preview);
    }

    pub fn removed(&mut self, id: AttachmentId) {
        self.model.removed(id);
    }

    pub fn submission_succeeded(&mut self, result: SendPendingInputResult) {
        self.model.submission_succeeded(result);
    }

    pub fn submission_failed(&mut self, code: ErrorCode) {
        self.model.submission_failed(code);
    }

    pub fn set_error(&mut self, code: ErrorCode) {
        self.model.set_error(code);
    }

    pub fn insert_text(&mut self, text: &str) -> bool {
        self.model.insert_text(text)
    }

    pub fn backspace(&mut self) -> bool {
        self.model.backspace()
    }

    pub fn set_focused(&mut self, focused: bool) -> bool {
        let changed = self.focused != focused;
        self.focused = focused;
        changed
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn hovered(&self) -> Option<PendingInputHit> {
        self.hovered
    }

    pub fn layout(&self, area: Rect) -> PendingInputLayout {
        PendingInputLayout::solve(area, self.model.attachments().iter().map(|item| item.id))
    }

    pub fn pointer_moved(&mut self, area: Rect, x: f32, y: f32) -> bool {
        let layout = self.layout(area);
        let next = layout.hit(x, y, self.model.has_retry());
        let changed = self.hovered != next;
        self.hovered = next;
        self.last_layout = Some(layout);
        changed
    }

    pub fn click(&mut self, area: Rect, x: f32, y: f32) -> bool {
        let layout = self.layout(area);
        let hit = layout.hit(x, y, self.model.has_retry());
        self.last_layout = Some(layout);
        match hit {
            Some(PendingInputHit::Draft) => self.set_focused(true),
            Some(PendingInputHit::Attach)
                if !self.model.submitting() && !self.model.has_retry() =>
            {
                self.focused = false;
                self.interaction
                    .dispatch(SessionSurfaceIntent::PendingInput(
                        PendingInputIntent::AcquireClipboardImage,
                    ));
                true
            }
            Some(PendingInputHit::Send) => {
                let Some(input) = self.model.begin_submission() else {
                    return false;
                };
                self.interaction
                    .dispatch(SessionSurfaceIntent::PendingInput(
                        PendingInputIntent::Send { input },
                    ));
                true
            }
            Some(PendingInputHit::Retry) => {
                let Some(input) = self.model.begin_retry() else {
                    return false;
                };
                self.interaction
                    .dispatch(SessionSurfaceIntent::PendingInput(
                        PendingInputIntent::Retry { input },
                    ));
                true
            }
            Some(PendingInputHit::Remove(attachment_id)) if !self.model.submitting() => {
                let local_only = self.model.attachments().iter().any(|item| {
                    item.id == attachment_id
                        && matches!(item.status, PendingAttachmentStatus::Rejected { .. })
                });
                if local_only {
                    self.model.removed(attachment_id);
                    return true;
                }
                self.interaction
                    .dispatch(SessionSurfaceIntent::PendingInput(
                        PendingInputIntent::RemoveAttachment { attachment_id },
                    ));
                true
            }
            _ => false,
        }
    }

    pub fn dropped_files(&mut self, paths: Vec<PathBuf>) -> bool {
        if paths.is_empty()
            || self.model.session_id().is_none()
            || self.model.submitting()
            || self.model.has_retry()
        {
            return false;
        }
        self.interaction
            .dispatch(SessionSurfaceIntent::PendingInput(
                PendingInputIntent::AddFiles { paths },
            ));
        true
    }

    pub fn paste_clipboard(&mut self) -> bool {
        if self.model.session_id().is_none() || self.model.submitting() || self.model.has_retry() {
            return false;
        }
        self.interaction
            .dispatch(SessionSurfaceIntent::PendingInput(
                PendingInputIntent::PasteClipboard,
            ));
        true
    }

    pub fn draw(&mut self, canvas: &mut Scene, root: Affine, engine: &mut TextEngine, area: Rect) {
        let layout = self.layout(area);
        render::draw(
            canvas,
            root,
            engine,
            &layout,
            render::RenderState {
                model: &self.model,
                copy: &self.copy,
                focused: self.focused,
                hovered: self.hovered,
            },
        );
        self.last_layout = Some(layout);
    }

    pub fn control_rect(&self, hit: PendingInputHit) -> Option<Rect> {
        let layout = self.last_layout.as_ref()?;
        match hit {
            PendingInputHit::Draft => Some(layout.draft),
            PendingInputHit::Attach => Some(layout.attach),
            PendingInputHit::Send | PendingInputHit::Retry => Some(layout.send),
            PendingInputHit::Remove(id) => layout
                .attachments
                .iter()
                .find(|item| item.id == id)
                .map(|item| item.remove),
        }
    }
}
