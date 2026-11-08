// SPDX-License-Identifier: MPL-2.0

//! Text offsets and row height share the same line-box measurements.

use koru_draw::theme;

const LINE_GAP: f32 = 2.0;
const CONTENT_PAD: f32 = 2.0;

pub(super) struct ResultContent {
    pub title: f32,
    pub context: f32,
    pub snippet: f32,
}

impl ResultContent {
    fn height(title_match: bool) -> f32 {
        theme::role::row().metrics.line_height
            + theme::role::meta().metrics.line_height
            + LINE_GAP
            + if title_match {
                0.0
            } else {
                theme::role::meta().metrics.line_height + LINE_GAP
            }
    }

    pub fn minimum_row_height() -> f32 {
        Self::height(false) + 2.0 * (super::CARD_INSET_Y + CONTENT_PAD)
    }

    pub fn new(row_height: f32, title_match: bool) -> Self {
        let title = (row_height - Self::height(title_match)) * 0.5;
        let context = title + theme::role::row().metrics.line_height + LINE_GAP;
        let snippet = context + theme::role::meta().metrics.line_height + LINE_GAP;
        Self {
            title,
            context,
            snippet,
        }
    }
}

#[cfg(test)]
mod tests;
