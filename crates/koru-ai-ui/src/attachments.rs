// SPDX-License-Identifier: MPL-2.0

//! Pictures and files in the chat (chat-attachments R1–R4): the composer's
//! strip, the tiles and file chips on a sent message, the threads that read,
//! store and decode them, the shared viewer a picture opens in
//! (`koru_ui::media_viewer`, ADR-482), and the copy a file opens from.
//!
//! **Only allowed types get in** (D16, ADR-481). A copied file whose name is
//! off the allowlist is never added — the status line names its type — and
//! the service checks the name and the content again when it stores one.
//!
//! **Nothing here runs on the window thread but bookkeeping and drawing.**
//! The clipboard is read on `koru-clipboard`'s worker, an upload goes out on
//! `koru-ai-upload` (one job at a time, its own API client, so a 20 MB upload
//! never delays the reader), and a thumbnail is decoded on `koru-ai-thumbs`.
//! Their answers come back as [`Event`]s the surface drains on its
//! maintenance tick; the window thread lays tiles out from sizes it already
//! knows and draws brushes it already has. A picture opened in the viewer is
//! fetched and decoded, with its mips, on `koru-ai-thumbs` too, and opened in
//! an external app from a copy written on a helper thread.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use koru_ai_core::attachments::{AttachmentFamily, allowed_type, extension_of};
use koru_ai_core::{
    AttachmentKind, AttachmentSource, ChatAttachment, ChatAttachmentId, ChatId, WorkspaceFileOrigin,
};
use koru_ai_wire::{Failure, attachment_codes};
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_draw::scene;
use koru_draw::theme::{self, role};
use koru_i18n::{Key, Lang};
use koru_ui::media_viewer::{
    Action as ViewerAction, ActionSet, MediaViewer, Outcome as ViewerOutcome, Stage, ViewerImage,
    ViewerOptions, ViewerSemantics, WheelAxis, ZoomKey,
};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key as KeyPress, ModifiersState};
use winit::window::CursorIcon;

use super::{COMPOSER_PAD_TOP, COMPOSER_PAD_X, ChatSurface, LoadedImage, agents};

/// A tile on a sent message.
pub(crate) const CARD_TILE_H: f32 = 112.0;
pub(crate) const TILE_GAP: f32 = 6.0;
const CARD_TILE_MIN_W: f32 = 56.0;
const CARD_TILE_MAX_W: f32 = 240.0;
/// A file's tile, which shows its name rather than its content.
const FILE_TILE_W: f32 = 168.0;
/// The long edge a decoded thumbnail is kept at: enough for a sharp tile on
/// a high-density screen, small enough to keep many.
const THUMB_EDGE: u32 = 512;
/// How much decoded thumbnail a window keeps.
const THUMB_CACHE_BYTES: usize = 64 * 1024 * 1024;
/// How long a thumbnail that could not be read waits before it is asked
/// for again.
const THUMB_RETRY: Duration = Duration::from_secs(30);

/// Where one attachment of the draft stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DraftState {
    /// Being stored by the AI service; the tile is dimmed.
    Storing,
    Stored,
    /// Refused or not stored; the tile is red, and the reason was said.
    Failed {
        retryable: bool,
    },
}

/// One attachment of the message being written.
#[derive(Clone, Debug)]
pub(crate) struct Draft {
    pub id: ChatAttachmentId,
    pub file_name: String,
    pub source: AttachmentSource,
    pub kind: AttachmentKind,
    /// Width and height, for a picture's tile before the service answers.
    pub size: Option<(u32, u32)>,
    pub byte_count: u64,
    pub state: DraftState,
    /// Kept until it is stored, so a failed store can be tried again with
    /// the same id.
    pub bytes: Option<bytes::Bytes>,
    /// A copied file's path, read on the upload thread — its size checked
    /// first — and kept, like `bytes`, so a failed store can be tried again.
    pub path: Option<PathBuf>,
    /// A local path is delivered to an agent as a reference, without reading
    /// or copying bytes. It is retained for retry and recall.
    pub reference_path: Option<String>,
    pub origin: Option<WorkspaceFileOrigin>,
}

impl Draft {
    /// A fresh staged copy requested from a sent attachment. Its bytes stay
    /// at the AI service; only metadata and a pending id enter the window.
    pub(crate) fn restaged(source: &ChatAttachment, id: ChatAttachmentId) -> Self {
        Self {
            id,
            file_name: source.file_name.clone(),
            source: source.source,
            kind: source.kind,
            size: source.dimensions,
            byte_count: source.byte_count,
            state: DraftState::Storing,
            bytes: None,
            path: None,
            reference_path: source.reference_path.clone(),
            origin: source.origin.clone(),
        }
    }

    /// A pasted bitmap, named the way a screenshot file is.
    pub(crate) fn pasted(png: koru_clipboard::PngBitmap, now: std::time::SystemTime) -> Self {
        Self {
            id: ChatAttachmentId::new(),
            file_name: screenshot_name(now),
            source: AttachmentSource::Clipboard,
            kind: AttachmentKind::Image,
            size: Some((png.width, png.height)),
            byte_count: png.bytes.len() as u64,
            state: DraftState::Storing,
            bytes: Some(png.bytes.into()),
            path: None,
            reference_path: None,
            origin: None,
        }
    }

    /// A copied file, allowed by its name. Nothing is read here: its size
    /// and bytes are the upload thread's to find out. A picture's tile
    /// shows the picture once the service has made its thumbnail.
    pub(crate) fn from_path(path: PathBuf, source: AttachmentSource) -> Self {
        let file_name = file_name_of(&path);
        let picture = allowed_type(&file_name)
            .is_some_and(|allowed| allowed.family == AttachmentFamily::Picture);
        Self {
            id: ChatAttachmentId::new(),
            file_name,
            source,
            kind: if picture {
                AttachmentKind::Image
            } else {
                AttachmentKind::File
            },
            size: None,
            byte_count: 0,
            state: DraftState::Storing,
            bytes: None,
            path: Some(path),
            reference_path: None,
            origin: None,
        }
    }

    /// A file or folder whose absolute path is sent to an agent as context.
    pub(crate) fn reference(path: PathBuf, kind: AttachmentKind) -> Self {
        let file_name = file_name_of(&path);
        Self {
            id: ChatAttachmentId::new(),
            file_name,
            source: AttachmentSource::Path,
            kind,
            size: None,
            byte_count: 0,
            state: DraftState::Storing,
            bytes: None,
            path: None,
            reference_path: Some(path.to_string_lossy().into_owned()),
            origin: None,
        }
    }

    /// A selected file being granted, imported, and copied through Files.
    /// It owns no local bytes in the composer; the Store import is the source.
    pub(crate) fn workspace_pending(id: ChatAttachmentId, path: &std::path::Path) -> Self {
        Self {
            id,
            file_name: file_name_of(path),
            source: AttachmentSource::Workspace,
            kind: AttachmentKind::File,
            size: None,
            byte_count: 0,
            state: DraftState::Storing,
            bytes: None,
            path: None,
            reference_path: None,
            origin: None,
        }
    }

    /// Whether a failed store can be tried again: its bytes, or the file
    /// they come from, are still here.
    pub(crate) fn can_retry(&self) -> bool {
        self.bytes.is_some() || self.path.is_some() || self.reference_path.is_some()
    }

    pub(crate) fn tile(&self) -> Tile {
        Tile {
            id: self.id,
            kind: self.kind,
            size: self.size,
            name: self.file_name.clone(),
            byte_count: self.byte_count,
        }
    }
}

/// The last part of a path, as the attachment's name.
fn file_name_of(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// How a file opens from the chat: text and source code as a `.txt` copy, so
/// a click never runs a script or opens a browser (D12); anything else under
/// its own name.
pub(crate) fn open_as(file_name: &str) -> koru_window::OpenAs {
    if allowed_type(file_name).is_some_and(|allowed| allowed.family.opens_as_text()) {
        koru_window::OpenAs::Text
    } else {
        koru_window::OpenAs::AsIs
    }
}

/// The paths a paste may attach, and the types it may not: the allowlist
/// asked by name (D16). Each refused file is named by its extension, or by
/// its name when it has none.
pub(crate) fn sort_by_allowlist(paths: Vec<PathBuf>) -> (Vec<PathBuf>, Vec<String>) {
    let mut allowed = Vec::new();
    let mut refused = Vec::new();
    for path in paths {
        let name = file_name_of(&path);
        if allowed_type(&name).is_some() {
            allowed.push(path);
        } else {
            refused.push(match extension_of(&name) {
                Some(extension) => format!(".{extension}"),
                None => name,
            });
        }
    }
    (allowed, refused)
}

/// The one line for the files a paste did not attach: *.exe files can't be
/// attached.*, or *3 files weren't attached: .exe, .zip.*
pub(crate) fn refused_line(refused: &[String], lang: Lang) -> Option<String> {
    match refused {
        [] => None,
        [one] => Some(koru_i18n::fill(
            koru_i18n::t(lang, Key::AiAttachmentTypeRefused),
            &[one],
        )),
        many => {
            let mut types: Vec<&str> = many.iter().map(String::as_str).collect();
            types.dedup();
            Some(koru_i18n::fill(
                koru_i18n::t(lang, Key::AiAttachmentsTypeRefused),
                &[&many.len().to_string(), &types.join(", ")],
            ))
        }
    }
}

/// What a tile shows: a picture by its thumbnail, a file by its name.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Tile {
    pub id: ChatAttachmentId,
    pub kind: AttachmentKind,
    pub size: Option<(u32, u32)>,
    pub name: String,
    /// A file chip's size line; `0` while a copied file's is not yet known.
    pub byte_count: u64,
}

impl Tile {
    pub(crate) fn of(attachment: &ChatAttachment) -> Self {
        Self {
            id: attachment.id,
            kind: attachment.kind,
            size: attachment.dimensions,
            name: attachment.file_name.clone(),
            byte_count: attachment.byte_count,
        }
    }

    /// The tile's width at `height`, from the picture's known shape, so a
    /// row is laid out before any thumbnail arrives and nothing jumps.
    fn width_at(&self, height: f32, min: f32, max: f32) -> f32 {
        match (self.kind, self.size) {
            (AttachmentKind::Image, Some((width, height_px))) if width > 0 && height_px > 0 => {
                (height * width as f32 / height_px as f32).clamp(min, max)
            }
            (AttachmentKind::Image, _) => height,
            (AttachmentKind::File | AttachmentKind::Folder, _) => FILE_TILE_W.min(max),
        }
    }
}

/// The tiles of a sent message, wrapped into rows no wider than `max_w`:
/// each tile's rect relative to the row's origin, and the total height.
pub(crate) fn card_tiles(tiles: &[Tile], max_w: f32) -> (Vec<Rect>, f32) {
    let mut rects = Vec::with_capacity(tiles.len());
    let (mut x, mut y) = (0.0_f32, 0.0_f32);
    for tile in tiles {
        let w = tile.width_at(
            CARD_TILE_H,
            CARD_TILE_MIN_W,
            CARD_TILE_MAX_W.min(max_w.max(1.0)),
        );
        if x > 0.0 && x + w > max_w {
            x = 0.0;
            y += CARD_TILE_H + TILE_GAP;
        }
        rects.push(Rect::new(x, y, w, CARD_TILE_H));
        x += w + TILE_GAP;
    }
    let height = if tiles.is_empty() {
        0.0
    } else {
        y + CARD_TILE_H
    };
    (rects, height)
}

/// `screenshot-YYYY-MM-DD-HHMMSS.png`, in UTC: ASCII, untranslated, a real
/// file name (chat-attachments §6).
pub(crate) fn screenshot_name(now: std::time::SystemTime) -> String {
    let seconds = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let days = (seconds / 86_400) as i64;
    let of_day = seconds % 86_400;
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "screenshot-{year:04}-{month:02}-{day:02}-{:02}{:02}{:02}.png",
        of_day / 3_600,
        of_day % 3_600 / 60,
        of_day % 60
    )
}

/// A copied file the upload thread could not read: gone, locked, or a
/// cloud placeholder that would not download. The window's own code, never
/// the service's.
const UNREADABLE: &str = "attachment_unreadable";

/// The one line a refused or failed store shows, by the service's code.
pub(crate) fn failure_line(failure: &Failure, lang: Lang) -> (String, bool) {
    let key = match failure.code.as_str() {
        attachment_codes::TOO_LARGE => Key::AiAttachmentTooLarge,
        attachment_codes::LIMIT => Key::AiAttachmentLimit,
        attachment_codes::TYPE_NOT_ALLOWED => Key::AiAttachmentMismatch,
        attachment_codes::STORAGE => Key::AiAttachmentDiskFull,
        attachment_codes::MISSING => Key::AiAttachmentMissing,
        attachment_codes::UNSUPPORTED => Key::AiAttachmentsAgentsOnly,
        UNREADABLE => Key::AiAttachmentUnreadable,
        "ai_unavailable" | "gateway_unavailable" | "ai_worker_saturated" | "ai_worker_timeout" => {
            Key::AiAttachmentServiceDown
        }
        _ => Key::AiAttachmentFailed,
    };
    (koru_i18n::t(lang, key).to_owned(), failure.retryable)
}

/// A thumbnail in the surface's cache, keyed by attachment id — the same
/// key before and after the send, so a sent card shows the strip's picture
/// without fetching it again.
pub(crate) enum Thumb {
    /// Asked for once; not asked again while it is on its way.
    Pending,
    Ready(LoadedImage),
    /// Could not be read or decoded; asked again after [`THUMB_RETRY`].
    Failed(Instant),
}

/// The thumbnails a window keeps: decoded brushes up to [`THUMB_CACHE_BYTES`],
/// the least recently drawn going first. An evicted picture is fetched
/// again when it is next on screen.
#[derive(Default)]
pub(crate) struct ThumbCache {
    entries: HashMap<ChatAttachmentId, (Thumb, u64)>,
    bytes: usize,
    tick: u64,
}

impl ThumbCache {
    /// The thumbnail to draw, marking it as just used.
    pub(crate) fn get(&mut self, id: ChatAttachmentId) -> Option<&Thumb> {
        self.tick += 1;
        let tick = self.tick;
        let (thumb, used) = self.entries.get_mut(&id)?;
        *used = tick;
        Some(thumb)
    }

    /// Whether a tile on screen should ask for its thumbnail: never asked,
    /// or failed long enough ago to try again.
    pub(crate) fn wants(&self, id: ChatAttachmentId, now: Instant) -> bool {
        match self.entries.get(&id) {
            None => true,
            Some((Thumb::Failed(at), _)) => now.duration_since(*at) >= THUMB_RETRY,
            Some(_) => false,
        }
    }

    pub(crate) fn pending(&mut self, id: ChatAttachmentId) {
        self.put(id, Thumb::Pending);
    }

    pub(crate) fn failed(&mut self, id: ChatAttachmentId, now: Instant) {
        self.put(id, Thumb::Failed(now));
    }

    pub(crate) fn ready(&mut self, id: ChatAttachmentId, image: LoadedImage) {
        self.put(id, Thumb::Ready(image));
        while self.bytes > THUMB_CACHE_BYTES {
            let oldest = self
                .entries
                .iter()
                .filter(|(other, (thumb, _))| **other != id && matches!(thumb, Thumb::Ready(_)))
                .min_by_key(|(_, (_, used))| *used)
                .map(|(other, _)| *other);
            let Some(oldest) = oldest else {
                break;
            };
            self.remove(oldest);
        }
    }

    pub(crate) fn remove(&mut self, id: ChatAttachmentId) {
        if let Some((thumb, _)) = self.entries.remove(&id) {
            self.bytes = self.bytes.saturating_sub(weight(&thumb));
        }
    }

    pub(crate) fn any_pending(&self) -> bool {
        self.entries
            .values()
            .any(|(thumb, _)| matches!(thumb, Thumb::Pending))
    }

    #[cfg(test)]
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    fn put(&mut self, id: ChatAttachmentId, thumb: Thumb) {
        self.remove(id);
        self.tick += 1;
        self.bytes += weight(&thumb);
        self.entries.insert(id, (thumb, self.tick));
    }
}

fn weight(thumb: &Thumb) -> usize {
    match thumb {
        Thumb::Ready(image) => image.width as usize * image.height as usize * 4,
        Thumb::Pending | Thumb::Failed(_) => 0,
    }
}

/// Answers from the threads, drained on the surface's maintenance tick.
pub(crate) enum Event {
    Clipboard(Result<koru_clipboard::AttachmentPayload, koru_clipboard::Error>),
    Stored {
        chat: ChatId,
        id: ChatAttachmentId,
        result: Result<ChatAttachment, Failure>,
    },
    /// A thumbnail decoded, or not: fetched for a sent picture, or a
    /// draft's own bytes.
    Decoded {
        id: ChatAttachmentId,
        image: Option<LoadedImage>,
    },
    /// A picture for the viewer, decoded with its mips, and the original's
    /// bytes for *Open in external app*; `None` when it could not be read or
    /// decoded.
    ViewerReady {
        chat: ChatId,
        id: ChatAttachmentId,
        picture: Option<(Arc<ViewerImage>, bytes::Bytes)>,
    },
    /// A copy was handed to the system's app; or the line that says why it
    /// could not be.
    Opened {
        failed: Option<Key>,
    },
}

/// What an upload stores: bytes already here, or a copied file read on the
/// upload thread.
enum Payload {
    Bytes(bytes::Bytes),
    Path(PathBuf),
}

enum Upload {
    Put {
        chat: ChatId,
        id: ChatAttachmentId,
        file_name: String,
        source: AttachmentSource,
        payload: Payload,
    },
    Reference {
        chat: ChatId,
        id: ChatAttachmentId,
        kind: AttachmentKind,
        path: String,
    },
    Remove {
        chat: ChatId,
        id: ChatAttachmentId,
    },
}

enum ThumbJob {
    /// A draft's own bytes, already here.
    Decode(ChatAttachmentId, bytes::Bytes),
    /// A sent picture's thumbnail, from the service.
    Fetch(ChatId, ChatAttachmentId),
    /// A picture for the viewer: its own bytes when they are here (a draft
    /// still being stored), else the original from the service.
    Viewer(ChatId, ChatAttachmentId, Option<bytes::Bytes>),
}

/// The threads behind the strip and the tiles, started on first use. None
/// of them shares the reader's queue, so a screen of thumbnails can never
/// keep a person's own command out of it.
pub(crate) struct Work {
    root: Option<PathBuf>,
    events_tx: Sender<Event>,
    events: Receiver<Event>,
    uploads: Option<Sender<Upload>>,
    thumbs: Option<Sender<ThumbJob>>,
    /// Jobs handed out and not yet answered: while any is, the surface
    /// keeps a maintenance deadline.
    in_flight: usize,
    /// The thumbnails a test asked for, which it answers itself.
    #[cfg(test)]
    pub(crate) fetched: Vec<ChatAttachmentId>,
    /// The pictures a test asked the viewer for, and the copies it asked to
    /// open, which it answers itself.
    #[cfg(test)]
    pub(crate) viewer_fetched: Vec<ChatAttachmentId>,
    #[cfg(test)]
    pub(crate) opened: Vec<(ChatAttachmentId, String, koru_window::OpenAs)>,
}

impl Work {
    pub(crate) fn new(root: Option<PathBuf>) -> Self {
        let (events_tx, events) = channel();
        Self {
            root,
            events_tx,
            events,
            uploads: None,
            thumbs: None,
            in_flight: 0,
            #[cfg(test)]
            fetched: Vec::new(),
            #[cfg(test)]
            viewer_fetched: Vec::new(),
            #[cfg(test)]
            opened: Vec::new(),
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.in_flight > 0
    }

    /// Read the clipboard's picture or files on `koru-clipboard`'s worker.
    /// Never in a test: the suite must not touch the desktop clipboard.
    pub(crate) fn read_clipboard(&mut self) -> bool {
        if cfg!(any(test, feature = "no-client")) {
            return false;
        }
        let events = self.events_tx.clone();
        let spawned = koru_clipboard::spawn_attachment_read(
            koru_clipboard::ImageLimits::default(),
            move |result| {
                let _ = events.send(Event::Clipboard(result));
            },
        );
        if spawned.is_ok() {
            self.in_flight += 1;
        }
        spawned.is_ok()
    }

    /// Store a draft with the AI service, on the upload thread: its bytes,
    /// or the copied file they are read from there.
    pub(crate) fn put(&mut self, chat: ChatId, draft: &Draft) {
        if let Some(path) = &draft.reference_path {
            self.upload(Upload::Reference {
                chat,
                id: draft.id,
                kind: draft.kind,
                path: path.clone(),
            });
            return;
        }
        let payload = match (&draft.bytes, &draft.path) {
            (Some(bytes), _) => Payload::Bytes(bytes.clone()),
            (None, Some(path)) => Payload::Path(path.clone()),
            (None, None) => return,
        };
        self.upload(Upload::Put {
            chat,
            id: draft.id,
            file_name: draft.file_name.clone(),
            source: draft.source,
            payload,
        });
    }

    /// Remove a staged attachment the person took out of the strip. Queued
    /// behind its own upload, so it goes after it whatever that answered —
    /// a timed-out upload may still have been stored.
    pub(crate) fn remove(&mut self, chat: ChatId, id: ChatAttachmentId) {
        self.upload(Upload::Remove { chat, id });
    }

    fn upload(&mut self, job: Upload) {
        // A test, or a host built without a client, dials nothing: the job
        // goes nowhere and its answer is the test's to hand in.
        if cfg!(any(test, feature = "no-client")) {
            return;
        }
        let put = match &job {
            Upload::Put { chat, id, .. } | Upload::Reference { chat, id, .. } => Some((*chat, *id)),
            Upload::Remove { .. } => None,
        };
        let (root, events) = (self.root.clone(), self.events_tx.clone());
        let sender = self
            .uploads
            .get_or_insert_with(|| spawn_uploader(root, events));
        // Unbounded: a strip holds at most ten drafts, and a removal must
        // never be dropped for want of room.
        let sent = sender.send(job).is_ok();
        if let Some((chat, id)) = put {
            self.in_flight += 1;
            if !sent {
                // Answered at once, through the same door, so the tile turns
                // red rather than spinning.
                let _ = self.events_tx.send(Event::Stored {
                    chat,
                    id,
                    result: Err(unavailable()),
                });
            }
        }
    }

    /// Decode a draft's own bytes on `koru-ai-thumbs`; the bytes are shared,
    /// never copied.
    pub(crate) fn decode(&mut self, id: ChatAttachmentId, bytes: bytes::Bytes) {
        self.thumb(ThumbJob::Decode(id, bytes));
    }

    /// Fetch and decode a sent picture's thumbnail on `koru-ai-thumbs`, on
    /// a client of its own.
    pub(crate) fn fetch_thumbnail(&mut self, chat: ChatId, id: ChatAttachmentId) {
        let job = ThumbJob::Fetch(chat, id);
        // A test answers its own fetches; nothing is dialed.
        #[cfg(test)]
        if let ThumbJob::Fetch(_, id) = job {
            self.fetched.push(id);
        }
        #[cfg(not(test))]
        self.thumb(job);
    }

    /// Fetch, decode and build the mips of a picture for the viewer, on
    /// `koru-ai-thumbs`; a draft's own bytes are shared, never copied.
    pub(crate) fn open_viewer(
        &mut self,
        chat: ChatId,
        id: ChatAttachmentId,
        bytes: Option<bytes::Bytes>,
    ) {
        let job = ThumbJob::Viewer(chat, id, bytes);
        // A test answers its own; nothing is dialed or decoded.
        #[cfg(test)]
        if let ThumbJob::Viewer(_, id, _) = job {
            self.viewer_fetched.push(id);
        }
        // A host built without a client dials nothing for the original.
        #[cfg(not(test))]
        if !(cfg!(feature = "no-client") && matches!(job, ThumbJob::Viewer(_, _, None))) {
            self.thumb(job);
        }
    }

    /// Open a picture's bytes in the system's app, from a temporary copy
    /// written on a helper thread (`koru_window::open_bytes_as_file`).
    pub(crate) fn open_externally(
        &mut self,
        id: ChatAttachmentId,
        file_name: &str,
        bytes: bytes::Bytes,
    ) {
        #[cfg(test)]
        {
            let _ = bytes;
            self.opened
                .push((id, file_name.to_owned(), koru_window::OpenAs::AsIs));
        }
        #[cfg(not(test))]
        {
            if cfg!(feature = "no-client") {
                return;
            }
            let events = self.events_tx.clone();
            let name = file_name.to_owned();
            let spawned = std::thread::Builder::new()
                .name("koru-ai-open".into())
                .spawn(move || {
                    let result = koru_window::open_bytes_as_file(
                        &id.to_string(),
                        &name,
                        &bytes,
                        koru_window::OpenAs::AsIs,
                    );
                    let _ = events.send(Event::Opened {
                        failed: opened_failure(&result),
                    });
                });
            if spawned.is_ok() {
                self.in_flight += 1;
            }
        }
    }

    /// Open a stored file in the system's app: fetched from the service and
    /// written as a temporary copy on a helper thread — a text or
    /// source-code file as `<name>.txt` (D12).
    pub(crate) fn open_file(&mut self, chat: ChatId, id: ChatAttachmentId, file_name: &str) {
        let how = open_as(file_name);
        #[cfg(test)]
        {
            let _ = chat;
            self.opened.push((id, file_name.to_owned(), how));
        }
        #[cfg(not(test))]
        {
            if cfg!(feature = "no-client") {
                return;
            }
            let (root, events) = (self.root.clone(), self.events_tx.clone());
            let name = file_name.to_owned();
            let spawned = std::thread::Builder::new()
                .name("koru-ai-open".into())
                .spawn(move || {
                    let bytes = super::reader::connect_client(root.as_deref())
                        .ok()
                        .and_then(|client| client.ai().attachment_bytes(chat, id).ok());
                    let failed = match bytes {
                        None => Some(Key::AiAttachmentMissing),
                        Some(bytes) => opened_failure(&koru_window::open_bytes_as_file(
                            &id.to_string(),
                            &name,
                            &bytes,
                            how,
                        )),
                    };
                    let _ = events.send(Event::Opened { failed });
                });
            if spawned.is_ok() {
                self.in_flight += 1;
            }
        }
    }

    fn thumb(&mut self, job: ThumbJob) {
        let (root, events) = (self.root.clone(), self.events_tx.clone());
        let sender = self
            .thumbs
            .get_or_insert_with(|| spawn_thumbnailer(root, events));
        if sender.send(job).is_ok() {
            self.in_flight += 1;
        }
    }

    pub(crate) fn try_event(&mut self) -> Option<Event> {
        let event = self.events.try_recv().ok()?;
        self.in_flight = self.in_flight.saturating_sub(1);
        Some(event)
    }

    /// Hand an answer in as the threads would, for a test.
    #[cfg(test)]
    pub(crate) fn inject(&mut self, event: Event) {
        self.in_flight += 1;
        let _ = self.events_tx.send(event);
    }
}

/// The line for a copy the system would not open, logged without its name.
#[cfg(not(test))]
fn opened_failure(result: &std::io::Result<PathBuf>) -> Option<Key> {
    let error = result.as_ref().err()?;
    tracing::debug!(
        target: "koru_ai_ui::attachments",
        kind = ?error.kind(),
        "an attachment could not be opened in another app"
    );
    Some(Key::AiOpenNoApp)
}

fn unavailable() -> Failure {
    Failure {
        code: "ai_unavailable".into(),
        message: "The AI service is not available.".into(),
        retryable: true,
        details: Default::default(),
    }
}

/// `koru-ai-upload`: one job at a time on a client of its own; a store
/// answers that the service is not available when no client connects.
fn spawn_uploader(root: Option<PathBuf>, events: Sender<Event>) -> Sender<Upload> {
    let (sender, jobs) = channel::<Upload>();
    let spawned = std::thread::Builder::new()
        .name("koru-ai-upload".into())
        .spawn(move || {
            let mut client = None;
            while let Ok(job) = jobs.recv() {
                if client.is_none() {
                    client = super::reader::connect_client(root.as_deref()).ok();
                }
                match job {
                    Upload::Put {
                        chat,
                        id,
                        file_name,
                        source,
                        payload,
                    } => {
                        let started = std::time::Instant::now();
                        let result = read_payload(payload).and_then(|bytes| match &client {
                            Some(client) => client
                                .ai()
                                .put_attachment(chat, id, &file_name, source, bytes)
                                .map(|response| response.attachment),
                            None => Err(unavailable()),
                        });
                        let byte_count = result
                            .as_ref()
                            .map_or(0, |attachment| attachment.byte_count);
                        tracing::debug!(
                            target: "koru_ai_ui::attachments",
                            attachment_id = %id,
                            byte_count,
                            outcome = if result.is_ok() { "stored" } else { "failed" },
                            upload_ms = started.elapsed().as_secs_f64() * 1_000.0,
                            "attachment upload finished"
                        );
                        if events.send(Event::Stored { chat, id, result }).is_err() {
                            return;
                        }
                    }
                    Upload::Reference {
                        chat,
                        id,
                        kind,
                        path,
                    } => {
                        let result = match &client {
                            Some(client) => client
                                .ai()
                                .reference_attachment(
                                    chat,
                                    koru_ai_wire::ReferenceAttachmentRequest { id, kind, path },
                                )
                                .map(|response| response.attachment),
                            None => Err(unavailable()),
                        };
                        if events.send(Event::Stored { chat, id, result }).is_err() {
                            return;
                        }
                    }
                    Upload::Remove { chat, id } => {
                        if let Some(client) = &client
                            && let Err(failure) = client.ai().remove_attachment(chat, id)
                        {
                            tracing::debug!(
                                target: "koru_ai_ui::attachments",
                                code = failure.code,
                                "a staged attachment was not removed; the service's sweep will"
                            );
                        }
                    }
                }
            }
        });
    if spawned.is_err() {
        tracing::warn!(target: "koru_ai_ui::attachments", "the attachment upload thread did not start");
    }
    sender
}

/// A copied file's bytes, its size checked before a byte is read: one over
/// the limit is refused without reading it (D7), one that cannot be read is
/// said to be unreadable.
fn read_payload(payload: Payload) -> Result<bytes::Bytes, Failure> {
    let path = match payload {
        Payload::Bytes(bytes) => return Ok(bytes),
        Payload::Path(path) => path,
    };
    let unreadable = || Failure {
        code: UNREADABLE.into(),
        message: "The file could not be read.".into(),
        retryable: true,
        details: Default::default(),
    };
    let metadata = std::fs::metadata(&path).map_err(|_| unreadable())?;
    if !metadata.is_file() {
        return Err(unreadable());
    }
    if metadata.len() > koru_ai_core::attachments::MAX_ATTACHMENT_BYTES {
        return Err(Failure {
            code: attachment_codes::TOO_LARGE.into(),
            message: "The attachment is larger than the limit.".into(),
            retryable: false,
            details: Default::default(),
        });
    }
    std::fs::read(&path)
        .map(bytes::Bytes::from)
        .map_err(|_| unreadable())
}

/// `koru-ai-thumbs`: fetch a sent picture's thumbnail on a client of its
/// own, or take a draft's bytes, and decode either to a brush no larger
/// than [`THUMB_EDGE`] — never on the window thread, never through the
/// reader's queue.
fn spawn_thumbnailer(root: Option<PathBuf>, events: Sender<Event>) -> Sender<ThumbJob> {
    let (sender, jobs) = channel::<ThumbJob>();
    let spawned = std::thread::Builder::new()
        .name("koru-ai-thumbs".into())
        .spawn(move || {
            let mut client = None;
            while let Ok(job) = jobs.recv() {
                let started = std::time::Instant::now();
                let (id, bytes) = match job {
                    ThumbJob::Viewer(chat, id, bytes) => {
                        let bytes = bytes.or_else(|| {
                            if client.is_none() {
                                client = super::reader::connect_client(root.as_deref()).ok();
                            }
                            client
                                .as_ref()
                                .and_then(|client| client.ai().attachment_bytes(chat, id).ok())
                                .map(bytes::Bytes::from)
                        });
                        let picture = bytes.and_then(|bytes| {
                            ViewerImage::decode(&bytes, viewer_limits())
                                .map(|image| (Arc::new(image), bytes))
                        });
                        tracing::debug!(
                            target: "koru_ai_ui::attachments",
                            attachment_id = %id,
                            decoded = picture.is_some(),
                            viewer_ms = started.elapsed().as_secs_f64() * 1_000.0,
                            "attachment ready for the viewer"
                        );
                        if events
                            .send(Event::ViewerReady { chat, id, picture })
                            .is_err()
                        {
                            return;
                        }
                        continue;
                    }
                    ThumbJob::Decode(id, bytes) => (id, Some(bytes)),
                    ThumbJob::Fetch(chat, id) => {
                        if client.is_none() {
                            client = super::reader::connect_client(root.as_deref()).ok();
                        }
                        let bytes = client
                            .as_ref()
                            .and_then(|client| client.ai().attachment_thumbnail(chat, id).ok())
                            .map(bytes::Bytes::from);
                        (id, bytes)
                    }
                };
                let image = bytes.as_deref().and_then(decode_thumbnail);
                tracing::debug!(
                    target: "koru_ai_ui::attachments",
                    attachment_id = %id,
                    decoded = image.is_some(),
                    thumbnail_ms = started.elapsed().as_secs_f64() * 1_000.0,
                    "attachment thumbnail ready"
                );
                if events.send(Event::Decoded { id, image }).is_err() {
                    return;
                }
            }
        });
    if spawned.is_err() {
        tracing::warn!(target: "koru_ai_ui::attachments", "the thumbnail thread did not start");
    }
    sender
}

/// The bounds a picture is decoded within for the viewer: the service's own
/// preview limits (D7), so a decode bomb is refused here as it was there.
fn viewer_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(koru_ai_core::attachments::MAX_PREVIEW_EDGE);
    limits.max_image_height = Some(koru_ai_core::attachments::MAX_PREVIEW_EDGE);
    limits.max_alloc = Some(koru_ai_core::attachments::MAX_PREVIEW_PIXELS * 4 + 64 * 1024 * 1024);
    limits
}

pub(crate) fn decode_thumbnail(bytes: &[u8]) -> Option<LoadedImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(koru_ai_core::attachments::MAX_PREVIEW_EDGE);
    limits.max_image_height = Some(koru_ai_core::attachments::MAX_PREVIEW_EDGE);
    limits.max_alloc = Some(koru_ai_core::attachments::MAX_PREVIEW_PIXELS * 4 + 64 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    let decoded = if decoded.width().max(decoded.height()) > THUMB_EDGE {
        decoded.resize(
            THUMB_EDGE,
            THUMB_EDGE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        decoded
    };
    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    let data = vello::peniko::ImageData {
        data: vello::peniko::Blob::new(Arc::new(rgba.into_raw())),
        format: vello::peniko::ImageFormat::Rgba8,
        alpha_type: vello::peniko::ImageAlphaType::Alpha,
        width,
        height,
    };
    Some(LoadedImage {
        brush: vello::peniko::ImageBrush::new(data).with_quality(vello::peniko::ImageQuality::High),
        width,
        height,
    })
}

/// Where a host lifts the chat's viewer to: the window. `origin` is where the
/// surface's own coordinates put the window's top-left — `(0, 0)` for a
/// surface laid out in window coordinates, the pane's top-left for one laid
/// out in its pane's — and `size` the window's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewerLift {
    pub origin: (f32, f32),
    pub size: (f32, f32),
}

/// A picture open in the shared viewer (R3, F3).
pub(crate) struct ChatViewer {
    pub id: ChatAttachmentId,
    pub file_name: String,
    pub viewer: MediaViewer,
    /// The original's bytes, kept while the viewer is up for *Open in
    /// external app*; released when it closes.
    pub bytes: bytes::Bytes,
}

/// The drafts of every chat, kept per chat so a draft survives a switch to
/// another chat and back (D15).
#[derive(Default)]
pub(crate) struct Strips {
    by_chat: HashMap<ChatId, Vec<Draft>>,
}

impl Strips {
    pub(crate) fn get(&self, chat: ChatId) -> &[Draft] {
        self.by_chat.get(&chat).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn push(&mut self, chat: ChatId, draft: Draft) {
        self.by_chat.entry(chat).or_default().push(draft);
    }

    pub(crate) fn draft_mut(&mut self, chat: ChatId, id: ChatAttachmentId) -> Option<&mut Draft> {
        self.by_chat
            .get_mut(&chat)?
            .iter_mut()
            .find(|draft| draft.id == id)
    }

    pub(crate) fn remove(&mut self, chat: ChatId, id: ChatAttachmentId) -> Option<Draft> {
        let drafts = self.by_chat.get_mut(&chat)?;
        let index = drafts.iter().position(|draft| draft.id == id)?;
        Some(drafts.remove(index))
    }

    pub(crate) fn take(&mut self, chat: ChatId) -> Vec<Draft> {
        self.by_chat.remove(&chat).unwrap_or_default()
    }

    /// How many of a chat's drafts are still being stored.
    pub(crate) fn storing(&self, chat: ChatId) -> usize {
        self.get(chat)
            .iter()
            .filter(|draft| draft.state == DraftState::Storing)
            .count()
    }

    pub(crate) fn any_failed(&self, chat: ChatId) -> bool {
        self.get(chat)
            .iter()
            .any(|draft| matches!(draft.state, DraftState::Failed { .. }))
    }
}

// ----- the surface's side -------------------------------------------------

impl ChatSurface {
    /// The open chat's draft attachments.
    pub(super) fn strip(&self) -> &[Draft] {
        self.current_chat_id()
            .map_or(&[], |chat| self.strips.get(chat))
    }

    pub(super) fn has_strip(&self) -> bool {
        !self.strip().is_empty()
    }

    /// How much taller the composer's box is for its strip: the strip's row
    /// above the text, at the box's own top padding.
    pub(super) fn strip_band(&self) -> f32 {
        if self.chip_band.shown_height > 0.0 {
            COMPOSER_PAD_TOP + self.chip_band.shown_height + crate::composer_band::GAP
        } else {
            0.0
        }
    }

    pub(super) fn attachment_chip_secondary(&self, draft: &Draft) -> String {
        match draft.state {
            DraftState::Storing => koru_i18n::t(self.lang, Key::AiAttachmentAddingOne).into(),
            DraftState::Failed { .. } => koru_i18n::t(self.lang, Key::AiAttachmentFailed).into(),
            DraftState::Stored if draft.source == AttachmentSource::Path => {
                koru_i18n::t(self.lang, Key::AiComposerFilesAndFolders).into()
            }
            DraftState::Stored if draft.byte_count > 0 => {
                koru_i18n::number::bytes(self.lang, draft.byte_count)
            }
            DraftState::Stored => String::new(),
        }
    }

    pub(super) fn attachment_chip_icon(&self, draft: &Draft) -> char {
        match draft.kind {
            AttachmentKind::Image => icons::IMAGE,
            AttachmentKind::Folder => icons::FOLDER,
            AttachmentKind::File => koru_ui::file_icon::glyph(None, Some(&draft.file_name)),
        }
    }

    pub(super) fn attachment_chip_state(
        &self,
        draft: &Draft,
    ) -> koru_ui::context_chip::ContextState {
        use koru_ui::context_chip::ContextState;
        match draft.state {
            DraftState::Storing => ContextState::Pending,
            DraftState::Failed { .. } => ContextState::Refused,
            DraftState::Stored => ContextState::Ready,
        }
    }

    fn chat_takes_attachments(&self) -> bool {
        self.transcript.as_ref().is_some_and(|transcript| {
            agents::is_agent_provider(&transcript.chat.current_model.provider)
        })
    }

    /// A paste found no text: read the clipboard's picture off this thread.
    /// Only a local agent's chat takes one (D8).
    pub(super) fn begin_attachment_paste(&mut self) -> bool {
        if self.transcript.is_none() {
            return false;
        }
        if !self.chat_takes_attachments() {
            self.status = Some(koru_i18n::t(self.lang, Key::AiAttachmentsAgentsOnly).to_owned());
            return true;
        }
        if self.clipboard_reading {
            return false;
        }
        // A picture is stored under its chat, so a new chat not made yet is
        // made for it — a pasted picture is the chat's first content — and
        // the paste is read once the service has it.
        if self.chat_unmade() {
            let chat = self.shown_chat_id();
            if self.make_unmade_chat() {
                self.paste_after_create = chat;
            }
            return true;
        }
        self.clipboard_reading = self.attachment_work.read_clipboard();
        self.clipboard_reading
    }

    /// Answers from the clipboard, upload and thumbnail threads.
    pub(super) fn maintain_attachments(&mut self) -> bool {
        let mut changed = false;
        while let Some(event) = self.attachment_work.try_event() {
            changed = true;
            match event {
                Event::Clipboard(result) => {
                    self.clipboard_reading = false;
                    self.on_clipboard(result);
                }
                Event::Stored { chat, id, result } => self.on_stored(chat, id, result),
                Event::Decoded { id, image } => match image {
                    Some(image) => self.thumbnails.ready(id, image),
                    None => self.thumbnails.failed(id, Instant::now()),
                },
                Event::ViewerReady { chat, id, picture } => self.on_viewer_ready(chat, id, picture),
                Event::Opened { failed } => {
                    if let Some(key) = failed {
                        self.status = Some(koru_i18n::t(self.lang, key).to_owned());
                    }
                }
            }
        }
        changed
    }

    /// While a thread owes an answer, the host keeps ticking.
    pub(super) fn attachment_deadline(&self) -> Option<Instant> {
        let waiting = self.attachment_work.busy() || self.thumbnails.any_pending();
        waiting.then(|| Instant::now() + Duration::from_millis(50))
    }

    fn on_clipboard(
        &mut self,
        result: Result<koru_clipboard::AttachmentPayload, koru_clipboard::Error>,
    ) {
        let Some(chat) = self.current_chat_id() else {
            return;
        };
        match result {
            Ok(koru_clipboard::AttachmentPayload::Bitmap(png)) => {
                tracing::debug!(target: "koru_ai_ui::attachments", payload = "bitmap", "clipboard read");
                let draft = Draft::pasted(png, std::time::SystemTime::now());
                self.add_draft(chat, draft);
            }
            Ok(koru_clipboard::AttachmentPayload::Files(paths)) => {
                let (allowed, refused) = sort_by_allowlist(paths);
                tracing::debug!(
                    target: "koru_ai_ui::attachments",
                    payload = "files",
                    allowed = allowed.len(),
                    refused = refused.len(),
                    "clipboard read"
                );
                for path in allowed {
                    self.add_draft(chat, Draft::from_path(path, AttachmentSource::Clipboard));
                }
                // Said after the drafts, so a limit one of them met does not
                // hide which types were refused.
                if let Some(line) = refused_line(&refused, self.lang) {
                    self.status = Some(line);
                }
            }
            Ok(koru_clipboard::AttachmentPayload::Empty) => {}
            Err(_) => {
                self.status = Some(koru_i18n::t(self.lang, Key::AiClipboardReadFailed).to_owned());
            }
        }
    }

    /// Add one draft, within the message's limits, and start storing it and
    /// decoding its thumbnail.
    pub(super) fn add_draft(&mut self, chat: ChatId, draft: Draft) {
        let drafts = self.strips.get(chat);
        let total: u64 = drafts.iter().map(|draft| draft.byte_count).sum();
        if draft.byte_count > koru_ai_core::attachments::MAX_ATTACHMENT_BYTES {
            self.status = Some(koru_i18n::t(self.lang, Key::AiAttachmentTooLarge).to_owned());
            return;
        }
        if drafts.len() >= koru_ai_core::attachments::MAX_ATTACHMENTS_PER_MESSAGE
            || total + draft.byte_count > koru_ai_core::attachments::MAX_MESSAGE_ATTACHMENT_BYTES
        {
            self.status = Some(koru_i18n::t(self.lang, Key::AiAttachmentLimit).to_owned());
            return;
        }
        if let Some(bytes) = &draft.bytes {
            // Shared, not copied: the bytes are counted, never duplicated.
            self.thumbnails.pending(draft.id);
            self.attachment_work.decode(draft.id, bytes.clone());
        }
        self.attachment_work.put(chat, &draft);
        self.strips.push(chat, draft);
    }

    pub(super) fn on_stored(
        &mut self,
        chat: ChatId,
        id: ChatAttachmentId,
        result: Result<ChatAttachment, Failure>,
    ) {
        let Some(draft) = self.strips.draft_mut(chat, id) else {
            // Taken out of the strip while it was being stored: its removal
            // is already queued behind this upload.
            return;
        };
        match result {
            Ok(attachment) => {
                let from_file = draft.bytes.is_none();
                draft.state = DraftState::Stored;
                draft.bytes = None;
                draft.path = None;
                draft.size = attachment.dimensions;
                draft.kind = attachment.kind;
                draft.byte_count = attachment.byte_count;
                draft.file_name = attachment.file_name;
                draft.origin = attachment.origin;
                draft.reference_path = attachment.reference_path;
                // A copied picture had no bytes here to decode: its tile
                // shows the thumbnail the service made.
                if from_file && attachment.kind == AttachmentKind::Image {
                    self.thumbnails.pending(id);
                    self.attachment_work.fetch_thumbnail(chat, id);
                }
            }
            Err(failure) => {
                let (line, retryable) = failure_line(&failure, self.lang);
                draft.state = DraftState::Failed { retryable };
                if self.current_chat_id() == Some(chat) {
                    self.status = Some(line);
                }
            }
        }
    }

    /// A press on the strip: × takes a draft out and removes its stored copy
    /// whatever its state — one whose upload timed out here may still have
    /// been stored; a red draft the service could not take for a passing
    /// reason is tried again with the same id.
    pub(super) fn strip_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        let composer = self.composer_hit_rect;
        let origin_x = composer.x + COMPOSER_PAD_X;
        let origin_y = composer.y + COMPOSER_PAD_TOP;
        let available = (composer.w - COMPOSER_PAD_X * 2.0).max(1.0);
        let index = self.chip_band.hit(x, y, origin_x, origin_y, available)?;
        let rect = self.chip_band.chip_rect(index, origin_x, origin_y)?;
        if index == 0 && self.context_document.is_some() {
            let (_, title) = self.context_document.as_ref()?;
            let chip = koru_ui::context_chip::ContextChip {
                label: title,
                secondary: "Koru",
                icon: icons::DOCUMENT,
                state: koru_ui::context_chip::ContextState::Ready,
            };
            if chip.remove_rect(rect).contains(x, y) {
                self.remove_context_document();
            }
            return Some(true);
        }
        let chat = self.current_chat_id()?;
        let index = index - usize::from(self.context_document.is_some());
        let draft = &self.strips.get(chat)[index];
        let id = draft.id;
        let secondary = self.attachment_chip_secondary(draft);
        let chip = koru_ui::context_chip::ContextChip {
            label: &draft.file_name,
            secondary: &secondary,
            icon: self.attachment_chip_icon(draft),
            state: self.attachment_chip_state(draft),
        };
        if chip.remove_rect(rect).contains(x, y) {
            self.strips.remove(chat, id)?;
            self.workspace_imports.remove(&id);
            self.workspace_import_documents.remove(&id);
            if self
                .pending_workspace_import_grant
                .as_ref()
                .is_some_and(|(_, request)| request.attachment == id)
            {
                self.pending_workspace_import_grant = None;
            }
            if self
                .workspace_import_request
                .as_ref()
                .is_some_and(|request| request.attachment == id)
            {
                self.workspace_import_request = None;
            }
            self.attachment_work.remove(chat, id);
            self.thumbnails.remove(id);
            return Some(true);
        }
        if matches!(draft.state, DraftState::Failed { retryable: true })
            && self.workspace_imports.contains_key(&id)
        {
            return Some(self.retry_workspace_import(chat, id));
        }
        if draft.state == (DraftState::Failed { retryable: true }) && draft.can_retry() {
            let draft = self.strips.draft_mut(chat, id)?;
            draft.state = DraftState::Storing;
            let draft = draft.clone();
            self.attachment_work.put(chat, &draft);
            return Some(true);
        }
        // A picture opens in the viewer, from its own bytes while it is
        // still being stored, from the service once it is; a stored file
        // opens in the system's app.
        match draft.state {
            DraftState::Failed { .. } => {}
            DraftState::Stored if draft.source == AttachmentSource::Path => {}
            DraftState::Stored if draft.kind == AttachmentKind::File => {
                let name = draft.file_name.clone();
                self.attachment_work.open_file(chat, id, &name);
            }
            DraftState::Storing if draft.bytes.is_none() => {}
            DraftState::Storing | DraftState::Stored => {
                let (tile, bytes) = (draft.tile(), draft.bytes.clone());
                self.open_picture(chat, &tile, bytes);
            }
        }
        Some(true)
    }

    // ----- the viewer (R3) ------------------------------------------------

    /// Open a picture tile in the viewer. The bytes are fetched and decoded
    /// off this thread; the viewer opens when they are ready, if the person
    /// is still on the chat they clicked in. A file has no viewer.
    pub(super) fn open_picture(
        &mut self,
        chat: ChatId,
        tile: &Tile,
        bytes: Option<bytes::Bytes>,
    ) -> bool {
        if tile.kind != AttachmentKind::Image {
            return false;
        }
        if self.viewer_opening.as_ref().map(|(_, id, _)| *id) == Some(tile.id) {
            return true;
        }
        self.viewer_opening = Some((chat, tile.id, tile.name.clone()));
        self.attachment_work.open_viewer(chat, tile.id, bytes);
        true
    }

    fn on_viewer_ready(
        &mut self,
        chat: ChatId,
        id: ChatAttachmentId,
        picture: Option<(Arc<ViewerImage>, bytes::Bytes)>,
    ) {
        let Some((asked_chat, asked_id, file_name)) = self.viewer_opening.take() else {
            return;
        };
        if asked_chat != chat || asked_id != id || self.current_chat_id() != Some(chat) {
            // Superseded by another click, or the person moved on.
            self.viewer_opening = (asked_id != id).then_some((asked_chat, asked_id, file_name));
            return;
        }
        let Some((image, bytes)) = picture else {
            self.status = Some(koru_i18n::t(self.lang, Key::AiViewerDecodeFailed).to_owned());
            return;
        };
        let viewport = self.viewer_viewport();
        self.viewer = Some(ChatViewer {
            id,
            file_name: file_name.clone(),
            viewer: MediaViewer::open(
                Stage::Bitmap { image },
                ViewerOptions {
                    actions: ActionSet::PICTURE,
                    title: file_name,
                },
                viewport,
            ),
            bytes,
        });
    }

    /// Whether a picture is open in the viewer.
    pub fn viewer_open(&self) -> bool {
        self.viewer.is_some()
    }

    /// **Lift the viewer to the window.** A host that lays this surface out
    /// in a pane or a column says where the window is and how big; the
    /// viewer then covers the window — its scrim over everything, the
    /// picture free to grow past the column — and draws through
    /// [`draw_lifted_viewer`](Self::draw_lifted_viewer) rather than in this
    /// surface's own `draw`. Input still arrives in the surface's
    /// coordinates and is translated here. `None` keeps the viewer inside
    /// the surface. Cheap to call every frame.
    pub fn set_viewer_lift(&mut self, lift: Option<ViewerLift>) {
        if self.viewer_lift == lift {
            return;
        }
        self.viewer_lift = lift;
        let (w, h) = self.viewer_viewport();
        if let Some(open) = &mut self.viewer {
            open.viewer.set_viewport(w, h);
        }
    }

    /// The viewer's window: the one it is lifted to, or this surface's.
    pub(super) fn viewer_viewport(&self) -> (f32, f32) {
        self.viewer_lift
            .map_or((self.viewport.w, self.viewport.h), |lift| lift.size)
    }

    /// A point in this surface's coordinates, in the viewer's.
    fn viewer_point(&self, x: f32, y: f32) -> (f32, f32) {
        match self.viewer_lift {
            Some(lift) => (x + lift.origin.0, y + lift.origin.1),
            None => (x - self.viewport.x, y - self.viewport.y),
        }
    }

    /// Paint the lifted viewer at the window's root. Nothing unless a
    /// picture is open and lifted.
    pub fn draw_lifted_viewer(&mut self, canvas: &mut Scene, root: Affine) {
        if let (Some(open), Some(_)) = (&self.viewer, &self.viewer_lift) {
            open.viewer.draw(canvas, root, &mut self.engine);
        }
    }

    /// Paint a viewer that is not lifted, over this surface, last.
    pub(super) fn draw_unlifted_viewer(&mut self, canvas: &mut Scene, root: Affine) {
        if let (Some(open), None) = (&self.viewer, &self.viewer_lift) {
            let root =
                root * Affine::translate((f64::from(self.viewport.x), f64::from(self.viewport.y)));
            open.viewer.draw(canvas, root, &mut self.engine);
        }
    }

    /// The viewer as a screen reader hears it, in the coordinates this
    /// surface is laid out in — the window's, when it is lifted.
    pub fn viewer_semantics(&self, lang: Lang) -> Option<ViewerSemantics> {
        let open = self.viewer.as_ref()?;
        let mut semantics = open.viewer.semantics(lang);
        if self.viewer_lift.is_none() {
            let (dx, dy) = (self.viewport.x, self.viewport.y);
            let shift = |rect: &mut Rect| {
                rect.x += dx;
                rect.y += dy;
            };
            shift(&mut semantics.bounds);
            shift(&mut semantics.zoom.rect);
            for button in &mut semantics.buttons {
                shift(&mut button.rect);
            }
        }
        Some(semantics)
    }

    /// A screen reader pressed one of the viewer's controls.
    pub fn viewer_act(&mut self, action: ViewerAction) -> bool {
        let Some(out) = self.viewer.as_mut().map(|open| open.viewer.act(action)) else {
            return false;
        };
        self.settle_viewer(out)
    }

    /// The display-zoom chords, claimed for the picture while it is up.
    pub fn viewer_zoom_key(&mut self, key: ZoomKey) -> bool {
        let Some(open) = &mut self.viewer else {
            return false;
        };
        open.viewer.zoom_key(key);
        true
    }

    /// `Ctrl`+wheel and a pinch, claimed for the picture while it is up.
    pub fn viewer_zoom_wheel(&mut self, delta: MouseScrollDelta, x: f32, y: f32) -> bool {
        let (x, y) = self.viewer_point(x, y);
        let Some(open) = &mut self.viewer else {
            return false;
        };
        open.viewer.zoom_wheel(delta, x, y);
        true
    }

    /// Act on what the viewer left behind. `true` always: the viewer is
    /// modal and the input was its own.
    fn settle_viewer(&mut self, outcome: ViewerOutcome) -> bool {
        match outcome {
            // Not offered for a chat's picture.
            ViewerOutcome::Handled | ViewerOutcome::Copy | ViewerOutcome::SaveAs => {}
            ViewerOutcome::OpenExternally => {
                if let Some(open) = &self.viewer {
                    let (id, name, bytes) = (open.id, open.file_name.clone(), open.bytes.clone());
                    self.attachment_work.open_externally(id, &name, bytes);
                }
            }
            ViewerOutcome::Close => {
                self.viewer = None;
            }
        }
        true
    }

    /// The viewer's share of each input, while it is up: `None` when it is
    /// down and the input is the surface's.
    pub(super) fn viewer_key(
        &mut self,
        key: &KeyPress,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        let out = self.viewer.as_mut()?.viewer.key(key, text, mods);
        Some(self.settle_viewer(out))
    }

    pub(super) fn viewer_mouse_down(&mut self, x: f32, y: f32) -> Option<bool> {
        let (x, y) = self.viewer_point(x, y);
        let out = self.viewer.as_mut()?.viewer.mouse_down(x, y);
        Some(self.settle_viewer(out))
    }

    pub(super) fn viewer_mouse_move(&mut self, x: f32, y: f32) -> Option<bool> {
        let (vx, vy) = self.viewer_point(x, y);
        let open = self.viewer.as_mut()?;
        self.pointer = (x, y);
        Some(open.viewer.mouse_move(vx, vy))
    }

    pub(super) fn viewer_mouse_up(&mut self) -> Option<bool> {
        let out = self.viewer.as_mut()?.viewer.mouse_up();
        Some(self.settle_viewer(out))
    }

    pub(super) fn viewer_double_click(&mut self, x: f32, y: f32) -> Option<bool> {
        let (x, y) = self.viewer_point(x, y);
        let out = self.viewer.as_mut()?.viewer.double_click(x, y);
        Some(self.settle_viewer(out))
    }

    pub(super) fn viewer_scroll(&mut self, delta: MouseScrollDelta) -> Option<bool> {
        let open = self.viewer.as_mut()?;
        open.viewer.pan_wheel(delta, WheelAxis::AsTurned);
        Some(true)
    }

    pub(super) fn viewer_cursor(&self) -> Option<CursorIcon> {
        self.viewer.as_ref().map(|open| open.viewer.cursor())
    }

    pub(super) fn viewer_resized(&mut self) {
        let (w, h) = self.viewer_viewport();
        if self.viewer_lift.is_none()
            && let Some(open) = &mut self.viewer
        {
            open.viewer.set_viewport(w, h);
        }
    }

    /// A sent attachment's tile under `(x, y)`, from the tiles last drawn.
    pub(super) fn card_tile_at(&self, x: f32, y: f32) -> Option<Tile> {
        if !self.conversation_hit_rect.contains(x, y) {
            return None;
        }
        self.card_tile_hits
            .iter()
            .find(|(rect, _)| rect.contains(x, y))
            .map(|(_, tile)| tile.clone())
    }

    /// A click on a sent attachment: a picture opens in the viewer, a file
    /// in the system's app.
    pub(super) fn open_tile(&mut self, chat: ChatId, tile: &Tile) -> bool {
        match tile.kind {
            AttachmentKind::Image => self.open_picture(chat, tile, None),
            AttachmentKind::File => {
                self.attachment_work.open_file(chat, tile.id, &tile.name);
                true
            }
            AttachmentKind::Folder => false,
        }
    }

    /// What a send carries, or `None` when it must wait: while a draft is
    /// still being stored (*Adding 1 attachment…*), or while one could not
    /// be stored. The drafts move to the sent card as its tiles, and are
    /// kept with the sent message until the service accepts it.
    pub(super) fn take_attachments_for_send(&mut self, chat: ChatId) -> Option<Vec<Draft>> {
        if !self.attachments_ready(chat) {
            return None;
        }
        let drafts = self.strips.take(chat);
        self.pending_user_tiles = drafts.iter().map(Draft::tile).collect();
        Some(drafts)
    }

    /// What a queued message carries (`queue`): the strip's drafts, on the
    /// same terms as a send — none still being stored, none that could not
    /// be — but with no sent card to show them on yet.
    pub(super) fn take_attachments_for_queue(&mut self, chat: ChatId) -> Option<Vec<Draft>> {
        self.attachments_ready(chat).then(|| self.strips.take(chat))
    }

    /// Whether the chat's drafts can go with a message now; the status line
    /// says why not.
    fn attachments_ready(&mut self, chat: ChatId) -> bool {
        let storing = self.strips.storing(chat);
        if storing > 0 {
            self.status = Some(if storing == 1 {
                koru_i18n::t(self.lang, Key::AiAttachmentAddingOne).to_owned()
            } else {
                koru_i18n::fill(
                    koru_i18n::t(self.lang, Key::AiAttachmentsAdding),
                    &[&storing.to_string()],
                )
            });
            return false;
        }
        if self.strips.any_failed(chat) {
            self.status = Some(koru_i18n::t(self.lang, Key::AiAttachmentsRemoveFailed).to_owned());
            return false;
        }
        // Copied files' sizes are known only once stored, so the message's
        // total is checked again here; the service checks it at bind too.
        let total: u64 = self
            .strips
            .get(chat)
            .iter()
            .map(|draft| draft.byte_count)
            .sum();
        if total > koru_ai_core::attachments::MAX_MESSAGE_ATTACHMENT_BYTES {
            self.status = Some(koru_i18n::t(self.lang, Key::AiAttachmentLimit).to_owned());
            return false;
        }
        true
    }

    /// A refused send's drafts, the one the service named marked red, kept
    /// together rather than put back on the strip — for a queued message,
    /// which goes back to its queue (`queue`).
    pub(super) fn mark_refused(drafts: &mut [Draft], failure: Option<&Failure>) {
        let named = failure
            .and_then(|failure| failure.details.get("attachment_id"))
            .and_then(|value| value.as_str())
            .and_then(ChatAttachmentId::parse);
        for draft in drafts {
            if Some(draft.id) == named {
                draft.state = DraftState::Failed { retryable: false };
            }
        }
    }

    /// Remove a queued message's drafts from the service, as a strip's are
    /// when its chat stops taking them.
    pub(super) fn discard_drafts(&mut self, chat: ChatId, drafts: Vec<Draft>) {
        for draft in drafts {
            self.attachment_work.remove(chat, draft.id);
            self.thumbnails.remove(draft.id);
        }
    }

    /// A send that did not go: its drafts go back to the strip, the one the
    /// service named red.
    pub(super) fn restore_drafts(
        &mut self,
        chat: ChatId,
        drafts: Vec<Draft>,
        failure: Option<&Failure>,
    ) {
        let named = failure
            .and_then(|failure| failure.details.get("attachment_id"))
            .and_then(|value| value.as_str())
            .and_then(ChatAttachmentId::parse);
        for mut draft in drafts {
            if Some(draft.id) == named {
                draft.state = DraftState::Failed { retryable: false };
            }
            self.strips.push(chat, draft);
        }
    }

    /// Whether a send's failure is about one of its attachments, so its line
    /// is the attachment's own.
    pub(super) fn refused_for_attachment(failure: &Failure) -> bool {
        [
            attachment_codes::TOO_LARGE,
            attachment_codes::LIMIT,
            attachment_codes::TYPE_NOT_ALLOWED,
            attachment_codes::NOT_FOUND,
            attachment_codes::MISSING,
            attachment_codes::SENT,
            attachment_codes::UNSUPPORTED,
            attachment_codes::STORAGE,
            attachment_codes::WORKSPACE_NOT_GRANTED,
            attachment_codes::WORKSPACE_FILE_UNAVAILABLE,
        ]
        .contains(&failure.code.as_str())
    }

    /// A chat whose provider is no longer a local agent takes no
    /// attachments: its drafts are removed, and the person told (D8).
    pub(super) fn drop_strip_if_not_agent(&mut self, chat: ChatId) {
        if self.chat_takes_attachments() || self.strips.get(chat).is_empty() {
            return;
        }
        for draft in self.strips.take(chat) {
            self.attachment_work.remove(chat, draft.id);
            self.thumbnails.remove(draft.id);
        }
        self.status =
            Some(koru_i18n::t(self.lang, Key::AiAttachmentsRemovedForProvider).to_owned());
    }

    /// Ask once for the thumbnails of tiles on screen that have none yet; a
    /// hidden chat, or a tile scrolled away, fetches nothing. They go on the
    /// thumbnail thread's own client, never the reader's queue, so a screen
    /// of pictures cannot keep a person's own command out of it.
    pub(super) fn request_thumbnails(&mut self, tiles: &[(Rect, Tile)]) {
        let Some(chat) = self.current_chat_id() else {
            return;
        };
        let now = Instant::now();
        for (_, tile) in tiles {
            if tile.kind == AttachmentKind::Image && self.thumbnails.wants(tile.id, now) {
                self.thumbnails.pending(tile.id);
                self.attachment_work.fetch_thumbnail(chat, tile.id);
            }
        }
    }

    /// The strip, inside the composer's box above the text.
    pub(super) fn draw_strip(&mut self, canvas: &mut Scene, root: Affine, band_parent: Rect) {
        if self.chip_band.rects.is_empty() {
            return;
        }
        let drafts = self
            .current_chat_id()
            .map_or_else(Vec::new, |chat| self.strips.get(chat).to_vec());
        let origin_x = band_parent.x + COMPOSER_PAD_X;
        let origin_y = band_parent.y + COMPOSER_PAD_TOP;
        let available = (band_parent.w - COMPOSER_PAD_X * 2.0).max(1.0);
        let band = self.chip_band.band_rect(origin_x, origin_y, available);
        let _strip = koru_draw::inspect::scope(
            "ComposerContextBand",
            "part",
            root,
            scene::rect(band.x, band.y, band.w, band.h),
            |r| {
                r.property(
                    "chips",
                    self.chip_band.rects.len() as f32,
                    "context and attachments",
                );
                r.property(
                    "max lines",
                    crate::composer_band::MAX_LINES as f32,
                    "bounded band",
                );
            },
        );
        canvas.push_clip_layer(
            vello::peniko::Fill::NonZero,
            root,
            &scene::rect(band.x, band.y, band.w, band.h),
        );
        let mut index = 0;
        if let Some((_, title)) = &self.context_document {
            if let Some(rect) = self.chip_band.chip_rect(index, origin_x, origin_y) {
                koru_ui::context_chip::ContextChip {
                    label: title,
                    secondary: "Koru",
                    icon: icons::DOCUMENT,
                    state: koru_ui::context_chip::ContextState::Ready,
                }
                .draw(canvas, root, &mut self.engine, rect, self.pointer);
            }
            index += 1;
        }
        for draft in &drafts {
            let Some(rect) = self.chip_band.chip_rect(index, origin_x, origin_y) else {
                break;
            };
            let secondary = self.attachment_chip_secondary(draft);
            koru_ui::context_chip::ContextChip {
                label: &draft.file_name,
                secondary: &secondary,
                icon: self.attachment_chip_icon(draft),
                state: self.attachment_chip_state(draft),
            }
            .draw(canvas, root, &mut self.engine, rect, self.pointer);
            index += 1;
        }
        canvas.pop_layer();
    }

    /// Tiles collected while the conversation was drawn.
    pub(super) fn draw_card_tiles(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        tiles: &[(Rect, Tile)],
    ) {
        for (rect, tile) in tiles {
            self.draw_tile(canvas, root, *rect, tile, false, false);
        }
    }

    /// A file's chip (R4): its type's icon, its name on one line, cut with
    /// an ellipsis where it does not fit, and its size under it once known.
    fn draw_file_chip(&mut self, canvas: &mut Scene, root: Affine, rect: Rect, tile: &Tile) {
        let p = theme::p();
        let glyph = koru_ui::file_icon::glyph(None, Some(&tile.name));
        let icon = self.engine.icon_layout(glyph, 18.0);
        scene::draw_layout(
            canvas,
            root,
            &icon,
            rect.x + 8.0,
            rect.y + 8.0,
            Some(p.text_dim),
        );
        let width = (rect.w - 16.0).max(1.0);
        let name = self.engine.elided(&tile.name, role::meta(), width);
        let name = self.engine.simple_layout(&name, role::meta());
        scene::draw_layout_first_line(
            canvas,
            root,
            &name,
            rect.x + 8.0,
            rect.y + 30.0,
            Some(p.text),
        );
        if tile.byte_count > 0 && rect.h >= 56.0 {
            let size = koru_i18n::number::bytes(self.lang, tile.byte_count);
            let size = self.engine.simple_layout(&size, role::meta());
            scene::draw_layout_first_line(
                canvas,
                root,
                &size,
                rect.x + 8.0,
                rect.y + 30.0 + name.height(),
                Some(p.text_dim),
            );
        }
    }

    /// One tile: the picture fitted inside it, or its name for a file or a
    /// picture whose thumbnail has not arrived.
    fn draw_tile(
        &mut self,
        canvas: &mut Scene,
        root: Affine,
        rect: Rect,
        tile: &Tile,
        dimmed: bool,
        failed: bool,
    ) {
        let p = theme::p();
        let shape = scene::rrect(rect.x, rect.y, rect.w, rect.h, 6.0);
        scene::fill(canvas, root, p.code_bg, &shape);
        let picture = match self.thumbnails.get(tile.id) {
            Some(Thumb::Ready(image)) if tile.kind == AttachmentKind::Image => Some(image),
            _ => None,
        };
        if let Some(image) = picture {
            let scale =
                (rect.w / image.width.max(1) as f32).min(rect.h / image.height.max(1) as f32);
            let (w, h) = (image.width as f32 * scale, image.height as f32 * scale);
            let transform =
                root * Affine::translate((
                    (rect.x + (rect.w - w) * 0.5) as f64,
                    (rect.y + (rect.h - h) * 0.5) as f64,
                )) * Affine::scale(scale as f64);
            canvas.push_clip_layer(vello::peniko::Fill::NonZero, root, &shape);
            canvas.draw_image(&image.brush, transform);
            canvas.pop_layer();
        } else if tile.kind == AttachmentKind::File {
            self.draw_file_chip(canvas, root, rect, tile);
        } else {
            let icon = self.engine.icon_layout(icons::IMAGE, 18.0);
            scene::draw_layout(
                canvas,
                root,
                &icon,
                rect.x + 8.0,
                rect.y + 8.0,
                Some(p.text_dim),
            );
        }
        if dimmed {
            scene::fill(canvas, root, p.panel_bg.multiply_alpha(0.55), &shape);
        }
        scene::stroke(
            canvas,
            root,
            if failed {
                p.close_hover
            } else {
                p.panel_border
            },
            if failed { 2.0 } else { theme::BORDER_W },
            &shape,
        );
    }
}

#[cfg(test)]
mod tests;
