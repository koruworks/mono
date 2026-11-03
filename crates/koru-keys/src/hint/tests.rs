// SPDX-License-Identifier: MPL-2.0

use super::*;

/// The whole point of the `Id` form: the printed chord is the bound chord, so
/// it is right on whatever platform this is compiled for.
#[test]
fn an_id_prints_what_the_registry_binds() {
    assert_eq!(
        show_hint(ShortcutId::NewTab, Section::Menu).as_deref(),
        Some(crate::label(&registry::get(ShortcutId::NewTab).chord).as_str())
    );
}

/// A platform chord prints itself — there is no row to look it up in, which is
/// what `Literal` means.
#[test]
fn a_literal_prints_itself() {
    assert_eq!(
        show_hint(Hinted::Literal("Esc"), Section::Menu).as_deref(),
        Some("Esc")
    );
}

/// **`None`, never an empty string.** A caller that got `""` would lay it out
/// and leave the gap where the chord used to be.
///
/// The sections are independent, which is the reason there is a `Section` at
/// all: turning menus quiet must not take the tooltips with them.
///
/// **It flips `Heading` and nothing else**, because this is a process global and
/// the other tests in this file read `Menu` — cargo runs them in parallel, so a
/// test that turned `Menu` off would fail one of them at random rather than
/// itself.
#[test]
fn a_section_can_be_turned_off_on_its_own() {
    set_shown(Section::Heading, false);
    assert_eq!(show_hint(ShortcutId::NewTab, Section::Heading), None);
    assert!(
        show_hint(ShortcutId::NewTab, Section::Menu).is_some(),
        "the menus are a different section"
    );
    assert!(!shown(Section::Heading));
    set_shown(Section::Heading, true);
    assert!(shown(Section::Heading));
}

/// Every section has its own bit. A `Section` added without one would silently
/// share another's switch, which is the failure this shape invites.
#[test]
fn every_section_has_a_bit_of_its_own() {
    let bits: Vec<u32> = [
        Section::Menu,
        Section::Tooltip,
        Section::Heading,
        Section::Guide,
    ]
    .into_iter()
    .map(Section::bit)
    .collect();
    for (i, a) in bits.iter().enumerate() {
        assert!(a.is_power_of_two(), "{i}");
        for b in &bits[i + 1..] {
            assert_ne!(a, b, "two sections share a switch");
        }
    }
}

/// A chord whose surface the build hides is never printed, in any section —
/// and its registry row is still there (koru-features).
#[test]
fn a_hidden_surfaces_chord_is_not_advertised() {
    let hidden = [
        ShortcutId::ToggleAiPane,
        ShortcutId::CopyCloudDetails,
        ShortcutId::Reminders,
        ShortcutId::Delegate,
    ];
    koru_features::testing::with(koru_features::Features::ALL_OFF, || {
        for id in hidden {
            assert!(!id.shown_in_product(), "{id:?}");
            for section in [Section::Menu, Section::Tooltip, Section::Guide] {
                assert_eq!(show_hint(id, section), None, "{id:?}");
            }
            let _ = registry::get(id);
        }
        // The rail's digits belong to its slots, not to its views: no switch
        // hides one, because a hidden view takes no slot at all.
        for id in registry::RAIL_SLOTS {
            assert!(id.shown_in_product(), "{id:?}");
        }
    });
    for id in hidden {
        assert!(id.shown_in_product(), "{id:?} with every surface on");
    }
}
