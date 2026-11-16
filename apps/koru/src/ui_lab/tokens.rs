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
pub(crate) const NAVIGATOR_WIDTH: Metric = Metric::new("UI_LAB_NAVIGATOR_W", 200.0);

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

/// Between the navigator's headings and its entries: 4.
pub(crate) const NAVIGATOR_GAP: Tagged<Space> = Tagged::new("UI_LAB_NAVIGATOR_GAP", Space::S1);

/// The preview column's inset round the stage: 16.
pub(crate) const PREVIEW_INSET: Tagged<Space> = Tagged::new("UI_LAB_PREVIEW_PAD", Space::S4);

/// Between the stage and the result line under it: 16.
pub(crate) const PREVIEW_GAP: Tagged<Space> = Tagged::new("UI_LAB_PREVIEW_GAP", Space::S4);

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
