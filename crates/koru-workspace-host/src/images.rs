// SPDX-License-Identifier: MPL-2.0

//! Loading image blobs off the event loop.
//!
//! An image row is a content hash until its bytes are here. Local bytes decode
//! inline; store-backed bytes may have to be *hydrated* from a peer, which can
//! take seconds or park until a device comes back — so store loads run on a
//! small worker pool and the answer arrives as a [`Wake::Store`].
//!
//! Two sets bound the bookkeeping:
//!
//! - `pending` — a load that is queued or running, so a redraw storm cannot
//!   queue the same hash twice.
//! - `retry` — the exact hashes whose last hydration failed. Unlike a dirty
//!   flag, this is a bounded set of known misses, so a reconnect retry never
//!   walks the document on a timer.
//!
//! A parked hydration names the devices it waits for (`Error::waiting_on`);
//! those names land in the [`ImageCache`] beside the pixels, so the
//! placeholder can say *which* device to wait for rather than "the device".

use std::collections::HashSet;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use koru_core::WorkspaceId;
use koru_core::node::BlobHash;
use koru_core::timing;
use koru_draw::ImageCache;
use koru_store_core::errors::Code;
use winit::event_loop::EventLoopProxy;

use crate::app::{Wake, decode_image};

/// The decoded cache's scope for a workspace: its id as a plain integer, so
/// `koru-draw` needs none of the store's types. A read that returned bytes is
/// the proof the cache records — the shell never asserts an association it did
/// not perform.
fn scope(workspace: WorkspaceId) -> koru_draw::image::Scope {
    workspace.as_uuid().as_u128()
}

/// A missing remote blob may be waiting for a device to reconnect. Retry only
/// the known misses at this cadence; walking every document on a timer would
/// turn an unavailable image into permanent idle work.
const IMAGE_RETRY_EVERY: Duration = Duration::from_secs(3);

struct ImageLoadRequest {
    workspace: WorkspaceId,
    hash: BlobHash,
    blobs: koru_workspace::store::StoreBlobs,
}

struct ImageLoadResult {
    workspace: WorkspaceId,
    hash: BlobHash,
    /// The store's typed error, not its rendering: `waiting_on` is display
    /// data the placeholder needs, and parsing it back out of a string is how
    /// it used to be lost.
    bytes: Result<Vec<u8>, koru_store_core::errors::Error>,
}

/// The off-loop half of image loading: worker pool, in-flight dedupe, and the
/// bounded retry set. The event loop owns the [`ImageCache`] this fills.
pub struct ImageLoader {
    tx: Vec<mpsc::Sender<ImageLoadRequest>>,
    rx: mpsc::Receiver<ImageLoadResult>,
    pending: HashSet<(WorkspaceId, String)>,
    retry: HashSet<(WorkspaceId, String)>,
    retry_due: Option<Instant>,
    next_worker: usize,
}

impl ImageLoader {
    /// Four workers, each answering one load at a time; every completion wakes
    /// the loop with [`Wake::Store`].
    pub fn spawn(proxy: &EventLoopProxy<Wake>) -> Self {
        let (results, rx) = mpsc::channel::<ImageLoadResult>();
        let mut tx = Vec::new();
        for worker in 0..4 {
            let (requests, image_load_requests) = mpsc::channel::<ImageLoadRequest>();
            tx.push(requests);
            let results = results.clone();
            let proxy = proxy.clone();
            let _ = std::thread::Builder::new()
                .name(format!("koru-image-hydration-{worker}"))
                .spawn(move || {
                    loop {
                        let Ok(request) = image_load_requests.recv() else {
                            break;
                        };
                        let bytes = request.blobs.get(&request.hash);
                        if results
                            .send(ImageLoadResult {
                                workspace: request.workspace,
                                hash: request.hash,
                                bytes,
                            })
                            .is_err()
                        {
                            break;
                        }
                        let _ = proxy.send_event(Wake::Store);
                    }
                });
        }
        Self {
            tx,
            rx,
            pending: HashSet::new(),
            retry: HashSet::new(),
            retry_due: None,
            next_worker: 0,
        }
    }

    /// The next moment a known miss deserves another attempt, for the event
    /// loop's deadline.
    pub fn retry_due(&self) -> Option<Instant> {
        self.retry_due
    }

    /// A workspace switch drops every in-flight address and queued retry: the
    /// new workspace's scan replaces them.
    pub fn clear(&mut self) {
        self.pending.clear();
        self.retry.clear();
        self.retry_due = None;
    }

    /// Drain finished loads, decode what arrived, then queue what is missing.
    /// Returns whether anything was decoded, so the caller can request a
    /// redraw.
    ///
    /// `dirty_hashes` is `Some` when the document changed: the hashes the
    /// workspace references that the cache does not hold yet. When it is
    /// `None` only the bounded retry set can queue work.
    pub fn decode_missing(
        &mut self,
        current: WorkspaceId,
        images: &mut ImageCache,
        dirty_hashes: Option<Vec<String>>,
        blobs: Option<&koru_workspace::Blobs>,
    ) -> bool {
        let mut decoded = false;
        while let Ok(result) = self.rx.try_recv() {
            decoded |= self.receive(result, current, images);
        }
        let retries: Vec<String> = if self.retry_due.is_some_and(|due| Instant::now() >= due) {
            self.retry_due = None;
            self.retry
                .iter()
                .filter(|(workspace, _)| *workspace == current)
                .map(|(_, hash)| hash.clone())
                .collect()
        } else {
            Vec::new()
        };
        let mut hashes = retries;
        if let Some(dirty) = dirty_hashes {
            hashes.extend(dirty);
        }
        if hashes.is_empty() {
            return decoded;
        }
        hashes.sort();
        hashes.dedup();
        let Some(blobs) = blobs else {
            return decoded;
        };
        if let Some(loader) = blobs.store_loader() {
            // Only `spawn` builds workers; a test harness has none, and no
            // workers means nothing here can be sent.
            if self.tx.is_empty() {
                return decoded;
            }
            for hash in hashes {
                let key = (current, hash.clone());
                if !self.pending.insert(key.clone()) {
                    continue;
                }
                let worker = self.next_worker % self.tx.len();
                self.next_worker = self.next_worker.wrapping_add(1);
                if self.tx[worker]
                    .send(ImageLoadRequest {
                        workspace: current,
                        hash: BlobHash(hash.clone()),
                        blobs: loader.clone(),
                    })
                    .is_err()
                {
                    self.pending.remove(&key);
                    self.retry_due = Some(Instant::now() + IMAGE_RETRY_EVERY);
                }
            }
            return decoded;
        }
        let to_decode: Vec<(String, Vec<u8>)> = hashes
            .into_iter()
            .filter_map(|hash| blobs.get(&BlobHash(hash.clone())).ok().map(|b| (hash, b)))
            .collect();
        for (h, bytes) in to_decode {
            let _watch = timing::Watch::start("image.decode");
            if let Some((brush, _, _, _)) = decode_image(&bytes) {
                images.insert(scope(current), &h, brush);
                decoded = true;
            }
        }
        decoded
    }

    /// One finished load: decode it, or remember it for the reconnect retry.
    /// Split from [`decode_missing`](Self::decode_missing) so the parked and
    /// arrived paths are testable without an event loop. Returns whether what
    /// a draw would show changed — pixels arriving, or the placeholder's
    /// device names moving.
    fn receive(
        &mut self,
        result: ImageLoadResult,
        current: WorkspaceId,
        images: &mut ImageCache,
    ) -> bool {
        let key = (result.workspace, result.hash.0.clone());
        self.pending.remove(&key);
        if result.workspace != current {
            return false;
        }
        match result.bytes {
            Ok(bytes) => {
                self.retry.remove(&key);
                let _watch = timing::Watch::start("image.decode");
                if let Some((brush, _, _, _)) = decode_image(&bytes) {
                    images.insert(scope(current), &result.hash.0, brush);
                    return true;
                }
                false
            }
            Err(error) => {
                // The Store's hydration request can fail before a peer is
                // connected. Remember this one address so a reconnect retries
                // it without requiring a document edit or reopen.
                self.retry.insert(key);
                self.retry_due = Some(Instant::now() + IMAGE_RETRY_EVERY);
                // **The reason, whatever it turns out to be** (D259). The
                // Store carries the blob engine's own hold beside its code
                // precisely so the placeholder does not have to guess, and a
                // hold that is not recorded is a placeholder back to saying
                // rung 1's peer sentence about a Workspace that was never
                // switched on for Cloud.
                //
                // Recorded before the peer branch below, because both can be
                // true at once and the answers are not exclusive: a hold is
                // *why nothing is being fetched*, and the names are *who
                // would have it*. `set_hold` answers whether anything moved,
                // so the retry cadence re-reporting the same hold every few
                // seconds is not a redraw.
                let hold_changed = images.set_hold(&result.hash.0, error.hold());
                // A parked hydration names the devices it waits for; the
                // placeholder shows them. Only a change is news — the retry
                // cadence re-reports the same park every few seconds.
                if error.code() == Code::BlobWaitingForPeer
                    && !error.waiting_on().is_empty()
                    && images.pending(&result.hash.0) != Some(error.waiting_on())
                {
                    images.set_pending(&result.hash.0, error.waiting_on().to_vec());
                    // The placeholder's words changed: that is a redraw, the
                    // same as pixels landing.
                    return true;
                }
                if hold_changed {
                    return true;
                }
                // A definitive miss is not a wait: nobody authorized holds
                // the bytes and nobody offline could bring them, so the
                // placeholder must stop claiming otherwise. Anything else
                // (a timeout, a transient store failure) keeps the names —
                // it is still plausibly waiting.
                if error.code() == Code::BlobNotFound && images.clear_pending(&result.hash.0) {
                    return true;
                }
                false
            }
        }
    }

    /// One without its worker pool, for driving `receive` directly.
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        let (_results, rx) = mpsc::channel::<ImageLoadResult>();
        Self {
            tx: Vec::new(),
            rx,
            pending: HashSet::new(),
            retry: HashSet::new(),
            retry_due: None,
            next_worker: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn receive_for_test(
        &mut self,
        workspace: WorkspaceId,
        hash: &str,
        bytes: Result<Vec<u8>, koru_store_core::errors::Error>,
        current: WorkspaceId,
        images: &mut ImageCache,
    ) -> bool {
        self.receive(
            ImageLoadResult {
                workspace,
                hash: BlobHash(hash.to_string()),
                bytes,
            },
            current,
            images,
        )
    }
}

#[cfg(test)]
mod tests;
