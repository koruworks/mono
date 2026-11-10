// SPDX-License-Identifier: MPL-2.0

//! Rail control identities and the geometry projected from its element frame.

use koru_draw::geometry::Rect;

use crate::panes::explore::NavigatorMode;

pub const SQUARE: f32 = koru_draw::theme::metrics::RAIL_SQUARE.value;
pub const PAD_TOP: f32 = 10.0;
#[cfg(test)]
pub const GAP: f32 = 4.0;

/// Every rect in the rail. Zero-width in a collapsed column, the idiom the
/// rest of the chrome uses, so a hit test must exclude them explicitly.
#[derive(Clone, PartialEq, Debug)]
pub struct Rects {
    pub band: Rect,
    pub workspaces: Vec<(Option<koru_core::WorkspaceId>, Rect)>,
    /// One square per app the build shows, in the person's order
    /// (`rail_order::RailOrder::visible`).
    /// Nothing under the last: the *Manage apps* `+` is out until the
    /// manager lands (the review's note, E68).
    pub apps: Vec<(NavigatorMode, Rect)>,
    pub overflow: Rect,
    pub help: Rect,
    /// App settings, between Help and the profile square.
    pub settings: Rect,
    /// The profile square at the foot.
    pub profile: Rect,
}

/// What is under a point in the rail.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Hit {
    Workspace(koru_core::WorkspaceId),
    WaitingWorkspace,
    App(NavigatorMode),
    Overflow,
    Help,
    Settings,
    Profile,
}
