// SPDX-License-Identifier: MPL-2.0

//! **The UI Lab**: Koru's own components, shown one at a time over synthetic
//! state so they can be tried without a workspace.
//!
//! It is this binary launched as `koru --ui`, in its own process. Nothing in
//! here opens a profile, a Store, the Agent or a settings file, and nothing is
//! saved when the window closes.
//!
//! [`registry`] is the single source for what the Lab lists: each entry names
//! the production symbol that draws it and the options that symbol really
//! takes. The components themselves stay with their owners; the Lab holds
//! stories and fixtures only.
//!
//! The window ([`shell`]) is a gallery in three regions. The navigator lists
//! every record the registry has for a desktop platform, in three groups
//! that fold, under a search that narrows it; a record that exists only on
//! a phone stays in the registry and is neither listed nor found. The centre is the page of the record on show: its story
//! when it has one, and otherwise its status said in words, with nothing
//! drawn in the component's place. The properties column states what is
//! recorded about it and holds a story's inputs. The stories so far are the
//! shared dialog button, the shared text input, the element layer's row and
//! column, and the spacing, color-role and text-role tables; each is the
//! production thing, changed through the inputs its registration lists, with
//! Reset. A story's state is [`story`]'s and lives in memory. The toolbar sets the
//! theme, how wide the preview is and the display language, for this
//! process only and in memory only. F12 opens the same
//! inspector the app has, in a window of its own ([`windows`]), over the
//! Lab's own elements and the production controls they hold. There is no
//! way to name a story on the command line.
//!
//! ## What `run` starts, and what it must never start
//!
//! [`run`] is reached from `main` before any of the ordinary startup: the
//! update reopen claim, the file log, the taskbar hand-off and Jump List, the
//! Agent link, the Store, persisted search and the workspace host. It then
//! starts exactly two things: the display language, from the operating
//! system's locale, and the Lab's window on `koru_window`'s multi-window
//! runner, which opens the inspector's panel beside it while F12 has it
//! open. It reads no Koru preference, so the theme, font and density are the
//! built-in defaults whatever the working app is set to, and the element
//! layer's fixture theme is never pinned here even when its variable is set.
//! The theme and the language the toolbar then chooses are set on this
//! process's own drawing and translation state and written nowhere.
//! Neither window's size or position is read from or written to a file.
//!
//! No type can hold that ordering, so `tests.rs` pins it by reading the
//! source, the way the Agent pins its worker dispatch.

mod placement;
pub(crate) mod registry;
mod shell;
mod story;
mod tokens;
mod windows;

use koru_workspace_host::cli::UiCommand;

/// Open the Lab and stay until its window closes.
///
/// Returns when the window has closed. A window that could not be created at
/// all is said on stderr and is exit code 1: the command line was understood,
/// so it is not the usage error 2 stands for.
pub(crate) fn run(command: UiCommand) {
    // The nearest of the eight languages to what the system reports. A
    // question to the operating system, not a read of anyone's Koru settings.
    koru_i18n::set_current(koru_i18n::Lang::for_system(
        koru_window::system_locale::system_locale().as_deref(),
    ));
    // The record the Lab opens on: the first story it had. The registry's
    // own tests hold it in place, so a missing record is a broken build
    // rather than a state a person can reach; it still ends here and not in
    // an empty window.
    let Some((entry, _)) = registry::story(registry::BUTTON_DIALOG_STORY) else {
        eprintln!("Koru's UI Lab has no registered story to open");
        std::process::exit(1);
    };
    let placement = placement::Placement::in_memory(command.window_size);
    let opened = koru_window::run_windows(|engine| {
        windows::LabWindows::new(shell::Lab::new(engine, placement, entry))
    });
    if let Err(error) = opened {
        eprintln!("Koru could not open the UI Lab window: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests;
