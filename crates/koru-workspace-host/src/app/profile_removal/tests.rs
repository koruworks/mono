// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn lost_completion_reply_can_finish_cleanup_only_after_authoritative_absence() {
    assert_eq!(removal_absence(false, false), Ok(()));
    // A retained workspace contradicts removal even if the Profile listing
    // omitted its owner; do not clean up its local state.
    assert_eq!(
        removal_absence(false, true),
        Err(CascadeFailure::Incomplete)
    );
}

#[test]
fn keep_or_an_unfinished_empty_profile_cannot_be_mistaken_for_completed_removal() {
    // Keep clears the attempt too. The remaining Profile distinguishes it
    // from completion, even after the cascade trashed its last workspace.
    assert_eq!(
        removal_absence(true, false),
        Err(CascadeFailure::Incomplete)
    );
    assert_eq!(removal_absence(true, true), Err(CascadeFailure::Incomplete));
}
