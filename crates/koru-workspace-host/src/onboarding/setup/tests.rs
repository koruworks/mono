// SPDX-License-Identifier: MPL-2.0

//! **The setup card's frame**, laid out in memory: its width beside the
//! welcome sidebar, its fit at the smallest windows in every language, its
//! targets, its wrapping swatch row and its place in the inspector. Nothing
//! here opens a window, touches a disk or reaches a Store.

use super::*;

fn view(width: f32, height: f32) -> OnboardingView {
    let mut view = OnboardingView::default();
    view.set_viewport(width, height);
    view.show_setup();
    view
}

fn frame(view: &mut OnboardingView) -> Frame {
    view.setup_frame().1
}

fn rect(frame: &Frame, key: ElKey) -> Rect {
    frame
        .node(key)
        .unwrap_or_else(|| panic!("{key:?} is on the card"))
        .rect
}

fn capture(view: &mut OnboardingView) -> Vec<inspect::Record> {
    let mut canvas = Scene::new();
    let root = Affine::IDENTITY;
    let capture = inspect::Frame::begin(
        1,
        root,
        scene::rect(0.0, 0.0, view.viewport.0, view.viewport.1),
    );
    view.draw(&mut canvas, root);
    capture.finish().records
}

fn within(records: &[inspect::Record], mut at: usize, ancestor: usize) -> bool {
    while let Some(parent) = records[at].parent {
        if parent == ancestor {
            return true;
        }
        at = parent;
    }
    false
}

/// **At most 540, never wider than the area beside the sidebar less 32**
/// (D6, Q-B): 420 at the 452 minimum, 540 at 759, 408.8 at 760 where the
/// sidebar appears, 540 at the default 1000. Always wholly beside the
/// sidebar. Embedded, it takes the host's column and draws no panel.
#[test]
fn the_card_width_follows_the_area_beside_the_sidebar() {
    for (width, height, expected) in [
        (452.0, 640.0, 420.0),
        (759.0, 640.0, 540.0),
        (760.0, 640.0, 408.8),
        (1000.0, 680.0, 540.0),
    ] {
        let mut view = view(width, height);
        let frame = frame(&mut view);
        let card = rect(&frame, KEY_CARD);
        assert!(
            (card.w - expected).abs() < 0.01,
            "{width}×{height}: card {} wide, expected {expected}",
            card.w
        );
        let (_, right) = welcome::split(width, height);
        assert!(card.x >= right.x && card.x + card.w <= right.x + right.w + 0.01);
        assert!(card.y >= right.y && card.y + card.h <= right.y + right.h + 0.01);
    }

    let mut embedded = OnboardingView::default();
    embedded.set_embedded(true);
    embedded.show_setup();
    embedded.set_viewport(480.0, 900.0);
    let frame = frame(&mut embedded);
    let card = rect(&frame, KEY_CARD);
    assert_eq!((card.x, card.y), (0.0, 0.0));
    assert!(
        (card.w - 480.0).abs() < 0.01,
        "the host's column: {}",
        card.w
    );
    let records = capture(&mut embedded);
    assert!(
        !records.iter().any(|record| record.name == "FloatingPanel"),
        "no panel when embedded"
    );
}

/// **The card fits the smallest windows in every language**, the minimum and
/// the narrowest card beside the sidebar, on the local route and the
/// connected one with its note: nothing overflows, the card's bottom is in
/// the window, and the primary is whole.
#[test]
fn the_card_fits_the_minimum_window_in_every_language() {
    for lang in koru_i18n::Lang::ALL {
        for (width, height) in [(452.0, 640.0), (760.0, 640.0)] {
            for connected in [false, true] {
                let mut view = OnboardingView::default();
                view.language = lang;
                view.set_viewport(width, height);
                if connected {
                    view.begin_connected_profile_creation();
                } else {
                    view.show_setup();
                }
                let frame = frame(&mut view);
                let case = format!("{}/{width}×{height}/connected {connected}", lang.tag());
                for node in &frame.nodes {
                    assert!(!node.overflow, "{case}: {} overflows", node.name);
                }
                let card = rect(&frame, KEY_CARD);
                assert!(
                    card.y >= 0.0 && card.y + card.h <= height,
                    "{case}: {card:?}"
                );
                assert!(
                    card.x >= 0.0 && card.x + card.w <= width,
                    "{case}: {card:?}"
                );
                let primary = rect(&frame, KEY_PRIMARY);
                assert!(primary.w > 0.0 && primary.h > 0.0, "{case}");
                assert!(
                    primary.x >= card.x
                        && primary.x + primary.w <= card.x + card.w + 0.01
                        && primary.y + primary.h <= card.y + card.h + 0.01,
                    "{case}: the primary is cut off: {primary:?} in {card:?}"
                );
                assert!(
                    frame.diagnostics.is_empty(),
                    "{case}: {:?}",
                    frame.diagnostics
                );
                view.draw(&mut Scene::new(), Affine::IDENTITY);
            }
        }
    }
}

/// **Every target comes from the frame**: a press at the middle of each
/// keyed node answers the control the table maps it to, and a press on each
/// swatch's cell answers that cell.
#[test]
fn every_target_comes_from_the_frame() {
    let mut view = OnboardingView::default();
    view.set_viewport(1000.0, 680.0);
    // A parked account creation offers *Resume*; a recovery list offers the
    // link; a name makes the primary pressable.
    view.account_creation_required(&AccountState::default());
    view.activate_account_action(2);
    assert_eq!(view.screen, Screen::Setup);
    view.show_recovery(
        vec![RecoverableProfile {
            profile: koru_core::ProfileId::new(),
            deletion: koru_store_core::ids::DeletionId::new(),
            name: "Work".into(),
            at_ms: 1_000,
            workspaces: 1,
            unavailable: 0,
        }],
        Vec::new(),
    );
    view.owner_name
        .borrow_mut()
        .set_text(&mut view.engine, "Ana");
    let frame = frame(&mut view);
    for (key, hit) in [
        (KEY_NAME, Hit::Name),
        (KEY_TILE, Hit::WorkspaceAppearance),
        (KEY_WORKSPACE_NAME, Hit::WorkspaceName),
        (KEY_PRIMARY, Hit::Primary),
        (KEY_BACK, Hit::Back),
        (KEY_RESUME, Hit::ResumeAccount),
        (KEY_RECOVERY, Hit::Recovery),
    ] {
        let (x, y) = rect(&frame, key).centre();
        assert_eq!(view.hit(x, y), Some(hit), "{key:?}");
        assert_eq!(hit_for_key(key), Some(hit));
        assert_eq!(key_for_hit(hit), Some(key));
    }
    let cells = swatch_geometry().wrapped_swatches(rect(&frame, KEY_COLORS));
    assert_eq!(cells.len(), SWATCH_COUNT);
    for (index, cell) in cells.iter().enumerate() {
        let (x, y) = cell.centre();
        assert_eq!(view.hit(x, y), Some(Hit::Swatch(index)));
    }
    // The chip is decorative: no target.
    let (x, y) = rect(&frame, KEY_AVATAR).centre();
    assert_eq!(view.hit(x, y), None);
}

/// **The swatch row wraps instead of clipping**: narrower than the ten
/// swatches' 310, the leaf is as tall as the component says it needs, on two
/// lines, and every cell is inside it.
#[test]
fn the_swatch_row_wraps_instead_of_clipping() {
    let mut view = OnboardingView::default();
    view.set_embedded(true);
    view.show_setup();
    view.set_viewport(300.0, 900.0);
    let frame = frame(&mut view);
    let row = rect(&frame, KEY_COLORS);
    assert!(row.w < 310.0, "{row:?}");
    let geometry = swatch_geometry();
    assert!((row.h - geometry.wrapped_height(row.w)).abs() < 0.01);
    assert_eq!(geometry.lines(row).len(), 2);
    for cell in geometry.wrapped_swatches(row) {
        assert!(
            cell.x >= row.x
                && cell.x + cell.w <= row.x + row.w
                && cell.y >= row.y
                && cell.y + cell.h <= row.y + row.h + 0.01,
            "{cell:?} outside {row:?}"
        );
    }
}

/// **One floating panel, and the card inside it** (D7): the inspector shows
/// the card's element tree under the panel, with the shared components —
/// the name fields, `KoruAvatar`, `ColorPickerRow` and the tile — inside the
/// card.
#[test]
fn the_card_tree_nests_under_one_floating_panel_in_the_inspector() {
    let mut view = view(1000.0, 680.0);
    let records = capture(&mut view);
    let panels: Vec<usize> = records
        .iter()
        .enumerate()
        .filter_map(|(i, r)| (r.name == "FloatingPanel").then_some(i))
        .collect();
    assert_eq!(panels.len(), 1, "one floating panel");
    let card = records
        .iter()
        .position(|r| r.name == CARD)
        .expect("the card's element");
    assert!(within(&records, card, panels[0]));
    for component in [
        "NameField",
        "KoruAvatar",
        "ColorPickerRow",
        "Workspace tile",
        "WorkspaceIcon",
        "KoruSwatch",
    ] {
        let found: Vec<usize> = records
            .iter()
            .enumerate()
            .filter_map(|(i, r)| (r.name == component).then_some(i))
            .collect();
        assert!(!found.is_empty(), "{component} is drawn");
        for index in found {
            assert!(within(&records, index, card), "{component} is in the card");
        }
    }
    let swatches = records.iter().filter(|r| r.name == "KoruSwatch").count();
    assert_eq!(swatches, SWATCH_COUNT);
}

/// The ten swatches are the palette's ten named colors, in its order, and
/// the accessibility parts are keyed by those names in the same order.
#[test]
fn the_swatches_are_the_ten_named_colors_in_palette_order() {
    let names = theme::workspace_color_names();
    assert_eq!(names.len(), SWATCH_COUNT);
    for (name, part) in names.iter().zip(super::super::accessibility::parts::COLOR) {
        assert_eq!(part.to_string(), format!("color-{name}"));
    }
}
