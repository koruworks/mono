// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn stale_or_wrong_family_creation_completion_cannot_release_current_slot() {
    let mut lane = CreationLane {
        serial: 12,
        active: Some((12, Family::Memory)),
        last: Family::Memory,
    };
    assert!(!lane.complete(11, Family::Memory));
    assert!(!lane.complete(12, Family::Bot));
    assert_eq!(lane.active, Some((12, Family::Memory)));
    assert!(lane.complete(12, Family::Memory));
    assert!(lane.active.is_none());
    lane.serial = 13;
    lane.active = Some((13, Family::Bot));
    assert!(!lane.complete(12, Family::Memory));
    assert!(!lane.complete(13, Family::Memory));
    assert_eq!(lane.active, Some((13, Family::Bot)));
    assert!(lane.complete(13, Family::Bot));
}
