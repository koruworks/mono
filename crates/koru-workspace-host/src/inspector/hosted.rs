// SPDX-License-Identifier: MPL-2.0

//! **The F12 inspector for a host that is not [`App`](crate::app).**
//!
//! [`HostedInspector`] is the same [`Inspector`] the workspace window and the
//! first-run window open: the same capture, the same pick, the same tree and
//! details, the same F12 and F11 rules. What it leaves out is everything
//! those two do with a `Window` of their own. It is handed window events and
//! answers with what the host should do, so a host on `koru-window`'s
//! multi-window runner, which never holds a `Window`, can drive it.
//!
//! ```text
//! source window event  ->  source_event  ->  SourceEvent  (withhold it? repaint?)
//! panel window event   ->  panel_event   ->  PanelEvent   (drag? pin? close?)
//! source frame         ->  begin_capture ... the host draws ... end_capture
//! panel frame          ->  draw_panel
//! ```
//!
//! ## Nothing is remembered outside the process
//!
//! The panel's size, position and pin are kept in
//! [`Placement::in_memory`]: no geometry file is read when the panel opens
//! and none is written when it moves or closes. A second F12 in the same
//! process reopens the panel where it was; a new process starts from the
//! default.
//!
//! ## Nothing runs while it is closed
//!
//! A closed inspector is one `Option` test on each call. No frame is
//! captured, no snapshot is kept and no text engine exists until the first
//! F12, and all of it is dropped by the F12 that closes it.

use std::time::Instant;

use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowAttributes, WindowLevel};

use super::*;
use crate::accessibility::{Scope, WindowSemantics};

/// The smallest the panel's window goes, in logical pixels: the floor the
/// workspace's own panel has.
const MIN_PANEL: f64 = 360.0;

/// What a host does after one event of the window being inspected.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceEvent {
    /// The inspector took the event: the host's own content must not act on
    /// it. True for F12 and F11 themselves, and for every press, key and
    /// composition while a target is being picked.
    pub consumed: bool,
    /// The inspected window shows something different: a new highlight, or
    /// none.
    pub redraw_source: bool,
    /// The panel shows something different.
    pub redraw_panel: bool,
    /// This event opened the inspector: the panel's window is now wanted.
    pub opened: bool,
    /// This event closed it: the panel's window is no longer wanted.
    pub closed: bool,
}

/// What a host does after one event of the panel's own window.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct PanelEvent {
    pub redraw_panel: bool,
    /// The highlight in the inspected window changed with the panel.
    pub redraw_source: bool,
    /// The press began on the panel's title: start a native window drag
    /// inside this same press.
    pub drag_window: bool,
    /// A tree row was opened: bring the inspected window forward.
    pub reveal_source: bool,
    /// The inspector closed: the panel's window is no longer wanted, and the
    /// keyboard goes back to the inspected window.
    pub closed: bool,
}

/// One frame of the inspected window being measured. Made by
/// [`HostedInspector::begin_capture`] and given back to
/// [`HostedInspector::end_capture`] once the host has drawn.
pub struct Capture {
    started: Instant,
    frame: inspect::Frame,
    owner: inspect::Scope,
}

/// What the inspector needs to know about one of the two windows between
/// events: winit reports modifiers and focus as events of their own.
#[derive(Copy, Clone, Debug)]
struct Seat {
    focused: bool,
    mods: ModifiersState,
}

impl Seat {
    /// Follow the event stream. Modifiers are cleared on both focus edges,
    /// as the shared runner clears them: a window that gains focus while a
    /// chord is held never sees those keys released.
    fn follow(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::ModifiersChanged(mods) => self.mods = mods.state(),
            WindowEvent::Focused(focused) => {
                self.focused = *focused;
                self.mods = ModifiersState::empty();
            }
            _ => {}
        }
    }
}

/// The inspector, its panel's placement, and nothing a window owns.
pub struct HostedInspector {
    inspector: Option<Inspector>,
    placement: Placement,
    source: Seat,
    panel: Seat,
    /// The panel's client size as last drawn, so its layout follows the
    /// window without the host forwarding a resize.
    panel_size: Option<(f32, f32)>,
    /// The panel's semantic tree is out of date. Set whenever the panel's
    /// contents change; taken by the host when it next publishes.
    panel_semantics_dirty: bool,
}

impl HostedInspector {
    /// A closed inspector whose panel placement lives only in this value.
    /// See [`Placement::in_memory`] for what that leaves out.
    pub fn in_memory() -> Self {
        Self {
            inspector: None,
            placement: Placement::in_memory(None),
            // A host's first window is created focused; the first `Focused`
            // event corrects this if it was not.
            source: Seat {
                focused: true,
                mods: ModifiersState::empty(),
            },
            panel: Seat {
                focused: false,
                mods: ModifiersState::empty(),
            },
            panel_size: None,
            panel_semantics_dirty: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.inspector.is_some()
    }

    /// Whether the inspected window is being picked from: its presses and
    /// keys are the inspector's, and its pointer is the plain arrow.
    pub fn wants_arrow_cursor(&self) -> bool {
        self.inspector
            .as_ref()
            .is_some_and(Inspector::wants_arrow_cursor)
    }

    /// What F12 does, without the key: open a closed inspector, resume a
    /// paused one, close a live one. Returns whether it is open afterwards.
    ///
    /// [`source_event`](Self::source_event) and
    /// [`panel_event`](Self::panel_event) call this for a deliberate press of
    /// the chord. It is public for a host with another way to ask, such as a
    /// screen reader's action, and because a key event cannot be constructed
    /// outside winit, which leaves this the only way a test can ask.
    ///
    /// `source_size` is the inspected window's client size in logical pixels.
    pub fn toggle(&mut self, source_size: (f32, f32)) -> bool {
        match self.inspector.as_mut() {
            Some(inspector) => {
                if inspector.shortcut_requests_close() {
                    self.close();
                } else {
                    self.panel_semantics_dirty = true;
                }
            }
            None => {
                let geometry = self.placement.geometry;
                let mut inspector = Inspector::new(source_size.0, source_size.1);
                inspector.pinned = geometry.pinned;
                inspector.panel_viewport(geometry.size.0, geometry.size.1);
                self.inspector = Some(inspector);
                self.panel_size = Some(geometry.size);
                self.panel_semantics_dirty = true;
            }
        }
        self.is_open()
    }

    /// Close the inspector: the panel's ✕, Escape, or its window's own close.
    /// The panel's placement stays for the next F12 in this process.
    pub fn close(&mut self) {
        self.inspector = None;
        self.panel_size = None;
        self.panel_semantics_dirty = false;
    }

    /// One event of the window being inspected, **before** the host acts on
    /// it. `scale` is that window's scale factor and `source_size` its client
    /// size in logical pixels.
    ///
    /// F12 is answered only for a deliberate first press: not an auto-repeat,
    /// and not the replay winit synthesizes when focus moves between the two
    /// windows with the key still held ([`shortcut_pressed`]).
    pub fn source_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        source_size: (f32, f32),
    ) -> SourceEvent {
        self.source.follow(event);
        let Seat { focused, mods } = self.source;
        let mut out = SourceEvent::default();
        if focused
            && let Some(changed) = self
                .inspector
                .as_mut()
                .and_then(|inspector| inspector.spacing_shortcut(event, mods))
        {
            out.consumed = true;
            out.redraw_source = changed;
            out.redraw_panel = changed;
            self.panel_semantics_dirty |= changed;
            return out;
        }
        if let WindowEvent::KeyboardInput {
            event: key,
            is_synthetic,
            ..
        } = event
            && focused
            && koru_keys::registry::get(ShortcutId::ToggleUiInspector)
                .chord
                .matches(&key.logical_key, key.text.as_deref(), mods)
        {
            out.consumed = true;
            if shortcut_pressed(key.state, key.repeat, *is_synthetic) {
                let was_open = self.is_open();
                let open = self.toggle(source_size);
                out.opened = open && !was_open;
                out.closed = was_open && !open;
                out.redraw_source = true;
                out.redraw_panel = open;
            }
            return out;
        }
        let Some(inspector) = self.inspector.as_mut() else {
            return out;
        };
        let before = watched(inspector);
        let result = inspector.source_event(event, scale, 1.0);
        let changed = before != watched(inspector);
        out.consumed = result.consumed;
        out.redraw_source = result.redraw;
        out.redraw_panel = result.redraw || changed;
        self.panel_semantics_dirty |= changed || result.structure_changed;
        if result.close {
            self.close();
            out.closed = true;
            out.redraw_source = true;
        }
        out
    }

    /// One event of the panel's own window. `scale` is that window's scale
    /// factor. Every event of the panel's window is the inspector's; the
    /// host has no content of its own there.
    ///
    /// The window's own close request is not an event here: a host answers
    /// it with [`close`](Self::close).
    pub fn panel_event(&mut self, event: &WindowEvent, scale: f64) -> PanelEvent {
        self.panel.follow(event);
        let mods = self.panel.mods;
        let mut out = PanelEvent::default();
        let Some(inspector) = self.inspector.as_mut() else {
            return out;
        };
        if let Some(changed) = inspector.spacing_shortcut(event, mods) {
            out.redraw_panel = changed;
            out.redraw_source = changed;
            self.panel_semantics_dirty |= changed;
            return out;
        }
        let toggle = matches!(event, WindowEvent::KeyboardInput { event: key, is_synthetic, .. }
            if shortcut_pressed(key.state, key.repeat, *is_synthetic)
                && koru_keys::registry::get(ShortcutId::ToggleUiInspector)
                    .chord
                    .matches(&key.logical_key, key.text.as_deref(), mods));
        if toggle {
            // A paused inspection resumes; only a live one closes.
            if inspector.shortcut_requests_close() {
                self.close();
                out.closed = true;
            } else {
                self.panel_semantics_dirty = true;
                out.redraw_panel = true;
            }
            out.redraw_source = true;
            return out;
        }
        let before = watched(inspector);
        let result = inspector.panel_event(event, scale, mods);
        let changed = before != watched(inspector);
        out.drag_window = result.drag_window;
        out.reveal_source = result.reveal_source;
        out.redraw_panel = result.redraw;
        out.redraw_source = result.redraw && changed;
        self.panel_semantics_dirty |= inspector.dirty_semantics || result.structure_changed;
        if result.close {
            self.close();
            out.closed = true;
            out.redraw_source = true;
        }
        out
    }

    /// Whether the panel asks to stay above other windows. A host compares
    /// this with its window after each panel event and applies a difference.
    pub fn panel_pinned(&self) -> bool {
        self.inspector
            .as_ref()
            .map_or(self.placement.geometry.pinned, |inspector| inspector.pinned)
    }

    /// The panel's window, for a host about to create it: titled in the
    /// display language, undecorated, resizable, at the size and pin it last
    /// had in this process.
    ///
    /// Its last position is restored only when a caption-sized part of the
    /// window would land on one of `monitors` (each `left, top, right,
    /// bottom` in physical pixels), with `scale` the scale factor of the
    /// monitor the host is on; otherwise the OS chooses.
    pub fn panel_window(&self, scale: f64, monitors: &[(i32, i32, i32, i32)]) -> WindowAttributes {
        let geometry = self.placement.geometry;
        let title = t(crate::accessibility::ui_lang(), CopyKey::InspectorTitle);
        let attributes = Window::default_attributes()
            .with_title(title)
            .with_decorations(false)
            .with_resizable(true)
            .with_inner_size(winit::dpi::LogicalSize::new(
                geometry.size.0,
                geometry.size.1,
            ))
            .with_min_inner_size(winit::dpi::LogicalSize::new(MIN_PANEL, MIN_PANEL))
            .with_window_level(if geometry.pinned {
                WindowLevel::AlwaysOnTop
            } else {
                WindowLevel::Normal
            });
        match crate::settings::restorable_position(
            geometry.position,
            geometry.size,
            scale,
            monitors,
        ) {
            Some((x, y)) => attributes.with_position(winit::dpi::PhysicalPosition::new(x, y)),
            None => attributes,
        }
    }

    /// The panel's window is going, with its last live state. Kept in memory
    /// for the next F12; written nowhere.
    ///
    /// `position` is the outer position in physical pixels and `size` the
    /// client size in logical pixels.
    pub fn panel_closing(
        &mut self,
        position: Option<(i32, i32)>,
        size: (f32, f32),
        maximised: bool,
        pinned: bool,
    ) {
        self.placement.record(position, size, maximised, pinned);
    }

    /// Start measuring a frame of the inspected window, when an inspection
    /// is open and live. `None` otherwise, which costs one test.
    ///
    /// `owner` names the window in the tree; everything the host then draws
    /// through the shared draw seams is recorded beneath it, in the host's
    /// own logical coordinates. `source_size` is the frame's size.
    pub fn begin_capture(
        &mut self,
        owner: &'static str,
        source_size: (f32, f32),
    ) -> Option<Capture> {
        let inspector = self.inspector.as_mut()?;
        inspector.constrain(source_size.0, source_size.1);
        if !inspector.capture_live() {
            return None;
        }
        inspector.frame += 1;
        let started = Instant::now();
        let viewport =
            vello::kurbo::Rect::new(0.0, 0.0, f64::from(source_size.0), f64::from(source_size.1));
        let frame = inspect::Frame::begin(inspector.frame, Affine::IDENTITY, viewport);
        let owner = inspect::scope(owner, "container", Affine::IDENTITY, viewport, |record| {
            record.property(
                "layout",
                "window / overlay",
                "hosted; only instrumented descendants appear",
            );
        });
        Some(Capture {
            started,
            frame,
            owner,
        })
    }

    /// Finish the frame: take what was measured, and draw the selection's
    /// highlight and hint over what the host drew. Call it after every frame
    /// of the inspected window, with whatever
    /// [`begin_capture`](Self::begin_capture) answered.
    ///
    /// Returns whether the panel now shows something different.
    pub fn end_capture(&mut self, capture: Option<Capture>, scene: &mut Scene) -> bool {
        let Some(inspector) = self.inspector.as_mut() else {
            return false;
        };
        if let Some(Capture {
            started,
            frame,
            owner,
        }) = capture
        {
            // The owner's scope closes before the frame is read.
            drop(owner);
            let snapshot = frame.finish();
            inspector.capture_micros = started.elapsed().as_micros();
            inspector.accept(snapshot);
        }
        inspector.draw_overlay(scene, Affine::IDENTITY);
        let changed = inspector.capture_live() || inspector.dirty_semantics;
        if changed {
            inspector.dirty_semantics = false;
            self.panel_semantics_dirty = true;
        }
        changed
    }

    /// Draw the panel into its own window's frame, `size` logical pixels.
    pub fn draw_panel(&mut self, scene: &mut Scene, size: (f32, f32)) {
        let Some(inspector) = self.inspector.as_mut() else {
            return;
        };
        if self.panel_size != Some(size) {
            inspector.panel_viewport(size.0, size.1);
            self.panel_size = Some(size);
        }
        inspector.draw(scene, Affine::IDENTITY);
        if inspector.dirty_semantics {
            inspector.dirty_semantics = false;
            self.panel_semantics_dirty = true;
        }
    }

    /// Show the inspector what the inspected window last published to a
    /// screen reader, so Details can name the semantic node under a pick.
    /// The inspector never asks for a tree to be built for its own sake:
    /// call this only with a tree the host was publishing anyway.
    pub fn observe_source_semantics(&mut self, nodes: &[SemanticNode]) {
        if let Some(inspector) = self.inspector.as_mut() {
            inspector.observe_semantics(nodes);
        }
    }

    /// Whether the panel's semantic tree changed since this was last asked.
    pub fn take_panel_semantics_dirty(&mut self) -> bool {
        let inspector_dirty = self
            .inspector
            .as_mut()
            .is_some_and(|inspector| std::mem::take(&mut inspector.dirty_semantics));
        std::mem::take(&mut self.panel_semantics_dirty) || inspector_dirty
    }

    /// Everything the panel offers a screen reader, as a whole window.
    /// `None` while the inspector is closed.
    pub fn panel_semantics(
        &self,
        slot: WindowSlot,
        scale: f64,
        size: (f32, f32),
    ) -> Option<WindowSemantics> {
        let inspector = self.inspector.as_ref()?;
        Some(WindowSemantics {
            window: slot,
            label: t(crate::accessibility::ui_lang(), CopyKey::InspectorTitle).into(),
            scale,
            client: Rect::new(0.0, 0.0, size.0, size.1),
            focus: inspector.focused(slot),
            children: inspector.semantics(slot, 1.0),
            scope: Scope::Whole,
            retired: Vec::new(),
        })
    }

    /// A screen reader's request on one of the panel's controls.
    pub fn panel_action(&mut self, part: ControlPart, action: Action) -> PanelEvent {
        let mut out = PanelEvent::default();
        let Some(inspector) = self.inspector.as_mut() else {
            return out;
        };
        let Some(close) = inspector.accessibility_action(part, action) else {
            return out;
        };
        out.redraw_panel = true;
        out.redraw_source = true;
        self.panel_semantics_dirty = true;
        if close {
            self.close();
            out.closed = true;
        }
        out
    }

    /// The names of the selected record and its ancestors, outermost first:
    /// the path the Hierarchy tab shows. Empty with nothing selected.
    pub fn selected_ancestry(&self) -> Vec<&'static str> {
        let Some(inspector) = self.inspector.as_ref() else {
            return Vec::new();
        };
        let mut path = Vec::new();
        let mut at = inspector.selected;
        while let Some(record) = at.and_then(|index| inspector.snapshot.records.get(index)) {
            path.push(record.name);
            at = record.parent;
        }
        path.reverse();
        path
    }

    /// How long the last live capture took, in microseconds: the enabled
    /// cost of inspection, measured where the frame is drawn. Zero before
    /// the first capture and while closed.
    pub fn capture_micros(&self) -> u128 {
        self.inspector
            .as_ref()
            .map_or(0, |inspector| inspector.capture_micros)
    }
}

/// What the two windows both show of the inspector's state: a change in any
/// of it made by one window's event is a repaint owed to the other.
fn watched(inspector: &Inspector) -> (Option<usize>, bool, bool, bool) {
    (
        inspector.selected,
        inspector.picking,
        inspector.capture_live(),
        inspector.spacing_rays,
    )
}

#[cfg(test)]
mod tests;
