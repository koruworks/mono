// SPDX-License-Identifier: MPL-2.0

//! The bytes of chat attachments (chat-attachments D1, D5, D14, D16;
//! ADR-479).
//!
//! The AI service owns a folder beside `ai.db`:
//!
//! ```text
//! attachments/<chat_id>/<attachment_id>/file/<file_name>   the original
//! attachments/<chat_id>/<attachment_id>/thumb.png          pictures only
//! attachments/<chat_id>/<attachment_id>/sent.png|sent.jpg  the agent's copy, when needed
//! attachments/.staging/<attachment_id>.<nonce>/            a store in flight
//! ```
//!
//! Everything here runs on a caller's thread — the gateway's blocking
//! thread, the cleanup thread, a provider thread — and never on the
//! dispatcher, which only reads and writes rows. A store is checked
//! (allowlist, content), hashed, decoded within bounds, prepared, written
//! into a staging folder and then published by one rename, so a crash
//! never leaves half an attachment where a row could name it. Logs carry
//! no file name, path, hash or content.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Instant, SystemTime};

use image::{DynamicImage, ImageFormat, ImageReader};
use koru_ai_core::attachments::{
    AttachmentFamily, CONTENT_CHECK_BYTES, MAX_ATTACHMENT_BYTES, MAX_PREVIEW_EDGE,
    MAX_PREVIEW_PIXELS, MAX_SENT_COPY_BYTES, SENT_COPY_EDGE, THUMBNAIL_EDGE, allowed_type,
    content_agrees, extension_of, picture_media_type, sanitize_file_name,
};
use koru_ai_core::{
    AttachmentKind, AttachmentRecord, AttachmentSource, ChatAttachment, ChatAttachmentId, ChatId,
    Timestamp,
};
use koru_ai_sqlite::KnownAttachmentDirs;
use koru_ai_wire::{Failure, attachment_codes};

pub(crate) const ATTACHMENTS_DIR: &str = "attachments";
const STAGING_DIR: &str = ".staging";
const ORIGINAL_DIR: &str = "file";
const THUMBNAIL_FILE: &str = "thumb.png";
const SENT_PNG: &str = "sent.png";
const SENT_JPEG: &str = "sent.jpg";
/// The JPEG qualities tried, in order, for a sent copy PNG cannot keep
/// under [`MAX_SENT_COPY_BYTES`].
const JPEG_QUALITIES: [u8; 3] = [85, 75, 65];
/// The smallest a sent copy is made while it still will not fit; a JPEG of
/// this size is far under the bound whatever it shows.
const MIN_SENT_COPY_EDGE: u32 = 256;

/// Where the attachment folders of one AI root live, and the one place that
/// knows their layout.
#[derive(Clone, Debug)]
pub(crate) struct AttachmentStore {
    root: Arc<PathBuf>,
    /// The ids being stored right now, so a second upload of the same id
    /// cannot publish over the first's folder.
    storing: Arc<Mutex<HashSet<ChatAttachmentId>>>,
    /// The source identity for restages made by this process. A copy in
    /// progress can be cancelled before its row is published.
    restaging: Arc<Mutex<HashMap<ChatAttachmentId, RestageState>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestageState {
    Copying {
        source: ChatAttachmentId,
        cancelled: bool,
    },
    Completed(ChatAttachmentId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RestageStart {
    Copy,
    Existing,
    Conflict,
}

/// One id's claim on being stored; released when dropped.
pub(crate) struct StoringClaim {
    storing: Arc<Mutex<HashSet<ChatAttachmentId>>>,
    id: ChatAttachmentId,
}

impl Drop for StoringClaim {
    fn drop(&mut self) {
        self.storing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.id);
    }
}

/// One store, checked and written to staging, not yet published.
#[derive(Debug)]
pub(crate) struct Prepared {
    staging: PathBuf,
    pub(crate) record: AttachmentRecord,
}

/// What the start-up sweep removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SweepCounts {
    pub chats: u64,
    pub attachments: u64,
    pub staging: u64,
}

/// One attachment folder a branch copies into its own chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BranchFolder {
    pub from: (ChatId, ChatAttachmentId),
    pub to: (ChatId, ChatAttachmentId),
}

impl AttachmentStore {
    /// The store under an AI root (the folder that holds `ai.db`).
    pub(crate) fn new(ai_root: &Path) -> Self {
        Self {
            root: Arc::new(ai_root.join(ATTACHMENTS_DIR)),
            storing: Arc::default(),
            restaging: Arc::default(),
        }
    }

    pub(crate) fn start_restage(
        &self,
        new_id: ChatAttachmentId,
        source: ChatAttachmentId,
    ) -> RestageStart {
        let mut states = self
            .restaging
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match states.get(&new_id) {
            Some(RestageState::Completed(existing)) if *existing == source => {
                RestageStart::Existing
            }
            Some(_) => RestageStart::Conflict,
            None => {
                states.insert(
                    new_id,
                    RestageState::Copying {
                        source,
                        cancelled: false,
                    },
                );
                RestageStart::Copy
            }
        }
    }

    /// A delete marks a copy that has not published its row. If publication
    /// already holds this lock, the caller waits and then removes the row.
    pub(crate) fn cancel_restage(&self, id: ChatAttachmentId) -> bool {
        let mut states = self
            .restaging
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match states.get_mut(&id) {
            Some(RestageState::Copying { cancelled, .. }) => {
                *cancelled = true;
                true
            }
            Some(RestageState::Completed(_)) => {
                states.remove(&id);
                false
            }
            None => false,
        }
    }

    /// Serialize the cancellation check with row publication. The closure
    /// may call the dispatcher; deletion waits for it to complete.
    pub(crate) fn finish_restage<T>(
        &self,
        id: ChatAttachmentId,
        publish: impl FnOnce() -> Result<T, koru_ai_wire::Failure>,
    ) -> Result<Option<T>, koru_ai_wire::Failure> {
        let mut states = self
            .restaging
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let source = match states.get(&id) {
            Some(RestageState::Copying {
                source,
                cancelled: false,
            }) => *source,
            _ => {
                states.remove(&id);
                return Ok(None);
            }
        };
        let result = publish();
        match result {
            Ok(value) => {
                states.insert(id, RestageState::Completed(source));
                Ok(Some(value))
            }
            Err(error) => {
                states.remove(&id);
                Err(error)
            }
        }
    }

    pub(crate) fn abandon_restage(&self, id: ChatAttachmentId) {
        self.restaging
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&id);
    }

    /// Claim an id for one upload; `None` while another upload of it runs.
    pub(crate) fn claim(&self, id: ChatAttachmentId) -> Option<StoringClaim> {
        self.storing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id)
            .then(|| StoringClaim {
                storing: Arc::clone(&self.storing),
                id,
            })
    }

    pub(crate) fn chat_dir(&self, chat: ChatId) -> PathBuf {
        self.root.join(chat.to_storage())
    }

    pub(crate) fn attachment_dir(&self, chat: ChatId, id: ChatAttachmentId) -> PathBuf {
        self.chat_dir(chat).join(id.to_storage())
    }

    /// The stored original: the path an agent is told about.
    pub(crate) fn original(&self, record: &AttachmentRecord) -> PathBuf {
        self.attachment_dir(record.chat_id, record.attachment.id)
            .join(ORIGINAL_DIR)
            .join(&record.attachment.file_name)
    }

    pub(crate) fn thumbnail(&self, chat: ChatId, id: ChatAttachmentId) -> PathBuf {
        self.attachment_dir(chat, id).join(THUMBNAIL_FILE)
    }

    /// Check, hash and prepare one upload, and write it to a staging folder.
    /// The caller publishes it with [`Self::publish`] or drops it with
    /// [`Self::discard`].
    pub(crate) fn prepare(
        &self,
        chat: ChatId,
        id: ChatAttachmentId,
        file_name: &str,
        source: AttachmentSource,
        bytes: &[u8],
    ) -> Result<Prepared, Failure> {
        let started = Instant::now();
        let byte_count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if byte_count == 0 {
            return Err(failure(
                "invalid_request",
                "An attachment cannot be empty.",
                false,
            ));
        }
        if byte_count > MAX_ATTACHMENT_BYTES {
            return Err(refused(
                attachment_codes::TOO_LARGE,
                "The attachment is larger than the limit.",
                "size",
            ));
        }
        let file_name = sanitize_file_name(file_name);
        let Some(allowed) = allowed_type(&file_name) else {
            return Err(type_not_allowed(&file_name, "type"));
        };
        let head = &bytes[..bytes.len().min(CONTENT_CHECK_BYTES)];
        if !content_agrees(allowed.family, head)
            || !archive_agrees(&file_name, allowed.family, bytes)
        {
            return Err(type_not_allowed(&file_name, "content"));
        }
        let content_blake3 = blake3::hash(bytes).to_hex().to_string();

        let decode_started = Instant::now();
        let picture = match allowed.family {
            AttachmentFamily::Picture => Some(prepare_picture(bytes, &file_name)?),
            _ => None,
        };
        let decode_ms = decode_started.elapsed().as_secs_f64() * 1_000.0;
        let (kind, media_type, dimensions) = match &picture {
            Some(Picture::Previewed {
                media_type,
                width,
                height,
                ..
            }) => (AttachmentKind::Image, *media_type, Some((*width, *height))),
            Some(Picture::TooLargeToPreview { media_type }) => {
                (AttachmentKind::File, *media_type, None)
            }
            None => (AttachmentKind::File, allowed.media_type, None),
        };

        let write_started = Instant::now();
        let staging = self.root.join(STAGING_DIR).join(format!(
            "{}.{}",
            id.to_storage(),
            uuid::Uuid::new_v4().simple()
        ));
        let written = (|| -> io::Result<Option<usize>> {
            fs::create_dir_all(staging.join(ORIGINAL_DIR))?;
            write_synced(&staging.join(ORIGINAL_DIR).join(&file_name), bytes)?;
            let mut sent_bytes = None;
            if let Some(Picture::Previewed {
                thumbnail, sent, ..
            }) = &picture
            {
                write_synced(&staging.join(THUMBNAIL_FILE), thumbnail)?;
                if let Some(sent) = sent {
                    let name = match sent.format {
                        ImageFormat::Png => SENT_PNG,
                        _ => SENT_JPEG,
                    };
                    write_synced(&staging.join(name), &sent.bytes)?;
                    sent_bytes = Some(sent.bytes.len());
                }
            }
            Ok(sent_bytes)
        })();
        let sent_bytes = match written {
            Ok(sent_bytes) => sent_bytes,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging);
                return Err(storage_failure(&error));
            }
        };
        tracing::debug!(
            target: "koru_ai::attachments",
            attachment_id = %id,
            source = source.as_token(),
            kind = kind.as_token(),
            byte_count,
            width = dimensions.map(|(width, _)| width),
            height = dimensions.map(|(_, height)| height),
            sent_copy_bytes = sent_bytes,
            decode_ms,
            write_ms = write_started.elapsed().as_secs_f64() * 1_000.0,
            total_ms = started.elapsed().as_secs_f64() * 1_000.0,
            "attachment prepared"
        );
        Ok(Prepared {
            staging,
            record: AttachmentRecord {
                chat_id: chat,
                attachment: ChatAttachment {
                    id,
                    message_id: None,
                    ordinal: None,
                    kind,
                    source,
                    file_name,
                    media_type: media_type.to_owned(),
                    byte_count,
                    dimensions,
                    origin: None,
                    created_at: Timestamp::now(),
                },
                has_sent_copy: sent_bytes.is_some(),
                content_blake3,
            },
        })
    }

    /// Move a prepared store into its place with one rename. A folder left
    /// there by a store whose row was never written (a crash between the
    /// two) is replaced; the caller has already checked there is no row.
    pub(crate) fn publish(&self, prepared: &Prepared) -> Result<(), Failure> {
        let record = &prepared.record;
        let target = self.attachment_dir(record.chat_id, record.attachment.id);
        let result = (|| -> io::Result<()> {
            fs::create_dir_all(self.chat_dir(record.chat_id))?;
            if target.exists() {
                fs::remove_dir_all(&target)?;
            }
            fs::rename(&prepared.staging, &target)
        })();
        result.map_err(|error| {
            let _ = fs::remove_dir_all(&prepared.staging);
            storage_failure(&error)
        })
    }

    /// Remove one attachment's folder. Missing is fine.
    pub(crate) fn remove_attachment(&self, chat: ChatId, id: ChatAttachmentId) {
        remove_dir_if_present(&self.attachment_dir(chat, id));
    }

    /// Remove a chat's whole folder. Missing is fine.
    pub(crate) fn remove_chat(&self, chat: ChatId) {
        remove_dir_if_present(&self.chat_dir(chat));
    }

    /// Copy each source folder's files into its branch folder. A copy, not a
    /// hard link: the two chats then hold separate files, so a person or an
    /// agent editing one chat's file never changes the other's (D14). All or
    /// nothing: on a failure every folder this call made is removed.
    pub(crate) fn copy_for_branch(&self, folders: &[BranchFolder]) -> io::Result<()> {
        let mut made = Vec::new();
        for folder in folders {
            let from = self.attachment_dir(folder.from.0, folder.from.1);
            let to = self.attachment_dir(folder.to.0, folder.to.1);
            if to.exists() {
                remove_all(&made);
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "a branch attachment folder already exists",
                ));
            }
            made.push(to.clone());
            if let Err(error) = copy_tree(&from, &to) {
                remove_all(&made);
                return Err(error);
            }
        }
        Ok(())
    }

    /// Remove what no row accounts for: chat folders with no chat,
    /// attachment folders with no row, and everything in staging. Only
    /// UUID-named entries are touched, and nothing modified after `started`
    /// (a store published while the sweep ran is not yet an orphan).
    pub(crate) fn sweep(&self, known: &KnownAttachmentDirs, started: SystemTime) -> SweepCounts {
        let mut counts = SweepCounts::default();
        let Ok(entries) = fs::read_dir(self.root.as_path()) else {
            return counts;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name == STAGING_DIR {
                if let Ok(staged) = fs::read_dir(&path) {
                    for staged in staged.flatten() {
                        if older_than(&staged.path(), started) && remove_entry(&staged.path()) {
                            counts.staging += 1;
                        }
                    }
                }
                continue;
            }
            let Some(chat) = ChatId::parse(name) else {
                continue;
            };
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            if !known.chats.contains(&chat) {
                if older_than(&path, started) && remove_entry(&path) {
                    counts.chats += 1;
                }
                continue;
            }
            let Ok(attachments) = fs::read_dir(&path) else {
                continue;
            };
            for attachment in attachments.flatten() {
                let Some(id) = attachment
                    .file_name()
                    .to_str()
                    .and_then(ChatAttachmentId::parse)
                else {
                    continue;
                };
                if !known.attachments.contains(&id)
                    && older_than(&attachment.path(), started)
                    && remove_entry(&attachment.path())
                {
                    counts.attachments += 1;
                }
            }
        }
        counts
    }
}

/// The copy of a picture to hand an agent natively: the sent copy when one
/// was written, otherwise the original. `original` is a path
/// [`AttachmentStore::original`] gave. Runs where a file may be read (a
/// provider thread), never on the dispatcher.
pub(crate) fn native_copy(original: &Path) -> PathBuf {
    let Some(folder) = original.parent().and_then(Path::parent) else {
        return original.to_path_buf();
    };
    [SENT_PNG, SENT_JPEG]
        .iter()
        .map(|name| folder.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| original.to_path_buf())
}

// ----- pictures (D5) ------------------------------------------------------

enum Picture {
    Previewed {
        media_type: &'static str,
        width: u32,
        height: u32,
        thumbnail: Vec<u8>,
        sent: Option<SentCopy>,
    },
    /// Within the byte limit but beyond what is previewed or sent natively:
    /// attached as a file, never refused.
    TooLargeToPreview { media_type: &'static str },
}

struct SentCopy {
    format: ImageFormat,
    bytes: Vec<u8>,
}

fn prepare_picture(bytes: &[u8], file_name: &str) -> Result<Picture, Failure> {
    // RGBA at the pixel limit, and room for the decoder's own buffers.
    prepare_picture_within(bytes, file_name, MAX_PREVIEW_PIXELS * 4 + 64 * 1024 * 1024)
}

/// [`prepare_picture`] with the decoder's allocation bound named, so a test
/// can reach it without a picture of tens of megapixels.
fn prepare_picture_within(
    bytes: &[u8],
    file_name: &str,
    max_alloc: u64,
) -> Result<Picture, Failure> {
    let media_type =
        picture_media_type(bytes).ok_or_else(|| type_not_allowed(file_name, "content"))?;
    let format = image::guess_format(bytes).map_err(|_| type_not_allowed(file_name, "content"))?;
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| type_not_allowed(file_name, "content"))?;
    if width == 0 || height == 0 {
        return Err(type_not_allowed(file_name, "content"));
    }
    if width > MAX_PREVIEW_EDGE
        || height > MAX_PREVIEW_EDGE
        || u64::from(width) * u64::from(height) > MAX_PREVIEW_PIXELS
    {
        return Ok(Picture::TooLargeToPreview { media_type });
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PREVIEW_EDGE);
    limits.max_image_height = Some(MAX_PREVIEW_EDGE);
    limits.max_alloc = Some(max_alloc);
    reader.limits(limits);
    let decoded = match reader.decode() {
        Ok(decoded) => decoded,
        // Within the byte limit but more than a preview may decode (a
        // 16-bit picture spends eight bytes a pixel): attached as a file,
        // never refused (D5, §10).
        Err(image::ImageError::Limits(_)) => {
            return Ok(Picture::TooLargeToPreview { media_type });
        }
        Err(_) => return Err(type_not_allowed(file_name, "content")),
    };

    let thumbnail = if width.max(height) > THUMBNAIL_EDGE {
        decoded.resize(
            THUMBNAIL_EDGE,
            THUMBNAIL_EDGE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        decoded.clone()
    };
    let thumbnail = encode_png(&thumbnail).map_err(|_| storage_failure_message())?;

    let fits = width.max(height) <= SENT_COPY_EDGE
        && u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= MAX_SENT_COPY_BYTES;
    let sent = if fits {
        None
    } else {
        Some(sent_copy(&decoded).map_err(|_| storage_failure_message())?)
    };
    Ok(Picture::Previewed {
        media_type,
        width,
        height,
        thumbnail,
        sent,
    })
}

/// The copy an agent receives: at most [`SENT_COPY_EDGE`] on the long edge
/// (Lanczos3), as PNG when that stays within [`MAX_SENT_COPY_BYTES`] so
/// screenshots and text stay crisp, otherwise as JPEG at falling quality.
fn sent_copy(decoded: &DynamicImage) -> image::ImageResult<SentCopy> {
    let resized = if decoded.width().max(decoded.height()) > SENT_COPY_EDGE {
        decoded.resize(
            SENT_COPY_EDGE,
            SENT_COPY_EDGE,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        decoded.clone()
    };
    let png = encode_png(&resized)?;
    if png.len() as u64 <= MAX_SENT_COPY_BYTES {
        return Ok(SentCopy {
            format: ImageFormat::Png,
            bytes: png,
        });
    }
    let mut flattened = flatten_on_white(&resized);
    loop {
        for quality in JPEG_QUALITIES {
            let mut bytes = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
                .encode_image(&flattened)?;
            if bytes.len() as u64 <= MAX_SENT_COPY_BYTES {
                return Ok(SentCopy {
                    format: ImageFormat::Jpeg,
                    bytes,
                });
            }
        }
        // Still over at the lowest quality (a picture of noise): smaller,
        // a quarter at a time, until it fits. Ten copies must fit the 16 MiB
        // prompt frame, so the bound is never let go.
        let (width, height) = (flattened.width(), flattened.height());
        if width.max(height) <= MIN_SENT_COPY_EDGE {
            return Err(image::ImageError::Limits(
                image::error::LimitError::from_kind(image::error::LimitErrorKind::DimensionError),
            ));
        }
        flattened = flattened.resize(
            (width * 3 / 4).max(1),
            (height * 3 / 4).max(1),
            image::imageops::FilterType::Lanczos3,
        );
    }
}

fn encode_png(image: &DynamicImage) -> image::ImageResult<Vec<u8>> {
    let mut bytes = Vec::new();
    image.write_to(Cursor::new(&mut bytes), ImageFormat::Png)?;
    Ok(bytes)
}

/// JPEG has no alpha: a transparent screenshot region becomes white, as it
/// looked on the page, rather than whatever color its pixels held.
fn flatten_on_white(image: &DynamicImage) -> DynamicImage {
    let mut rgba = image.to_rgba8();
    for pixel in rgba.pixels_mut() {
        let alpha = u16::from(pixel[3]);
        for channel in 0..3 {
            let value = u16::from(pixel[channel]);
            pixel[channel] = ((value * alpha + 255 * (255 - alpha)) / 255) as u8;
        }
        pixel[3] = 255;
    }
    DynamicImage::ImageRgb8(DynamicImage::ImageRgba8(rgba).to_rgb8())
}

// ----- the archive check (D16) --------------------------------------------

/// The ZIP families must hold what their type promises: `[Content_Types].xml`
/// for an Office file, a `mimetype` entry naming the type for odt and epub.
fn archive_agrees(file_name: &str, family: AttachmentFamily, bytes: &[u8]) -> bool {
    let expected_mimetype = match family {
        AttachmentFamily::OfficeOpenXml => None,
        AttachmentFamily::OpenDocument => match extension_of(file_name).as_deref() {
            Some("odt") => Some("application/vnd.oasis.opendocument.text"),
            Some("epub") => Some("application/epub+zip"),
            _ => return false,
        },
        _ => return true,
    };
    let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(bytes)) else {
        return false;
    };
    match expected_mimetype {
        None => archive.by_name("[Content_Types].xml").is_ok(),
        Some(expected) => {
            let Ok(entry) = archive.by_name("mimetype") else {
                return false;
            };
            let mut named = String::new();
            entry.take(256).read_to_string(&mut named).is_ok() && named.trim() == expected
        }
    }
}

// ----- files --------------------------------------------------------------

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Copy a folder's files, each synced like a store's, so a branch's
/// attachments are as durable as the originals once it commits.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            let mut source = fs::File::open(entry.path())?;
            let mut copy = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)?;
            io::copy(&mut source, &mut copy)?;
            copy.sync_all()?;
        }
    }
    Ok(())
}

fn remove_all(paths: &[PathBuf]) {
    for path in paths {
        let _ = fs::remove_dir_all(path);
    }
}

fn remove_dir_if_present(path: &Path) {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            target: "koru_ai::attachments",
            kind = ?error.kind(),
            "an attachment folder could not be removed; the next start's sweep retries"
        ),
    }
}

fn remove_entry(path: &Path) -> bool {
    if path.is_dir() {
        fs::remove_dir_all(path).is_ok()
    } else {
        fs::remove_file(path).is_ok()
    }
}

fn older_than(path: &Path, started: SystemTime) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| modified < started)
}

// ----- failures -----------------------------------------------------------

pub(crate) fn failure(code: &str, message: &str, retryable: bool) -> Failure {
    Failure {
        code: code.into(),
        message: message.into(),
        retryable,
        details: BTreeMap::new(),
    }
}

fn refused(code: &str, message: &str, class: &str) -> Failure {
    tracing::debug!(target: "koru_ai::attachments", refusal = class, "attachment refused");
    failure(code, message, false)
}

/// A type off the allowlist, or content that is not what the name says.
/// The extension is safe to return (it is the person's own and names no
/// path); the UI names it in its one line.
fn type_not_allowed(file_name: &str, reason: &str) -> Failure {
    let mut failure = refused(
        attachment_codes::TYPE_NOT_ALLOWED,
        "This type of file can't be attached.",
        "type",
    );
    failure.details.insert("reason".into(), reason.into());
    if let Some(extension) = extension_of(file_name) {
        failure.details.insert("extension".into(), extension.into());
    }
    failure
}

fn storage_failure(error: &io::Error) -> Failure {
    tracing::warn!(target: "koru_ai::attachments", kind = ?error.kind(), "an attachment could not be stored");
    storage_failure_message()
}

fn storage_failure_message() -> Failure {
    failure(
        attachment_codes::STORAGE,
        "The attachment could not be stored on this computer.",
        true,
    )
}

// ----- the cleanup thread -------------------------------------------------

/// Work on attachment folders the dispatcher hands off rather than doing:
/// removals after a delete, the start-up sweep, and a branch's copies.
pub(crate) enum CleanupJob {
    RemoveChat(ChatId),
    Sweep {
        known: KnownAttachmentDirs,
        started: SystemTime,
        staged_removed: u64,
    },
    CopyBranch {
        folders: Vec<BranchFolder>,
        done: Box<dyn FnOnce(io::Result<()>) + Send>,
    },
}

/// One thread, `koru-ai-attachments`, working through [`CleanupJob`]s in
/// order. Order matters: a branch's copies and a later removal of the same
/// chat run one after the other, never interleaved.
pub(crate) struct Cleanup {
    sender: Option<Sender<CleanupJob>>,
    thread: Option<JoinHandle<()>>,
}

impl Cleanup {
    pub(crate) fn spawn(store: AttachmentStore) -> Result<Self, String> {
        let (sender, receiver) = channel();
        let thread = std::thread::Builder::new()
            .name("koru-ai-attachments".into())
            .spawn(move || run_cleanup(&store, &receiver))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
        })
    }

    pub(crate) fn submit(&self, job: CleanupJob) {
        let sent = self
            .sender
            .as_ref()
            .is_some_and(|sender| sender.send(job).is_ok());
        if !sent {
            tracing::warn!(
                target: "koru_ai::attachments",
                "the attachment cleanup thread has stopped; the next start's sweep catches up"
            );
        }
    }

    pub(crate) fn stop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_cleanup(store: &AttachmentStore, jobs: &Receiver<CleanupJob>) {
    while let Ok(job) = jobs.recv() {
        match job {
            CleanupJob::RemoveChat(chat) => store.remove_chat(chat),
            CleanupJob::Sweep {
                known,
                started,
                staged_removed,
            } => {
                let timer = Instant::now();
                let counts = store.sweep(&known, started);
                let duration_ms = timer.elapsed().as_secs_f64() * 1_000.0;
                // News only when something was left behind to sweep.
                if counts == SweepCounts::default() && staged_removed == 0 {
                    tracing::debug!(target: "koru_ai::attachments", duration_ms, "attachment sweep found nothing");
                } else {
                    tracing::info!(
                        target: "koru_ai::attachments",
                        swept_chats = counts.chats,
                        swept_attachments = counts.attachments,
                        swept_staging = counts.staging,
                        swept_staged = staged_removed,
                        duration_ms,
                        "attachment sweep removed leftovers"
                    );
                }
            }
            CleanupJob::CopyBranch { folders, done } => done(store.copy_for_branch(&folders)),
        }
    }
}

#[cfg(test)]
mod tests;
