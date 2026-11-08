// SPDX-License-Identifier: MPL-2.0

//! A cache of decoded images keyed by blob hash.
//!
//! Bytes never enter the CRDT ([ADR-12](../../../docs/decisions/README.md)): a node
//! holds only a hash, and the bytes live in the blob store. Decoding those bytes
//! into pixels is expensive and has nothing to do with the document, so it lives
//! in the shell — which owns the disk — and the result lands here. Views read
//! this cache at draw time to paint an image row, falling back to a placeholder
//! when a hash is not yet decoded (a remote image whose bytes have not arrived,
//! or one that failed to decode).
//!
//! Beside the pixels sits one fact about the bytes that are *not* here yet:
//! `pending` names the devices a parked hydration is waiting on, so the
//! placeholder can say "waiting for Pixel 8" rather than "the device holding
//! it". It is display data the shell owns — the network knows it, the view only
//! reads it, and `insert` clears it the moment the pixels arrive.
//!
//! ## Keyed by hash, scoped by association
//!
//! Bytes now live once per store root and the workspace's own record is the
//! permission to read them, so *"the pixels are here"* and *"this workspace may
//! see them"* are two different facts. Both are kept:
//!
//! - `pixels` holds every image decoded this session, by hash — decoding is the
//!   expensive half and the bytes are the same bytes whoever asks;
//! - `authorized` remembers, per hash, every workspace a read has *already been
//!   proved for*. A read that returned bytes is that proof; the shell never
//!   asserts one it did not perform.
//!
//! [`scope`](ImageCache::scope) says which workspace's associations govern the
//! frame being drawn, and every read below is filtered through it — so a hash
//! referenced by a node in a workspace that holds no record of it draws as
//! missing, even though the object is on this disk and decoded in memory.
//!
//! Before, changing workspace emptied the cache outright, because there was
//! nothing here that could tell the two facts apart. Emptying it meant reading
//! and decoding every picture again on the way back, on the thread that has to
//! stay responsive. Now the pixels survive and the *permission* moves, so a
//! picture shared by two workspaces is decoded once rather than once per
//! switch.
//!
//! **Nothing is evicted**, which is the cost of that and is stated rather than
//! discovered: the cache holds what a session has actually displayed, where it
//! used to hold what the current workspace had displayed. The existing note on
//! `mips` — that an LRU here is a decision about the masters — is the same
//! decision, now slightly larger, and still not taken.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use peniko::{Blob, ImageAlphaType, ImageBrush, ImageData, ImageFormat};

/// A workspace id as a plain integer, so this crate stays free of the store's
/// types. See [`ImageCache::UNSCOPED`] for the one value that is not one.
pub type Scope = u128;

/// Decoded images by blob hash, plus what is known about the ones still
/// missing. Filled by the shell once a blob is decoded; read by views at draw
/// time.
#[derive(Default)]
pub struct ImageCache {
    pixels: HashMap<String, Arc<ImageBrush>>,
    /// Display-sized mips, keyed by (hash, quantised larger axis). Filled
    /// lazily by [`scaled`](Self::scaled); the master stays in `pixels`.
    mips: Mutex<HashMap<(String, u32), Arc<ImageBrush>>>,
    /// Display names of the devices a parked hydration waits on, by hash.
    pending: HashMap<String, Vec<String>>,
    /// **Why blob transfer is holding this hash**, by hash — the blob
    /// engine's own stable code, exactly as the Store answered it.
    ///
    /// Display data the shell owns, like `pending` beside it, and a *code*
    /// rather than a sentence: this crate has no locale and no business
    /// owning words. The placeholder branches on it.
    ///
    /// Before this existed the placeholder had one sentence for every
    /// reason — rung 1's *"waiting for a device with this image to come
    /// online"* — so a Workspace this device had never been switched on for
    /// sent a person to restart hardware they did not need (D259).
    holds: HashMap<String, String>,
    /// Every workspace a read of this hash has actually succeeded for.
    ///
    /// **A record of proof, never a claim.** The shell writes a scope here
    /// only after a read returned those bytes for it.
    authorized: HashMap<String, HashSet<Scope>>,
    /// Whose associations govern what may be drawn right now.
    scope: Scope,
}

impl ImageCache {
    /// For a cache whose keys are not blob hashes at all — the feedback
    /// screenshot strip, say — where there is no workspace to be associated
    /// with. Zero is not a v4 UUID, so it can never collide with a real
    /// workspace id.
    pub const UNSCOPED: Scope = 0;

    pub fn new() -> Self {
        Self::default()
    }

    /// Record a decoded image under its hash, and that `scope` was allowed to
    /// read it.
    ///
    /// Idempotent for the pixels — the same hash decodes to the same pixels, so
    /// re-inserting is a no-op — and additive for the association, because a
    /// second workspace reading the same picture is a second fact, not a
    /// correction of the first. Pixels arriving ends any pending wait: the
    /// placeholder is done with its names.
    pub fn insert(&mut self, scope: Scope, hash: &str, brush: ImageBrush) {
        self.pending.remove(hash);
        // The pixels are here: nothing is holding this any more, and a
        // placeholder is not what anybody is about to draw.
        self.holds.remove(hash);
        self.authorized
            .entry(hash.to_string())
            .or_default()
            .insert(scope);
        self.pixels
            .entry(hash.to_string())
            .or_insert_with(|| Arc::new(brush));
    }

    /// Point every read below at one workspace's associations.
    ///
    /// **Nothing is discarded.** The permission moves, not the pixels, which
    /// is the whole reason switching between two tabs of different workspaces
    /// no longer re-decodes a picture they both show.
    ///
    /// The shell sets this on every frame from the active document's
    /// workspace, so there is no window in which the cache answers for a
    /// workspace that is no longer the one being drawn.
    pub fn set_scope(&mut self, scope: Scope) {
        self.scope = scope;
    }

    /// Whether a read of this hash has been proved for `scope`.
    ///
    /// Distinct from [`contains`](Self::contains): pixels can be absent for a
    /// hash this workspace is allowed to read — it has simply not been decoded
    /// yet — and that is work to schedule rather than a refusal.
    pub fn authorized(&self, scope: Scope, hash: &str) -> bool {
        self.authorized
            .get(hash)
            .is_some_and(|scopes| scopes.contains(&scope))
    }

    /// Whether a decoded image is on hand for this hash **and the current
    /// scope may see it**.
    ///
    /// The second half is not a detail: one object serves every workspace
    /// associated with it, so decoded pixels prove nothing about whether *this*
    /// workspace holds a record of them. Drawing them anyway would show a
    /// person content that is not in the workspace they are looking at, which
    /// is a correctness bug rather than a cache hit.
    pub fn contains(&self, hash: &str) -> bool {
        self.pixels.contains_key(hash) && self.authorized(self.scope, hash)
    }

    /// The decoded image for this hash, if it has been decoded here and the
    /// current scope is associated with it.
    pub fn get(&self, hash: &str) -> Option<&Arc<ImageBrush>> {
        self.authorized(self.scope, hash)
            .then(|| self.pixels.get(hash))
            .flatten()
    }

    /// The image for `hash` resampled so its larger axis is no more than
    /// `wanted` device pixels, cached by quantised size.
    ///
    /// **vello has no mipmaps** (see [`crate::brand`]): `Scene::draw_image`
    /// samples the source with a single tap, so a full-resolution picture drawn
    /// into a small display box is point-sampled and — worse — its whole texture
    /// is still sampled every frame. The master is downscaled here on the CPU,
    /// once per size, so both the draw and the vello render touch a small image.
    /// Sizes are quantised to powers of two, so a resize or zoom cannot thrash
    /// the cache; a master yields at most `log2(native)` entries.
    pub fn scaled(&self, hash: &str, wanted: u32) -> Option<Arc<ImageBrush>> {
        let full = self.get(hash)?.clone();
        let Some(size) = mip_size(&full, wanted) else {
            return Some(full);
        };
        let key = (hash.to_string(), size);
        {
            let mips = self.mips.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(mip) = mips.get(&key) {
                return Some(mip.clone());
            }
        }
        let brush = Arc::new(downscale(&full, size)?);
        let mut mips = self.mips.lock().unwrap_or_else(|p| p.into_inner());
        Some(mips.entry(key).or_insert_with(|| brush.clone()).clone())
    }

    /// **File a mip somebody else already paid for**, so the first
    /// [`scaled`](Self::scaled) at that size is a lookup.
    ///
    /// The lazy path above runs on whichever thread draws — the event loop —
    /// and a Lanczos pass over a full-screen screenshot is hundreds of
    /// milliseconds there. An image the shell has just decoded on a worker
    /// can have its display-sized mip built on the same worker; `size` must be
    /// the [`mip_size`] of the master for the size the view will ask for, or
    /// the entry is simply never hit. Ignored unless the master is here.
    pub fn insert_mip(&self, hash: &str, size: u32, mip: ImageBrush) {
        if !self.pixels.contains_key(hash) {
            return;
        }
        let mut mips = self.mips.lock().unwrap_or_else(|p| p.into_inner());
        mips.entry((hash.to_string(), size))
            .or_insert_with(|| Arc::new(mip));
    }

    /// Record the devices a parked hydration is waiting on for this hash.
    pub fn set_pending(&mut self, hash: &str, waiting_on: Vec<String>) {
        self.pending.insert(hash.to_string(), waiting_on);
    }

    /// Forget a pending wait — the hydration failed for good, so the
    /// placeholder must stop claiming it is waiting. Returns whether there was
    /// anything to forget.
    pub fn clear_pending(&mut self, hash: &str) -> bool {
        self.pending.remove(hash).is_some()
    }

    /// The devices a missing image is known to be waiting on, if any.
    pub fn pending(&self, hash: &str) -> Option<&[String]> {
        self.pending.get(hash).map(Vec::as_slice)
    }

    /// **Record why blob transfer is holding this hash** — the Store's own
    /// stable code, or `None` when the last attempt named no hold.
    ///
    /// Returns whether the answer moved, so a caller can skip a redraw it
    /// does not need: this arrives on every retry and the retry cadence
    /// re-reports the same hold every few seconds.
    pub fn set_hold(&mut self, hash: &str, hold: Option<&str>) -> bool {
        match hold {
            Some(hold) => {
                if self.holds.get(hash).map(String::as_str) == Some(hold) {
                    return false;
                }
                self.holds.insert(hash.to_string(), hold.to_string());
                true
            }
            None => self.holds.remove(hash).is_some(),
        }
    }

    /// Why blob transfer is holding this hash, if the Store has said.
    pub fn hold(&self, hash: &str) -> Option<&str> {
        self.holds.get(hash).map(String::as_str)
    }
}

/// The quantised larger axis [`ImageCache::scaled`] files a mip of `full`
/// under for a draw that wants `wanted` device pixels, or `None` when the
/// master itself is drawn (it is already that small, or nothing was asked).
///
/// Powers of two, so a resize or zoom cannot thrash the cache. Public so a
/// worker can build the same entry ahead of the draw ([`downscale`],
/// [`ImageCache::insert_mip`]) without a second copy of the rule.
pub fn mip_size(full: &ImageBrush, wanted: u32) -> Option<u32> {
    let native = full.image.width.max(full.image.height);
    if wanted == 0 || native <= wanted {
        return None;
    }
    let size = wanted.next_power_of_two().min(native);
    (size < native).then_some(size)
}

/// `full` resampled so its larger axis is `size` pixels, aspect kept.
///
/// **Expensive** — a Lanczos pass over every source pixel — which is why it is
/// its own function: [`ImageCache::scaled`] calls it lazily at draw time, and a
/// worker that has just decoded an image calls it before the draw ever asks.
pub fn downscale(full: &ImageBrush, size: u32) -> Option<ImageBrush> {
    let (iw, ih) = (full.image.width, full.image.height);
    let (tw, th) = if iw >= ih {
        (size, ((ih as u64 * size as u64) / iw as u64).max(1) as u32)
    } else {
        (((iw as u64 * size as u64) / ih as u64).max(1) as u32, size)
    };
    let rgba = image::RgbaImage::from_raw(iw, ih, full.image.data.data().to_vec())?;
    let small = image::imageops::resize(&rgba, tw, th, image::imageops::FilterType::Lanczos3);
    Some(ImageBrush::new(ImageData {
        data: Blob::new(Arc::new(small.into_raw())),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: tw,
        height: th,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mip built off the draw thread is the one the draw uses: the first
    /// `scaled` at that size hands back the filed entry rather than resampling
    /// the master again on the event loop.
    #[test]
    fn a_mip_filed_ahead_is_the_one_a_draw_gets() {
        let mut cache = ImageCache::new();
        let master = brush(64, 32);
        let size = mip_size(&master, 20).expect("a 64px master is mipped for 20px");
        assert_eq!(size, 32);
        let mip = downscale(&master, size).expect("downscaled");
        assert_eq!((mip.image.width, mip.image.height), (32, 16));
        let filed = mip.image.data.data().as_ptr();
        cache.insert(ONE, "h", master);
        cache.set_scope(ONE);
        cache.insert_mip("h", size, mip);
        let drawn = cache.scaled("h", 20).expect("drawn");
        assert_eq!(drawn.image.data.data().as_ptr(), filed);
        // A master already small enough is drawn as itself.
        assert_eq!(mip_size(&brush(16, 8), 20), None);
    }

    #[test]
    fn a_mip_for_pixels_that_are_not_here_is_not_filed() {
        let cache = ImageCache::new();
        let mip = brush(4, 4);
        cache.insert_mip("h", 4, mip);
        assert!(cache.mips.lock().unwrap().is_empty());
    }

    fn brush(w: u32, h: u32) -> ImageBrush {
        let rgba = image::RgbaImage::from_pixel(w, h, image::Rgba([128, 64, 32, 255]));
        ImageBrush::new(ImageData {
            data: Blob::new(Arc::new(rgba.into_raw())),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: w,
            height: h,
        })
    }

    // ------------------------------------------- association, not just pixels

    const ONE: Scope = 0x0000_0001_0000_4000_8000_0000_0000_0001;
    const TWO: Scope = 0x0000_0002_0000_4000_8000_0000_0000_0002;

    #[test]
    fn a_workspace_that_was_never_proved_sees_nothing() {
        // The file may well be on this disk — one object serves every
        // associated workspace — and the pixels may well be decoded in this
        // very process. Neither is permission, and this cache is the last
        // place that could get that wrong before a person sees it.
        let mut cache = ImageCache::new();
        cache.insert(ONE, "h", brush(8, 8));
        assert!(cache.authorized(ONE, "h"));
        assert!(!cache.authorized(TWO, "h"));

        cache.set_scope(TWO);
        assert!(
            !cache.contains("h"),
            "an unassociated workspace draws a placeholder"
        );
        assert!(cache.get("h").is_none());
        assert!(cache.scaled("h", 4).is_none());
    }

    #[test]
    fn switching_away_and_back_does_not_discard_identical_pixels() {
        // What the scope is for: a tab change used to empty the whole cache,
        // so every picture was read and decoded again on the way back — on the
        // thread that has to stay responsive.
        let mut cache = ImageCache::new();
        cache.insert(ONE, "h", brush(8, 8));
        cache.set_scope(ONE);
        let first = cache.get("h").expect("decoded").clone();

        cache.set_scope(TWO);
        assert!(!cache.contains("h"));
        cache.set_scope(ONE);
        assert!(
            Arc::ptr_eq(&first, cache.get("h").expect("the same pixels")),
            "the permission moved; the pixels never went anywhere"
        );
    }

    #[test]
    fn a_picture_in_two_workspaces_is_one_decode_and_two_permissions() {
        let mut cache = ImageCache::new();
        cache.insert(ONE, "h", brush(8, 8));
        cache.set_scope(ONE);
        let pixels = cache.get("h").expect("decoded").clone();
        // The second workspace proves its own read; the pixels are not
        // duplicated, because content addressing already made them one thing.
        cache.insert(TWO, "h", brush(8, 8));
        cache.set_scope(TWO);
        assert!(Arc::ptr_eq(
            &pixels,
            cache.get("h").expect("still one copy")
        ));
        cache.set_scope(ONE);
        assert!(cache.contains("h"), "and the first workspace kept it");
    }

    #[test]
    fn a_mip_is_the_same_permission_as_its_master() {
        let mut cache = ImageCache::new();
        cache.insert(ONE, "h", brush(400, 200));
        cache.set_scope(ONE);
        let mip = cache.scaled("h", 64).expect("a mip");
        assert_eq!((mip.image.width, mip.image.height), (64, 32));

        cache.set_scope(TWO);
        assert!(
            cache.scaled("h", 64).is_none(),
            "a smaller copy of a picture is still that picture"
        );
    }

    #[test]
    fn scaled_returns_the_master_when_the_wanted_size_exceeds_native() {
        let mut cache = ImageCache::new();
        cache.insert(ImageCache::UNSCOPED, "h", brush(64, 32));
        let out = cache.scaled("h", 128).unwrap();
        assert_eq!((out.image.width, out.image.height), (64, 32));
    }

    #[test]
    fn scaled_downsamples_preserving_aspect_ratio_and_caches() {
        let mut cache = ImageCache::new();
        cache.insert(ImageCache::UNSCOPED, "h", brush(400, 200));
        let small = cache.scaled("h", 64).unwrap();
        assert_eq!((small.image.width, small.image.height), (64, 32));
        let again = cache.scaled("h", 64).unwrap();
        assert!(
            Arc::ptr_eq(&small, &again),
            "same size reuses the cached mip"
        );
    }

    // ------------------------------------------------- what a size tier costs

    fn resident(brush: &ImageBrush) -> u64 {
        brush.image.width as u64 * brush.image.height as u64 * 4
    }

    /// Best of `reps` — the cheapest run is the one least disturbed by whatever
    /// else cargo is running in parallel.
    fn best_resample_ms(iw: u32, ih: u32, wanted: u32, reps: usize) -> f64 {
        let mut best = f64::MAX;
        for _ in 0..reps {
            // A fresh cache every time: `scaled` is only expensive on a miss.
            let mut cache = ImageCache::new();
            cache.insert(ImageCache::UNSCOPED, "h", brush(iw, ih));
            let at = std::time::Instant::now();
            let out = cache.scaled("h", wanted).expect("a mip");
            best = best.min(at.elapsed().as_secs_f64() * 1e3);
            assert!(out.image.width < iw, "the tier really was resampled");
        }
        best
    }

    /// **A 2× display asks for twice the size, which lands a tier higher.** What
    /// that costs is two different answers, and only one of them is the obvious
    /// one.
    ///
    /// *Resident bytes: exactly four times*, and this is arithmetic, so it is
    /// asserted exactly.
    ///
    /// *Resample time: about the same.* `image::imageops::resize` is separable
    /// and, downscaling, its filter support grows as the tier shrinks — so the
    /// horizontal pass costs about `source pixels` whatever the output is, and
    /// the tier only moves the smaller vertical pass. Measured on this tree
    /// (Apple M-series, `[profile.dev.package."*"] opt-level = 3`): a 3000×2000
    /// master resampled to the 1024 tier took 672 ms and to the 2048 tier
    /// 902 ms — 1.34×, against 4× the output pixels. Three runs gave 1.05,
    /// 1.16 and 1.34.
    ///
    /// **The ratio is the assertion, never a millisecond budget** — a budget
    /// passes on a fast machine and gets relaxed until it says nothing, which
    /// is what happened to `opening_a_tab_does_not_re_read_the_fonts` through a
    /// 125× regression (`guide/contributing/PRINCIPLES.md`, "measure ratios"). The bound
    /// below is loose enough for a busy machine and still fails if the resample
    /// ever becomes output-bound, which is the change that would make a retina
    /// display cost four times a 1× one.
    #[test]
    fn a_size_tier_costs_four_times_the_bytes_and_about_the_same_time() {
        let mut cache = ImageCache::new();
        cache.insert(ImageCache::UNSCOPED, "h", brush(1200, 800));
        let low = cache.scaled("h", 200).expect("the 256 tier");
        let high = cache.scaled("h", 400).expect("the 512 tier");
        assert_eq!((low.image.width, low.image.height), (256, 170));
        assert_eq!((high.image.width, high.image.height), (512, 341));
        let bytes = resident(&high) as f64 / resident(&low) as f64;
        assert!(
            (3.9..4.1).contains(&bytes),
            "a tier up is four times the resident pixels: {bytes:.2}x"
        );

        let low_ms = best_resample_ms(1200, 800, 200, 3);
        let high_ms = best_resample_ms(1200, 800, 400, 3);
        assert!(
            high_ms < low_ms * 2.5,
            "the resample follows the source, not the tier: \
             {low_ms:.1} ms -> {high_ms:.1} ms is {:.2}x for 4x the output",
            high_ms / low_ms
        );
    }

    /// **Nothing evicts a mip, and the measurement says that is survivable.**
    ///
    /// Tiers are powers of two below the master's larger axis, so every tier a
    /// session can produce for one image sums to at most 4/3 of its *largest*
    /// tier — and the largest tier is smaller than the master, which lives in
    /// `map` and is not evicted either. So mips can never be the larger half of
    /// what this cache holds, and evicting them without evicting masters could
    /// not reclaim most of it.
    ///
    /// Worked: a 3000×2000 master (22.9 MB resident) dragged between a 1× and a
    /// 2× display and resized across a tier produces the 512, 1024 and 2048
    /// tiers — 14.0 MB, **0.61× the master**. An LRU here is therefore a
    /// decision about `map`, not about `mips`; it is not taken in this test's
    /// name. Recorded so it is not re-argued from first principles.
    #[test]
    fn every_mip_of_one_image_together_stays_under_its_master() {
        let cache = {
            let mut cache = ImageCache::new();
            cache.insert(ImageCache::UNSCOPED, "h", brush(1200, 800));
            cache
        };
        // Every tier this master can yield, which is more than a session would.
        let mut wanted = 1u32;
        while wanted < 1200 {
            let _ = cache.scaled("h", wanted);
            wanted *= 2;
        }
        let master = resident(cache.get("h").expect("the master"));
        let mips: u64 = cache
            .mips
            .lock()
            .expect("no poisoned mips")
            .values()
            .map(|b| resident(b))
            .sum();
        assert!(
            mips < master,
            "every tier at once is still under the master it came from: \
             {mips} vs {master}"
        );
        assert!(
            mips as f64 > master as f64 * 0.2,
            "the bound is worth stating: the mips are a real fraction, \
             {mips} vs {master}"
        );
    }

    /// The one that is not obvious: **on a 2× display a mid-sized picture skips
    /// the mip entirely.** `wanted` doubles, `next_power_of_two` lands at or
    /// above the native size, and `scaled` hands back the master — so vello
    /// point-samples the full-resolution texture instead. Less CPU than the 1×
    /// path, not more; whether it *looks* right on a retina screen is the half
    /// of M3 that needs a person.
    #[test]
    fn a_retina_ask_above_the_native_size_returns_the_master_unresampled() {
        let mut cache = ImageCache::new();
        cache.insert(ImageCache::UNSCOPED, "h", brush(1500, 1000));
        let one_x = cache.scaled("h", 800).expect("the 1024 tier");
        assert_eq!(one_x.image.width, 1024, "1x builds a mip");
        let two_x = cache.scaled("h", 1600).expect("the master");
        assert!(
            Arc::ptr_eq(&two_x, cache.get("h").expect("the master")),
            "2x wants 2048, which is above native 1500, so no mip is built"
        );
    }
}
