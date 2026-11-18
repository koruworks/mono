// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn four_threads_unless_the_variable_says_otherwise() {
    assert_eq!(from_value(None).threads(), 4);
    assert_eq!(from_value(Some("2")).threads(), 2);
    assert_eq!(from_value(Some("lots")).threads(), 4);
}
