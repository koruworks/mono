// SPDX-License-Identifier: MPL-2.0

//! **The microphone panel's own tokens** (koru-ai D103): every size its
//! element tree ([`super::elements`]) uses that no shared token names — a
//! tagged metric or a tagged spacing alias, so the inspector shows the name
//! beside the value. The tree is on the source scan's migrated list
//! (`koru_draw::theme::ui_scan`) and names only these; this file is where
//! they are defined, as the approval-mode list's `tokens.rs` is for its own.

use koru_draw::theme::{Metric, Space, Tagged};

use crate::TOOL_ACCESS_WORKSPACE_ROW_H;

/// The air around the rows inside the panel: 4, as the composer's other
/// panels have it.
pub(crate) const PANEL_INSET: Tagged<Space> = Tagged::new("MIC_PANEL_PAD", Space::S1);

/// The panel at its widest: room for a device's name before it is elided.
pub(crate) const PANEL_WIDTH: Metric = Metric::new("MIC_PANEL_W", 300.0);

/// One row — a device, or *Hold to record* — the workspace-access panel's
/// row height ([`TOOL_ACCESS_WORKSPACE_ROW_H`]).
pub(crate) const ROW_HEIGHT: Metric = Metric::new("MIC_ROW_H", TOOL_ACCESS_WORKSPACE_ROW_H);

/// A row's content inset at its start and its end.
pub(crate) const ROW_INSET: Tagged<Space> = Tagged::new("MIC_ROW_INSET", Space::S2);

/// Between a row's words and the ✓ or the switch after them.
pub(crate) const ROW_GAP: Tagged<Space> = Tagged::new("MIC_ROW_GAP", Space::S2);

/// The space above and below the divider before *Hold to record*.
pub(crate) const DIVIDER_GAP: Tagged<Space> = Tagged::new("MIC_DIVIDER_GAP", Space::S1);

/// The input level bar's thickness: a thin rule with round ends, read beside
/// the microphone glyph rather than as a control.
pub(crate) const LEVEL_BAR_HEIGHT: Metric = Metric::new("MIC_LEVEL_BAR_H", 4.0);
