// SPDX-License-Identifier: MPL-2.0

//! The chords the container claims before the active tab sees anything.
//!
//! Every one was checked against `koru_editor::input::keymap::DEFAULT` and the
//! one the shell already owns (Ctrl+Shift+T). Where only the Ctrl+Shift
//! form is bound in the editor — Ctrl+Shift+E is align-centre — the plain form
//! is free, which is why each arm below also requires `!shift`.
//!
//! **Ctrl+Tab is not here, and cannot be.** On Windows Ctrl+I arrives as
//! `NamedKey::Tab`, and `keymap::ctrl_letter` maps it back to `'i'` so italic
//! works — deliberately, and pinned by a test in the editor. The two are
//! indistinguishable from a logical key, so tab cycling is resolved by the shell
//! from `physical_key` and arrives as `View::cycle_tab`. Do not add a
//! `NamedKey::Tab` arm here to "fix" it; that breaks Ctrl+I instead.
//!
//! **Ctrl+M is not resolved here.** On Windows it is `0x0D`, the same byte as
//! Enter, so the shell claims the physical `KeyM` and hands down
//! `View::open_move_to` — the same shape as Ctrl+Tab / `cycle_tab`.

use koru_editor::input::keymap::ctrl_letter;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::panes::explore::NavigatorMode;

/// What a container-level chord asks for.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Chord {
    /// Close every tab. What is left is one welcome tab, the same as closing
    /// the last one by hand — never an empty window.
    CloseAllTabs,
    NewTab,
    /// Open the New… launcher, the tab strip's `+`.
    New,
    CloseTab,
    /// Ctrl+P or Ctrl+Shift+F — global search in this window.
    Search,
    /// Select by position, zero-based. Ctrl+1 is the first.
    SelectTab(usize),
    ToggleSidebar,
    ToggleContext,
    /// Ctrl+Shift+J opens or closes the workspace's Koru AI pane.
    ToggleAi,
    /// Ctrl+, opens the singleton Settings tab.
    OpenSettings,
    /// Rename what is under the pointer in the pane, or the document showing.
    /// F2, which is what every file list on this platform uses.
    Rename,
    /// `Alt+H` — Home, the one view with a chord of its own (E69). Opens the
    /// column if it is closed: a view you asked for by name is a view you
    /// meant to see.
    ShowNavigator(NavigatorMode),
    /// `Alt+1` … `Alt+8` — the view in that slot down the rail, counted from
    /// zero here, **whichever view the person has put there** (the user,
    /// 2026-11-03: the chords are per slot, not per app). Which view that is
    /// is the rail's to say (`ExploreView::rail_mode_at`), so the container
    /// answers it; a slot with no view in it is not claimed at all.
    ShowRailSlot(usize),
    /// Ctrl+Shift+O — the workspace manager. `O` is free at every level: the
    /// editor binds neither it nor its shifted form.
    ManageWorkspaces,
    /// Ctrl+S — ask to save a named snapshot of the document showing.
    ///
    /// **The plain form, and only the plain form.** `Ctrl+Shift+S` is the
    /// editor's Strikethrough; because this function runs before the active tab
    /// sees anything, claiming the shifted form here would not conflict, it
    /// would silently remove Strikethrough — the `Ctrl+Shift+N` trap one arm
    /// down, which is why each arm below requires `!shift`.
    SaveSnapshot,
    /// Ctrl+Alt+W — the workspaces list in the explore header, opened from the
    /// keyboard without changing whether the pane itself is open.
    WorkspacesPopover,
    /// Ctrl+Alt+P — the profiles list in the Explore header, opened from the
    /// keyboard without changing whether the pane itself is open.
    ProfilesPopover,
    /// Ctrl+Alt+J — step the context column to its next section, opening the
    /// column if it is shut. A section you asked for by name is a section you
    /// meant to see, the same bargain `ShowNavigator` makes.
    NextContextSection,
    /// Ctrl+Alt+K — copy what the context column's Cloud tab is showing.
    CopyCloudDetails,
    /// F6 moves focus into window chrome without stealing Tab indentation.
    FocusChrome,
    /// F9 — start/stop the internal session recorder
    /// (the internal session-recorder design). Resolved only on internal Windows
    /// builds; everywhere else F9 falls through to whatever is underneath.
    ToggleRecording,
    /// Ctrl+Shift+R — the reminders on the item the caret is in: reopen the one
    /// already saved, or start a new one when there is none.
    ///
    /// **The shifted form, and only the shifted form.** `Ctrl+R` is the
    /// editor's Align right, and because this function runs before the active
    /// tab sees anything, claiming the plain form here would not conflict — it
    /// would silently remove alignment, which is the `Ctrl+Shift+N` trap the
    /// comment below spells out.
    Reminders,
    /// Ctrl+Shift+T - bring back the tab closed last.
    ReopenClosedTab,
}

impl Chord {
    /// Whether this build shows the surface the chord opens
    /// (`koru_features`, guide/contributing/FEATURE-FLAGS.md). A hidden one does
    /// not resolve at all, so the key falls through to the tab underneath
    /// exactly as an unbound key would.
    ///
    /// **The view digits count only what the rail draws.** They are the
    /// rail's slots, and a view this build hides takes no slot: with Tasks
    /// hidden, `Alt+3` is whatever sits third. So no switch hides a digit
    /// here; a digit past the last view falls through at the container
    /// (`MainView::key`), as an unbound key would. Home's `Alt+H` is its own:
    /// with Home hidden for now (`koru_features::HOME_SHOWN`) it does nothing.
    pub fn shown_in_product(self) -> bool {
        match self {
            Chord::ShowNavigator(mode) => mode.shown_in_product(),
            Chord::ToggleAi => koru_features::ai(),
            Chord::Reminders => koru_features::reminders(),
            Chord::CopyCloudDetails => koru_features::cloud_sync(),
            _ => true,
        }
    }
}

/// Resolve a key press, or `None` if it belongs to the tab underneath — or
/// opens a surface this build hides ([`Chord::shown_in_product`]).
pub fn resolve(key: &Key, text: Option<&str>, mods: ModifiersState) -> Option<Chord> {
    resolve_any(key, text, mods).filter(|chord| chord.shown_in_product())
}

/// Every chord the container binds, whatever this build shows.
fn resolve_any(key: &Key, text: Option<&str>, mods: ModifiersState) -> Option<Chord> {
    // Resolved here rather than in the editor's table for the same reason: a
    // welcome tab has no editor to route it to, and F2 over the pane has to
    // work whatever the tab underneath happens to be.
    if mods.is_empty() && key == &Key::Named(NamedKey::F2) {
        return Some(Chord::Rename);
    }
    if mods.is_empty() && key == &Key::Named(NamedKey::F6) {
        return Some(Chord::FocusChrome);
    }
    // **F9 is internal tooling, not a shortcut.** It resolves only where the
    // session recorder exists — internal Windows builds — so a public build
    // passes the key through exactly as if nothing were bound, and the
    // registry row is `internal_only` so SHORTCUTS.md never teaches it.
    if cfg!(windows)
        && koru_channel::ReleaseChannel::this_build_is_internal()
        && mods.is_empty()
        && key == &Key::Named(NamedKey::F9)
    {
        return Some(Chord::ToggleRecording);
    }
    // **The view modifier, and only on the view digits** — Alt alone, or ⌃⌘ on
    // a Mac, where ⌥digit types a character (`koru_keys::VIEW`, M42). Ahead of
    // the primary gate because Alt alone carries no primary modifier and ⌃⌘
    // carries the foreign one — `Ctrl+Alt+digit` is a heading and `Ctrl+digit`
    // a tab, and neither arrives here as exactly this set.
    if mods == koru_keys::VIEW {
        let digit = match key {
            Key::Character(c) => c.chars().next(),
            _ => text.and_then(|t| t.chars().next()),
        };
        return match digit? {
            // Home is its letter, not a digit (the review's note, E69).
            'h' | 'H' => Some(Chord::ShowNavigator(NavigatorMode::Home)),
            // The rail's slots: `Alt+1` is the first view down it. `Alt+0`
            // and `Alt+9` are nobody's (`koru_keys::RAIL_SLOTS`).
            d @ '1'..='8' => Some(Chord::ShowRailSlot(d as usize - '1' as usize)),
            _ => None,
        };
    }
    // **The platform's primary modifier, not Control.** On macOS that is ⌘, and
    // the foreign one is refused outright: Ctrl+T on a Mac must not open a tab,
    // because Ctrl there is a modifier with its own meanings rather than noise.
    if !koru_keys::has_primary(mods) || koru_keys::has_foreign_primary(mods) {
        return None;
    }
    // Ctrl+Alt belongs to the layers below — Ctrl+Alt+digit is a heading — and
    // the container claims four letters of it.
    //
    // **`Ctrl+Alt+letter` only arrives at all because the shell rebuilds it.**
    // On Windows Ctrl+Alt is AltGr, so a layout with no AltGr entry for `W`
    // (the US one included) reports `Key::Unidentified` with no text and a
    // chord written on the logical key never matches — which is why the page
    // widths gave up `Ctrl+Alt+P/N/W` for `Ctrl+Shift`, and why `SetHeading`
    // sits on digits. `alt_letter` in `koru-workspace-host` repairs it from the
    // physical key, the same way `alt_digit` has always repaired the headings.
    // Its cost is honest and unavoidable: on a layout where either physical
    // letter has an AltGr character, that character wins and its switcher
    // chord does not fire.
    if mods.alt_key() {
        return match ctrl_letter(key, text)? {
            'w' if !mods.shift_key() => Some(Chord::WorkspacesPopover),
            'p' if !mods.shift_key() => Some(Chord::ProfilesPopover),
            'j' if !mods.shift_key() => Some(Chord::NextContextSection),
            'k' if !mods.shift_key() => Some(Chord::CopyCloudDetails),
            _ => None,
        };
    }
    // The chords that want Shift:
    //
    // - `Ctrl+W` closes a tab, so `Ctrl+Shift+W` closing *every* tab is the
    //   shape people already expect from a browser.
    // - **The explore pane is `Ctrl+Shift+E`, not `Ctrl+E`.** It gave the plain
    //   form up so the editor could have Word's `Ctrl+L`/`Ctrl+E`/`Ctrl+R`
    //   alignment — `Ctrl+Shift+L` is grabbed as a global hotkey by enough
    //   Windows utilities that it never reaches the window on a machine running
    //   one, and nothing in a key map can answer for a key the process is never
    //   handed. `Ctrl+Shift+E` is VS Code's Explorer, so the pane landed on the
    //   chord its own tradition already spells that way.
    // - `Ctrl+Shift+O` opens the workspace manager. `O` is free at every level.
    //
    // The container resolving first is what forced the trade: a chord claimed
    // here never reaches the editor at all.
    //
    // **`Ctrl+Shift+N` is not free**, whatever the switcher used to print beside
    // "New workspace". It is the document's Narrow view mode, one of the
    // N/G/P family — and because this function runs *before* the active tab
    // sees anything, claiming it here would not conflict, it would silently
    // remove Narrow. `ctrl_shift_w_closes_every_tab` is what catches that; do
    // not "fix" a failure there by relaxing the assertion.
    if mods.shift_key() {
        return match ctrl_letter(key, text)? {
            'w' => Some(Chord::CloseAllTabs),
            'e' => Some(Chord::ToggleSidebar),
            'o' => Some(Chord::ManageWorkspaces),
            'f' => Some(Chord::Search),
            'j' => Some(Chord::ToggleAi),
            'r' => Some(Chord::Reminders),
            't' => Some(Chord::ReopenClosedTab),
            _ => None,
        };
    }
    if matches!(key, Key::Character(c) if c == ",") || text == Some(",") {
        return Some(Chord::OpenSettings);
    }
    let c = ctrl_letter(key, text)?;
    match c {
        't' => Some(Chord::NewTab),
        // The New… launcher. The shell gave Ctrl+N up when New window lost
        // its chord: a second window is the tab menu's *Move to New Window*.
        'n' => Some(Chord::New),
        'w' => Some(Chord::CloseTab),
        'j' => Some(Chord::ToggleContext),
        'p' => Some(Chord::Search),
        's' => Some(Chord::SaveSnapshot),
        // Ctrl+1..9 select by position. `ctrl_letter` returns digits verbatim
        // through its non-control arm, so they arrive here unchanged.
        '1'..='9' => Some(Chord::SelectTab(c as usize - '1' as usize)),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
