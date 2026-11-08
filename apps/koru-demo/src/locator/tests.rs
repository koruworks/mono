// SPDX-License-Identifier: MPL-2.0

//! **These prove the grammar, and nothing about a real tree.**
//!
//! Everything here is string work. A locator that parses is not a control that
//! exists, and a pattern that matches a string is not a match against a live
//! provider — that is `uia.rs` and the interactive proof beside it.

use super::*;

#[test]
fn every_shape_the_projection_writes_reads_back() {
    // The five object shapes, taken from the projection's own examples.
    let cases = [
        "koru:w1/v0/s:window#window",
        "koru:w1/v0/s:window#close",
        "koru:w1/v1/s:body#root",
        "koru:w3/v7/n:6f9619ff8b86d011b42d00cf4fc964ff#label",
        "koru:w1/v2/tab:7#root",
        "koru:w1/v1/onb:3#status",
    ];
    for case in cases {
        let parsed = LocatorPattern::parse(case).unwrap_or_else(|e| panic!("{case}: {e}"));
        assert_eq!(parsed.to_string(), case, "a locator must round trip");
        assert_eq!(parsed.exact().as_deref(), Some(case));
        assert!(parsed.matches(case));
    }
}

#[test]
fn a_wildcard_slot_matches_any_number_and_nothing_else_about_the_control() {
    let pattern = LocatorPattern::parse("koru:w*/v*/s:window#close").expect("a pattern");
    assert!(pattern.matches("koru:w1/v0/s:window#close"));
    assert!(pattern.matches("koru:w9/v4/s:window#close"));
    // The part and the object are still exact.
    assert!(!pattern.matches("koru:w1/v0/s:window#minimize"));
    assert!(!pattern.matches("koru:w1/v0/s:body#close"));
    // And a wildcard is not an exact locator, so it cannot become a property
    // condition. This is the branch that decides FindFirst against FindAll.
    assert_eq!(pattern.exact(), None);
}

#[test]
fn binding_a_wildcard_window_to_the_root_turns_a_scan_into_a_lookup() {
    let pattern = LocatorPattern::parse("koru:w*/v1/s:body#root").expect("a pattern");
    assert_eq!(pattern.exact(), None, "unbound, this costs a subtree walk");

    let bound = pattern.bound_to_window(4);
    assert_eq!(bound.exact().as_deref(), Some("koru:w4/v1/s:body#root"));
    // Binding a window that was already named changes nothing — a case that
    // pinned `w2` must not be quietly redirected at whatever window the runner
    // was pointed at.
    let pinned = LocatorPattern::parse("koru:w2/v1/s:body#root").expect("a locator");
    assert_eq!(
        pinned.bound_to_window(4).exact().as_deref(),
        Some("koru:w2/v1/s:body#root")
    );
}

#[test]
fn a_wildcard_instance_number_matches_any_instance_of_that_kind() {
    let pattern = LocatorPattern::parse("koru:w1/v1/onb:*#status").expect("a pattern");
    assert!(pattern.matches("koru:w1/v1/onb:1#status"));
    assert!(pattern.matches("koru:w1/v1/onb:12#status"));
    assert!(!pattern.matches("koru:w1/v1/tab:1#status"));
    assert_eq!(
        pattern.exact(),
        None,
        "an unknown instance number cannot become one property condition"
    );
}

#[test]
fn a_wildcard_may_not_stand_for_what_the_control_is() {
    // The whole argument for this rule: `s:*` is a case that does not know what
    // it is looking for, and the honest place to fail is here rather than as an
    // ambiguity against a live tree three minutes later.
    for refused in [
        "koru:w1/v1/s:*#root",
        "koru:w1/v1/n:*#text",
        "koru:w1/v1/*#root",
    ] {
        assert_eq!(
            LocatorPattern::parse(refused),
            Err(LocatorError::WildcardObject),
            "{refused} must be refused at parse time"
        );
    }
}

#[test]
fn each_malformed_component_names_itself() {
    let cases: [(&str, LocatorError); 7] = [
        ("w1/v1/s:body#root", LocatorError::MissingPrefix),
        ("koru:w1/v1/s:body", LocatorError::MissingPart),
        ("koru:w1/v1/s:body#", LocatorError::EmptyPart),
        ("koru:x1/v1/s:body#root", LocatorError::Window("x1".into())),
        ("koru:w1/x1/s:body#root", LocatorError::View("x1".into())),
        ("koru:w1/v1/body#root", LocatorError::Object("body".into())),
        (
            "koru:w1/v1/s:body/extra#root",
            LocatorError::TrailingSegments,
        ),
    ];
    for (text, want) in cases {
        assert_eq!(LocatorPattern::parse(text), Err(want), "{text}");
    }
}

#[test]
fn a_model_id_is_a_uuid_with_its_hyphens_removed_and_nothing_else() {
    // `Uuid::simple` is what the projection writes, so the hyphenated form is
    // wrong even though it names the same object. Accepting both would make two
    // locators for one control and the ambiguity rule would then be lying.
    let hyphenated = "6f9619ff-8b86-d011-b842-00cf4fc964ff";
    assert!(matches!(
        LocatorPattern::parse(&format!("koru:w1/v1/n:{hyphenated}#text")),
        Err(LocatorError::ModelId(_))
    ));
    assert!(matches!(
        LocatorPattern::parse("koru:w1/v1/n:6F9619FF8B86D011B42D00CF4FC964FF#text"),
        Err(LocatorError::ModelId(_)),
    ));
    assert!(LocatorPattern::parse("koru:w1/v1/n:6f9619ff8b86d011b42d00cf4fc964ff#text").is_ok());
}

#[test]
fn nothing_that_is_not_a_koru_locator_ever_matches() {
    let pattern = LocatorPattern::parse("koru:w*/v*/s:window#window").expect("a pattern");
    // The two that actually turn up on a live desktop: a control belonging to
    // another application, and `accesskit_windows`' placeholder root, which has
    // no `author_id` at all and therefore reads back as the empty string.
    for other in ["", "Notepad", "TextEditPane", "koru", "koru:"] {
        assert!(!pattern.matches(other), "{other:?} is not a Koru locator");
    }
}

#[test]
fn the_window_number_a_root_carries_is_what_every_later_locator_is_scoped_by() {
    assert_eq!(window_slot_of("koru:w4/v0/s:window#window"), Some(4));
    // A placeholder root, an unrelated application, or a pattern rather than a
    // locator: each is "I do not know which window this is", and the caller
    // must refuse rather than fall back to 1.
    assert_eq!(window_slot_of(""), None);
    assert_eq!(window_slot_of("koru:w*/v0/s:window#window"), None);
}
