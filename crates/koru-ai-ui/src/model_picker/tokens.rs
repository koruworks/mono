// SPDX-License-Identifier: MPL-2.0

//! **The picker's own tokens** (QUESTIONS E4, M7): every size its element
//! tree uses that no shared token names — a tagged metric or a tagged
//! spacing alias, so the inspector shows the name beside the value. The
//! tree ([`super::elements`]) and its paint ([`super::paint`]) are on the
//! source scan's migrated list (`koru_draw::theme::ui_scan`) and name only
//! these; this file is where they are defined, as `koru_draw::theme`'s
//! tokens module is for the shared ones.
//!
//! Two are exceptions, raw lengths with a reason: the panel's hairline is
//! the shell's border width (`BORDER_W`), not a spacing step, and the
//! rail's inset inside it is the 8 the marks always sat from the panel's
//! edge, less that hairline.

use koru_draw::theme::{Metric, Space, Tagged};

use super::{RAIL_BUTTON, RAIL_W, ROW_H, ROW_MARK, SEARCH_H, STAR};

/// The rail's width ([`RAIL_W`]).
pub(crate) const RAIL_WIDTH: Metric = Metric::new("PICKER_RAIL_W", RAIL_W);

/// A rail mark's square, and the divider's width under it.
pub(crate) const RAIL_MARK: Metric = Metric::new("PICKER_RAIL_MARK", RAIL_BUTTON);

/// The search row, ↻ at its end ([`SEARCH_H`]).
pub(crate) const SEARCH_ROW_H: Metric = Metric::new("PICKER_SEARCH_H", SEARCH_H);

/// Air between the rail and the content: 8 px, the composer toolbar's
/// control gap.
pub(crate) const CONTENT_GAP: Tagged<Space> = Tagged::new("RAIL_CONTENT_GAP", Space::S2);

/// The content's inset from the panel's top, end and foot ([`super::PAD`]).
pub(crate) const PANEL_INSET: Tagged<Space> = Tagged::new("PICKER_PAD", Space::S1);

/// The panel's hairline (`BORDER_W`): the rail's ground keeps inside it on
/// its start, top and foot, so the shell's border stays whole.
pub(crate) const HAIRLINE: Tagged<Space> = Tagged::new(
    "PICKER_PANEL_HAIRLINE",
    Space::Px(koru_draw::theme::BORDER_W as f32),
);

/// The rail's inset above its first mark and below its last, inside the
/// hairline: with it, the 8 units (`Space::S2`) the marks always sat from
/// the panel's edge.
pub(crate) const GROUND_INSET: Tagged<Space> = Tagged::new(
    "PICKER_RAIL_INSET",
    Space::Px(8.0 - koru_draw::theme::BORDER_W as f32),
);

/// The ground's start corners: the panel's radius inside its hairline.
pub(crate) const GROUND_RADIUS: Metric = Metric::new(
    "PICKER_RAIL_GROUND_RADIUS",
    10.0 - koru_draw::theme::BORDER_W as f32,
);

/// The room after each mark ([`super::RAIL_GAP`]).
pub(crate) const MARK_GAP: Tagged<Space> = Tagged::new("PICKER_RAIL_GAP", Space::S1_5);

/// A rail divider's inset on every side of its hairline: with the hairline,
/// the divider's room ([`super::RAIL_DIVIDER`]).
pub(crate) const DIVIDER_INSET: Tagged<Space> = Tagged::new("PICKER_RAIL_DIVIDER_INSET", Space::S1);

/// The search box's inset in its row.
pub(crate) const SEARCH_INSET: Tagged<Space> = Tagged::new("PICKER_SEARCH_INSET", Space::S1);

/// Between the search field and ↻, and after ↻.
pub(crate) const REFRESH_GAP: Tagged<Space> = Tagged::new("PICKER_REFRESH_GAP", Space::S0_5);

/// A row of the list ([`ROW_H`]): a heading, a note or a model, one line.
/// The list scrolls by it.
pub(crate) const ROW_HEIGHT: Metric = Metric::new("PICKER_ROW_H", ROW_H);

/// A favorite's agent mark, or a search heading's, at its row's start
/// ([`ROW_MARK`]).
pub(crate) const ROW_MARK_SIZE: Metric = Metric::new("PICKER_ROW_MARK", ROW_MARK);

/// The ☆'s square ([`STAR`]).
pub(crate) const STAR_BOX: Metric = Metric::new("PICKER_STAR", STAR);

/// The room before the ✓: 9, as it always had.
pub(crate) const CHECK_GAP: Metric = Metric::new("PICKER_CHECK_GAP", 9.0);

/// A row's content inset at its start, and at its end without a ☆.
pub(crate) const ROW_INSET: Tagged<Space> = Tagged::new("PICKER_ROW_INSET", Space::S2);

/// The ☆'s inset from the row's end, clear of the scrollbar's lane.
pub(crate) const STAR_INSET: Tagged<Space> = Tagged::new("PICKER_STAR_INSET", Space::S3);

/// Between a row's mark and its name.
pub(crate) const MARK_ROOM: Tagged<Space> = Tagged::new("PICKER_ROW_MARK_GAP", Space::S2);

/// Before an older model's tag.
pub(crate) const TAG_GAP: Tagged<Space> = Tagged::new("PICKER_TAG_GAP", Space::S1_5);

/// Before the *Legacy models* row's caret.
pub(crate) const CARET_GAP: Tagged<Space> = Tagged::new("PICKER_CARET_GAP", Space::S2);
