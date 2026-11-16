// SPDX-License-Identifier: MPL-2.0

use super::*;

#[test]
fn a_launch_opens_at_the_default_unless_a_size_was_asked_for() {
    assert_eq!(Placement::in_memory(None).size, Placement::DEFAULT_SIZE);
    assert_eq!(
        Placement::in_memory(Some((1024.0, 768.0))).size,
        (1024.0, 768.0)
    );
}

/// The default has to be a size the command line itself would accept, or the
/// plain launch would open smaller than the smallest size a person may ask
/// for.
#[test]
fn the_default_is_no_smaller_than_the_smallest_accepted_size() {
    assert!(Placement::DEFAULT_SIZE.0 >= MIN_SIZE.0);
    assert!(Placement::DEFAULT_SIZE.1 >= MIN_SIZE.1);
}

#[test]
fn the_window_is_undecorated_at_the_placed_size_with_the_shared_floor() {
    let attributes = Placement::in_memory(Some((900.0, 640.0))).attributes("Lab");
    assert!(!attributes.decorations);
    assert_eq!(attributes.title, "Lab");
    assert_eq!(
        attributes.inner_size,
        Some(LogicalSize::new(900.0_f32, 640.0).into())
    );
    assert_eq!(
        attributes.min_inner_size,
        Some(LogicalSize::new(MIN_SIZE.0, MIN_SIZE.1).into())
    );
    // The system places it: no remembered position is ever supplied.
    assert_eq!(attributes.position, None);
}
