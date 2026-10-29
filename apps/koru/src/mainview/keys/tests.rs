// SPDX-License-Identifier: MPL-2.0

//! The container's chords, and the one it must never claim.

use super::*;
use winit::keyboard::NamedKey;

const NONE: ModifiersState = ModifiersState::empty();
const PRIMARY: ModifiersState = koru_keys::PRIMARY;
const PRIMARY_SHIFT: ModifiersState =
    ModifiersState::from_bits_truncate(koru_keys::PRIMARY.bits() | ModifiersState::SHIFT.bits());
const PRIMARY_ALT: ModifiersState = koru_keys::PRIMARY_ALT;

fn ch(c: &str) -> Key {
    Key::Character(c.into())
}

/// **F9 is the internal session recorder's**, and only where the recorder
/// exists: internal Windows builds. A plain `cargo build` is one
/// (`KORU_RELEASE_CHANNEL` defaults to `internal`), so here the chord
/// resolves; on a public or non-Windows build the key falls through exactly
/// as if unbound — the gate *is* the product, and the registry walker's named
/// exemption records that no single answer exists for every build.
#[test]
fn f9_resolves_only_where_the_recorder_exists() {
    let resolved = resolve(&Key::Named(NamedKey::F9), None, NONE);
    if cfg!(windows) && koru_channel::ReleaseChannel::this_build_is_internal() {
        assert_eq!(resolved, Some(Chord::ToggleRecording));
    } else {
        assert_eq!(resolved, None);
    }
    // Shift+F9 is nobody's, everywhere.
    assert_eq!(
        resolve(&Key::Named(NamedKey::F9), None, ModifiersState::SHIFT),
        None
    );
}

/// `Alt+1..8` are the explore column's eight views, in icon order; the
/// other digits under Alt are nobody's, and Ctrl+Alt+digit stays a heading.
#[test]
fn alt_digits_show_the_eight_views() {
    use crate::panes::explore::NavigatorMode;
    // The view modifier: Alt, or ⌃⌘ on a Mac (`koru_keys::VIEW`, M42).
    const ALT: ModifiersState = koru_keys::VIEW;
    assert_eq!(
        resolve(&ch("1"), Some("1"), ALT),
        Some(Chord::ShowNavigator(NavigatorMode::Documents))
    );
    assert_eq!(
        resolve(&ch("2"), Some("2"), ALT),
        Some(Chord::ShowNavigator(NavigatorMode::Files))
    );
    assert_eq!(
        resolve(&ch("3"), Some("3"), ALT),
        Some(Chord::ShowNavigator(NavigatorMode::Tasks))
    );
    // Chats sits right of Tasks, so Agents and Artifacts moved up one (R3).
    assert_eq!(
        resolve(&ch("4"), Some("4"), ALT),
        Some(Chord::ShowNavigator(NavigatorMode::Chats))
    );
    assert_eq!(
        resolve(&ch("5"), Some("5"), ALT),
        Some(Chord::ShowNavigator(NavigatorMode::Agents))
    );
    assert_eq!(
        resolve(&ch("6"), Some("6"), ALT),
        Some(Chord::ShowNavigator(NavigatorMode::Artifacts))
    );
    // Canvases is the seventh icon, so Alt+7 (CANVAS P1.3), and Socials the
    // eighth, so Alt+8; the digit past the last view is still nobody's.
    assert_eq!(
        resolve(&ch("7"), Some("7"), ALT),
        Some(Chord::ShowNavigator(NavigatorMode::Canvases))
    );
    assert_eq!(
        resolve(&ch("8"), Some("8"), ALT),
        Some(Chord::ShowNavigator(NavigatorMode::Socials))
    );
    assert_eq!(resolve(&ch("9"), Some("9"), ALT), None);
    assert_eq!(
        resolve(&ch("1"), Some("1"), PRIMARY_ALT),
        None,
        "Ctrl+Alt+1 is the editor's heading"
    );
    assert_eq!(
        resolve(&ch("1"), Some("1"), PRIMARY),
        Some(Chord::SelectTab(0)),
        "and Ctrl+1 the first tab"
    );
}

#[test]
fn the_container_chords_resolve() {
    assert_eq!(
        resolve(&Key::Named(NamedKey::F6), None, NONE),
        Some(Chord::FocusChrome)
    );
    assert_eq!(resolve(&ch("t"), Some("t"), PRIMARY), Some(Chord::NewTab));
    assert_eq!(
        resolve(&ch("a"), Some("a"), PRIMARY_SHIFT),
        Some(Chord::AddContent)
    );
    assert_eq!(resolve(&ch("w"), Some("w"), PRIMARY), Some(Chord::CloseTab));
    assert_eq!(resolve(&ch("p"), Some("p"), PRIMARY), Some(Chord::Search));
    assert_eq!(
        resolve(&ch("f"), Some("f"), PRIMARY_SHIFT),
        Some(Chord::Search)
    );
    assert_eq!(
        resolve(&ch("p"), Some("p"), PRIMARY_ALT),
        Some(Chord::ProfilesPopover)
    );
    // **Shifted**: plain Ctrl+E is the editor's align-centre. See `resolve`.
    assert_eq!(
        resolve(&ch("e"), Some("e"), PRIMARY_SHIFT),
        Some(Chord::ToggleSidebar)
    );
    assert_eq!(
        resolve(&ch("e"), Some("e"), PRIMARY),
        None,
        "plain Ctrl+E belongs to the editor now, and must fall through"
    );
    assert_eq!(
        resolve(&ch("j"), Some("j"), PRIMARY),
        Some(Chord::ToggleContext)
    );
    assert_eq!(
        resolve(&ch("j"), Some("j"), PRIMARY_SHIFT),
        Some(Chord::ToggleAi)
    );
    // Ctrl+S names a snapshot; the shifted form stays the editor's
    // strikethrough and must fall through.
    assert_eq!(
        resolve(&ch("s"), Some("s"), PRIMARY),
        Some(Chord::SaveSnapshot)
    );
    assert_eq!(resolve(&ch("s"), Some("s"), PRIMARY_SHIFT), None);
}

/// **Ctrl+S is the container's; Ctrl+Shift+S must still fall through.**
///
/// The container resolves before the active tab, so a shifted arm here would
/// take Strikethrough away from the editor without any conflict being reported
/// — the `Ctrl+Shift+N`/Narrow trap that `resolve`'s own comment warns about.
/// Do not "fix" a failure here by adding an `'s'` arm to the shifted branch.
#[test]
fn ctrl_s_saves_a_snapshot_and_the_shifted_form_stays_the_editors() {
    assert_eq!(
        resolve(&ch("s"), Some("s"), PRIMARY),
        Some(Chord::SaveSnapshot)
    );
    assert_eq!(
        resolve(&ch("s"), Some("s"), PRIMARY_SHIFT),
        None,
        "Ctrl+Shift+S is the editor's Strikethrough and must reach it"
    );
    // The control-code form Windows actually delivers: Ctrl+S is 0x13.
    assert_eq!(
        resolve(&ch("\u{13}"), Some("\u{13}"), PRIMARY),
        Some(Chord::SaveSnapshot)
    );
}

/// Ctrl+letter arrives as an ASCII control code on Windows — Ctrl+T is 0x14,
/// not `Character("t")`. This is the form that actually shows up in the wild.
#[test]
fn a_control_code_resolves_the_same_as_the_letter() {
    assert_eq!(
        resolve(&ch("\u{14}"), Some("\u{14}"), PRIMARY),
        Some(Chord::NewTab)
    );
    assert_eq!(
        resolve(&ch("\u{17}"), Some("\u{17}"), PRIMARY),
        Some(Chord::CloseTab)
    );
    assert_eq!(
        resolve(&ch("\u{10}"), Some("\u{10}"), PRIMARY),
        Some(Chord::Search)
    );
}

#[test]
fn the_digits_select_by_position() {
    assert_eq!(
        resolve(&ch("1"), Some("1"), PRIMARY),
        Some(Chord::SelectTab(0))
    );
    assert_eq!(
        resolve(&ch("9"), Some("9"), PRIMARY),
        Some(Chord::SelectTab(8))
    );
    assert_eq!(
        resolve(&ch("0"), Some("0"), PRIMARY),
        None,
        "there is no tab zero"
    );
}

/// Nothing without Ctrl is ours, or typing `t` would open a tab.
#[test]
fn a_bare_letter_is_not_a_container_chord() {
    for c in ["t", "w", "e", "j", "1"] {
        assert_eq!(resolve(&ch(c), Some(c), NONE), None, "{c}");
    }
}

/// The shifted forms the container must **not** claim: `Ctrl+Shift+N` is the
/// document's Narrow width, and a chord claimed here never reaches the editor.
/// Ctrl+Shift+J is deliberately claimed by the workspace AI pane and is asserted
/// above.
///
/// `e` is deliberately not in this list any more: the pane's toggle moved onto
/// Ctrl+Shift+E so the editor could have Word's plain Ctrl+E for align-centre.
/// It is asserted the other way round in `the_container_chords_resolve`.
#[test]
fn the_shifted_forms_belong_to_someone_else() {
    for c in ["n"] {
        assert_eq!(
            resolve(&ch(c), Some(c), PRIMARY_SHIFT),
            None,
            "Ctrl+Shift+{c}"
        );
    }
}

/// The browser's shape: `Ctrl+T` opens a tab, `Ctrl+Shift+T` brings back the
/// one closed last. Keeping a window on top moved to `Ctrl+Alt+A`.
#[test]
fn ctrl_shift_t_reopens_the_closed_tab() {
    assert_eq!(
        resolve(&ch("t"), Some("t"), PRIMARY_SHIFT),
        Some(Chord::ReopenClosedTab)
    );
}

/// **The one exception**, and it is the browser's shape: `Ctrl+W` closes a tab,
/// so `Ctrl+Shift+W` closes every tab. Nothing below the container binds it.
#[test]
fn ctrl_shift_w_closes_every_tab() {
    assert_eq!(
        resolve(&ch("w"), Some("w"), PRIMARY_SHIFT),
        Some(Chord::CloseAllTabs)
    );
    // **Not N.** `Ctrl+Shift+N` is the document's Narrow view mode, and this
    // function runs before the active tab sees anything — so claiming it here
    // would not conflict with Narrow, it would silently remove it.
    assert_eq!(resolve(&ch("n"), Some("n"), PRIMARY_SHIFT), None);
}

/// `Ctrl+Shift+O` opens the workspace manager. `O` is free at every level: the
/// editor's table binds neither it nor its shifted form.
#[test]
fn ctrl_shift_o_opens_the_workspace_manager() {
    assert_eq!(
        resolve(&ch("o"), Some("o"), PRIMARY_SHIFT),
        Some(Chord::ManageWorkspaces)
    );
    // The unshifted form belongs to whatever is underneath.
    assert_eq!(resolve(&ch("o"), Some("o"), PRIMARY), None);
}

#[test]
fn ctrl_alt_p_opens_the_profiles_switcher() {
    assert_eq!(
        resolve(&ch("p"), Some("p"), PRIMARY_ALT),
        Some(Chord::ProfilesPopover)
    );
    assert_eq!(
        resolve(&ch("p"), Some("p"), PRIMARY_SHIFT),
        None,
        "Ctrl+Shift+P falls through to the document's Page-width command"
    );
}

/// `Ctrl+Alt+W` shows the workspaces switcher; W and P are the two letters the
/// container takes out of the Ctrl+Alt family.
///
/// The rest of that family belongs to the document — `Ctrl+Alt+`*digit* is a
/// heading, and this function runs first, so an over-eager arm here would remove
/// one rather than conflict with it.
#[test]
fn ctrl_alt_w_opens_the_workspaces_switcher() {
    assert_eq!(
        resolve(&ch("w"), Some("w"), PRIMARY_ALT),
        Some(Chord::WorkspacesPopover)
    );
    // The headings, and anything else with Alt held, still fall through.
    for c in ["1", "0", "n", "t", "o"] {
        assert_eq!(resolve(&ch(c), Some(c), PRIMARY_ALT), None, "Ctrl+Alt+{c}");
    }
    // Ctrl+Alt+Shift+W is nobody's, and must not be mistaken for this one.
    let all_three =
        ModifiersState::from_bits_truncate(PRIMARY_ALT.bits() | ModifiersState::SHIFT.bits());
    assert_eq!(resolve(&ch("w"), Some("w"), all_three), None);
}

/// **The one that matters.** Ctrl+I arrives as `NamedKey::Tab` on Windows, and
/// `ctrl_letter` maps it back to `'i'` so italic works. If the container ever
/// grows a Tab arm to implement Ctrl+Tab, this is what breaks — silently, and
/// in the editor rather than here. Tab cycling comes from `View::cycle_tab`,
/// which the shell resolves from `physical_key`.
#[test]
fn ctrl_tab_is_not_claimed_here_because_it_is_ctrl_i() {
    let tab = Key::Named(NamedKey::Tab);
    assert_eq!(
        resolve(&tab, None, PRIMARY),
        None,
        "claiming this would turn Ctrl+I from italic into a tab switch"
    );
    // And plain Ctrl+I is likewise left to the editor.
    assert_eq!(resolve(&ch("i"), Some("i"), PRIMARY), None);
}

// ------------------------------------------------------------------ the trash

/// **Delete is never a container shortcut.** In an editor it keeps its editing
/// meaning — plain Delete removes a character or a row selection, and
/// `Ctrl+Delete` deletes a word; in Explore, moving a node to Trash is an
/// explicit menu action. Claiming any of these here would take them away
/// silently, because this function runs before the active tab sees anything.
#[test]
fn delete_with_any_supported_modifiers_is_not_a_container_chord() {
    let del = Key::Named(NamedKey::Delete);
    assert_eq!(resolve(&del, None, NONE), None, "the editor's");
    assert_eq!(resolve(&del, None, PRIMARY), None, "also the editor's");
    assert_eq!(
        resolve(&del, None, PRIMARY_SHIFT),
        None,
        "not a trash chord"
    );
    assert_eq!(
        resolve(&del, None, ModifiersState::SHIFT),
        None,
        "shift alone is not ours either"
    );
}

/// And it does not shadow the shifted letters that share the branch.
#[test]
fn the_shifted_letters_still_resolve() {
    assert_eq!(
        resolve(&ch("a"), Some("a"), PRIMARY_SHIFT),
        Some(Chord::AddContent)
    );
    assert_eq!(
        resolve(&ch("w"), Some("w"), PRIMARY_SHIFT),
        Some(Chord::CloseAllTabs)
    );
    assert_eq!(
        resolve(&ch("o"), Some("o"), PRIMARY_SHIFT),
        Some(Chord::ManageWorkspaces)
    );
    assert_eq!(
        resolve(&ch("p"), Some("p"), PRIMARY_ALT),
        Some(Chord::ProfilesPopover)
    );
}

/// **The registry is checked against this resolver, not copied from it.**
///
/// Every container-layer entry in `koru_keys::REGISTRY` must actually resolve
/// here. Without this the table would be a fifth record of the truth and would
/// drift from the four resolvers within a release.
///
/// `RenameDocument` is `F2` with no modifiers and resolves through the same
/// function, so it is covered too.
///
/// **`MoveDocument` is the one exemption, and it is named rather than skipped
/// by a rule.** Its row is a *label*: `Ctrl+M` on Windows is the same ASCII byte
/// as Enter, so the shell dispatches it off the physical `KeyM` and this
/// function never sees it — the same shape as `Ctrl+Tab`. Exempting it by id
/// means the next chord that quietly stops resolving still fails here.
#[test]
fn every_container_shortcut_in_the_registry_resolves_here() {
    use koru_keys::{Layer, On as KOn, ShortcutId, registry};
    for entry in registry::of_layer(Layer::Container) {
        // Ctrl+M is intentionally physical-key dispatch in the shell: on
        // Windows its logical value is the same control byte as Enter. It is
        // catalogued as container UI but cannot resolve through this logical
        // key function (the same exception as Ctrl+Tab).
        if entry.id == ShortcutId::MoveDocument {
            continue;
        }
        // **`Delegate` is the second exemption, and also named.** `Ctrl+Enter`
        // fires only inside the delegation modal, which is a blocker: it takes
        // every key before this resolver is reached, and binding the chord here
        // as well would give the same gesture two owners. The registry carries
        // it because the surface teaches it — that is what the table is for —
        // and `mainview::delegate::tests` drives it where it fires.
        if entry.id == ShortcutId::Delegate {
            continue;
        }
        // **`ToggleRecording` is the third.** F9 resolves only on internal
        // Windows builds — the gate *is* the product (a public build passes
        // the key through as unbound), so no single answer exists for every
        // build this suite compiles on. `f9_resolves_only_where_the_recorder_
        // exists` drives both halves of the conditional directly.
        if entry.id == ShortcutId::ToggleRecording {
            continue;
        }
        let key = match entry.chord.on {
            KOn::Named(n) => Key::Named(n),
            KOn::Letter(c) => Key::Character(c.to_string().into()),
        };
        let text = match entry.chord.on {
            KOn::Letter(c) => Some(c.to_string()),
            KOn::Named(_) => None,
        };
        assert!(
            resolve(&key, text.as_deref(), entry.chord.mods).is_some(),
            "the registry says {:?} is `{}`, but the container does not bind it \
             — one of the two moved without the other",
            entry.id,
            koru_keys::label(&entry.chord),
        );
    }
}

/// The other direction for the one thing that *can* be checked both ways: a
/// chord the container claims must not also be claimed by a layer below it,
/// which is what `no_shortcut_shadows_a_lower_layer` enforces inside the
/// registry. Here we only confirm the container's own chords are all recorded,
/// so a new one cannot be added without the collision check seeing it.
#[test]
fn the_container_binds_nothing_the_registry_has_not_recorded() {
    use koru_keys::{Layer, On as KOn, registry};
    let recorded: Vec<_> = registry::of_layer(Layer::Container)
        .map(|e| e.chord)
        .collect();
    // The letters and named keys the container is known to claim. A new arm in
    // `resolve` that is not in the registry fails here.
    // Every letter the container is known to claim. **A chord added to
    // `resolve` without a registry row fails here** — which is what this list
    // is for, so extend it whenever the container learns a new letter.
    // **`f` is deliberately not here**, and that is a finding rather than an
    // omission: the container binds `Ctrl+Shift+F` as a second chord for
    // Search and the registry has no row for it, so the collision check cannot
    // see it. Adding one means minting a `ShortcutId`, which is stable storage
    // the moment bindings are customisable — a decision, not a repair. Written
    // up in `docs/contributing/techdebt/README.md`; found by Plan 2.2 while adding `r`.
    let candidates = ['t', 'w', 'j', 'e', 'o', 'p', 's', 'a', 'r', ','];
    for c in candidates {
        for mods in [
            koru_keys::PRIMARY,
            koru_keys::PRIMARY_SHIFT,
            koru_keys::PRIMARY_ALT,
        ] {
            let key = Key::Character(c.to_string().into());
            if resolve(&key, Some(&c.to_string()), mods).is_none() {
                continue;
            }
            let chord = koru_keys::Chord::new(KOn::Letter(c), mods);
            assert!(
                recorded.contains(&chord),
                "the container binds `{}` but the registry has no row for it, \
                 so the collision check cannot see it",
                koru_keys::label(&chord),
            );
        }
    }
}

/// **The simplified product's chords** (koru-features all off): the hidden
/// surfaces' chords fall through as if unbound, the view digits skip rather
/// than renumber, and the rest resolve unchanged.
#[test]
fn hidden_surfaces_chords_fall_through() {
    // The view modifier: Alt, or ⌃⌘ on a Mac (`koru_keys::VIEW`, M42).
    const ALT: ModifiersState = koru_keys::VIEW;
    koru_features::testing::with(koru_features::Features::ALL_OFF, || {
        assert_eq!(
            resolve(&ch("1"), Some("1"), ALT),
            Some(Chord::ShowNavigator(NavigatorMode::Documents))
        );
        assert_eq!(
            resolve(&ch("2"), Some("2"), ALT),
            Some(Chord::ShowNavigator(NavigatorMode::Files))
        );
        for digit in ["3", "4", "5", "6", "7", "8"] {
            assert_eq!(resolve(&ch(digit), Some(digit), ALT), None, "Alt+{digit}");
        }
        assert_eq!(resolve(&ch("j"), Some("j"), PRIMARY_SHIFT), None, "AI pane");
        assert_eq!(
            resolve(&ch("r"), Some("r"), PRIMARY_SHIFT),
            None,
            "reminders"
        );
        assert_eq!(
            resolve(&ch("k"), Some("k"), PRIMARY_ALT),
            None,
            "Cloud details"
        );
        assert_eq!(
            resolve(&ch("j"), Some("j"), PRIMARY_ALT),
            Some(Chord::NextContextSection)
        );
        assert_eq!(resolve(&ch("t"), Some("t"), PRIMARY), Some(Chord::NewTab));
    });
    // One flag on brings back only its own digit.
    koru_features::testing::with(
        koru_features::Features {
            ai: true,
            ..koru_features::Features::ALL_OFF
        },
        || {
            assert_eq!(resolve(&ch("3"), Some("3"), ALT), None);
            assert_eq!(
                resolve(&ch("4"), Some("4"), ALT),
                Some(Chord::ShowNavigator(NavigatorMode::Chats))
            );
            assert_eq!(
                resolve(&ch("j"), Some("j"), PRIMARY_SHIFT),
                Some(Chord::ToggleAi)
            );
        },
    );
    // Canvases and Socials keep their own digits: Alt+7 and Alt+8 skip, never
    // renumber, and each comes back with its own flag alone.
    koru_features::testing::with(
        koru_features::Features {
            socials: true,
            ..koru_features::Features::ALL_OFF
        },
        || {
            assert_eq!(resolve(&ch("7"), Some("7"), ALT), None);
            assert_eq!(
                resolve(&ch("8"), Some("8"), ALT),
                Some(Chord::ShowNavigator(NavigatorMode::Socials))
            );
        },
    );
    koru_features::testing::with(
        koru_features::Features {
            canvas: true,
            ..koru_features::Features::ALL_OFF
        },
        || {
            assert_eq!(
                resolve(&ch("7"), Some("7"), ALT),
                Some(Chord::ShowNavigator(NavigatorMode::Canvases))
            );
            assert_eq!(resolve(&ch("8"), Some("8"), ALT), None);
        },
    );
}

/// **On a Mac, ⌥digit types and switches nothing** — `¡ ™ £ ¢ ∞ §` stay
/// characters; the views are ⌃⌘digit (M42).
#[cfg(target_os = "macos")]
#[test]
fn option_digits_type_on_a_mac_and_control_command_digits_switch() {
    use crate::panes::explore::NavigatorMode;
    for (key, text) in [("1", "1"), ("¡", "¡"), ("2", "™")] {
        assert_eq!(
            resolve(&ch(key), Some(text), ModifiersState::ALT),
            None,
            "⌥ {key}"
        );
    }
    let control_command = ModifiersState::CONTROL | ModifiersState::SUPER;
    assert_eq!(
        resolve(&ch("1"), Some("1"), control_command),
        Some(Chord::ShowNavigator(NavigatorMode::Documents))
    );
    assert_eq!(
        resolve(&ch("2"), Some("2"), control_command),
        Some(Chord::ShowNavigator(NavigatorMode::Files))
    );
}
