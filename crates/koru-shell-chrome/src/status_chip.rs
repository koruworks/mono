// SPDX-License-Identifier: MPL-2.0

//! A **status chip** in the caption, and the popover that says what is behind it.
//!
//! One chip shape, drawn from strings the host hands over. Koru's only use of it
//! today is the store being away — the one thing on screen that says a person's
//! work is not reaching storage *while it is happening*, rather than ten seconds
//! later when a dialog arrives. It has to be visible without being alarming: a
//! store being restarted shows this for a second or two as a matter of course,
//! and a chip that shouted would be a chip somebody learns to ignore.
//!
//! **Here rather than in a view, because two windows draw a caption.** The
//! container draws one, and a popped-out document draws its own — and for a
//! while only the container had a chip, so the window somebody had put a
//! document into was the one window that never mentioned the store being away.
//! A second copy would have drifted the same way.
//!
//! **This module names no workspace, no session and no state.** It is given a
//! rect and [`Text`] — three strings that are already in the reader's language.
//! It used to hold the English word *Offline* itself, which made a crate
//! chartered to hold "only rects, indices, slots and drawing primitives" the
//! owner of a piece of product copy in one language
//! ([ADR-262](../../../docs/decisions/README.md)). What counts as trouble, how
//! it is worded, and what language it is worded in belong to whoever knows what
//! a store is.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

/// Everything the chip says, in the reader's language, supplied by the host.
///
/// A struct rather than three `&str` parameters so the contract is one type a
/// caller cannot half-satisfy: a chip drawn from anything but host copy has to
/// invent a `Text`, and there is nowhere in this crate to invent one from.
pub struct Text<'a> {
    /// The word on the chip itself. One word if the language has one — see
    /// [`CHIP_W`] for the room it is given.
    pub label: &'a str,
    /// The popover's first line: what has happened.
    pub title: &'a str,
    /// The popover's second line: what is at stake, and any diagnostic detail.
    pub detail: &'a str,
}

/// How wide the chip is, before it is clamped to the band it sits in.
///
/// **One word, one width.** How much is at stake is the popover's to say —
/// clicking is how somebody asks. A count sitting permanently in the caption
/// reads as an alarm, and this is a state a store restart puts a window in for
/// a second at a time.
///
/// **Fixed rather than measured**, so a caller can reserve the room without a
/// `TextEngine` in hand — the tab strip's width is asked for from hit-testing,
/// where there is no drawing context and no reason to build one. The cost of
/// that is paid in [`draw`], which clips a label too long for the room rather
/// than letting it spill into the tabs.
pub const CHIP_W: f32 = 68.0;

/// The chip's rect at the right-hand end of a band.
///
/// The caller decides which band: the container hands the tab strip, so the
/// tabs make room for it, and a document window hands the space beside its own
/// buttons.
pub fn chip(band: Rect) -> Rect {
    let w = CHIP_W.min(band.w);
    let h = theme::CAPTION_H * 0.55;
    Rect::new(band.x + band.w - w, band.y + (band.h - h) * 0.5, w, h)
}

/// Draw the chip, and its popover when it is open.
///
/// Every string comes in through `text`, already translated. Only the caller
/// knows what is unconfirmed, whether it is safe, and which of the eight
/// languages to say it in — this module would have to learn about sessions and
/// locales to answer any of the three, and then every window that draws a
/// caption would link both.
pub fn draw(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    chip: Rect,
    text: Text<'_>,
    open: bool,
) {
    let p = theme::p();
    // **The one chip component, inverted** — `bubble_bg`, the tooltip's
    // recipe, and no new colour. The inverted surface reads as an instrument
    // rather than as part of the page — which is exactly what this is — and
    // `DESIGN.md` records a standing preference for not adding `Palette`
    // fields. The label is centred in the fixed [`CHIP_W`] and **clipped to
    // the chip** by the component: the word is the host's, and *Sin
    // conexión* is not the word the width was measured for.
    koru_ui::LabelChip::new(text.label)
        .size(koru_ui::label_chip::Size::Regular)
        .tone(koru_ui::label_chip::Tone::Inverted)
        .draw(
            scene,
            root,
            engine,
            chip,
            koru_ui::label_chip::State::default(),
        );

    if !open {
        return;
    }

    let pad = 12.0;
    let gap = 4.0;
    // Diagnostics can include an endpoint or protocol error. Keep that useful
    // text in the window instead of growing a one-line popover off-screen.
    let available = (chip.x + chip.w - 8.0 - pad * 2.0).max(180.0);
    let content_w = available.min(620.0);
    let title_layout = engine.simple_layout_wrapped(text.title, theme::role::row(), content_w);
    let detail_layout = engine.simple_layout_wrapped(text.detail, theme::role::hint(), content_w);
    let w = title_layout.width().max(detail_layout.width()) + pad * 2.0;
    let h = title_layout.height() + gap + detail_layout.height() + pad * 2.0;
    // Right-aligned under the chip, and kept inside the window: the chip lives
    // at the right-hand end of its band, so a left-aligned sheet would hang off
    // the edge.
    let x = (chip.x + chip.w - w).max(4.0);
    let y = chip.y + chip.h + 6.0;

    scene::floating_panel(scene, root, x, y, w, h, &scene::Panel::popover(10.0));
    scene::draw_layout_first_line(scene, root, &title_layout, x + pad, y + pad, Some(p.text));
    scene::draw_layout_first_line(
        scene,
        root,
        &detail_layout,
        x + pad,
        y + pad + title_layout.height() + gap,
        Some(p.text_dim),
    );
}
