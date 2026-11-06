// SPDX-License-Identifier: MPL-2.0

//! A Mac's traffic lights, seated in the window's own top band.
//!
//! A main window on macOS keeps its `Titled` style mask, which brings the
//! platform's corner radius, shadow, resize border and native drag. But its
//! titlebar is made **transparent with a full-size content view**, so the
//! scene runs to the window's top edge and the 28 pt strip AppKit used to own
//! above the caption is gone. The lights stay AppKit's; this module moves them
//! into the band the shell draws, the lane (`koru_shell_chrome::lane`), where
//! the lane's leading slot leaves [`LANE_W`] of room for them in place of the
//! Koru mark.
//!
//! **Why the lights have to be moved and not just revealed.** AppKit places
//! them for a 28 pt titlebar: centred 14 pt below the top. The lane is
//! `theme::CAPTION_H` (36 pt) tall, so left where they are they sit visibly
//! high in it. [`place`] centres them in the band and insets them from the
//! left edge by the same amount.
//!
//! **Why it is re-applied.** AppKit lays the titlebar out again whenever it
//! likes — on a resize, on entering and leaving full screen, on a change of
//! key window — and each layout puts the buttons back where it wants them.
//! winit owns the window's delegate (AGENTS.md: replacing it aborts the
//! process), so there is no `windowDidResize:` to hang this on; the host
//! calls [`place`] from the winit events that follow those layouts instead.
//! It is idempotent and cheap: three frame reads and three frame writes.
//!
//! Elsewhere this is a no-op, and [`LANE_W`] is never read.

use winit::window::Window;

/// The room the lane's leading slot leaves for the three lights, in logical
/// points: the left inset, three 14 pt buttons with AppKit's 6 pt gaps
/// between them, and a trailing gap wide enough that the first control after
/// them does not read as a fourth light.
pub const LANE_W: f32 = 78.0;

/// The `NSWindow` behind a winit window, when there is one.
#[cfg(target_os = "macos")]
fn ns_window(window: &Window) -> Option<objc2::rc::Retained<objc2_app_kit::NSWindow>> {
    use objc2::rc::Retained;
    use objc2_app_kit::NSView;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let handle = window.window_handle().ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: the handle is this live winit window's content view, an
    // `NSView`, and retaining it keeps it alive for the calls that follow.
    // Every caller is on the main thread, where winit delivers its events.
    let view = unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) }?;
    view.window()
}

/// Stop AppKit dragging the window from the band its titlebar used to own.
///
/// **A transparent titlebar still moves the window.** With a full-size
/// content view AppKit keeps treating the top 28 pt as a titlebar: a press
/// there that the content view would let move the window *does* move it —
/// and that band is now the tab strip and the lane. Dragging a tab to split
/// the pane dragged the whole window instead, because AppKit took the
/// gesture alongside the view.
///
/// Nothing is lost by turning it off. On macOS the shell already drags the
/// window itself, by polling the pointer from a press on bare caption
/// (`WindowDrag` in `koru-workspace-host`), since `performWindowDragWithEvent:`
/// moves nothing there; that places the window with `setFrameOrigin`, which
/// `movable` does not govern. A non-movable window still zooms, minimises,
/// tiles from the Window menu and goes full screen.
#[cfg(target_os = "macos")]
pub fn shell_owns_drag(window: &Window) {
    if let Some(ns_window) = ns_window(window) {
        ns_window.setMovable(false);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn shell_owns_drag(_window: &Window) {}

/// Seat the lights in a top band `band_h` logical points tall.
///
/// Does nothing to a window without standard buttons — a borderless one, or
/// one whose buttons were hidden — and nothing at all off macOS.
#[cfg(target_os = "macos")]
pub fn place(window: &Window, band_h: f64) {
    use objc2_app_kit::NSWindowButton;
    use objc2_foundation::NSPoint;

    let Some(ns_window) = ns_window(window) else {
        return;
    };
    let buttons = [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ]
    .map(|kind| ns_window.standardWindowButton(kind));
    let [Some(close), Some(minimise), Some(zoom)] = buttons else {
        return;
    };
    // The buttons' own superview is the titlebar view; its parent is the
    // container AppKit sizes to the titlebar's height. Grow the container to
    // the band, pinned to the window's top (AppKit's y grows upward), so the
    // buttons are hit inside it rather than clipped at its 28 pt.
    // SAFETY: plain view-hierarchy reads on the main thread.
    let Some(titlebar) = (unsafe { close.superview() }) else {
        return;
    };
    if let Some(container) = unsafe { titlebar.superview() } {
        let mut frame = container.frame();
        let window_h = ns_window.frame().size.height;
        if (frame.size.height - band_h).abs() > f64::EPSILON {
            frame.size.height = band_h;
            frame.origin.y = window_h - band_h;
            container.setFrame(frame);
        }
    }
    let close_frame = close.frame();
    let size = close_frame.size;
    // AppKit's spacing between the lights, kept as AppKit has it.
    let step = minimise.frame().origin.x - close_frame.origin.x;
    let inset = ((band_h - size.height) * 0.5).max(0.0);
    // The titlebar view fills the container, so its height is the band's.
    let y = (titlebar.frame().size.height - size.height) * 0.5;
    for (i, button) in [&close, &minimise, &zoom].into_iter().enumerate() {
        button.setFrameOrigin(NSPoint::new(inset + step * i as f64, y.max(0.0)));
    }
}

#[cfg(not(target_os = "macos"))]
pub fn place(_window: &Window, _band_h: f64) {}
