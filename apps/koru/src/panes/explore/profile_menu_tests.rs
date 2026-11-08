// SPDX-License-Identifier: MPL-2.0

use super::*;

fn profile(name: &str) -> ProfileSummary {
    ProfileSummary {
        id: ProfileId::new(),
        name: name.into(),
        email: String::new(),
        avatar: None,
        icon: None,
        color: None,
        account: None,
        folder: String::new(),
    }
}

#[test]
fn profile_focus_follows_identity_and_falls_to_the_neighbor_then_manage() {
    let a = profile("Same name");
    let mut b = profile("Same name");
    b.email = "second@example.test".into();
    let c = profile("Third");
    let mut view = ExploreView::new();
    view.set_context(vec![a.clone(), b.clone(), c.clone()], a.id, vec![], None);
    view.toggle_profile_popover();
    view.popover.as_mut().unwrap().hover = Some(2);
    view.popover_part = Some(PopoverPart::Gear);
    for profiles in [
        vec![a.clone(), b.clone(), c.clone()],
        vec![b.clone(), c.clone(), a.clone()],
    ] {
        view.set_context(profiles, a.id, vec![], None);
        let row = view.popover_rows()[view.live().unwrap().hover.unwrap()];
        assert!(matches!(row, PopoverRow::Profile(i) if view.profiles[i].id == b.id));
        assert_eq!(view.keyboard_part(row), PopoverPart::Gear);
    }
    view.set_context(vec![c.clone(), a.clone()], a.id, vec![], None);
    let row = view.popover_rows()[view.live().unwrap().hover.unwrap()];
    assert!(matches!(row, PopoverRow::Profile(i) if view.profiles[i].id == c.id));
    assert_eq!(view.keyboard_part(row), PopoverPart::Gear);
    view.set_context(vec![], a.id, vec![], None);
    let row = view.popover_rows()[view.live().unwrap().hover.unwrap()];
    assert_eq!(row, PopoverRow::ProfilesHeading);
    assert_eq!(view.keyboard_part(row), PopoverPart::Gear);
}

#[test]
fn profile_switch_closes_and_preserves_the_target_identity() {
    let a = profile("First");
    let b = profile("Second");
    let mut view = ExploreView::new();
    view.set_context(vec![a.clone(), b.clone()], a.id, vec![], None);
    view.toggle_profile_popover();
    assert_eq!(
        view.press(PopoverRow::Profile(1), PopoverPart::Row),
        Outcome::SwitchProfile(b.id)
    );
    assert!(!view.popover_open());
}
