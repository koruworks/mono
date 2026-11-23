// SPDX-License-Identifier: MPL-2.0

//! **A preview is owned, and only its owner ends it.**
//!
//! Settings shows a choice before it is made: the lit font in a font list, the
//! lit size in the document size list, the lit icon style. Each is a stand-in
//! for one value that drawing reads, and each has the same lifetime: it begins
//! when a menu lights a row, moves as the light moves, and ends when that menu
//! closes, by a choice or not.
//!
//! The guard is what keeps an old owner from ending a newer preview. Every
//! preview carries a generation number, and both moving and ending it are a
//! compare-and-swap on that number, so a menu that is dropped after another
//! one has started previewing the same slot changes nothing. This began as
//! the icon style's `IconPreview`; fonts and the document size now share it,
//! so there is one rule for every list that previews.

use std::sync::atomic::{AtomicU64, Ordering};

use super::FontSlot;

/// What a preview stands in for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PreviewSlot {
    /// A font role's face, as [`font_preview_value`](super::font_preview_value)
    /// encodes it.
    Font(FontSlot),
    /// The document body size, in px.
    DocTextSize,
    /// The icon pack, as an index into its `ALL`.
    IconPack,
    HeadingWeight,
    TextAntialiasing(bool),
    TextOffset(bool),
    TextGamma(bool),
    TextCutoff(bool),
}

impl PreviewSlot {
    const COUNT: usize = 16;

    fn index(self) -> usize {
        match self {
            Self::Font(slot) => slot as usize,
            Self::DocTextSize => 5,
            Self::IconPack => 6,
            Self::HeadingWeight => 7,
            Self::TextAntialiasing(dark) => 8 + usize::from(dark) * 4,
            Self::TextOffset(dark) => 9 + usize::from(dark) * 4,
            Self::TextGamma(dark) => 10 + usize::from(dark) * 4,
            Self::TextCutoff(dark) => 11 + usize::from(dark) * 4,
        }
    }
}

/// `0` is "no preview"; otherwise `generation << 33 | 1 << 32 | value`.
/// The value holds either a discrete choice or exact f32 bits.
static SLOTS: [AtomicU64; PreviewSlot::COUNT] = [const { AtomicU64::new(0) }; PreviewSlot::COUNT];
static NEXT: AtomicU64 = AtomicU64::new(1);

const PRESENT: u64 = 1 << 32;
const VALUE: u64 = 0xffff_ffff;

/// One menu's preview of one slot. Dropping it ends the preview, unless a
/// newer one has taken the slot since.
#[derive(Debug)]
pub struct Preview {
    slot: PreviewSlot,
    token: u64,
}

impl Preview {
    /// Begin previewing `value` in `slot`, replacing whatever previewed it.
    pub fn new(slot: PreviewSlot, value: u32) -> Self {
        let token = (NEXT.fetch_add(1, Ordering::Relaxed) << 33) | PRESENT | u64::from(value);
        SLOTS[slot.index()].store(token, Ordering::Relaxed);
        Self { slot, token }
    }

    /// Move this preview to `value`. Does nothing once a newer preview owns
    /// the slot.
    pub fn set(&mut self, value: u32) {
        let next = (self.token & !VALUE) | u64::from(value);
        if SLOTS[self.slot.index()]
            .compare_exchange(self.token, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            self.token = next;
        }
    }

    pub fn slot(&self) -> PreviewSlot {
        self.slot
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        let _ = SLOTS[self.slot.index()].compare_exchange(
            self.token,
            0,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
}

/// The value previewed in `slot`, if anything previews it.
pub fn previewed(slot: PreviewSlot) -> Option<u32> {
    let token = SLOTS[slot.index()].load(Ordering::Relaxed);
    (token & PRESENT != 0).then_some((token & VALUE) as u32)
}

/// End any preview of `slot`: a choice replaces what was being shown.
pub fn clear(slot: PreviewSlot) {
    SLOTS[slot.index()].store(0, Ordering::Relaxed);
}

/// Tests in this crate that move the document size slot or its global run one
/// at a time: the slots are process globals.
#[cfg(test)]
pub(crate) static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_distinct_and_float_previews_keep_every_bit() {
        let _serial = SERIAL.lock().unwrap_or_else(|held| held.into_inner());
        let slots = [
            PreviewSlot::Font(FontSlot::Ui),
            PreviewSlot::Font(FontSlot::Body),
            PreviewSlot::Font(FontSlot::Headings),
            PreviewSlot::Font(FontSlot::Code),
            PreviewSlot::Font(FontSlot::Chat),
            PreviewSlot::DocTextSize,
            PreviewSlot::IconPack,
            PreviewSlot::HeadingWeight,
            PreviewSlot::TextAntialiasing(false),
            PreviewSlot::TextOffset(false),
            PreviewSlot::TextGamma(false),
            PreviewSlot::TextCutoff(false),
            PreviewSlot::TextAntialiasing(true),
            PreviewSlot::TextOffset(true),
            PreviewSlot::TextGamma(true),
            PreviewSlot::TextCutoff(true),
        ];
        let mut indices = slots.map(PreviewSlot::index).to_vec();
        indices.sort_unstable();
        indices.dedup();
        assert_eq!(
            indices.len(),
            PreviewSlot::COUNT,
            "chat font and document size must not alias"
        );
        let bits = 1.234567_f32.to_bits();
        let old = Preview::new(PreviewSlot::TextGamma(false), bits);
        assert_eq!(previewed(PreviewSlot::TextGamma(false)), Some(bits));
        let new = Preview::new(PreviewSlot::TextGamma(false), 1.75_f32.to_bits());
        drop(old);
        assert_eq!(
            previewed(PreviewSlot::TextGamma(false)),
            Some(1.75_f32.to_bits())
        );
        drop(new);
        assert_eq!(previewed(PreviewSlot::TextGamma(false)), None);
    }

    /// The document size slot is used here because no other test in this
    /// crate previews it.
    #[test]
    fn only_the_newest_owner_moves_or_ends_a_preview() {
        let _serial = SERIAL.lock().unwrap_or_else(|held| held.into_inner());
        let slot = PreviewSlot::DocTextSize;
        let mut old = Preview::new(slot, 18);
        assert_eq!(previewed(slot), Some(18));
        let new = Preview::new(slot, 20);
        old.set(22);
        assert_eq!(previewed(slot), Some(20), "a stale owner cannot move it");
        drop(old);
        assert_eq!(previewed(slot), Some(20), "nor end it");
        drop(new);
        assert_eq!(previewed(slot), None, "its owner ends it");
        let preview = Preview::new(slot, 15);
        clear(slot);
        assert_eq!(previewed(slot), None, "a choice ends any preview");
        drop(preview);
        assert_eq!(previewed(slot), None);
    }
}
