// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn every_text_line_and_highlight_fits_inside_the_result_card() {
    for preferred in [60.0_f32, 70.0, 78.0] {
        let height = preferred.max(ResultContent::minimum_row_height());
        for title_match in [false, true] {
            let layout = ResultContent::new(height, title_match);
            let mut lines = vec![
                (layout.title, theme::role::row().metrics.line_height),
                (layout.context, theme::role::meta().metrics.line_height),
            ];
            if !title_match {
                lines.push((layout.snippet, theme::role::meta().metrics.line_height));
            }
            for (top, line_height) in lines {
                // Match highlights begin one pixel above the text origin.
                assert!(top - 1.0 >= crate::CARD_INSET_Y);
                assert!(top + line_height <= height - crate::CARD_INSET_Y);
            }
            assert!(layout.context >= layout.title + theme::role::row().metrics.line_height);
        }
    }
}
