// SPDX-License-Identifier: MPL-2.0

//! **Where the Lab's window opens, decided in memory.**
//!
//! Every other Koru window remembers where it was, in a file on this machine.
//! The Lab deliberately does not: it must leave the working app's remembered
//! geometry exactly as it found it, and it must open the same way for the
//! next person following a written recipe. So its placement is this value and
//! nothing else — the size asked for on the command line, or the default —
//! with no path to read from and no writer to save through. The operating
//! system chooses the position.

use winit::dpi::LogicalSize;
use winit::window::{Window, WindowAttributes};

/// The smallest the Lab's window goes: the same floor `--window-size`
/// refuses below, so a size that parsed is always one the window can hold.
const MIN_SIZE: (f32, f32) = koru_workspace_host::settings::MIN_WINDOW_SIZE;

/// The Lab window's size for this launch, in logical pixels.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Placement {
    size: (f32, f32),
}

impl Placement {
    /// Wide enough for the navigator, a preview and the properties side by
    /// side at their natural widths.
    pub(crate) const DEFAULT_SIZE: (f32, f32) = (1120.0, 720.0);

    /// The placement for a launch: the validated `--window-size`, else the
    /// default. There is no third source.
    pub(crate) fn in_memory(requested: Option<(f32, f32)>) -> Self {
        Self {
            size: requested.unwrap_or(Self::DEFAULT_SIZE),
        }
    }

    /// The window to create: undecorated like every Koru window, because the
    /// Lab draws its own caption.
    pub(crate) fn attributes(self, title: &str) -> WindowAttributes {
        Window::default_attributes()
            .with_decorations(false)
            .with_title(title)
            .with_inner_size(LogicalSize::new(self.size.0, self.size.1))
            .with_min_inner_size(LogicalSize::new(MIN_SIZE.0, MIN_SIZE.1))
    }
}

#[cfg(test)]
mod tests;
