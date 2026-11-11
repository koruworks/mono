// SPDX-License-Identifier: MPL-2.0
//! Native picture staging for one Bot task (P1.5 attachment delivery addendum).
//!
//! A phase of the existing step machine, between the pre-turn Observe and the
//! strict initial Begin. Each picture's session id is minted once, when the
//! task's context starts. It is sent in at most one `IngestAttachment`;
//! afterwards it is only listed. A picture that is not confirmed natively goes
//! as its path, which the composed prompt names anyway, with the existing
//! `ImagesAsFiles` notice. The full input is frozen before the first Begin.
//!
//! Nothing here waits. The writer applies replies; the provider pool reads a
//! file and makes one owner call per step; listings are spaced by the step
//! machine's own `due`. Koru never removes a staged copy itself: the session
//! host removes every copy when the Run's session ends.
use super::*;
use base64::Engine as _;
use koru_session_protocol::{
    Attachment, AttachmentId, AttachmentKind, AttachmentSource as SessionSource, AttachmentStatus,
};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[cfg(test)]
mod tests;

/// The session host's per-attachment cap and the delegate's decoded bound.
pub(super) const MAX_PICTURE_BYTES: u64 = 8 * 1024 * 1024;
/// A count of listings, not a clock: the step machine's `due` spaces them.
pub(in crate::worker) const MAX_LISTINGS: u8 = 3;
/// Made on the provider thread when no owner call happened at all.
pub(super) const NOT_SENT: &str = "koru_ai_attachment_not_sent";
/// The owner's own refusal before any session call.
const TOO_LARGE: &str = "bot_attachment_too_large";

/// One picture's progress. `Planned` exists only from [`plan`]; no transition
/// leads back to it, so an id that may have been sent is never sent again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Planned,
    /// Its one `IngestAttachment` left the writer.
    Issued,
    Reconciling {
        listings: u8,
    },
    Native,
    AsPath,
    /// Never confirmed and never resent; a late commit stays unused.
    Uncertain,
}
impl State {
    fn settled(self) -> bool {
        matches!(self, Self::Native | Self::AsPath | Self::Uncertain)
    }
}

struct Picture {
    session: AttachmentId,
    /// The stored attachment's own folder; a read must stay inside it.
    dir: PathBuf,
    original: PathBuf,
    /// The exact byte count read for the one ingest, recorded off-writer.
    size: Arc<OnceLock<u64>>,
    state: State,
}

#[derive(Default)]
pub(in crate::worker) struct Staging {
    pictures: Vec<Picture>,
    /// The fence of the first issued step. Every later step reuses it, so a
    /// changed execution is refused by the owner rather than restaged.
    fence: Option<BotExecutionFence>,
    native_session: Option<bool>,
    prepared: Option<usize>,
    in_flight: Option<usize>,
    /// A picture already known to go as its path (no stored copy).
    as_files: bool,
    frozen: bool,
}

pub(super) enum Settled {
    Pending,
    Frozen {
        native: Vec<AttachmentId>,
        as_files: bool,
    },
    AlreadyFrozen,
}

/// One finite owner call and, for an ingest, the off-writer read it needs.
pub(super) struct StageStep {
    pub command: DelegateCommand,
    pub read: Option<IngestRead>,
}

pub(in crate::worker) struct IngestRead {
    dir: PathBuf,
    original: PathBuf,
    size: Arc<OnceLock<u64>>,
}

/// Mint each native picture's session id once. Pure: no disk access.
/// `as_paths` sends every picture as its path instead, with the notice.
pub(super) fn plan(
    request: &AiRequest,
    store: &crate::attachments::AttachmentStore,
    as_paths: bool,
) -> Staging {
    let mut staging = Staging::default();
    let (Some(chat), Some(last)) = (request.options.chat_id, request.messages.last()) else {
        return staging;
    };
    for attachment in koru_ai_provider_agent::native_pictures(&last.attachments) {
        match &attachment.path {
            Some(_) if as_paths => staging.as_files = true,
            Some(original) => staging.pictures.push(Picture {
                session: AttachmentId::mint(),
                dir: store.attachment_dir(chat, attachment.id),
                original: original.clone(),
                size: Arc::default(),
                state: State::Planned,
            }),
            None => staging.as_files = true,
        }
    }
    staging
}

impl Staging {
    #[cfg(test)]
    pub(super) fn with_pictures(files: Vec<(PathBuf, PathBuf)>) -> Self {
        Self {
            pictures: files
                .into_iter()
                .map(|(dir, original)| Picture {
                    session: AttachmentId::mint(),
                    dir,
                    original,
                    size: Arc::default(),
                    state: State::Planned,
                })
                .collect(),
            ..Self::default()
        }
    }

    /// The next owner call, or `None` when every picture is settled. It does
    /// not change any picture's state: only [`Self::mark_sent`] does.
    pub(super) fn next(&mut self, fence: Option<&BotExecutionFence>) -> Option<StageStep> {
        self.prepared = None;
        if self.frozen || self.in_flight.is_some() {
            return None;
        }
        self.skip_after_non_native();
        let index = self.pictures.iter().position(|p| !p.state.settled())?;
        let fence = self.fence.clone().or_else(|| fence.cloned())?;
        let picture = &self.pictures[index];
        let step = match picture.state {
            State::Planned => StageStep {
                command: DelegateCommand::IngestAttachment {
                    fence,
                    attachment_id: picture.session,
                    source: SessionSource::PublicUpload,
                    data_base64: String::new(),
                },
                read: Some(IngestRead {
                    dir: picture.dir.clone(),
                    original: picture.original.clone(),
                    size: Arc::clone(&picture.size),
                }),
            },
            State::Issued | State::Reconciling { .. } => StageStep {
                command: DelegateCommand::ListAttachments { fence },
                read: None,
            },
            State::Native | State::AsPath | State::Uncertain => return None,
        };
        self.prepared = Some(index);
        Some(step)
    }

    /// The prepared step was accepted by the provider pool. From here its
    /// picture is `Issued` for good, whatever reply (or none) arrives.
    pub(super) fn mark_sent(&mut self, fence: Option<&BotExecutionFence>) {
        let Some(index) = self.prepared.take() else {
            return;
        };
        if self.fence.is_none() {
            self.fence = fence.cloned();
        }
        let picture = &mut self.pictures[index];
        if picture.state == State::Planned {
            picture.state = State::Issued;
        }
        self.in_flight = Some(index);
    }

    /// Classify one reply for the in-flight picture. A non-retryable refusal
    /// other than size is the same failure a refused Begin is today.
    pub(super) fn apply(
        &mut self,
        result: Result<BotReply, koru_delegate_api::Failure>,
    ) -> Result<(), AiError> {
        let index = self.in_flight.take().ok_or_else(bot_route_error)?;
        let native_session = &mut self.native_session;
        let picture = &mut self.pictures[index];
        let next = match (picture.state, result) {
            (State::Issued, Ok(BotReply::Attachment { attachment })) => {
                if attachment.id != picture.session {
                    return Err(bot_route_error());
                }
                classify(&attachment, native_session)
            }
            (State::Issued, Err(error)) if error.code == NOT_SENT || error.code == TOO_LARGE => {
                State::AsPath
            }
            (State::Issued, Err(error)) if error.retryable => State::Reconciling { listings: 0 },
            (State::Reconciling { listings }, Ok(BotReply::Attachments { attachments })) => {
                match attachments.iter().find(|a| a.id == picture.session) {
                    Some(listed) if confirms(listed, picture.size.get().copied()) => {
                        classify(listed, native_session)
                    }
                    // Our random id with other metadata is not ours to use.
                    Some(listed) if picture.size.get().is_some() || !confirms(listed, None) => {
                        return Err(bot_route_error());
                    }
                    Some(_) => State::Uncertain,
                    None => listed_again(listings),
                }
            }
            (State::Reconciling { listings }, Err(error)) if error.retryable => {
                listed_again(listings)
            }
            _ => return Err(bot_route_error()),
        };
        tracing::debug!(
            target: "koru_ai::bots",
            attachment_id = %picture.session,
            stage = ?next,
            "Bot picture staging step"
        );
        picture.state = next;
        Ok(())
    }

    /// Freeze once every picture is settled. Later calls never refreeze.
    pub(super) fn settle(&mut self) -> Settled {
        if self.frozen {
            return Settled::AlreadyFrozen;
        }
        if self.in_flight.is_some() {
            return Settled::Pending;
        }
        self.skip_after_non_native();
        if self.pictures.iter().any(|p| !p.state.settled()) {
            return Settled::Pending;
        }
        self.frozen = true;
        Settled::Frozen {
            native: self
                .pictures
                .iter()
                .filter(|p| p.state == State::Native)
                .map(|p| p.session)
                .collect(),
            as_files: self.as_files || self.pictures.iter().any(|p| p.state != State::Native),
        }
    }

    /// A session that delivers no picture natively takes none of the rest:
    /// they go as paths without an owner call.
    fn skip_after_non_native(&mut self) {
        if self.native_session == Some(false) {
            for picture in &mut self.pictures {
                if picture.state == State::Planned {
                    picture.state = State::AsPath;
                }
            }
        }
    }
}

fn classify(attachment: &Attachment, native_session: &mut Option<bool>) -> State {
    let native = koru_ai_provider_agent::delivers_natively(attachment);
    *native_session = Some(native);
    if native { State::Native } else { State::AsPath }
}

/// Positive evidence only: the exact id, still unbound, of what was read.
fn confirms(listed: &Attachment, size: Option<u64>) -> bool {
    listed.status == AttachmentStatus::Pending
        && listed.turn_id.is_none()
        && listed.kind == AttachmentKind::Image
        && listed.source == SessionSource::PublicUpload
        && size.is_none_or(|size| size == listed.byte_count)
}

fn listed_again(listings: u8) -> State {
    let listings = listings.saturating_add(1);
    if listings >= MAX_LISTINGS {
        State::Uncertain
    } else {
        State::Reconciling { listings }
    }
}

impl IngestRead {
    /// On the provider thread: fill the one ingest with the picture's bytes,
    /// or answer [`NOT_SENT`] without any owner call.
    pub(super) fn load(
        self,
        command: DelegateCommand,
    ) -> Result<DelegateCommand, koru_delegate_api::Failure> {
        let DelegateCommand::IngestAttachment {
            fence,
            attachment_id,
            source,
            ..
        } = command
        else {
            return Ok(command);
        };
        let native = crate::attachments::native_copy(&self.original);
        let bytes = read_contained(&self.dir, &native).ok_or_else(|| {
            koru_delegate_api::Failure::new(NOT_SENT, "The picture was not read.", false)
        })?;
        let _ = self
            .size
            .set(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        Ok(DelegateCommand::IngestAttachment {
            fence,
            attachment_id,
            source,
            data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
        })
    }
}

/// A regular file inside `dir`, not a link, read whole, at most the cap, and
/// the same size as when it was looked at.
pub(super) fn read_contained(dir: &Path, file: &Path) -> Option<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(file).ok()?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > MAX_PICTURE_BYTES
    {
        return None;
    }
    let root = dir.canonicalize().ok()?;
    let real = file.canonicalize().ok()?;
    if !real.starts_with(&root) {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).ok()?);
    std::fs::File::open(&real)
        .ok()?
        .take(MAX_PICTURE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (u64::try_from(bytes.len()).ok()? == metadata.len()).then_some(bytes)
}

impl WorkerState {
    /// Whether this attempt retries one whose first input the owner refused
    /// for its pictures (or a frame they filled). That reason is kept on the
    /// failed attempt, so the Retry sends them as paths rather than failing
    /// the same way again. Any other prior outcome, or a failed read, keeps
    /// native delivery: nothing here guesses a refusal.
    pub(super) fn retries_refused_pictures(&self, id: InferenceAttemptId) -> bool {
        let Ok(Some(attempt)) = self.repository.attempt(id) else {
            return false;
        };
        let (Some(chat), Some(reply)) = (attempt.chat_id, attempt.assistant_message_id) else {
            return false;
        };
        // Only a Retry's reply names the reply it replaces.
        let Some(origin) = self
            .repository
            .message(reply)
            .ok()
            .flatten()
            .and_then(|message| message.origin_message_id)
        else {
            return false;
        };
        self.repository
            .attempts_for_chat(chat)
            .ok()
            .and_then(|attempts| {
                attempts
                    .into_iter()
                    .find(|prior| prior.assistant_message_id == Some(origin))
            })
            .is_some_and(|prior| {
                prior.status == AttemptStatus::Failed
                    && prior
                        .stop_reason
                        .as_deref()
                        .and_then(koru_delegate_core::BotInputRefusal::from_code)
                        .is_some_and(koru_delegate_core::BotInputRefusal::concerns_pictures)
            })
    }

    /// Freeze the staged pictures into the initial input when settled.
    /// Returns whether the task may now Begin.
    pub(super) fn bot_stage_settled(
        &mut self,
        id: InferenceAttemptId,
        chat: &mut LiveChat,
    ) -> bool {
        match chat.staging.settle() {
            Settled::Pending => false,
            Settled::AlreadyFrozen => true,
            Settled::Frozen { native, as_files } => {
                tracing::info!(
                    target: "koru_ai::bots",
                    attempt = %id,
                    native = native.len(),
                    as_files,
                    "Bot pictures frozen into the initial input"
                );
                chat.input.attachment_ids = native;
                if as_files {
                    self.publish(AiEvent::Notice {
                        attempt_id: id,
                        notice: koru_ai_core::AttemptNotice::ImagesAsFiles,
                        at: Timestamp::now(),
                    });
                }
                true
            }
        }
    }

    pub(super) fn apply_bot_stage(
        &mut self,
        id: InferenceAttemptId,
        chat: &mut LiveChat,
        result: Result<BotReply, koru_delegate_api::Failure>,
    ) -> Result<bool, AiError> {
        chat.staging.apply(result)?;
        Ok(self.bot_stage_settled(id, chat))
    }
}
