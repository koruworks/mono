// SPDX-License-Identifier: MPL-2.0

use peniko::ImageBrush;

use koru_session_protocol::{
    Attachment, AttachmentId, AttachmentSource, AttachmentStatus, DeliveryMode, DeliveryState,
    ErrorCode, IdempotencyKey, PendingInput, SendPendingInputResult, SessionId,
};

/// The composer never lays out more than this many Unicode scalar values.
/// Runtime and wire limits remain authoritative; this is the immediate UI
/// bound that keeps a pasted draft from turning every frame into linear work.
pub const MAX_DRAFT_CHARS: usize = 32_768;

#[derive(Clone)]
pub struct PreviewImage {
    pub brush: ImageBrush,
    pub width: u32,
    pub height: u32,
}

impl std::fmt::Debug for PreviewImage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreviewImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingAttachmentStatus {
    Ingesting,
    Ready,
    Rejected { code: ErrorCode },
}

#[derive(Clone, Debug)]
pub struct PendingAttachment {
    pub id: AttachmentId,
    pub source: AttachmentSource,
    pub status: PendingAttachmentStatus,
    pub delivery_mode: DeliveryMode,
    pub delivery: DeliveryState,
    pub preview: Option<PreviewImage>,
}

impl PendingAttachment {
    pub fn ingesting(id: AttachmentId, source: AttachmentSource) -> Self {
        Self {
            id,
            source,
            status: PendingAttachmentStatus::Ingesting,
            delivery_mode: DeliveryMode::Unsupported,
            delivery: DeliveryState::Pending,
            preview: None,
        }
    }

    pub fn from_attachment(attachment: Attachment) -> Self {
        let status = match attachment.status {
            AttachmentStatus::Ingesting => PendingAttachmentStatus::Ingesting,
            AttachmentStatus::Pending => PendingAttachmentStatus::Ready,
            AttachmentStatus::Rejected { error } => {
                PendingAttachmentStatus::Rejected { code: error.code }
            }
            AttachmentStatus::Removed => PendingAttachmentStatus::Rejected {
                code: ErrorCode::AttachmentNotFound,
            },
        };
        Self {
            id: attachment.id,
            source: attachment.source,
            status,
            delivery_mode: attachment.delivery_mode,
            delivery: attachment.delivery,
            preview: None,
        }
    }

    pub fn is_ready_to_send(&self) -> bool {
        self.status == PendingAttachmentStatus::Ready
            && matches!(self.delivery, DeliveryState::Pending)
    }

    pub fn failed_delivery(&self) -> bool {
        matches!(self.delivery, DeliveryState::Failed { .. })
    }
}

#[derive(Clone, Debug, Default)]
pub struct PendingInputModel {
    session_id: Option<SessionId>,
    draft: String,
    attachments: Vec<PendingAttachment>,
    submitting: bool,
    last_submission: Option<PendingInput>,
    error: Option<ErrorCode>,
}

impl PendingInputModel {
    pub fn session_id(&self) -> Option<SessionId> {
        self.session_id
    }

    pub fn draft(&self) -> &str {
        &self.draft
    }

    pub fn attachments(&self) -> &[PendingAttachment] {
        &self.attachments
    }

    pub fn submitting(&self) -> bool {
        self.submitting
    }

    pub fn error(&self) -> Option<&ErrorCode> {
        self.error.as_ref()
    }

    pub fn has_retry(&self) -> bool {
        self.last_submission.is_some()
            && !self.submitting
            && (self.error.is_some()
                || self
                    .attachments
                    .iter()
                    .any(PendingAttachment::failed_delivery))
    }

    pub fn attach(&mut self, session_id: SessionId, attachments: Vec<Attachment>) {
        let changed_session = self.session_id != Some(session_id);
        self.session_id = Some(session_id);
        if changed_session {
            self.draft.clear();
            self.submitting = false;
            self.last_submission = None;
            self.error = None;
        }
        let previews: std::collections::BTreeMap<_, _> = self
            .attachments
            .iter_mut()
            .filter_map(|item| item.preview.take().map(|preview| (item.id, preview)))
            .collect();
        self.attachments = attachments
            .into_iter()
            .filter(|attachment| attachment.status != AttachmentStatus::Removed)
            .map(PendingAttachment::from_attachment)
            .collect();
        for item in &mut self.attachments {
            item.preview = previews.get(&item.id).cloned();
        }
    }

    pub fn detach(&mut self) {
        self.session_id = None;
        self.draft.clear();
        self.attachments.clear();
        self.submitting = false;
        self.last_submission = None;
        self.error = None;
    }

    pub fn begin_ingest(&mut self, id: AttachmentId, source: AttachmentSource) {
        self.error = None;
        if let Some(item) = self.attachments.iter_mut().find(|item| item.id == id) {
            *item = PendingAttachment::ingesting(id, source);
        } else {
            self.attachments
                .push(PendingAttachment::ingesting(id, source));
        }
    }

    pub fn ingest_succeeded(&mut self, attachment: Attachment) {
        let id = attachment.id;
        let preview = self
            .attachments
            .iter_mut()
            .find(|item| item.id == id)
            .and_then(|item| item.preview.take());
        let mut replacement = PendingAttachment::from_attachment(attachment);
        replacement.preview = preview;
        if let Some(item) = self.attachments.iter_mut().find(|item| item.id == id) {
            *item = replacement;
        } else {
            self.attachments.push(replacement);
        }
        self.error = None;
    }

    pub fn ingest_failed(&mut self, id: AttachmentId, code: ErrorCode) {
        if let Some(item) = self.attachments.iter_mut().find(|item| item.id == id) {
            item.status = PendingAttachmentStatus::Rejected { code: code.clone() };
        }
        self.error = Some(code);
    }

    pub fn set_preview(&mut self, id: AttachmentId, preview: PreviewImage) {
        if let Some(item) = self.attachments.iter_mut().find(|item| item.id == id) {
            item.preview = Some(preview);
        }
    }

    pub fn removed(&mut self, id: AttachmentId) {
        self.attachments.retain(|item| item.id != id);
        if self
            .last_submission
            .as_ref()
            .is_some_and(|input| input.attachment_ids.contains(&id))
        {
            self.last_submission = None;
        }
        if !self.attachments.iter().any(|item| {
            matches!(item.status, PendingAttachmentStatus::Rejected { .. })
                || item.failed_delivery()
        }) {
            self.error = None;
        }
    }

    pub fn insert_text(&mut self, text: &str) -> bool {
        if self.submitting || self.has_retry() || text.is_empty() {
            return false;
        }
        let remaining = MAX_DRAFT_CHARS.saturating_sub(self.draft.chars().count());
        if remaining == 0 {
            self.error = Some(ErrorCode::InputTooLarge);
            return false;
        }
        self.draft.extend(text.chars().take(remaining));
        if text.chars().count() > remaining {
            self.error = Some(ErrorCode::InputTooLarge);
        } else {
            self.error = None;
        }
        true
    }

    pub fn backspace(&mut self) -> bool {
        if self.submitting || self.has_retry() {
            return false;
        }
        let Some((index, _)) = self.draft.char_indices().next_back() else {
            return false;
        };
        self.draft.truncate(index);
        self.error = None;
        true
    }

    pub fn can_send(&self) -> bool {
        self.session_id.is_some()
            && !self.submitting
            && !self.has_retry()
            && (!self.draft.is_empty()
                || self
                    .attachments
                    .iter()
                    .any(PendingAttachment::is_ready_to_send))
    }

    pub fn begin_submission(&mut self) -> Option<PendingInput> {
        if !self.can_send() {
            return None;
        }
        let input = PendingInput {
            model: None,
            idempotency_key: IdempotencyKey::mint(),
            text: self.draft.clone(),
            attachment_ids: self
                .attachments
                .iter()
                .filter(|item| item.is_ready_to_send())
                .map(|item| item.id)
                .collect(),
            mode: None,
            fast: None,
            reasoning: None,
            skills: Vec::new(),
        };
        self.submitting = true;
        self.last_submission = Some(input.clone());
        self.error = None;
        Some(input)
    }

    pub fn begin_retry(&mut self) -> Option<PendingInput> {
        if self.submitting || !self.has_retry() {
            return None;
        }
        self.submitting = true;
        self.error = None;
        self.last_submission.clone()
    }

    pub fn submission_succeeded(&mut self, result: SendPendingInputResult) {
        for outcome in result.attachments {
            if let Some(item) = self
                .attachments
                .iter_mut()
                .find(|item| item.id == outcome.attachment_id)
            {
                item.delivery_mode = outcome.mode;
                item.delivery = outcome.state;
            }
        }
        self.submitting = false;
        self.draft.clear();
        self.error = None;
        if !self
            .attachments
            .iter()
            .any(PendingAttachment::failed_delivery)
        {
            self.last_submission = None;
        }
    }

    pub fn submission_failed(&mut self, code: ErrorCode) {
        self.submitting = false;
        self.error = Some(code);
    }

    pub fn set_error(&mut self, code: ErrorCode) {
        self.error = Some(code);
    }
}
