// SPDX-License-Identifier: MPL-2.0

//! **The Lab window's own tokens**: every size its element tree
//! ([`super::shell`]) uses that no shared token already names. Each is a
//! tagged metric or a tagged spacing step, so the inspector shows the name
//! beside the value and a change here is a change everywhere it is used.
//!
//! The controls on show keep the heights of the components they are: the
//! dialog button's, the compact action's, the segmented control's.

use koru_draw::theme::{self, Metric, Space, Tagged};

/// The caption's height: the one every Koru window's caption has.
pub(crate) const CAPTION_HEIGHT: Metric = Metric::new("CAPTION_H", theme::CAPTION_H);

/// The room the three window controls take at the caption's end, kept clear
/// of the title.
pub(crate) const CAPTION_CONTROLS: Metric =
    Metric::new("CAPTION_CONTROLS_W", theme::CAPTION_BTN * 3.0);

/// The title's inset from the window's start edge: 16.
pub(crate) const CAPTION_INSET: Tagged<Space> = Tagged::new("UI_LAB_CAPTION_PAD", Space::S4);

/// The navigator column: wide enough for a component's name in any of the
/// eight languages, narrow enough to leave the preview room in the smallest
/// window.
pub(crate) const NAVIGATOR_WIDTH: Metric = Metric::new("UI_LAB_NAVIGATOR_W", 240.0);

/// The least the preview is left beside the side columns before one of them
/// gives way: room for the narrowest preview width on offer.
pub(crate) const PREVIEW_MIN_WIDTH: Metric = Metric::new("UI_LAB_PREVIEW_MIN_W", 320.0);

/// A side column's toggle in the toolbar: a square as high as the compact
/// action beside it.
pub(crate) const TOGGLE_SIZE: Metric = Metric::new("UI_LAB_TOGGLE", 26.0);

/// The properties column: the widest style control, in the language that
/// spells it longest, inside the panel's inset.
pub(crate) const PROPERTIES_WIDTH: Metric = Metric::new("UI_LAB_PROPERTIES_W", 344.0);

/// A property's text field: the form field's height.
pub(crate) const FIELD_HEIGHT: Metric = Metric::new("UI_LAB_FIELD_H", 32.0);

/// The toolbar's inset: 16 at its ends, 8 above and below.
pub(crate) const TOOLBAR_INSET_X: Tagged<Space> = Tagged::new("UI_LAB_TOOLBAR_PAD_X", Space::S4);
pub(crate) const TOOLBAR_INSET_Y: Tagged<Space> = Tagged::new("UI_LAB_TOOLBAR_PAD_Y", Space::S2);

/// Between the toolbar's title and its actions: 8.
pub(crate) const TOOLBAR_GAP: Tagged<Space> = Tagged::new("UI_LAB_TOOLBAR_GAP", Space::S2);

/// Between the navigator's heading and what is under it: 4.
pub(crate) const NAVIGATOR_GAP: Tagged<Space> = Tagged::new("UI_LAB_NAVIGATOR_GAP", Space::S1);

/// One row of the navigator's list, a group's or an entry's. Every row is
/// this high, which is what lets the list lay out only the rows in view.
pub(crate) const NAVIGATOR_ROW: Metric = Metric::new("UI_LAB_NAVIGATOR_ROW_H", 28.0);

/// An entry's inset under its group: 24, the chevron's width and its gap.
pub(crate) const NAVIGATOR_INDENT: Tagged<Space> =
    Tagged::new("UI_LAB_NAVIGATOR_INDENT", Space::S6);

/// Between a group's chevron, its heading and its count: 4.
pub(crate) const NAVIGATOR_GROUP_GAP: Tagged<Space> =
    Tagged::new("UI_LAB_NAVIGATOR_GROUP_GAP", Space::S1);

/// Between a page's sections, the stage and the result line under it
/// among them: 16.
pub(crate) const PAGE_GAP: Tagged<Space> = Tagged::new("UI_LAB_PAGE_GAP", Space::S4);

/// Between the lines of one symbol on a record's page: 2.
pub(crate) const SYMBOL_GAP: Tagged<Space> = Tagged::new("UI_LAB_SYMBOL_GAP", Space::S0_5);

/// Between one symbol and the next: 8.
pub(crate) const SYMBOLS_GAP: Tagged<Space> = Tagged::new("UI_LAB_SYMBOLS_GAP", Space::S2);

/// Between a fact's name and its value in the properties column: 2.
pub(crate) const FACT_GAP: Tagged<Space> = Tagged::new("UI_LAB_FACT_GAP", Space::S0_5);

/// The preview column's inset round the stage: 16.
pub(crate) const PREVIEW_INSET: Tagged<Space> = Tagged::new("UI_LAB_PREVIEW_PAD", Space::S4);

/// The stage's inset round the component under test: 24. With the preview's
/// own inset it still leaves the narrowest dialog button its full width in
/// the smallest window the Lab opens at.
pub(crate) const STAGE_INSET: Tagged<Space> = Tagged::new("UI_LAB_STAGE_PAD", Space::S6);

/// The properties column's inset: 16.
pub(crate) const PANEL_INSET: Tagged<Space> = Tagged::new("UI_LAB_PANEL_PAD", Space::S4);

/// Between the properties column's blocks: 12.
pub(crate) const PANEL_GAP: Tagged<Space> = Tagged::new("UI_LAB_PANEL_GAP", Space::S3);

/// Between a component's name and the identifiers under it: 2.
pub(crate) const IDENTITY_GAP: Tagged<Space> = Tagged::new("UI_LAB_IDENTITY_GAP", Space::S0_5);

/// Between a property's name and its control: 6.
pub(crate) const PROPERTY_GAP: Tagged<Space> = Tagged::new("UI_LAB_PROPERTY_GAP", Space::S1_5);

/// The air between a choice control and the focus ring drawn round it: 2.
pub(crate) const FOCUS_INSET: Tagged<Space> = Tagged::new("UI_LAB_FOCUS_INSET", Space::S0_5);

/// The shared text input on the stage: wide enough for a sentence of sample
/// text, narrower than the narrowest preview.
pub(crate) const FIELD_SUBJECT_WIDTH: Metric = Metric::new("UI_LAB_FIELD_SUBJECT_W", 260.0);

/// One of the three boxes a layout story's row or column holds.
pub(crate) const LAYOUT_ITEM: Metric = Metric::new("UI_LAB_LAYOUT_ITEM", 40.0);

/// A token table on the stage, when the preview is as wide as its column.
pub(crate) const TOKEN_TABLE_WIDTH: Metric = Metric::new("UI_LAB_TOKEN_TABLE_W", 520.0);

/// The cell a token's sample is drawn in: room for the longest spacing step.
pub(crate) const TOKEN_SAMPLE_WIDTH: Metric = Metric::new("UI_LAB_TOKEN_SAMPLE_W", 72.0);

/// A spacing step's bar and a surface's swatch: 12 high.
pub(crate) const TOKEN_BAR_HEIGHT: Metric = Metric::new("UI_LAB_TOKEN_BAR_H", 12.0);

/// Between a token's sample, its name and its value: 12.
pub(crate) const TOKEN_GAP: Tagged<Space> = Tagged::new("UI_LAB_TOKEN_GAP", Space::S3);

/// Between one token's row and the next: 2.
pub(crate) const TOKEN_ROWS_GAP: Tagged<Space> = Tagged::new("UI_LAB_TOKEN_ROWS_GAP", Space::S0_5);
