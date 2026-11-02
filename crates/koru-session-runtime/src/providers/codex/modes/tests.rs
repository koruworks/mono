// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn codex_is_offered_three_modes_with_ask_for_approval_current() {
    let modes = offered();
    let ids: Vec<_> = modes.modes().iter().map(|mode| mode.id.as_str()).collect();
    assert_eq!(ids, [ASK, AUTO_REVIEW, FULL_ACCESS]);
    assert_eq!(modes.current().map(|id| id.as_str()), Some(ASK));
}
