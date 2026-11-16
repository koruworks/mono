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
//! ## What `run` starts, and what it must never start
//!
//! [`run`] is reached from `main` before any of the ordinary startup: the
//! update reopen claim, the file log, the taskbar hand-off and Jump List, the
//! Agent link, the Store, persisted search and the workspace host. It then
//! starts exactly two things: the display language, from the operating
//! system's locale, and one window on `koru_window`'s single-window runner.
//! It reads no Koru preference, so the theme, font and density are the
//! built-in defaults whatever the working app is set to, and the element
//! layer's fixture theme is never pinned here even when its variable is set.
//!
//! No type can hold that ordering, so `tests.rs` pins it by reading the
//! source, the way the Agent pins its worker dispatch.

mod placement;
// The window does not list anything yet: the registry's records are checked
// against the production source by their own tests until the first story
// reads them.
#[allow(dead_code)]
pub(crate) mod registry;
mod shell;

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
    let placement = placement::Placement::in_memory(command.window_size);
    if let Err(error) = koru_window::run(|_engine| shell::Lab::new(placement)) {
        eprintln!("Koru could not open the UI Lab window: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests;
