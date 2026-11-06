// SPDX-License-Identifier: MPL-2.0

//! The macOS menu bar.
//!
//! A Mac app without one reads as broken before anything else is judged: no
//! ⌘Q, no About, no Services, nothing in the app menu at all. winit provides
//! none of this, so it is `muda` — the same authors as `tray-icon`, wrapping
//! `NSMenu`. On macOS it pulls no gtk and no libxdo; that weight is Linux's,
//! which is the objection `docs/deferred/` records against the tray.
//!
//! ## Rows do not *do* anything — they press the key
//!
//! **This is the whole design, and it is the one thing not to change.**
//! `apps/koru-document` already owns a `Command` vocabulary that the selection
//! bubble, the block menu and the page menu all lay out, and the shell cannot
//! see it: `koru-workspace-host` does not depend on `koru-editor`, which is
//! load-bearing and enforced by the manifests.
//!
//! So a row carries a [`koru_keys::ShortcutId`], and choosing it synthesises
//! that shortcut's chord into the focused window through the same
//! `View::key` the keyboard uses. Three things follow:
//!
//! - **The menu cannot drift from the keyboard.** Both read one registry row.
//!   Undo-from-the-menu and ⌘Z are the same code path, not two that agree
//!   today.
//! - **The shell needs no vocabulary of its own.** It never learns what "Bold"
//!   means; it presses the key that means it.
//! - **A row is enabled iff the chord is bound.** Nothing can offer a command
//!   the app cannot perform.
//!
//! The exceptions are the handful of rows that are *not* app commands —
//! Quit, About, Hide, Services, Minimise, Zoom — which AppKit implements
//! itself and which [`Command::Native`] covers.
//!
//! ## Quit has to save
//!
//! `PredefinedMenuItem::quit()` terminates the process through AppKit, which
//! would skip `ApplicationHandler::exiting` and with it `persist_if_changed`,
//! the workspace save and the profile save. Losing content is the one
//! unrecoverable bug, so **Quit is an ordinary row** that asks the event loop to
//! exit and lets the normal path run.

#[cfg(target_os = "macos")]
mod imp {
    use muda::accelerator::{Accelerator, Code, Modifiers};
    // No `MenuEvent` here: `menus.rs` installs the one process-wide handler
    // muda allows and fans both surfaces out from it (the support icon's popup
    // and this bar). This module builds rows and reads ids.
    use koru_keys::{On, ShortcutId, registry};
    use muda::{Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
    use winit::keyboard::NamedKey;

    /// What choosing a row asks for.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Command {
        /// Press this shortcut's chord in the focused window.
        Press(ShortcutId),
        /// Save and quit, through the ordinary exit path.
        Quit,
        /// AppKit did it; nothing for us to do.
        Native,
    }

    pub struct MenuBar {
        _menu: Menu,
        rows: Vec<(MenuId, Command)>,
    }

    /// Translate a registry chord into a muda accelerator.
    ///
    /// `None` where muda has no code for the key — the row is then offered
    /// without an accelerator rather than with a wrong one.
    fn accelerator(id: ShortcutId) -> Option<Accelerator> {
        let chord = registry::get(id).chord;
        let mut mods = Modifiers::empty();
        if chord.mods.super_key() {
            mods |= Modifiers::META;
        }
        if chord.mods.shift_key() {
            mods |= Modifiers::SHIFT;
        }
        if chord.mods.alt_key() {
            mods |= Modifiers::ALT;
        }
        if chord.mods.control_key() {
            mods |= Modifiers::CONTROL;
        }
        let code = match chord.on {
            On::Letter(c) => match c {
                // muda parses the W3C `KeyA` / `Digit1` names; the table is
                // large and mechanical, so it is built rather than written out.
                'a'..='z' => format!("Key{}", c.to_ascii_uppercase()).parse().ok()?,
                '0'..='9' => format!("Digit{c}").parse().ok()?,
                '=' => Code::Equal,
                '-' => Code::Minus,
                '[' => Code::BracketLeft,
                ']' => Code::BracketRight,
                '\\' => Code::Backslash,
                '`' => Code::Backquote,
                '.' => Code::Period,
                ',' => Code::Comma,
                _ => return None,
            },
            On::Named(NamedKey::Enter) => Code::Enter,
            On::Named(NamedKey::Delete) => Code::Delete,
            On::Named(NamedKey::Backspace) => Code::Backspace,
            On::Named(NamedKey::Space) => Code::Space,
            On::Named(NamedKey::Tab) => Code::Tab,
            On::Named(NamedKey::ArrowUp) => Code::ArrowUp,
            On::Named(NamedKey::ArrowDown) => Code::ArrowDown,
            On::Named(NamedKey::ArrowLeft) => Code::ArrowLeft,
            On::Named(NamedKey::ArrowRight) => Code::ArrowRight,
            // The function keys the registry binds (rename, focus the chrome).
            On::Named(NamedKey::F1) => Code::F1,
            On::Named(NamedKey::F2) => Code::F2,
            On::Named(NamedKey::F3) => Code::F3,
            On::Named(NamedKey::F4) => Code::F4,
            On::Named(NamedKey::F5) => Code::F5,
            On::Named(NamedKey::F6) => Code::F6,
            On::Named(NamedKey::F7) => Code::F7,
            On::Named(NamedKey::F8) => Code::F8,
            On::Named(NamedKey::F9) => Code::F9,
            On::Named(NamedKey::F10) => Code::F10,
            On::Named(NamedKey::F11) => Code::F11,
            On::Named(NamedKey::F12) => Code::F12,
            _ => return None,
        };
        Some(Accelerator::new(Some(mods), code))
    }

    /// One row for a registry entry, labelled and accelerated from it.
    fn row(rows: &mut Vec<(MenuId, Command)>, id: ShortcutId) -> MenuItem {
        let entry = registry::get(id);
        let item = MenuItem::new(entry.label, true, accelerator(id));
        rows.push((item.id().clone(), Command::Press(id)));
        item
    }

    impl MenuBar {
        /// Build and install the menu bar. `None` if AppKit refused it, which
        /// is not fatal — the app runs, it simply has no menus.
        pub fn install(app_name: &str) -> Option<Self> {
            let mut rows: Vec<(MenuId, Command)> = Vec::new();
            let menu = Menu::new();

            // **Quit is ours, not `PredefinedMenuItem::quit`** — see the module
            // docs: AppKit's version skips the save.
            let quit = MenuItem::new(
                format!("Quit {app_name}"),
                true,
                Some(Accelerator::new(Some(Modifiers::META), Code::KeyQ)),
            );
            rows.push((quit.id().clone(), Command::Quit));

            let app = Submenu::with_items(
                app_name,
                true,
                &[
                    &PredefinedMenuItem::about(Some(&format!("About {app_name}")), None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::services(None),
                    &PredefinedMenuItem::separator(),
                    &PredefinedMenuItem::hide(None),
                    &PredefinedMenuItem::hide_others(None),
                    &PredefinedMenuItem::show_all(None),
                    &PredefinedMenuItem::separator(),
                    &quit,
                ],
            )
            .ok()?;

            let file = Submenu::with_items(
                "File",
                true,
                &[
                    &row(&mut rows, ShortcutId::New),
                    &row(&mut rows, ShortcutId::NewTab),
                    &PredefinedMenuItem::separator(),
                    &row(&mut rows, ShortcutId::RenameDocument),
                    // **No *Move to Trash* row**, and it is the `Press` model
                    // that decides it rather than taste: a row here presses a
                    // chord in the focused window, and the container binds none
                    // for trashing. A row that pressed a chord nothing resolves
                    // would be a menu item that silently does nothing. Trashing
                    // is the explore pane's context menu, which needs no chord.
                    &PredefinedMenuItem::separator(),
                    &row(&mut rows, ShortcutId::CloseTab),
                    &row(&mut rows, ShortcutId::CloseAllTabs),
                ],
            )
            .ok()?;

            let edit = Submenu::with_items(
                "Edit",
                true,
                &[
                    &row(&mut rows, ShortcutId::Undo),
                    &row(&mut rows, ShortcutId::Redo),
                    &PredefinedMenuItem::separator(),
                    &row(&mut rows, ShortcutId::Cut),
                    &row(&mut rows, ShortcutId::Copy),
                    &row(&mut rows, ShortcutId::Paste),
                    &row(&mut rows, ShortcutId::SelectAll),
                    &PredefinedMenuItem::separator(),
                    &row(&mut rows, ShortcutId::InsertLink),
                    &row(&mut rows, ShortcutId::EmojiPicker),
                    &row(&mut rows, ShortcutId::DeleteRows),
                ],
            )
            .ok()?;

            let format = Submenu::with_items(
                "Format",
                true,
                &[
                    &row(&mut rows, ShortcutId::Bold),
                    &row(&mut rows, ShortcutId::Italic),
                    &row(&mut rows, ShortcutId::Underline),
                    &row(&mut rows, ShortcutId::Strikethrough),
                    &row(&mut rows, ShortcutId::InlineCode),
                    &row(&mut rows, ShortcutId::CycleHighlight),
                    &PredefinedMenuItem::separator(),
                    &row(&mut rows, ShortcutId::BodyText),
                    &row(&mut rows, ShortcutId::Heading1),
                    &row(&mut rows, ShortcutId::Heading2),
                    &row(&mut rows, ShortcutId::Heading3),
                    &row(&mut rows, ShortcutId::Heading4),
                    &PredefinedMenuItem::separator(),
                    &row(&mut rows, ShortcutId::AlignLeft),
                    &row(&mut rows, ShortcutId::AlignCentre),
                    &row(&mut rows, ShortcutId::AlignRight),
                    &PredefinedMenuItem::separator(),
                    &row(&mut rows, ShortcutId::Superscript),
                    &row(&mut rows, ShortcutId::Subscript),
                    &row(&mut rows, ShortcutId::ClearFormatting),
                ],
            )
            .ok()?;

            let view = Submenu::with_items(
                "View",
                true,
                &[
                    &row(&mut rows, ShortcutId::ToggleExplore),
                    &row(&mut rows, ShortcutId::ToggleContext),
                    &PredefinedMenuItem::separator(),
                    &row(&mut rows, ShortcutId::ViewModePage),
                    &row(&mut rows, ShortcutId::ViewModeNarrow),
                    &row(&mut rows, ShortcutId::ViewModeFull),
                    &PredefinedMenuItem::separator(),
                    &row(&mut rows, ShortcutId::ZoomIn),
                    &row(&mut rows, ShortcutId::ZoomOut),
                    &row(&mut rows, ShortcutId::ZoomReset),
                    &PredefinedMenuItem::separator(),
                    &row(&mut rows, ShortcutId::HistoryBack),
                    &row(&mut rows, ShortcutId::HistoryForward),
                ],
            )
            .ok()?;

            let window = Submenu::with_items(
                "Window",
                true,
                &[
                    &PredefinedMenuItem::minimize(None),
                    &PredefinedMenuItem::maximize(None),
                    &PredefinedMenuItem::separator(),
                    &row(&mut rows, ShortcutId::NextTab),
                    &row(&mut rows, ShortcutId::PreviousTab),
                    &row(&mut rows, ShortcutId::PinWindow),
                    &PredefinedMenuItem::separator(),
                    &row(&mut rows, ShortcutId::ManageWorkspaces),
                    &PredefinedMenuItem::bring_all_to_front(None),
                ],
            )
            .ok()?;

            for sub in [&app, &file, &edit, &format, &view, &window] {
                menu.append(sub).ok()?;
            }
            menu.init_for_nsapp();
            Some(Self { _menu: menu, rows })
        }

        /// What a chosen row means, if it is one of the bar's. Rows arrive
        /// through `menus::drain`, the one transport for every menu event;
        /// an id the bar does not know is a popup's or a predefined row
        /// AppKit already handled, and the caller decides which.
        pub fn command_for(&self, id: &str) -> Option<Command> {
            self.rows
                .iter()
                .find(|(row, _)| row.0 == id)
                .map(|(_, c)| *c)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Every row the menu offers must have an accelerator muda can express,
        /// or it silently appears without one — a menu that knows the chord and
        /// declines to teach it.
        #[test]
        fn every_menu_shortcut_has_an_accelerator() {
            for entry in registry::REGISTRY {
                assert!(
                    accelerator(entry.id).is_some(),
                    "{:?} ({}) has no muda accelerator",
                    entry.id,
                    koru_keys::label_of(entry.id),
                );
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub use imp::{Command, MenuBar};

// ---------------------------------------------------------------- elsewhere

/// Windows and Linux draw their own furniture — the ⋮ menu and the tray — so
/// there is no menu bar to install and every call here is a no-op.
#[cfg(not(target_os = "macos"))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    Press(koru_keys::ShortcutId),
    Quit,
    Native,
}

#[cfg(not(target_os = "macos"))]
pub struct MenuBar;

#[cfg(not(target_os = "macos"))]
impl MenuBar {
    pub fn install(_app_name: &str) -> Option<Self> {
        None
    }
    pub fn command_for(&self, _id: &str) -> Option<Command> {
        None
    }
}
