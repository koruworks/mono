// SPDX-License-Identifier: MPL-2.0

//! The app shell's furniture: the caption row and its tabs, the banner slots,
//! and the arithmetic that cuts a window into regions.
//!
//! Everything here is true of *any* Koru app, whatever its tabs happen to show.
//! It is spelled in `koru-draw` terms and **names no profile, workspace,
//! document or session** — check `Cargo.toml` rather than believing this
//! sentence. It cannot see `koru-core`, `koru-editor` or any host, which is
//! what stops the chrome growing opinions about what its slots mean
//! ([ADR-262](../../../docs/decisions/README.md)). The workspace-shaped panes
//! that used to live beside these modules — the explore tree, the context
//! column, the outcome vocabulary they report in — are their consumer's now, in
//! `apps/koru/src/panes/`.
//!
//! ## Three rules this crate exists to keep
//!
//! **Geometry lives in one function, shared by drawing and hit-testing.** That is
//! `DESIGN.md`'s first rule for chrome, and the reason it is a rule is that a
//! second copy drifts: a button ends up clickable somewhere other than where it
//! is drawn. [`regions::solve`], [`caption::tab_strip`], [`tabs::layout`] and
//! [`banner::slots`] are each the only arithmetic of their kind, and each has a
//! test that hit-tests every element back to itself.
//!
//! **Chrome reports what happened; it never reaches into the host.** A hit here
//! is a slot or an index — [`regions::Hit`], [`banner::Hit`] — and what pressing
//! it *means* is the host's business. Chrome knows where things are, not what
//! they mean.
//!
//! **Every word and every policy comes in from the host.** Dependency
//! neutrality is necessary and not sufficient: a crate can name no workspace
//! and still hold an English noun or an opinion about a branch name, and this
//! one held both. [`status_chip::draw`] takes a [`status_chip::Text`] of
//! already-localized strings rather than the word *Offline*, and
//! [`branch::draw`] draws whatever name it is handed rather than deciding that
//! `dev` is unremarkable — that gate is `apps/koru/src/build_stamp.rs`'s
//! ([ADR-262](../../../docs/decisions/README.md)). Neither is a `koru-i18n`
//! dependency here: the crate receives `&str`, and the eight columns are the
//! host's to read.
//!
//! ## Hit geometry is pure arithmetic; measure happens at draw
//!
//! Functions that produce rects for hit-testing take plain numbers (including
//! cached tab widths), never a `TextEngine`. Tab preferred widths are measured
//! when a font context is available — draw, or a font/density change — and
//! cached by the container, so multiple UI families size tabs correctly without
//! putting the engine on the hit path. Geometry tests pass synthetic widths and
//! need no `assets/fonts/*.ttf`.
//!
//! Nothing here reads [`koru_draw::theme::p`] — only the metric constants,
//! which are not behind the theme's atomic. So no test here needs the
//! process-wide *palette* mutex the editor's do. Do not add one. This crate
//! reads no mutable process state but the palette; anything that starts to must
//! say so here.

pub mod banner;
pub mod branch;
pub mod caption;
pub mod lane;
pub mod regions;
pub mod status_chip;
pub mod status_mark;
pub mod tabs;
pub mod title_icon;

pub use title_icon::{PAGE_EMOJI_SIZE, PAGE_ICON_SLOT, draw_page_emoji, split_leading_emoji};

#[cfg(test)]
mod tests;
