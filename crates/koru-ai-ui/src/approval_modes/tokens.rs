// SPDX-License-Identifier: MPL-2.0

//! **The approval-mode list's own tokens** (koru-ai D7, D105): every size
//! its element tree ([`super::elements`]) uses that no shared token names —
//! a tagged metric or a tagged spacing alias, so the inspector shows the name
//! beside the value. The tree is on the source scan's migrated list
//! (`koru_draw::theme::ui_scan`) and names only these; this file is where
//! they are defined, as the model picker's `tokens.rs` is for its own.
//!
//! One is a raw length with a reason: the 5 above a row's words, which no
//! spacing step names, and which the rows always had.

use koru_draw::theme::{Metric, Space, Tagged};

use super::MODE_ROW_H;

/// The air around the rows inside the panel: 4.
pub(crate) const PANEL_INSET: Tagged<Space> = Tagged::new("APPROVAL_PANEL_PAD", Space::S1);

/// One row of the list ([`MODE_ROW_H`]): an icon, a title and a line under
/// it.
pub(crate) const ROW_HEIGHT: Metric = Metric::new("APPROVAL_ROW_H", MODE_ROW_H);

/// A row's content inset at its start, and at its end after the ✓'s lane:
/// 10.
pub(crate) const ROW_INSET: Tagged<Space> = Tagged::new("APPROVAL_ROW_INSET", Space::S2_5);

/// The mode's icon below the row's top: 8.
pub(crate) const ICON_TOP: Tagged<Space> = Tagged::new("APPROVAL_ICON_TOP", Space::S2);

/// Between the icon and the words: with the inset and the icon, the 34 the
/// words always started at.
pub(crate) const WORDS_INSET: Tagged<Space> = Tagged::new("APPROVAL_WORDS_INSET", Space::S3);

/// The title below the row's top: 5, which no step names.
pub(crate) const WORDS_TOP: Tagged<Space> = Tagged::new("APPROVAL_WORDS_TOP", Space::Px(5.0));

/// Between the title and its line: 2.
pub(crate) const WORDS_GAP: Tagged<Space> = Tagged::new("APPROVAL_WORDS_GAP", Space::S0_5);
