// SPDX-License-Identifier: MPL-2.0

//! **Attaching an image without freezing the window.**
//!
//! A paste, a drop or an `/image` pick used to do all of its work on the event
//! loop between two frames: read the file, decode it to RGBA (`image`), hand
//! the bytes to the store (`put_blob`, which answers once they are on its
//! disk), and then — on the first frame that drew the new row — resample the
//! master to the display size with a Lanczos pass (`ImageCache::scaled`). For
//! a full-screen PNG screenshot each of the decode, the store write and the
//! resample is in the hundreds of milliseconds on a release build and several
//! times that on a debug one, so the window stopped answering for about a
//! second, and longer for a 5K capture.
//!
//! The split is the one every slow job here already follows (the image
//! hydration pool in [`crate::images`], the diagram and spelling workers):
//!
//! - **One worker thread** owns everything that does not need the document:
//!   the file read, a header read for the size, the decode, the store write
//!   and the display-sized mip. Jobs run in order, so two quick pastes land
//!   in the order they were made, and a burst never runs several full-size
//!   decodes at once.
//! - Each answer goes onto a channel and the loop is woken with
//!   [`Wake::Image`]; `about_to_wait`'s image service drains it. The wake
//!   carries nothing, like every other wake.
//! - **The document edit stays on the event loop**, in the view, once the
//!   blob has an address. The place it lands is mapped again then
//!   ([`map_place`]), because the person kept working while it loaded.
//!
//! What the person sees meanwhile is the view's business: a placeholder drawn
//! where the picture will land, from `View::begin_image_import` until
//! `View::end_image_import`.

use std::path::PathBuf;
use std::sync::mpsc;

use koru_core::NodeId;
use koru_core::node::BlobHash;
use koru_core::schema::WorkspaceDoc;
use peniko::ImageBrush;
use winit::event_loop::EventLoopProxy;

use crate::app::{Wake, decode_image};
use crate::{ImageMipSide, ImagePlace, ImageTicket, PasteTarget, PastedImage};

/// Where one picture's bytes come from.
#[derive(Clone, Debug)]
pub(crate) enum ImageSource {
    /// A file on disk — dropped, picked, or a clipboard file list. Read on the
    /// worker; the file stem becomes the image's name.
    File(PathBuf),
    /// Bytes already in memory: a clipboard bitmap (the clipboard worker has
    /// already encoded it as PNG) or a picture pulled out of clipboard HTML,
    /// with the display size the source declared, if it did.
    Bytes {
        bytes: Vec<u8>,
        width: Option<u32>,
        height: Option<u32>,
        name: Option<String>,
    },
}

/// Somewhere to put the bytes and learn their address. [`koru_workspace::Blobs`]
/// in the app; an in-memory map in the tests, which may not touch the disk or
/// reach a store.
pub(crate) trait BlobPut: Send {
    fn put(&self, bytes: &[u8]) -> Result<BlobHash, String>;
}

impl BlobPut for koru_workspace::Blobs {
    fn put(&self, bytes: &[u8]) -> Result<BlobHash, String> {
        koru_workspace::Blobs::put(self, bytes)
    }
}

/// One gesture's images, for the worker.
pub(crate) struct ImageJob {
    pub ticket: ImageTicket,
    pub sources: Vec<ImageSource>,
    /// The store of the document that was in front when the gesture was made
    /// — captured then, so a tab switch while it loads cannot file the bytes
    /// under another workspace.
    pub blobs: Box<dyn BlobPut>,
    pub mip_side: Option<ImageMipSide>,
}

/// One image the worker finished: what the view builds a row from, its
/// decoded master for the image cache, and the mip the first draw will ask
/// for.
pub(crate) struct Prepared {
    pub image: PastedImage,
    pub brush: ImageBrush,
    pub mip: Option<(u32, ImageBrush)>,
}

/// Why one source did not become an image. The words a person reads are the
/// report's; this is the detail under its fold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Failure(pub String);

pub(crate) enum ImportEvent {
    /// The headers are read: the pixel size of every image that has one, in
    /// order, before the slow decode starts.
    Sized {
        ticket: ImageTicket,
        sizes: Vec<(u32, u32)>,
    },
    /// Everything that could be prepared, and why the rest could not.
    Done {
        ticket: ImageTicket,
        images: Vec<Prepared>,
        failures: Vec<Failure>,
    },
}

/// The event loop's handle on the worker.
pub(crate) struct ImageImporter {
    jobs: Option<mpsc::Sender<ImageJob>>,
    events: mpsc::Receiver<ImportEvent>,
}

impl ImageImporter {
    /// One thread, answering one job at a time; every answer wakes the loop.
    pub(crate) fn spawn(proxy: &EventLoopProxy<Wake>) -> Self {
        let (jobs, queue) = mpsc::channel::<ImageJob>();
        let (events_tx, events) = mpsc::channel::<ImportEvent>();
        let proxy = proxy.clone();
        let spawned = std::thread::Builder::new()
            .name("koru-image-import".into())
            .spawn(move || {
                while let Ok(job) = queue.recv() {
                    let mut gone = false;
                    run(job, |event| {
                        gone |= events_tx.send(event).is_err();
                        let _ = proxy.send_event(Wake::Image);
                    });
                    if gone {
                        break;
                    }
                }
            });
        Self {
            // No thread means every job is refused at `submit`, which the
            // caller turns into a reported failure rather than a placeholder
            // that never resolves.
            jobs: spawned.is_ok().then_some(jobs),
            events,
        }
    }

    /// Queue a job. Hands it back when there is no worker to take it.
    pub(crate) fn submit(&self, job: ImageJob) -> Result<(), ImageJob> {
        match &self.jobs {
            Some(jobs) => jobs.send(job).map_err(|e| e.0),
            None => Err(job),
        }
    }

    /// Everything the worker has said since the last drain.
    pub(crate) fn drain(&self) -> Vec<ImportEvent> {
        self.events.try_iter().collect()
    }
}

/// The worker's whole job, with the channel abstracted away so the tests run
/// it inline.
pub(crate) fn run(job: ImageJob, mut emit: impl FnMut(ImportEvent)) {
    let ImageJob {
        ticket,
        sources,
        blobs,
        mip_side,
    } = job;
    let mut failures = Vec::new();
    // Read every source first, so the sizes go out before the first decode —
    // a batch of three placeholders takes its final shape at once.
    let mut loaded = Vec::new();
    for source in sources {
        match load(source) {
            Ok(one) => loaded.push(one),
            Err(failure) => failures.push(failure),
        }
    }
    let sizes: Vec<(u32, u32)> = loaded.iter().filter_map(|one| one.header).collect();
    if !sizes.is_empty() {
        emit(ImportEvent::Sized { ticket, sizes });
    }
    let mut images = Vec::new();
    for one in loaded {
        match prepare(one, blobs.as_ref(), mip_side.as_deref()) {
            Ok(prepared) => images.push(prepared),
            Err(failure) => failures.push(failure),
        }
    }
    emit(ImportEvent::Done {
        ticket,
        images,
        failures,
    });
}

struct Loaded {
    bytes: Vec<u8>,
    width: Option<u32>,
    height: Option<u32>,
    name: Option<String>,
    /// The pixel size from the header alone — microseconds, where the decode
    /// is the slow part.
    header: Option<(u32, u32)>,
}

fn load(source: ImageSource) -> Result<Loaded, Failure> {
    let (bytes, width, height, name) = match source {
        ImageSource::File(path) => {
            let bytes = std::fs::read(&path)
                .map_err(|error| Failure(format!("{}: {error}", path.display())))?;
            // Named after the file stem ("photo.png" → "photo") so the
            // explore column shows a real name, not "Image 1".
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string);
            (bytes, None, None, name)
        }
        ImageSource::Bytes {
            bytes,
            width,
            height,
            name,
        } => (bytes, width, height, name),
    };
    let header = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        // A declared display size wins, the way it wins for the row.
        .map(|(w, h)| (width.unwrap_or(w), height.unwrap_or(h)));
    Ok(Loaded {
        bytes,
        width,
        height,
        name,
        header,
    })
}

fn prepare(
    one: Loaded,
    blobs: &dyn BlobPut,
    mip_side: Option<&(dyn Fn(u32, u32) -> u32 + Send + Sync)>,
) -> Result<Prepared, Failure> {
    let _watch = koru_core::timing::Watch::start("image.import");
    // Decoded before it is stored: bytes that are not a picture must not
    // reach the store as one.
    let (brush, w, h, mime) = decode_image(&one.bytes).ok_or_else(|| {
        Failure(format!(
            "not a recognised image ({} bytes)",
            one.bytes.len()
        ))
    })?;
    let hash = blobs.put(&one.bytes).map_err(Failure)?;
    // The source's display size, when it declared one (`\picwgoal`, an
    // `<img width>`), so the row is not stretched to full resolution.
    let width = one.width.unwrap_or(w);
    let height = one.height.unwrap_or(h);
    let mip = mip_side.and_then(|side| {
        let size = koru_draw::image::mip_size(&brush, side(width, height))?;
        Some((size, koru_draw::image::downscale(&brush, size)?))
    });
    Ok(Prepared {
        image: PastedImage {
            hash: hash.0,
            mime,
            width: Some(width),
            height: Some(height),
            name: one.name,
        },
        brush,
        mip,
    })
}

/// What the shell remembers about a place when the gesture is made, so it
/// can be mapped onto the document as it is when the images are ready.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordedPlace {
    pub place: ImagePlace,
    /// The sibling before the anchor at the gesture: where the images go if
    /// the anchor itself is deleted while they load.
    pub before: Option<NodeId>,
    /// `/image`'s line as it read at the gesture. Converting a line somebody
    /// has typed into since would destroy what they typed.
    pub text: Option<String>,
    /// Each row a paste replaces, with its text at the gesture — for the same
    /// reason: a selected row somebody has written in since is theirs again.
    pub replaced: Vec<(NodeId, Option<String>)>,
}

impl RecordedPlace {
    pub(crate) fn record(ws: &WorkspaceDoc, place: ImagePlace) -> Self {
        let anchor = match &place {
            ImagePlace::Paste(Some(target)) => Some(target.anchor),
            ImagePlace::Convert(node) => Some(*node),
            ImagePlace::Paste(None) | ImagePlace::Append => None,
        };
        let before = anchor.and_then(|anchor| previous_sibling(ws, anchor));
        let text = match &place {
            ImagePlace::Convert(node) => text_of(ws, *node),
            _ => None,
        };
        let replaced = match &place {
            ImagePlace::Paste(Some(target)) => target
                .replacing
                .iter()
                .map(|id| (*id, text_of(ws, *id)))
                .collect(),
            _ => Vec::new(),
        };
        Self {
            place,
            before,
            text,
            replaced,
        }
    }
}

fn text_of(ws: &WorkspaceDoc, id: NodeId) -> Option<String> {
    ws.read(id)
        .ok()
        .and_then(|node| node.content.as_text().map(str::to_string))
}

fn alive(ws: &WorkspaceDoc, id: NodeId) -> bool {
    // A deleted node has left its parent's children, which is exactly the
    // test `sibling_index` makes.
    ws.sibling_index(id).is_ok()
}

fn previous_sibling(ws: &WorkspaceDoc, id: NodeId) -> Option<NodeId> {
    let index = ws.sibling_index(id).ok()?;
    let siblings = ws.children(ws.parent(id).ok()?).ok()?;
    index.checked_sub(1).and_then(|i| siblings.get(i).copied())
}

fn below(anchor: NodeId) -> ImagePlace {
    ImagePlace::Paste(Some(PasteTarget {
        anchor,
        replacing: Vec::new(),
    }))
}

/// **Where the images land now**, given where they were meant to land.
///
/// A node id is already a position that survives concurrent edits — typing
/// before it, or a peer's insert, does not move it — so the anchor is kept
/// whenever it is still in the document. What it cannot survive is its own
/// deletion: then the images go after the sibling that preceded it, and with
/// that gone too, at the end of the document. Never nowhere: a paste the
/// person watched load does not silently vanish. And never over somebody's
/// words: a line written on since the gesture is neither converted nor
/// replaced.
pub(crate) fn map_place(ws: &WorkspaceDoc, recorded: &RecordedPlace) -> ImagePlace {
    let fallback = || match recorded.before.filter(|before| alive(ws, *before)) {
        Some(before) => below(before),
        None => ImagePlace::Paste(None),
    };
    match &recorded.place {
        ImagePlace::Append => ImagePlace::Append,
        ImagePlace::Paste(None) => ImagePlace::Paste(None),
        ImagePlace::Paste(Some(target)) if !alive(ws, target.anchor) => fallback(),
        ImagePlace::Paste(Some(target)) if target.replacing.is_empty() => below(target.anchor),
        ImagePlace::Paste(Some(target)) => {
            // The selection the paste replaces, less whatever has gone and
            // whatever has been written in since.
            let replacing: Vec<NodeId> = recorded
                .replaced
                .iter()
                .filter(|(id, text)| alive(ws, *id) && text_of(ws, *id) == *text)
                .map(|(id, _)| *id)
                .collect();
            if !replacing.is_empty() {
                // Still the anchor's slot: the images go where it stands.
                return ImagePlace::Paste(Some(PasteTarget {
                    anchor: target.anchor,
                    replacing,
                }));
            }
            // Nothing left to replace: above the anchor, where the
            // placeholder stood, which is below whatever precedes it now.
            match previous_sibling(ws, target.anchor) {
                Some(before) => below(before),
                None => below(target.anchor),
            }
        }
        ImagePlace::Convert(node) if !alive(ws, *node) => fallback(),
        ImagePlace::Convert(node) if text_of(ws, *node) == recorded.text => {
            ImagePlace::Convert(*node)
        }
        // Somebody wrote on the line while the picture loaded: keep what they
        // wrote and put the picture below it.
        ImagePlace::Convert(node) => below(*node),
    }
}

/// What a person is told when a picture could not be added: a plain title,
/// what state the document is in, and the raw reasons under the fold.
pub(crate) fn failure_report(
    lang: koru_i18n::Lang,
    some_landed: bool,
    failures: &[Failure],
) -> crate::ErrorReport {
    use koru_i18n::{Key, t};
    let body = if some_landed {
        Key::ImageAddPartlyFailedBody
    } else {
        Key::ImageAddFailedBody
    };
    let details: Vec<&str> = failures.iter().map(|failure| failure.0.as_str()).collect();
    crate::ErrorReport::new(t(lang, Key::ImageAddFailed), t(lang, body))
        .with_details(details.join("\n"))
}

#[cfg(test)]
mod tests;
