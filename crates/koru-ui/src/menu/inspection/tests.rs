// SPDX-License-Identifier: MPL-2.0

use super::*;

#[derive(Clone)]
enum Row {
    Action,
    Disabled,
    Toggle(bool),
    Submenu,
    Header,
    Rule,
    Controls,
}

impl MenuItem for Row {
    fn label(&self) -> Cow<'_, str> {
        "private synthetic document name".into()
    }
    fn divider(&self) -> bool {
        matches!(self, Self::Rule)
    }
    fn selectable(&self) -> bool {
        !matches!(self, Self::Header | Self::Rule | Self::Controls)
    }
    fn checked(&self) -> Option<bool> {
        if let Self::Toggle(on) = self {
            Some(*on)
        } else {
            None
        }
    }
    fn has_submenu(&self) -> bool {
        matches!(self, Self::Submenu)
    }
    fn destructive(&self) -> bool {
        matches!(self, Self::Disabled)
    }
    fn disabled_reason(&self) -> Option<Cow<'static, str>> {
        Some("private disabled reason".into())
    }
    fn controls(&self) -> Vec<Control> {
        if matches!(self, Self::Controls) {
            vec![
                Control::Button {
                    glyph: koru_draw::icons::MINUS,
                    enabled: false,
                },
                Control::Readout("private value".into()),
                Control::Button {
                    glyph: koru_draw::icons::PLUS,
                    enabled: true,
                },
            ]
        } else {
            vec![]
        }
    }
}

const VIEWPORT: (f32, f32) = (900.0, 650.0);

fn fixture() -> Menu<Row> {
    Menu::at_width(
        (100.0, 100.0),
        vec![
            Row::Action,
            Row::Disabled,
            Row::Toggle(true),
            Row::Toggle(false),
            Row::Submenu,
            Row::Header,
            Row::Rule,
            Row::Controls,
        ],
        Anchor::Cursor,
        250.0,
    )
}

fn value<'a>(record: &'a Record, name: &str) -> &'a str {
    &record
        .properties
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("{} missing {name}", record.name))
        .value
}

fn capture(menu: &Menu<Row>, occurrence: u64, owner: Option<u64>) -> inspect::Snapshot {
    let mut engine = TextEngine::new();
    let root = Affine::scale(1.5);
    let frame = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, VIEWPORT.0, VIEWPORT.1));
    let row_id = |i| Some(occurrence + i as u64 + 1);
    menu.draw_with_inspection(
        &mut Scene::new(),
        root,
        &mut engine,
        VIEWPORT,
        &[true, false, true, true, true, false, false, false],
        KoruMenuInspection {
            occurrence: Some(occurrence),
            row_occurrence: Some(&row_id),
            owner_row: owner,
            trigger: None,
        },
    );
    frame.finish()
}

#[test]
fn actual_painter_captures_full_rows_and_distinct_categories_without_private_content() {
    let mut menu = fixture();
    menu.hover = Some(1);
    menu.control_hover = Some((7, 0));
    let snapshot = capture(&menu, 100, None);
    let rows: Vec<_> = snapshot
        .records
        .iter()
        .filter(|r| r.properties.iter().any(|p| p.name == "row index"))
        .collect();
    assert_eq!(rows.len(), 8);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(
            row.rect(),
            scene::rect(
                menu.item_rect(VIEWPORT, i).x,
                menu.item_rect(VIEWPORT, i).y,
                250.0,
                menu.item_rect(VIEWPORT, i).h
            )
        );
        assert_eq!(value(row, "occurrence"), (101 + i).to_string());
        assert_eq!(row.parent, Some(0));
    }
    assert_eq!(value(rows[0], "checked"), "not a toggle");
    assert_eq!(value(rows[1], "enabled"), "false");
    assert_eq!(value(rows[1], "highlighted"), "true");
    assert_eq!(value(rows[1], "hover paint visible"), "false");
    assert_eq!(value(rows[1], "destructive"), "true");
    assert_eq!(value(rows[2], "checked"), "true");
    assert_eq!(value(rows[3], "checked"), "false");
    assert_eq!(value(rows[4], "category"), "submenu");
    assert_eq!(rows[5].name, "Menu header");
    assert_eq!(rows[6].name, "Menu separator");
    assert_eq!(rows[7].name, "Menu controls");
    let controls: Vec<_> = snapshot
        .records
        .iter()
        .filter(|r| r.properties.iter().any(|p| p.name == "control index"))
        .collect();
    assert_eq!(controls.len(), 3);
    assert_eq!(value(controls[0], "enabled"), "false");
    assert_eq!(value(controls[0], "hover paint visible"), "false");
    assert_eq!(value(controls[1], "pressable"), "false");
    assert_eq!(value(controls[2], "pressable"), "true");
    for (i, control) in controls.iter().enumerate() {
        let cell = menu.control_rect(VIEWPORT, 7, i).unwrap();
        assert_eq!(control.rect(), scene::rect(cell.x, cell.y, cell.w, cell.h));
        assert_eq!(
            snapshot.records[control.parent.unwrap()].name,
            "Menu controls"
        );
    }
    let hint = snapshot
        .records
        .iter()
        .find(|r| r.name == "Menu disabled explanation")
        .unwrap();
    assert_eq!(
        hint.parent, None,
        "independently placed explanation is a sibling overlay"
    );
    assert_eq!(value(hint, "logical owner row"), "102");
    let export = snapshot.export(None).unwrap();
    assert!(
        !export.contains("private"),
        "labels, readouts and reasons are not identity"
    );
    assert!(!export.contains("KoruPopover"));
}

#[test]
fn full_hit_target_is_wider_than_the_hover_shape_and_matches_input_after_clamping() {
    let mut menu = fixture();
    menu.set_anchor((890.0, 640.0));
    menu.hover = Some(0);
    let snapshot = capture(&menu, 200, None);
    let row = snapshot
        .records
        .iter()
        .find(|r| r.name == "KoruMenuItem")
        .unwrap();
    let bounds = menu.item_rect(VIEWPORT, 0);
    assert_eq!(
        menu.item_at(VIEWPORT, bounds.x + 1.0, bounds.y + 5.0),
        Some(0)
    );
    assert_eq!(
        snapshot.records[snapshot
            .owner(
                snapshot
                    .pick((bounds.x + 1.0) as f64, (bounds.y + 5.0) as f64)
                    .unwrap()
            )
            .unwrap()]
        .name,
        "KoruMenuItem"
    );
    assert_ne!(
        value(row, "hit bounds (x y w h)"),
        value(row, "hover paint bounds (x y w h)")
    );
    assert_eq!(
        snapshot.records[0].rect().width(),
        250.0,
        "display zoom removed exactly once"
    );
    assert_eq!(value(&snapshot.records[0], "radius"), "8");
    assert_eq!(value(&snapshot.records[0], "viewport inset"), "4");
}

#[test]
fn associations_are_optional_and_unregistered_children_do_not_inherit_certification() {
    let frame = inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, 900.0, 650.0));
    let bounds = fixture().panel(VIEWPORT);
    {
        let _menu = menu(
            Affine::IDENTITY,
            bounds,
            Anchor::Cursor,
            &KoruMenuInspection::default(),
        );
        let _local = inspect::scope(
            "Local custom content",
            "unregistered",
            Affine::IDENTITY,
            scene::rect(100.0, 100.0, 20.0, 20.0),
            |_| {},
        );
        inspect::leaf(
            "fill",
            Affine::IDENTITY,
            scene::rect(100.0, 100.0, 20.0, 20.0),
            |_| {},
        );
    }
    let snapshot = frame.finish();
    assert_eq!(value(&snapshot.records[0], "occurrence"), "unavailable");
    assert_eq!(snapshot.owner(2), None);
}

#[test]
fn host_identity_is_borrowed_without_minting_or_inheriting_from_nearby_surfaces() {
    let first = capture(&fixture(), 300, None);
    let reopened = capture(&fixture(), 400, None);
    let flyout = capture(&fixture(), 500, Some(305));
    assert_eq!(value(&first.records[0], "occurrence"), "300");
    assert_eq!(value(&reopened.records[0], "occurrence"), "400");
    assert_eq!(value(&flyout.records[0], "logical owner row"), "305");
    assert_eq!(
        value(&flyout.records[0], "trigger occurrence"),
        "unavailable"
    );
    assert_eq!(flyout.records[0].parent, None);
}

#[test]
fn capture_off_never_calls_host_identity_or_collects_row_metadata() {
    struct NoMetadata;
    impl MenuItem for NoMetadata {
        fn label(&self) -> Cow<'_, str> {
            panic!("metadata must not run")
        }
        fn controls(&self) -> Vec<Control> {
            panic!("extra controls allocation while capture off")
        }
    }
    let id = |_| panic!("host callback must not run");
    let association = KoruMenuInspection {
        row_occurrence: Some(&id),
        ..Default::default()
    };
    assert!(
        row(
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 100.0, 26.0),
            &NoMetadata,
            0,
            true,
            false,
            &association
        )
        .is_none()
    );
    let mut engine = TextEngine::new();
    fixture().draw_with_inspection(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut engine,
        VIEWPORT,
        &[true; 8],
        association,
    );
    assert!(!inspect::enabled());
}

#[test]
fn artwork_and_rtl_text_keep_separate_provenance_with_transforms_and_clipping() {
    struct Mixed;
    impl MenuItem for Mixed {
        fn label(&self) -> Cow<'_, str> {
            "مستند تجريبي Test ١٢٣".into()
        }
        fn brand_icon(&self) -> Option<&'static koru_draw::brand::BrandIcon> {
            Some(&koru_draw::brand::OPENAI)
        }
        fn height(&self) -> f32 {
            48.0
        }
    }
    let menu = Menu::at_width((100.0, 100.0), vec![Mixed], Anchor::Cursor, 300.0);
    let mut engine = TextEngine::new();
    for scale in [1.0, 2.0] {
        let root = Affine::scale(scale);
        let frame = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, 900.0, 650.0));
        {
            let _clip = inspect::clip(root, scene::rect(110.0, 110.0, 150.0, 30.0));
            menu.draw(&mut Scene::new(), root, &mut engine, VIEWPORT, &[true]);
        }
        let snapshot = frame.finish();
        let item = snapshot
            .records
            .iter()
            .find(|r| r.name == "KoruMenuItem")
            .unwrap();
        assert_eq!(item.rect().height(), 48.0, "actual overridden row height");
        assert_eq!(item.visible(), scene::rect(110.0, 110.0, 150.0, 30.0));
        let brand = snapshot
            .records
            .iter()
            .find(|r| r.name == "Menu brand icon")
            .unwrap();
        assert_eq!(value(brand, "brand"), "OpenAI");
        assert!(!brand.properties.iter().any(|p| p.name == "typography role"));
        assert!(
            snapshot
                .records
                .iter()
                .any(|r| r.name == "Menu label"
                    && value(r, "typography role") == "theme::role::row()")
        );
        assert!(!snapshot.export(None).unwrap().contains("مستند"));
    }
}

/// **A label past the menu's ceiling ends in an ellipsis inside its row** —
/// a chat named after a long first message — and never runs past the panel.
#[test]
fn a_label_wider_than_the_menu_stays_inside_its_row() {
    struct Long;
    impl MenuItem for Long {
        fn label(&self) -> Cow<'_, str> {
            "[[koru://doc/a86024f5-970a-45d9-b54a-06a0e3b887d4]] summarise this for me".into()
        }
    }
    let mut engine = TextEngine::new();
    let menu = Menu::new(&mut engine, (100.0, 100.0), vec![Long]);
    let frame = inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, 900.0, 650.0));
    menu.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut engine,
        VIEWPORT,
        &[true],
    );
    let snapshot = frame.finish();
    let find = |name: &str| {
        snapshot
            .records
            .iter()
            .find(|r| r.name == name)
            .unwrap()
            .rect()
    };
    let (row, label) = (find("KoruMenuItem"), find("Menu label"));
    assert!(
        label.x1 <= row.x1,
        "the label ends at {} inside a row ending at {}",
        label.x1,
        row.x1
    );
}

#[test]
fn capture_does_not_change_paint_encoding_for_any_fixture_state() {
    let mut engine = TextEngine::new();
    let mut menu = fixture();
    for highlighted in [None, Some(0), Some(1), Some(2)] {
        menu.hover = highlighted;
        let mut off = Scene::new();
        let mut on = Scene::new();
        let enabled = [true, false, true, true, true, false, false, false];
        menu.draw(&mut off, Affine::IDENTITY, &mut engine, VIEWPORT, &enabled);
        let frame = inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, 900.0, 650.0));
        menu.draw(&mut on, Affine::IDENTITY, &mut engine, VIEWPORT, &enabled);
        frame.finish();
        let (a, b) = (off.encoding(), on.encoding());
        assert!(a.path_tags == b.path_tags, "path operations unchanged");
        assert_eq!(a.path_data, b.path_data);
        assert!(a.draw_tags == b.draw_tags, "draw operations unchanged");
        assert_eq!(a.draw_data, b.draw_data);
        assert_eq!(a.transforms, b.transforms);
        assert_eq!(a.styles, b.styles);
    }
}

#[test]
fn malformed_control_rows_are_disclosed_and_unknown_swatch_strings_are_private() {
    struct Odd;
    impl MenuItem for Odd {
        fn label(&self) -> Cow<'_, str> {
            "Private synthetic label".into()
        }
        fn controls(&self) -> Vec<Control> {
            vec![Control::Readout("private value".into())]
        }
        // Deliberately retains the default selectable=true, contrary to the
        // documented controls contract. Inspection must describe the mismatch.
        fn swatch(&self) -> Option<Option<&str>> {
            Some(Some("private invalid token"))
        }
    }
    let menu = Menu::at_width((100.0, 100.0), vec![Odd], Anchor::Cursor, 250.0);
    let mut engine = TextEngine::new();
    let frame = inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, 900.0, 650.0));
    menu.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut engine,
        VIEWPORT,
        &[true],
    );
    let snapshot = frame.finish();
    let group = snapshot
        .records
        .iter()
        .find(|r| r.name == "Menu controls")
        .unwrap();
    assert_eq!(value(group, "selectable"), "true");
    assert_eq!(
        value(group, "contract mismatch"),
        "controls row is also selectable"
    );
    let swatch = snapshot
        .records
        .iter()
        .find(|r| r.name == "Menu swatch")
        .unwrap();
    assert_eq!(value(swatch, "color token"), "neutral fallback");
    assert!(!snapshot.export(None).unwrap().contains("private"));
    assert_eq!(
        menu.item_at(VIEWPORT, 101.0, 110.0),
        Some(0),
        "existing host input is not repaired by inspection"
    );
}
