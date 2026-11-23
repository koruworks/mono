// SPDX-License-Identifier: MPL-2.0
use super::*;

fn spec<'a>(actions: &'a [(Part, String, bool)], busy: bool, previous: bool) -> Spec<'a> {
    Spec {
        id: Uuid::from_u128(7),
        heading: "Needs you",
        position: Some("1 of 8"),
        arrows: [
            Arrow {
                part: Part::Previous,
                enabled: previous,
                name: "Previous result",
            },
            Arrow {
                part: Part::Next,
                enabled: true,
                name: "Next result",
            },
            Arrow {
                part: Part::Close,
                enabled: true,
                name: "Close",
            },
        ],
        tools: &TOOLS,
        actions,
        focused: None,
        busy,
    }
}

const TOOLS: [Arrow<'static>; 2] = [
    Arrow {
        part: Part::Copy,
        enabled: true,
        name: "Copy reply",
    },
    Arrow {
        part: Part::Save,
        enabled: true,
        name: "Save reply as document",
    },
];

fn centre(rect: Rect) -> (f32, f32) {
    (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0)
}

#[test]
fn header_arrows_end_the_row_and_skip_ends_the_footer_at_every_width() {
    let (mut view, _, _) = crate::managed_surface::tests::surface();
    let actions = vec![
        (Part::More, "Show more".to_owned(), false),
        (Part::Skip, "Skip".to_owned(), false),
    ];
    for width in [180.0, 420.0, 900.0] {
        let chrome = Chrome::new(&mut view.engine, &spec(&actions, false, true), width);
        let header = Rect::new(12.0, 50.0, width, chrome.header_height(width));
        let footer = Rect::new(12.0, 300.0, width, chrome.footer_height(width));
        assert!(header.h > 0.0 && header.h < 80.0, "{width}: {}", header.h);
        assert!(footer.h > 0.0 && footer.h < 140.0, "{width}: {}", footer.h);
        let parts = [
            Part::Previous,
            Part::Next,
            Part::Close,
            Part::Copy,
            Part::Save,
            Part::More,
            Part::Skip,
        ];
        for part in parts {
            let rect = chrome.rect_of(part, header, Some(footer)).unwrap();
            let inside = if matches!(part, Part::Copy | Part::Save | Part::More | Part::Skip) {
                footer
            } else {
                header
            };
            assert!(rect.w > 0.0 && rect.h > 0.0, "{part:?} at {width}");
            assert!(rect.x >= inside.x - 0.01 && rect.x + rect.w <= inside.x + inside.w + 0.01);
            assert!(rect.y >= inside.y - 0.01 && rect.y + rect.h <= inside.y + inside.h + 0.01);
            let (x, y) = centre(rect);
            assert_eq!(
                chrome.hit(header, Some(footer), x, y),
                Some(part),
                "{width}"
            );
        }
        let previous = chrome
            .rect_of(Part::Previous, header, Some(footer))
            .unwrap();
        let next = chrome.rect_of(Part::Next, header, Some(footer)).unwrap();
        let close = chrome.rect_of(Part::Close, header, Some(footer)).unwrap();
        assert!(previous.x < next.x && next.x < close.x, "‹ › × in order");
        let inset = Space::S3.px(Profile::Desktop);
        assert!(
            close.x + close.w >= header.x + header.w - inset - 0.01,
            "× ends the header"
        );
        let skip = chrome.rect_of(Part::Skip, header, Some(footer)).unwrap();
        assert!(
            skip.x + skip.w >= footer.x + footer.w - inset - 0.01,
            "Skip ends the footer"
        );
        let copy = chrome.rect_of(Part::Copy, header, Some(footer)).unwrap();
        let save = chrome.rect_of(Part::Save, header, Some(footer)).unwrap();
        let more = chrome.rect_of(Part::More, header, Some(footer)).unwrap();
        assert!(
            copy.x <= footer.x + inset + 0.01,
            "the reply actions start the footer"
        );
        assert!(copy.x < save.x && save.x < more.x || more.y > copy.y);
        let targets = chrome.targets(header, Some(footer));
        assert_eq!(targets.len(), parts.len(), "every control is a target");
    }
}

#[test]
fn disabled_arrows_and_busy_actions_do_not_act_but_close_does() {
    let (mut view, _, _) = crate::managed_surface::tests::surface();
    let actions = vec![(Part::Skip, "Skip".to_owned(), false)];
    let width = 420.0;
    let chrome = Chrome::new(&mut view.engine, &spec(&actions, true, false), width);
    let header = Rect::new(0.0, 0.0, width, chrome.header_height(width));
    let footer = Rect::new(0.0, 200.0, width, chrome.footer_height(width));
    for (part, acts) in [
        (Part::Previous, None),
        (Part::Next, None),
        (Part::Skip, None),
        (Part::Close, Some(Part::Close)),
    ] {
        let (x, y) = centre(chrome.rect_of(part, header, Some(footer)).unwrap());
        assert_eq!(chrome.hit(header, Some(footer), x, y), acts, "{part:?}");
    }
}

#[test]
fn a_card_without_actions_or_a_result_has_no_footer() {
    let (mut view, _, _) = crate::managed_surface::tests::surface();
    let bare = Spec {
        tools: &[],
        ..spec(&[], false, true)
    };
    let chrome = Chrome::new(&mut view.engine, &bare, 420.0);
    assert_eq!(chrome.footer_height(420.0), 0.0);
}
