// SPDX-License-Identifier: MPL-2.0

//! **The picker's chrome is the agent-configuration control's**
//! (`koru_agent_config_ui::picker::elements`, Bots P1.8): its element tree,
//! names, keys, state recipes, tokens and the read-back of its geometry
//! moved there whole, and this module names them for the chat.
//!
//! What stays is the chat's own: **the element layout the composer's
//! panels share** (D64, D105) — one engine and text cache per UI thread, and
//! one way to key a row by its id — which the approval-mode list, the
//! workspace-access panel, the questions, the microphone, the skills and the
//! context ring are laid out on. The control keeps an identical pair of its
//! own, so it needs nothing of this crate; pushing both down into
//! `koru_draw::el` is the cleanup owed (P1.8.PICKER.notes.md §3, R6).

use std::cell::RefCell;

use koru_draw::el::{El, Frame, FrameCx, TextCache};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;

// The control's tree, by the names the suite reads it by.
#[cfg(test)]
pub(crate) use koru_agent_config_ui::picker::elements::*;

/// FNV-1a over `text`, widened: the same id gives the same key in every
/// frame and every run.
pub(crate) fn stable_id(text: &str) -> u128 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    u128::from(hash)
}

thread_local! {
    /// The engine and cache the composer's panels lay out on: their words
    /// are shaped here, only for the rows in view, and kept across frames.
    /// One per UI thread, as the switcher's input layouts have
    /// (`input_engine::with` in the app).
    static LAYOUT: RefCell<Option<(TextEngine, TextCache)>> = const { RefCell::new(None) };
}

/// `tree` laid out in `area` under `cx`.
pub(crate) fn layout(tree: &El, area: Rect, cx: FrameCx) -> Frame {
    LAYOUT.with_borrow_mut(|slot| {
        let (engine, cache) = slot.get_or_insert_with(|| (TextEngine::new(), TextCache::default()));
        tree.layout(engine, cache, area, cx)
    })
}
