// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn middle_elision_keeps_unicode_ends_and_fits_the_measured_width() {
    let path = "D:/資料/long-project-folder/工作區";
    let measure = |value: &str| {
        value
            .chars()
            .map(|c| if c.is_ascii() { 1.0 } else { 2.0 })
            .sum()
    };
    let shown = elide_middle_measured(path, 22.0, measure);
    assert!(shown.starts_with("D:/"), "{shown}");
    assert!(shown.ends_with("/工作區"), "{shown}");
    assert!(shown.contains(ELLIPSIS));
    assert!(measure(&shown) <= 22.0);
    assert_eq!(elide_middle_measured(path, 200.0, measure), path);
    assert_eq!(elide_middle_measured(path, 0.0, measure), "");
    assert_eq!(elide_middle_measured(path, 1.0, measure), "");
}
