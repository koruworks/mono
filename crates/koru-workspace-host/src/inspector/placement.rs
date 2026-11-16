// SPDX-License-Identifier: MPL-2.0

//! Screen position is physical; client size is logical and independent of app zoom.
use koru_window::{WindowRowWriter, read_window_row, window_geometry_path};
use std::path::Path;

const KEY: &str = "ui-inspector";

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Geometry {
    pub position: Option<(i32, i32)>,
    pub size: (f32, f32),
    pub pinned: bool,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            position: None,
            size: (520.0, 720.0),
            pinned: false,
        }
    }
}

pub(crate) struct Placement {
    pub geometry: Geometry,
    writer: WindowRowWriter,
}

impl Placement {
    pub fn open(root: Option<&Path>, remembered: Option<Geometry>) -> Self {
        let path = window_geometry_path(root);
        let geometry = remembered
            .or_else(|| {
                path.as_deref()
                    .and_then(|p| read_window_row(p, KEY))
                    .and_then(|row| parse(&row))
            })
            .unwrap_or_default();
        Self {
            geometry,
            writer: WindowRowWriter::open(path, KEY, "koru-inspector-position"),
        }
    }

    /// A placement that lives in this value and nowhere else.
    ///
    /// **No path is resolved, no row is read and no writer thread is
    /// started**, which is the whole difference from [`open`](Self::open):
    /// passing `root = None` there is not the same thing, because it resolves
    /// the machine-local `windows.conf` and starts a writer for it. A host
    /// that must leave the working app's remembered geometry exactly as it
    /// found it, and write nothing of its own, uses this.
    ///
    /// `remembered` is what the same host recorded earlier in this process;
    /// with none, the panel opens at its default size wherever the OS puts
    /// it. What is recorded afterwards is forgotten when the value is
    /// dropped.
    pub fn in_memory(remembered: Option<Geometry>) -> Self {
        Self {
            geometry: remembered.unwrap_or_default(),
            // The default writer has no worker: `write_if_changed` then only
            // remembers the row it was handed.
            writer: WindowRowWriter::default(),
        }
    }

    /// Record where the panel was from values a host already has, for one
    /// that is never handed the `Window` [`remember`](Self::remember) reads.
    ///
    /// `size` is the client size in logical pixels. A maximised panel keeps
    /// its last restored position and size and only its pin is updated, as
    /// `remember` does; so does one whose position the OS would not give.
    pub fn record(
        &mut self,
        position: Option<(i32, i32)>,
        size: (f32, f32),
        maximised: bool,
        pinned: bool,
    ) {
        match position {
            Some(position) if !maximised && size.0 > 0.0 && size.1 > 0.0 => {
                self.geometry = Geometry {
                    position: Some(position),
                    size,
                    pinned,
                };
            }
            _ => self.geometry.pinned = pinned,
        }
        self.write();
    }

    pub fn remember(&mut self, window: &winit::window::Window, pinned: bool) {
        if window.is_minimized() == Some(true) || window.is_maximized() {
            self.geometry.pinned = pinned;
            self.write();
            return;
        }
        let Ok(position) = window.outer_position() else {
            return;
        };
        let size = window.inner_size().to_logical::<f32>(window.scale_factor());
        if size.width <= 0.0 || size.height <= 0.0 {
            return;
        }
        self.geometry = Geometry {
            position: Some((position.x, position.y)),
            size: (size.width, size.height),
            pinned,
        };
        self.write();
    }

    fn write(&mut self) {
        let Some((x, y)) = self.geometry.position else {
            return;
        };
        self.writer.write_if_changed(format!(
            "v2 {} {} {} {} {}",
            x,
            y,
            self.geometry.size.0,
            self.geometry.size.1,
            u8::from(self.geometry.pinned)
        ));
    }
}

fn parse(row: &str) -> Option<Geometry> {
    let mut parts = row.split_whitespace();
    // v1 was an in-app panel position, which is not a screen coordinate.
    if parts.next()? != "v2" {
        return None;
    }
    let x = parts.next()?.parse().ok()?;
    let y = parts.next()?.parse().ok()?;
    let w: f32 = parts.next()?.parse().ok()?;
    let h: f32 = parts.next()?.parse().ok()?;
    let pinned = match parts.next()? {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    (parts.next().is_none()
        && w.is_finite()
        && h.is_finite()
        && (360.0..=4096.0).contains(&w)
        && (360.0..=4096.0).contains(&h))
    .then_some(Geometry {
        position: Some((x, y)),
        size: (w, h),
        pinned,
    })
}

#[cfg(test)]
mod tests;
