// SPDX-License-Identifier: MPL-2.0

use super::*;

/// **The test this table exists for.**
///
/// A chord claimed by an earlier layer never reaches a later one. `mainview
/// ::keys` carries a comment saying `Ctrl+Shift+N` is not free because it is the
/// document's Narrow mode, and that claiming it "would not conflict, it would
/// silently remove Narrow". That reasoning was correct and enforced by nothing
/// but the next person reading it.
///
/// Run per platform, because the chords are platform-resolved: a collision can
/// exist on macOS and not on Windows.
#[test]
fn no_shortcut_shadows_a_lower_layer() {
    for (i, a) in REGISTRY.iter().enumerate() {
        for b in REGISTRY.iter().skip(i + 1) {
            // Terminal and workspace layers belong to different binaries.
            // Both still collide with Shell because window-level shortcuts run
            // before either branch.
            if !layers_can_coexist(a.layer, b.layer) {
                continue;
            }
            if a.chord != b.chord {
                continue;
            }
            // A pair that says it overlaps, in either direction, is allowed.
            if a.shares_chord_with == Some(b.id) || b.shares_chord_with == Some(a.id) {
                continue;
            }
            let (first, second) = if a.layer <= b.layer { (a, b) } else { (b, a) };
            panic!(
                "{:?} ({:?}) and {:?} ({:?}) are both `{}`.\n\
                 {:?} resolves first, so {:?} would never fire.\n\
                 Give one of them another chord, or — if they genuinely cannot \
                 coexist in one window — set `shares_chord_with` and say why.",
                a.id,
                a.layer,
                b.id,
                b.layer,
                crate::format::label(&a.chord),
                first.layer,
                second.id,
            );
        }
    }
}

fn layers_can_coexist(left: Layer, right: Layer) -> bool {
    left == Layer::Shell
        || right == Layer::Shell
        || (left == Layer::Terminal) == (right == Layer::Terminal)
}

#[test]
fn terminal_shortcuts_are_complete_and_do_not_claim_ctrl_tab() {
    let ids: std::collections::BTreeSet<_> = super::of_layer(Layer::Terminal)
        .map(|entry| entry.id)
        .collect();
    assert_eq!(
        ids,
        std::collections::BTreeSet::from([
            ShortcutId::TerminalCopyOrInterrupt,
            ShortcutId::TerminalCopySelection,
            ShortcutId::TerminalPaste,
            ShortcutId::TerminalPasteAlternate,
            // The tab lane's one (SP-10). It resolves at the window layer,
            // above `Layer::Terminal`'s own, and it is the only chord the lane
            // took: `Ctrl+T` and `Ctrl+W` are readline's and stay the PTY's.
            ShortcutId::TerminalSettings,
            // The explore pane's, at the window layer for the same reason
            // (owner, 2026-09-02: the chord did nothing in the Terminal because
            // no row here claimed it).
            ShortcutId::TerminalToggleExplore,
        ])
    );
    assert!(super::of_layer(Layer::Terminal).all(|entry| {
        entry.chord != Chord::new(named(NamedKey::Tab), CTRL)
            && entry.chord != Chord::new(named(NamedKey::Tab), CTRL_SHIFT)
    }));
    // **And it claims neither of readline's own.** `Ctrl+T` transposes
    // characters and `Ctrl+W` deletes the previous word; a window layer that
    // took either would take it from the PTY, which is the rule that keeps
    // `Ctrl+C` working inverted.
    for taken in ['t', 'w'] {
        assert!(
            super::of_layer(Layer::Terminal)
                .all(|entry| entry.chord != Chord::new(On::Letter(taken), PRIMARY)),
            "Ctrl+{taken} belongs to the shell inside the terminal"
        );
    }
}

/// **The Terminal's explore chord is the container's, and that is allowed.**
///
/// `layers_can_coexist` says why — the two are different binaries — and this
/// says it out loud, because "the same chord twice" is otherwise exactly the
/// shape `no_shortcut_shadows_a_lower_layer` exists to refuse.
#[test]
fn the_terminals_explore_chord_matches_the_containers_and_neither_shadows_the_other() {
    let terminal = get(ShortcutId::TerminalToggleExplore);
    let container = get(ShortcutId::ToggleExplore);
    assert_eq!(terminal.chord, container.chord, "one pane, one chord");
    assert_eq!(terminal.layer, Layer::Terminal);
    assert_eq!(container.layer, Layer::Container);
    assert!(
        !layers_can_coexist(terminal.layer, container.layer),
        "they never run in one window, which is what makes the pair legal"
    );
    assert!(
        terminal.shares_chord_with.is_none() && container.shares_chord_with.is_none(),
        "`shares_chord_with` is for a real overlap inside one binary"
    );
}

/// A declared overlap has to be mutual and cross-layer, or it is being used to
/// silence a real collision.
#[test]
fn a_declared_overlap_names_a_real_partner_in_another_layer() {
    for entry in REGISTRY {
        let Some(other_id) = entry.shares_chord_with else {
            continue;
        };
        let other = get(other_id);
        assert_eq!(
            entry.chord, other.chord,
            "{:?} says it shares a chord with {:?}, but they are not the same chord",
            entry.id, other_id
        );
        assert_ne!(
            entry.layer, other.layer,
            "{:?} and {:?} are in the same layer — one of them simply never fires",
            entry.id, other_id
        );
    }
}

/// Ids are unique, and every one is reachable through [`get`].
///
/// `get` panics rather than returning an `Option` on the strength of this.
#[test]
fn every_id_appears_exactly_once() {
    for entry in REGISTRY {
        let n = REGISTRY.iter().filter(|e| e.id == entry.id).count();
        assert_eq!(n, 1, "{:?} appears {n} times", entry.id);
        assert_eq!(get(entry.id).label, entry.label);
    }
}

/// A label is what a menu row reads. It must not carry the chord — that is
/// rendered from the chord itself, and a label spelling one out is a second
/// claim that can drift.
#[test]
fn a_label_never_spells_out_its_own_chord() {
    for entry in REGISTRY {
        for bad in ["Ctrl", "Cmd", "Alt", "Shift", "⌘", "⌥", "⇧", "⌃"] {
            assert!(
                !entry.label.contains(bad),
                "{:?}'s label {:?} spells out a modifier; the chord is rendered \
                 from `chord` so the two cannot disagree",
                entry.id,
                entry.label
            );
        }
    }
}

/// **Tab cycling is the literal Control key on both platforms**, and this is
/// the one entry allowed to be. ⌘+Tab is the macOS application switcher.
#[test]
fn tab_cycling_does_not_follow_the_primary_modifier() {
    let next = get(ShortcutId::NextTab);
    assert!(next.chord.mods.control_key());
    if cfg!(target_os = "macos") {
        assert!(
            !next.chord.mods.super_key(),
            "⌘+Tab is the OS application switcher and is not ours to take"
        );
    }
}

/// Back/forward is the other deliberate platform split: `⌥←` is word-left in
/// every macOS text field, and this is resolved at the shell before any view
/// sees the key.
#[test]
fn history_uses_the_platforms_own_chord() {
    let back = get(ShortcutId::HistoryBack).chord;
    if cfg!(target_os = "macos") {
        assert_eq!(back.on, On::Letter('['));
        assert!(crate::has_primary(back.mods));
    } else {
        assert_eq!(back.on, named(NamedKey::ArrowLeft));
        assert!(back.mods.alt_key());
    }
}

/// **The plain primary `S` and its shifted form are two different chords.**
///
/// *Save a snapshot* is `Ctrl+S` at the container; *Strikethrough* is
/// `Ctrl+Shift+S` at the editor, four layers down. `no_shortcut_shadows_a_lower_layer`
/// already refuses a real collision, but it can only compare the chords it is
/// given — it cannot say that the pair which *looks* like one was checked. This
/// says it: the two differ by Shift, and the container's is the unshifted one,
/// so claiming it does not silently remove Strikethrough the way `Ctrl+Shift+N`
/// would have removed Narrow.
#[test]
fn saving_a_snapshot_is_the_unshifted_s_and_leaves_strikethrough_alone() {
    let save = get(ShortcutId::SaveSnapshot);
    let strike = get(ShortcutId::Strikethrough);

    assert_eq!(save.layer, Layer::Container);
    assert_eq!(save.chord.on, On::Letter('s'));
    assert!(crate::has_primary(save.chord.mods));
    assert!(
        !save.chord.mods.shift_key(),
        "Save a snapshot is the plain primary S; the shifted form is Strikethrough"
    );

    assert_eq!(strike.layer, Layer::Editor);
    assert_eq!(strike.chord.on, On::Letter('s'));
    assert!(strike.chord.mods.shift_key());

    assert_ne!(
        save.chord, strike.chord,
        "these must stay two chords — the container resolves first, so one chord \
         would mean Strikethrough never fires"
    );
    assert!(save.shares_chord_with.is_none());
}

/// **Ticking a box is Enter's shifted primary form, and it leaves the
/// unshifted one alone.**
///
/// The same shape as the `S` pair above, and it matters more here because the
/// two are about the *same* row: `Ctrl+Enter` means something else in a
/// document (leave a code block, open a paragraph under a table or prose) and
/// `Ctrl+Shift+Enter` ticks the box the row draws. One chord would have meant
/// the second never fires — and a checklist item would be back to having no
/// keyboard route at all, which is the hole this entry was added to close
/// (Q-T2, overriding `docs/features/tasks/SPEC.md` §11 for this one action).
#[test]
fn ticking_a_box_is_the_shifted_enter_and_leaves_the_unshifted_one_alone() {
    let tick = get(ShortcutId::TickCheckbox);
    let done = get(ShortcutId::ToggleDone);

    assert_eq!(
        tick.layer,
        Layer::Editor,
        "the press it mirrors is the editor's"
    );
    assert_eq!(tick.chord.on, named(NamedKey::Enter));
    assert!(crate::has_primary(tick.chord.mods));
    assert!(tick.chord.mods.shift_key());

    assert_eq!(done.chord.on, named(NamedKey::Enter));
    assert!(!done.chord.mods.shift_key());

    assert_ne!(
        tick.chord, done.chord,
        "these must stay two chords — one key cannot both tick the box and \
         open a paragraph under the row"
    );
    assert!(tick.shares_chord_with.is_none());
}

/// **No standing layer above the editor binds Enter**, which is what makes the
/// shifted form reachable from inside a document at all.
///
/// `no_shortcut_shadows_a_lower_layer` refuses an exact collision; this refuses
/// the near miss that would matter next. The editor's Enter family is five
/// chords wide — plain, Shift, Alt, primary and primary+Shift — so an entry on
/// Enter in a layer that resolves first is worth stopping to think about rather
/// than discovering when one of the five stops firing. Delegation is the one
/// deliberate exception: its modal blocks the editor and declares the exact
/// overlap with `ToggleDone`, so those listeners never stand together.
#[test]
fn no_layer_above_the_editor_binds_enter() {
    for entry in REGISTRY {
        if entry.layer >= Layer::Editor {
            continue;
        }
        if entry.id == ShortcutId::Delegate {
            assert_eq!(entry.shares_chord_with, Some(ShortcutId::ToggleDone));
            continue;
        }
        assert_ne!(
            entry.chord.on,
            named(NamedKey::Enter),
            "{:?} ({:?}) binds Enter above the editor, whose Enter family is \
             plain, Shift, Alt, primary and primary+Shift",
            entry.id,
            entry.layer,
        );
    }
}

/// The generated table and `reference/SHORTCUTS.md` say the same thing.
///
/// **The rail's digits are its slots** (the user, 2026-11-03): slot *n*,
/// counted from zero, is `Alt+(n+1)` under the view modifier, eight of them,
/// and nothing past the eighth. `Alt+0` and `Alt+9` stay nobody's.
#[test]
fn the_rail_slots_are_the_view_digits_in_order() {
    for (index, id) in RAIL_SLOTS.into_iter().enumerate() {
        assert_eq!(rail_slot(index), Some(id));
        let entry = get(id);
        assert_eq!(entry.layer, Layer::Container, "{id:?}");
        let digit = char::from_digit(index as u32 + 1, 10).unwrap();
        assert_eq!(
            entry.chord,
            Chord::new(On::Letter(digit), crate::VIEW),
            "{id:?}"
        );
    }
    assert_eq!(
        rail_slot(RAIL_SLOTS.len()),
        None,
        "the ninth view has no digit"
    );
    for free in ['0', '9'] {
        let chord = Chord::new(On::Letter(free), crate::VIEW);
        assert!(
            !REGISTRY.iter().any(|entry| entry.chord == chord),
            "{free} under the view modifier is nobody's"
        );
    }
}

/// **The user guide could previously claim any chord it liked** — 118 mentions
/// that nothing verified. This is the same guarantee `every_documented_flag_parses`
/// gives the CLI, and the same snapshot shape: it fails loudly and tells you how
/// to regenerate rather than rewriting your tree behind your back.
///
/// Skipped off the platform the file was generated on, since the chords in it
/// are written the way *that* platform writes them.
#[test]
fn the_generated_shortcut_table_is_up_to_date() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../reference/SHORTCUTS.md");
    // Not a source checkout — a packaged build has no `docs/`.
    if !path.parent().is_some_and(|d| d.is_dir()) {
        return;
    }
    let want = markdown();
    if std::env::var_os("KORU_UPDATE_SHORTCUTS").is_some() {
        std::fs::write(&path, &want).expect("write reference/SHORTCUTS.md");
        return;
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    // The file is generated on one platform and read on another; only compare
    // where the chords are written the same way it was.
    if have.contains("⌘") != cfg!(target_os = "macos") {
        return;
    }
    assert_eq!(
        have.trim(),
        want.trim(),
        "reference/SHORTCUTS.md is out of date — regenerate it with:\n    \
         KORU_UPDATE_SHORTCUTS=1 cargo test -p koru-keys"
    );
}
