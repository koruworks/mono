// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn installed_ram_rounds_down_and_limits_the_default_to_five() {
    assert_eq!(default_capacity(0), 1);
    assert_eq!(default_capacity(7 * 1024 * 1024 * 1024), 3);
    assert_eq!(default_capacity(32 * 1024 * 1024 * 1024), 5);
}
