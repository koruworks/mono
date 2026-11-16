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
use winit::window::{Icon, Window, WindowAttributes};

/// **The Koru mark, for the title bar, the taskbar and the window
/// switcher**: the picture every Koru window wears, so the Lab is found
/// where a Koru window is looked for.
///
/// The same artwork and the same recipe as the ordinary app's window icon
/// (`koru_workspace_host`'s `app_icon`, which the Lab cannot call: it is
/// private to the application the Lab must not start). The PNG is compiled
/// in, so nothing is read when the window opens, and it is tinted for the
/// build's channel so a nightly Lab looks nightly beside a stable Koru.
/// `None` when the bytes do not decode: a window with the system's default
/// icon is a cosmetic loss, and one that refused to open would not be.
pub(crate) fn icon() -> Option<Icon> {
    let rgba = image::load_from_memory(include_bytes!("../../../../assets/logo.png"))
        .ok()?
        .to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut pixels = rgba.into_raw();
    koru_channel::tint::apply(&mut pixels, koru_channel::tint::tint());
    Icon::from_rgba(pixels, width, height).ok()
}

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
    /// Lab draws its own caption, and wearing the Koru mark.
    pub(crate) fn attributes(self, title: &str) -> WindowAttributes {
        Window::default_attributes()
            .with_decorations(false)
            .with_window_icon(icon())
            .with_title(title)
            .with_inner_size(LogicalSize::new(self.size.0, self.size.1))
            .with_min_inner_size(LogicalSize::new(MIN_SIZE.0, MIN_SIZE.1))
    }
}

#[cfg(test)]
mod tests;
