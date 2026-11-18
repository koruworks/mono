// SPDX-License-Identifier: MPL-2.0
use super::*;

#[test]
fn long_permission_choice_keeps_full_accessible_name_inside_its_card() {
    let mut engine = TextEngine::new();
    let id = uuid::Uuid::new_v4();
    let label = "A long provider supplied permission choice ".repeat(12);
    let labels = vec![(label.clone(), true)];
    let actions = Actions {
        id,
        heading: "Permission",
        body: "Synthetic detail",
        labels: &labels,
        focused: Some(0),
        disabled: false,
    };
    let tree = actions.tree(&mut engine);
    let frame = elements::layout(&tree, Rect::new(0.0, 0.0, 240.0, 800.0), FrameCx::root());
    let target = frame.node(control_key(id, Control::Option(0, 0))).unwrap();
    assert!(target.rect.x >= 0.0 && target.rect.x + target.rect.w <= 240.0);
    assert!(matches!(&target.label, Some(koru_draw::el::Name::Text(value)) if *value == label));
    let hit = actions.hit(
        &mut engine,
        Rect::new(0.0, 0.0, 240.0, 800.0),
        target.rect.x + target.rect.w * 0.5,
        target.rect.y + target.rect.h * 0.5,
    );
    assert_eq!(hit, Some(0));
}
