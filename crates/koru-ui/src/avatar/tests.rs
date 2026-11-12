// SPDX-License-Identifier: MPL-2.0

use super::*;

fn records(name: &str) -> Vec<inspect::Record> {
    let mut engine = TextEngine::new();
    let mut canvas = Scene::new();
    let root = Affine::IDENTITY;
    let capture = inspect::Frame::begin(1, root, scene::rect(0.0, 0.0, 200.0, 200.0));
    KoruAvatar::initial(name).draw_in(
        &mut canvas,
        root,
        &mut engine,
        Rect::new(10.0, 10.0, 44.0, 44.0),
        theme::p(),
    );
    capture.finish().records
}

#[test]
fn the_letter_is_the_names_first_character_upper_cased() {
    assert_eq!(KoruAvatar::initial("ada").letter().as_deref(), Some("A"));
    assert_eq!(KoruAvatar::initial("Steven").letter().as_deref(), Some("S"));
    // The tile's rule for a letter with no single upper-case form.
    assert_eq!(KoruAvatar::initial("ßen").letter().as_deref(), Some("SS"));
    assert_eq!(KoruAvatar::initial("élodie").letter().as_deref(), Some("É"));
    assert_eq!(KoruAvatar::initial("  ana").letter().as_deref(), Some("A"));
}

#[test]
fn a_blank_name_draws_an_empty_circle() {
    assert_eq!(KoruAvatar::initial("").letter(), None);
    assert_eq!(KoruAvatar::initial("   ").letter(), None);
    let records = records("");
    let chip = records
        .iter()
        .find(|r| r.name == "KoruAvatar")
        .expect("one component");
    assert!(
        chip.properties
            .iter()
            .any(|p| p.name == "drew" && p.value == "empty")
    );
}

#[test]
fn the_chip_is_one_inspector_component_that_says_what_it_drew() {
    let records = records("Ada");
    let chips: Vec<_> = records.iter().filter(|r| r.name == "KoruAvatar").collect();
    assert_eq!(chips.len(), 1);
    assert_eq!(chips[0].kind, "component");
    assert!(
        chips[0]
            .properties
            .iter()
            .any(|p| p.name == "drew" && p.value == "initial")
    );
}
