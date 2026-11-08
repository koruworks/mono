// SPDX-License-Identifier: MPL-2.0

//! **The question card's own tokens** (koru-ai D108): every size its element
//! tree ([`super::elements`]) uses that no shared token names — a tagged
//! metric or a tagged spacing alias, so the inspector shows the name beside
//! the value. The tree is on the source scan's migrated list
//! (`koru_draw::theme::ui_scan`) and names only these.
//!
//! The field and the buttons keep the heights of the components they are:
//! the form field's and the compact action's.

use koru_draw::theme::{Metric, Space, Tagged};

/// The card's inset round everything in it: 12.
pub(crate) const CARD_INSET: Tagged<Space> = Tagged::new("QUESTION_CARD_PAD", Space::S3);

/// Between the card's heading, its questions and its actions: 12.
pub(crate) const CARD_GAP: Tagged<Space> = Tagged::new("QUESTION_CARD_GAP", Space::S3);

/// Between a question's header, its words and its options: 6.
pub(crate) const BLOCK_GAP: Tagged<Space> = Tagged::new("QUESTION_BLOCK_GAP", Space::S1_5);

/// Between one option and the next: 4.
pub(crate) const OPTION_GAP: Tagged<Space> = Tagged::new("QUESTION_OPTION_GAP", Space::S1);

/// An option's content inset: 8 round its words.
pub(crate) const OPTION_INSET: Tagged<Space> = Tagged::new("QUESTION_OPTION_INSET", Space::S2);

/// Between an option's label and the line under it: 2.
pub(crate) const OPTION_WORDS_GAP: Tagged<Space> =
    Tagged::new("QUESTION_OPTION_WORDS_GAP", Space::S0_5);

/// The least an option's row is: one row of words in its inset.
pub(crate) const OPTION_MIN_HEIGHT: Metric = Metric::new("QUESTION_OPTION_MIN_H", 32.0);

/// The answer box: the form field's height.
pub(crate) const FIELD_HEIGHT: Metric = Metric::new("QUESTION_FIELD_H", 32.0);

/// The heading's icon, before its words: 6.
pub(crate) const HEADING_GAP: Tagged<Space> = Tagged::new("QUESTION_HEADING_GAP", Space::S1_5);

/// Between the two actions: 8.
pub(crate) const ACTION_GAP: Tagged<Space> = Tagged::new("QUESTION_ACTION_GAP", Space::S2);

/// Between the lines of an answered set's summary: 4.
pub(crate) const SUMMARY_GAP: Tagged<Space> = Tagged::new("QUESTION_SUMMARY_GAP", Space::S1);

/// An answered set's summary inset: 10.
pub(crate) const SUMMARY_INSET: Tagged<Space> = Tagged::new("QUESTION_SUMMARY_PAD", Space::S2_5);
