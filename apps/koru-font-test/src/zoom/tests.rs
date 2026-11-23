// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn zoom_preserves_pixel_under_pointer() {
    let mut zoom = Zoom::default();
    zoom.cursor = [320.0, 240.0];
    zoom.change(4, [1024, 768]);
    assert_eq!(zoom.origin, [240.0, 180.0]);
    assert_eq!(zoom.origin[0] + zoom.cursor[0] / 4.0, 320.0);
    assert_eq!(zoom.origin[1] + zoom.cursor[1] / 4.0, 240.0);
}

#[test]
fn reset_restores_full_image_and_ends_pan() {
    let mut zoom = Zoom::default();
    zoom.cursor = [500.0, 300.0];
    zoom.change(16, [1024, 768]);
    zoom.dragging = true;
    zoom.change(1, [1024, 768]);
    assert_eq!(zoom.factor, 1);
    assert_eq!(zoom.origin, [0.0, 0.0]);
    assert!(!zoom.dragging);
}

#[test]
fn bounds_keep_view_inside_native_texture() {
    let mut zoom = Zoom::default();
    zoom.change(100, [1024, 768]);
    assert_eq!(zoom.factor, 32);
    zoom.origin = [5000.0, -50.0];
    zoom.clamp([1024, 768]);
    assert_eq!(zoom.origin, [992.0, 0.0]);
    zoom.change(0, [1024, 768]);
    assert_eq!(zoom.factor, 1);
}
