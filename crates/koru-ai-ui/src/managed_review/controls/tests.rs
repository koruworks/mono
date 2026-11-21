// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn measured_buttons_wrap_and_close_remains_reachable_while_loading() {
    let (mut view, _, _) = crate::managed_surface::tests::surface();
    let id = Uuid::new_v4();
    let labels = vec![
        ("review-previous", "Previous".into(), false),
        ("review-next", "Next".into(), false),
        ("review-close", "Close".into(), false),
        ("review-skip", "Skip".into(), false),
        ("review-more", "More".into(), false),
    ];
    for width in [180.0, 420.0, 900.0] {
        let controls = Controls::new(&mut view.engine, id, &labels, Some(2), false, width);
        let rect = Rect::new(12.0, 50.0, width, controls.height(width));
        let frame = elements::layout(&controls.tree, rect, FrameCx::root());
        assert!(rect.h > 0.0 && rect.h < 180.0);
        for (i, key) in controls.keys.iter().enumerate() {
            let button = frame.node(*key).unwrap().rect;
            assert!(button.x >= rect.x && button.x + button.w <= rect.x + rect.w + 0.01);
            assert!(button.y >= rect.y && button.y + button.h <= rect.y + rect.h + 0.01);
            assert_eq!(
                controls.hit(rect, button.x + button.w / 2.0, button.y + button.h / 2.0),
                Some(i)
            );
        }
        if width == 900.0 {
            assert_eq!(
                frame.node(controls.keys[0]).unwrap().rect.y,
                frame.node(controls.keys[4]).unwrap().rect.y
            );
        }
        let busy = Controls::new(&mut view.engine, id, &labels, Some(2), true, width);
        let close = frame.node(controls.keys[2]).unwrap().rect;
        let skip = frame.node(controls.keys[3]).unwrap().rect;
        assert_eq!(
            busy.hit(rect, close.x + close.w / 2.0, close.y + close.h / 2.0),
            Some(2)
        );
        assert_eq!(
            busy.hit(rect, skip.x + skip.w / 2.0, skip.y + skip.h / 2.0),
            None
        );
    }
}
