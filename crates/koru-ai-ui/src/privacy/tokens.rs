// SPDX-License-Identifier: MPL-2.0

//! **The workspace-access panel's own tokens** (koru-ai D106): every size
//! its element tree ([`super::elements`]) uses that no shared token names —
//! a tagged metric or a tagged spacing alias, so the inspector shows the
//! name beside the value. The tree is on the source scan's migrated list
//! (`koru_draw::theme::ui_scan`) and names only these; this file is where
//! they are defined, as the model picker's `tokens.rs` is for its own.
//!
//! Two are raw lengths with a reason, each what the panel always had and no
//! spacing step names: the heading's 9 below the panel's top, and the empty
//! note's 7 below the list's. A row's plate keeps its legacy radius, 5, as a
//! tagged metric.

use koru_draw::theme::{Metric, Space, Tagged};

use super::{WORKSPACE_LIST_Y, WORKSPACE_SEARCH_H};
use crate::TOOL_ACCESS_WORKSPACE_ROW_H;

/// The panel's inset at its sides and foot, round the list: 4.
pub(crate) const PANEL_INSET: Tagged<Space> = Tagged::new("ACCESS_PANEL_PAD", Space::S1);

/// The heading's band: the panel's top to the filter field, or to the list
/// without one ([`WORKSPACE_LIST_Y`]).
pub(crate) const HEADING_HEIGHT: Metric = Metric::new("ACCESS_HEADING_H", WORKSPACE_LIST_Y);

/// The heading's words below the panel's top: 9, which no step names.
pub(crate) const HEADING_TOP: Tagged<Space> = Tagged::new("ACCESS_HEADING_TOP", Space::Px(9.0));

/// The heading's start inside the panel's inset: with it, the 10 the
/// heading always sat from the panel's edge.
pub(crate) const HEADING_INSET: Tagged<Space> = Tagged::new("ACCESS_HEADING_INSET", Space::S1_5);

/// Between the heading and the count after it.
pub(crate) const COUNT_GAP: Tagged<Space> = Tagged::new("ACCESS_COUNT_GAP", Space::S3);

/// The filter field ([`WORKSPACE_SEARCH_H`]), shown over five workspaces.
pub(crate) const FILTER_HEIGHT: Metric = Metric::new("ACCESS_FILTER_H", WORKSPACE_SEARCH_H);

/// The filter field's inset inside the panel's: with it, 8 from the panel's
/// edge.
pub(crate) const FILTER_INSET: Tagged<Space> = Tagged::new("ACCESS_FILTER_INSET", Space::S1);

/// Between the filter field and the list.
pub(crate) const FILTER_GAP: Tagged<Space> = Tagged::new("ACCESS_FILTER_GAP", Space::S2);

/// A workspace's row ([`TOOL_ACCESS_WORKSPACE_ROW_H`]). The list scrolls by
/// it.
pub(crate) const ROW_HEIGHT: Metric = Metric::new("ACCESS_ROW_H", TOOL_ACCESS_WORKSPACE_ROW_H);

/// A row's content inset at its start and its end.
pub(crate) const ROW_INSET: Tagged<Space> = Tagged::new("ACCESS_ROW_INSET", Space::S2);

/// A row's plate's corners: 5, the legacy radius no step names.
pub(crate) const ROW_RADIUS: Metric = Metric::new("ACCESS_ROW_RADIUS", 5.0);

/// The ✓'s lane at a row's end: with the row's inset, the 30 a name always
/// stopped short of the row's end.
pub(crate) const CHECK_LANE_W: Metric = Metric::new("ACCESS_CHECK_LANE_W", 22.0);

/// The note an empty list shows, inside the panel's inset: 6 in.
pub(crate) const EMPTY_INSET: Tagged<Space> = Tagged::new("ACCESS_EMPTY_INSET", Space::S1_5);

/// The empty note below the list's top: 7.
pub(crate) const EMPTY_TOP: Tagged<Space> = Tagged::new("ACCESS_EMPTY_TOP", Space::Px(7.0));
