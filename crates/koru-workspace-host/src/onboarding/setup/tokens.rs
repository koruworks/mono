// SPDX-License-Identifier: MPL-2.0

//! **The setup card's own tokens** (welcome step 1): every size its element
//! tree ([`super`]) and its leaves ([`super::leaves`]) use that no shared
//! token names — a tagged metric or a tagged spacing alias, so the inspector
//! shows the name beside the value. Both of those files are on the source
//! scan's migrated list (`koru_draw::theme::ui_scan`) and name only these;
//! this file is not, as the question card's tokens are not.
//!
//! The values are the first-run form's own where it had one (the 44 field,
//! the 44 tile, the 42 primary); D6 says the card is tuned once built.

use koru_draw::theme::{Metric, Space, Tagged};

/// The card is at most this wide (D6).
pub(crate) const SETUP_CARD_MAX_W: Metric = Metric::new("SETUP_CARD_MAX_W", 540.0);

/// The card keeps this much of the area free on each side, so it is never
/// wider than the area less twice this: 16 a side, "the area less 32".
pub(crate) const SETUP_CARD_MARGIN: Tagged<Space> = Tagged::new("SETUP_CARD_MARGIN", Space::S4);

/// The card's inset round everything in it: 24, the first-run form's pad.
pub(crate) const SETUP_CARD_PAD: Tagged<Space> = Tagged::new("SETUP_CARD_PAD", Space::S6);

/// The floating panel's corner.
pub(crate) const SETUP_CARD_RADIUS: Metric = Metric::new("SETUP_CARD_RADIUS", 12.0);

/// Between the card's rows: 12.
pub(crate) const SETUP_ROW_GAP: Tagged<Space> = Tagged::new("SETUP_ROW_GAP", Space::S3);

/// Between a chip or tile and the name field beside it: 10, as the form had.
pub(crate) const SETUP_FIELD_GAP: Tagged<Space> = Tagged::new("SETUP_FIELD_GAP", Space::S2_5);

/// Between the actions under the primary: 4.
pub(crate) const SETUP_ACTION_GAP: Tagged<Space> = Tagged::new("SETUP_ACTION_GAP", Space::S1);

/// A name field's height: 44, today's first-run field.
pub(crate) const SETUP_FIELD_H: Metric = Metric::new("SETUP_FIELD_H", 44.0);

/// The avatar chip and the workspace tile: 44, the field's height.
pub(crate) const SETUP_CHIP: Metric = Metric::new("SETUP_CHIP", 44.0);

/// The primary's height: 42, today's first-run primary.
pub(crate) const SETUP_PRIMARY_H: Metric = Metric::new("SETUP_PRIMARY_H", 42.0);

/// *Back* and *Resume*: 36, today's *Resume* button.
pub(crate) const SETUP_ACTION_H: Metric = Metric::new("SETUP_ACTION_H", 36.0);

/// The focus ring round the tile: its width, its gap from the tile, and its
/// corner — the ring the form drew round its tiles.
pub(crate) const SETUP_RING_W: Metric = Metric::new("SETUP_RING_W", 2.0);
pub(crate) const SETUP_RING_GAP: Metric = Metric::new("SETUP_RING_GAP", 2.0);
pub(crate) const SETUP_RING_RADIUS: Metric = Metric::new("SETUP_RING_RADIUS", 12.0);

/// Under the card when it is embedded in a host page that scrolls it: 24.
pub(crate) const SETUP_EMBEDDED_BOTTOM: Metric = Metric::new("SETUP_EMBEDDED_BOTTOM", 24.0);

/// How much of itself the swatch row shows while the card is finishing.
pub(crate) const SETUP_DISABLED_ALPHA: f32 = 0.5;
