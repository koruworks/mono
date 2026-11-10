// SPDX-License-Identifier: MPL-2.0

//! **The skill list's own tokens** (koru-ai D121): every size its element
//! tree ([`super::elements`]) uses that no shared token names — a tagged
//! metric or a tagged spacing alias, so the inspector shows the name beside
//! the value. The tree is on the source scan's migrated list
//! (`koru_draw::theme::ui_scan`) and names only these; this file is where
//! they are defined, as the approval-mode list's `tokens.rs` is for its own.

use koru_draw::theme::{Metric, Space, Tagged};

use crate::approval_modes::MODE_ROW_H;

/// The air around the rows inside the panel: 4, as the composer's other
/// panels have it.
pub(crate) const PANEL_INSET: Tagged<Space> = Tagged::new("SKILL_PANEL_PAD", Space::S1);

/// The panel at its widest: room for a skill's name and most of its line
/// before they are elided.
pub(crate) const PANEL_WIDTH: Metric = Metric::new("SKILL_PANEL_W", 420.0);

/// One row — a skill's name and its line under it — the approval-mode
/// list's row height ([`MODE_ROW_H`]), which has the same two lines.
pub(crate) const ROW_HEIGHT: Metric = Metric::new("SKILL_ROW_H", MODE_ROW_H);

/// A row's content inset at its start and its end: 10.
pub(crate) const ROW_INSET: Tagged<Space> = Tagged::new("SKILL_ROW_INSET", Space::S2_5);

/// Between a skill's name and its line: 2.
pub(crate) const WORDS_GAP: Tagged<Space> = Tagged::new("SKILL_WORDS_GAP", Space::S0_5);

/// Placeholder bars while an agent list is arriving.
pub(crate) const LOADING_BAR_HEIGHT: Metric = Metric::new("SKILL_LOADING_BAR_H", 9.0);
pub(crate) const LOADING_TITLE_WIDE: Metric = Metric::new("SKILL_LOADING_TITLE_WIDE", 142.0);
pub(crate) const LOADING_TITLE_SHORT: Metric = Metric::new("SKILL_LOADING_TITLE_SHORT", 94.0);
pub(crate) const LOADING_LINE_WIDTH: Metric = Metric::new("SKILL_LOADING_LINE_W", 236.0);
