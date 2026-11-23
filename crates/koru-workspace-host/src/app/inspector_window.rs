// SPDX-License-Identifier: MPL-2.0

//! Native inspection surface, owned by the app pane it measures. It shares the
//! existing GPU device/renderer, but has its own DPI, input and AT provider.
use super::*;
use crate::inspector::{Inspector, Placement};

pub(super) struct NativeInspector {
    // Retire the provider before dropping the native window and surface.
    access: crate::accessibility::PaneAccessibility,
    surface: RenderSurface<'static>,
    window: Arc<Window>,
    placement: Placement,
    pinned: bool,
    mods: winit::keyboard::ModifiersState,
    resize_edge: Option<ResizeDirection>,
    maximized: bool,
    needs_frame: std::cell::Cell<bool>,
}

impl NativeInspector {
    pub fn remembered_geometry(&mut self) -> crate::inspector::Geometry {
        self.placement.remember(&self.window, self.pinned);
        self.placement.geometry
    }

    pub fn sync_theme(&self) {
        self.window.set_theme(window_theme_for_pref());
        if resize::is_ours() {
            frame::apply(
                &self.window,
                theme::p().panel_border,
                self.window.is_maximized(),
            );
        }
        self.request_redraw();
    }

    pub fn set_visible(&self, visible: bool) {
        self.window.set_visible(visible);
        if visible {
            self.request_redraw();
        }
    }

    pub fn invalidate(&mut self) {
        self.access.invalidate_structure();
        self.request_redraw();
    }

    pub fn request_redraw(&self) {
        self.needs_frame.set(true);
        self.window.request_redraw();
    }

    fn sync_pin(&mut self, pinned: bool) {
        if self.pinned == pinned {
            return;
        }
        self.pinned = pinned;
        self.window.set_window_level(if pinned {
            WindowLevel::AlwaysOnTop
        } else {
            WindowLevel::Normal
        });
        self.placement.remember(&self.window, pinned);
    }
}

impl Drop for NativeInspector {
    fn drop(&mut self) {
        self.placement.remember(&self.window, self.pinned);
        self.access.retire();
    }
}

impl<H: Host> App<H> {
    pub(super) fn open_inspector_window(&mut self, event_loop: &ActiveEventLoop, source: WindowId) {
        if let Err(error) = self.try_open_inspector_window(event_loop, source) {
            tracing::error!("Could not open UI inspector: {error}");
            // Opening a diagnostic tool must never terminate the workspace.
            crate::startup::warn(&format!("Could not open UI inspector: {error}"));
        }
    }

    fn try_open_inspector_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        source: WindowId,
    ) -> Result<(), String> {
        // Closing and immediately reopening must not race the async disk writer.
        let remembered = self.panes.get(&source).and_then(|p| p.inspector_geometry);
        let placement = Placement::open(self.gateway_root.as_deref(), remembered);
        let geometry = placement.geometry;
        let title = koru_i18n::t(
            crate::accessibility::ui_lang(),
            koru_i18n::Key::InspectorTitle,
        );
        let mut attrs = Window::default_attributes()
            .with_title(title)
            .with_visible(false)
            .with_resizable(true)
            .with_inner_size(winit::dpi::LogicalSize::new(
                geometry.size.0,
                geometry.size.1,
            ))
            .with_min_inner_size(winit::dpi::LogicalSize::new(360.0, 360.0))
            .with_window_level(if geometry.pinned {
                WindowLevel::AlwaysOnTop
            } else {
                WindowLevel::Normal
            });
        #[cfg(not(target_os = "macos"))]
        {
            attrs = attrs.with_decorations(false);
        }
        let monitors: Vec<_> = event_loop
            .available_monitors()
            .map(|m| {
                let p = m.position();
                let s = m.size();
                (
                    p.x,
                    p.y,
                    p.x.saturating_add(s.width as i32),
                    p.y.saturating_add(s.height as i32),
                )
            })
            .collect();
        let scale = self.panes.get(&source).map_or(1.0, |p| p.scale());
        if let Some((x, y)) =
            restorable_position(geometry.position, geometry.size, scale, &monitors)
        {
            attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
        }
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        let access = self.seat_accessibility(event_loop, &window);
        window.set_theme(window_theme_for_pref());
        if resize::is_ours() {
            frame::apply(&window, theme::p().panel_border, false);
        }
        let size = window.inner_size();
        let mut surface = pollster::block_on(self.context.create_surface(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            PRESENT_MODE,
        ))
        .map_err(|e| e.to_string())?;
        surface.config.desired_maximum_frame_latency = 1;
        self.context.set_present_mode(&mut surface, PRESENT_MODE);
        if !self.renderers.contains_key(&surface.dev_id) {
            let device = &self.context.devices[surface.dev_id].device;
            watch_device_errors(device);
            self.renderers
                .insert(surface.dev_id, koru_window::build_renderer(device)?);
        }
        let pane = self.panes.get_mut(&source).ok_or("Source window closed")?;
        let mut inspector = Inspector::new(
            pane.surface.config.width as f32 / pane.scale() as f32 / self.zoom,
            pane.surface.config.height as f32 / pane.scale() as f32 / self.zoom,
        );
        inspector.pinned = geometry.pinned;
        inspector.panel_viewport(
            size.width as f32 / window.scale_factor() as f32,
            size.height as f32 / window.scale_factor() as f32,
        );
        pane.inspector = Some(inspector);
        pane.inspector_window = Some(NativeInspector {
            access,
            surface,
            window: window.clone(),
            placement,
            pinned: geometry.pinned,
            mods: Default::default(),
            resize_edge: None,
            maximized: false,
            needs_frame: true.into(),
        });
        let native = pane.inspector_window.as_mut().unwrap();
        native.placement.remember(&native.window, native.pinned);
        // Hidden windows do not receive WM_PAINT. Reveal only after a successful present.
        if self.redraw_inspector(source) {
            let native = self
                .panes
                .get_mut(&source)
                .unwrap()
                .inspector_window
                .as_mut()
                .unwrap();
            native.access.revealed();
            window.set_visible(true);
            window.focus_window();
        } else {
            self.panes.get_mut(&source).unwrap().close_inspector();
            return Err("The inspector could not present its first frame".into());
        }
        Ok(())
    }

    pub(super) fn native_inspector_event(&mut self, id: WindowId, event: &WindowEvent) -> bool {
        let Some(source) = self.panes.iter().find_map(|(source, pane)| {
            pane.inspector_window
                .as_ref()
                .filter(|n| n.window.id() == id)
                .map(|_| *source)
        }) else {
            return false;
        };
        let pane = self.panes.get_mut(&source).unwrap();
        let native = pane.inspector_window.as_mut().unwrap();
        native.access.process_event(&native.window, event);
        if let Some(changed) = pane
            .inspector
            .as_mut()
            .unwrap()
            .spacing_shortcut(event, native.mods)
        {
            if changed {
                native.invalidate();
                self.request_redraw(source);
            }
            return true;
        }
        if matches!(event, WindowEvent::RedrawRequested) {
            self.redraw_inspector(source);
            return true;
        }
        let toggle = matches!(event, WindowEvent::KeyboardInput { event, is_synthetic, .. }
            if crate::inspector::shortcut_pressed(event.state, event.repeat, *is_synthetic) &&
            koru_keys::registry::get(koru_keys::ShortcutId::ToggleUiInspector).chord
                .matches(&event.logical_key, event.text.as_deref(), native.mods));
        if toggle && !pane.inspector.as_mut().unwrap().shortcut_requests_close() {
            native.invalidate();
            pane.sync_cursor_icon();
            self.request_redraw(source);
            return true;
        }
        if matches!(event, WindowEvent::CloseRequested) || toggle {
            pane.close_inspector();
            pane.sync_cursor_icon();
            self.request_redraw(source);
            return true;
        }
        let inspector = pane.inspector.as_mut().unwrap();
        if let WindowEvent::CursorMoved { position, .. } = event {
            let scale = native.window.scale_factor() as f32;
            let size = native.window.inner_size();
            native.resize_edge = if resize::is_ours() {
                resize::edge(
                    size.width as f32 / scale,
                    size.height as f32 / scale,
                    position.x as f32 / scale,
                    position.y as f32 / scale,
                    native.window.is_maximized(),
                )
            } else {
                None
            };
            native.window.set_cursor(
                native
                    .resize_edge
                    .map(CursorIcon::from)
                    .unwrap_or(CursorIcon::Default),
            );
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) && let Some(edge) = native.resize_edge
        {
            let _ = native.window.drag_resize_window(edge);
            return true;
        }
        match event {
            WindowEvent::ModifiersChanged(mods) => native.mods = mods.state(),
            WindowEvent::Focused(false) => native.mods = Default::default(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                let maximized = native.window.is_maximized();
                if resize::is_ours() && native.maximized != maximized {
                    native.maximized = maximized;
                    frame::apply(&native.window, theme::p().panel_border, maximized);
                }
                let size = native.window.inner_size();
                if size.width > 0 && size.height > 0 {
                    self.context
                        .resize_surface(&mut native.surface, size.width, size.height);
                    inspector.panel_viewport(
                        size.width as f32 / native.window.scale_factor() as f32,
                        size.height as f32 / native.window.scale_factor() as f32,
                    );
                    native.access.invalidate_structure();
                    native.request_redraw();
                    native.placement.remember(&native.window, native.pinned);
                }
            }
            WindowEvent::Moved(_) => {
                native.placement.remember(&native.window, native.pinned);
            }
            _ => {}
        }
        let before = (
            inspector.selected,
            inspector.picking,
            inspector.capture_live(),
            inspector.spacing_rays,
        );
        let result = inspector.panel_event(event, native.window.scale_factor(), native.mods);
        let source_changed = before
            != (
                inspector.selected,
                inspector.picking,
                inspector.capture_live(),
                inspector.spacing_rays,
            );
        if result.drag_window {
            let _ = native.window.drag_window();
        }
        native.sync_pin(inspector.pinned);
        if inspector.dirty_semantics || result.structure_changed {
            native.access.invalidate_structure();
        }
        if result.reveal_source && !pane.window.has_focus() {
            if pane.window.is_minimized() == Some(true) {
                pane.window.set_minimized(false);
            }
            pane.window.focus_window();
        }
        if result.close {
            pane.close_inspector();
        }
        if result.redraw {
            if let Some(native) = pane.inspector_window.as_ref() {
                native.request_redraw();
            }
            pane.sync_cursor_icon();
            if source_changed || result.close {
                self.request_redraw(source);
            }
        }
        true
    }

    fn redraw_inspector(&mut self, source: WindowId) -> bool {
        let Some(pane) = self.panes.get_mut(&source) else {
            return false;
        };
        let (Some(inspector), Some(native)) =
            (pane.inspector.as_mut(), pane.inspector_window.as_mut())
        else {
            return false;
        };
        if !native.needs_frame.replace(false) {
            return true;
        }
        let (width, height) = (native.surface.config.width, native.surface.config.height);
        if native.window.is_minimized() == Some(true) || width == 0 || height == 0 {
            return false;
        }
        self.scene.reset();
        inspector.draw(&mut self.scene, Affine::scale(native.window.scale_factor()));
        if inspector.dirty_semantics {
            native.access.invalidate_structure();
            inspector.dirty_semantics = false;
        }
        let device = &self.context.devices[native.surface.dev_id];
        let Some(renderer) = self.renderers.get_mut(&native.surface.dev_id) else {
            return false;
        };
        koru_window::renderer::prepare_window_text(&mut self.scene);
        if let Err(error) = renderer.render_to_texture(
            &device.device,
            &device.queue,
            &self.scene,
            &native.surface.target_view,
            &vello::RenderParams {
                base_color: theme::p().panel_bg,
                width,
                height,
                antialiasing_method: koru_window::renderer::window_antialiasing(),
            },
        ) {
            tracing::error!("Inspector rendering failed: {error}");
            return false;
        }
        let Ok(frame) = native.surface.surface.get_current_texture() else {
            return false;
        };
        let mut encoder = device
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("inspector blit"),
            });
        native.surface.blitter.copy(
            &device.device,
            &mut encoder,
            &native.surface.target_view,
            &frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default()),
        );
        device.queue.submit([encoder.finish()]);
        frame.present();
        true
    }

    pub(super) fn service_inspector_accessibility(&mut self) {
        for pane in self.panes.values_mut() {
            let (Some(inspector), Some(native)) =
                (pane.inspector.as_mut(), pane.inspector_window.as_mut())
            else {
                continue;
            };
            if inspector.dirty_semantics {
                native.access.invalidate_structure();
                inspector.dirty_semantics = false;
            }
            let wants = native.access.poll();
            if wants.snapshot {
                let size = native
                    .window
                    .inner_size()
                    .to_logical::<f32>(native.window.scale_factor());
                let slot = native.access.slot();
                native
                    .access
                    .publish(&crate::accessibility::WindowSemantics {
                        window: slot,
                        label: koru_i18n::t(
                            crate::accessibility::ui_lang(),
                            koru_i18n::Key::InspectorTitle,
                        )
                        .into(),
                        scale: native.window.scale_factor(),
                        client: koru_draw::geometry::Rect::new(0.0, 0.0, size.width, size.height),
                        focus: inspector.focused(slot),
                        children: inspector.semantics(slot, 1.0),
                        scope: crate::accessibility::Scope::Whole,
                        retired: Vec::new(),
                    });
            }
            let mut close = false;
            for action in native.access.resolve() {
                close |= inspector
                    .accessibility_action(action.occurrence.part, action.action)
                    .unwrap_or(false);
                native.sync_pin(inspector.pinned);
                native.request_redraw();
                pane.needs_frame.set(true);
                pane.window.request_redraw();
            }
            if close {
                pane.close_inspector();
                pane.sync_cursor_icon();
            }
        }
    }
}

/// **The first-run window's half of the inspector panel.** The tool is the
/// same one the workspace window gets — one `NativeInspector`, one
/// [`Inspector`] — so these are [`App`]'s four functions with the pane map
/// replaced by the single window `OnboardingApp` owns: no zoom, no source
/// lookup, one redraw to ask for. Deliberately a second copy of the glue, the
/// same reason `seat_accessibility` is: the two apps have no common type, and
/// the audit in `crate::accessibility::tests` counts window creation sites
/// against seat sites.
impl<H: Host> OnboardingApp<H> {
    pub(super) fn open_inspector_window(&mut self, event_loop: &ActiveEventLoop) {
        if let Err(error) = self.try_open_inspector_window(event_loop) {
            tracing::error!("Could not open UI inspector: {error}");
            // Opening a diagnostic tool must never terminate onboarding.
            crate::startup::warn(&format!("Could not open UI inspector: {error}"));
        }
    }

    fn try_open_inspector_window(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        // Closing and immediately reopening must not race the async disk writer.
        let placement = Placement::open(self.gateway_root.as_deref(), self.inspector_geometry);
        let geometry = placement.geometry;
        let title = koru_i18n::t(
            crate::accessibility::ui_lang(),
            koru_i18n::Key::InspectorTitle,
        );
        let mut attrs = Window::default_attributes()
            .with_title(title)
            .with_visible(false)
            .with_resizable(true)
            .with_inner_size(winit::dpi::LogicalSize::new(
                geometry.size.0,
                geometry.size.1,
            ))
            .with_min_inner_size(winit::dpi::LogicalSize::new(360.0, 360.0))
            .with_window_level(if geometry.pinned {
                WindowLevel::AlwaysOnTop
            } else {
                WindowLevel::Normal
            });
        #[cfg(not(target_os = "macos"))]
        {
            attrs = attrs.with_decorations(false);
        }
        let monitors: Vec<_> = event_loop
            .available_monitors()
            .map(|m| {
                let p = m.position();
                let s = m.size();
                (
                    p.x,
                    p.y,
                    p.x.saturating_add(s.width as i32),
                    p.y.saturating_add(s.height as i32),
                )
            })
            .collect();
        let scale = self
            .live
            .as_ref()
            .map_or(1.0, |live| live.window.scale_factor());
        if let Some((x, y)) =
            restorable_position(geometry.position, geometry.size, scale, &monitors)
        {
            attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
        }
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        let access = self.seat_accessibility(event_loop, &window);
        window.set_theme(window_theme_for_pref());
        if resize::is_ours() {
            frame::apply(&window, theme::p().panel_border, false);
        }
        let size = window.inner_size();
        let context = self
            .context
            .as_mut()
            .ok_or("The GPU context is not ready")?;
        let mut surface = pollster::block_on(context.create_surface(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            PRESENT_MODE,
        ))
        .map_err(|e| e.to_string())?;
        surface.config.desired_maximum_frame_latency = 1;
        context.set_present_mode(&mut surface, PRESENT_MODE);
        if !self.renderers.contains_key(&surface.dev_id) {
            let device = &context.devices[surface.dev_id].device;
            watch_device_errors(device);
            self.renderers
                .insert(surface.dev_id, koru_window::build_renderer(device)?);
        }
        let live = self.live.as_ref().ok_or("First-run window closed")?;
        let live_scale = live.window.scale_factor() as f32;
        let mut inspector = Inspector::new(
            live.surface.config.width as f32 / live_scale,
            live.surface.config.height as f32 / live_scale,
        );
        inspector.pinned = geometry.pinned;
        inspector.panel_viewport(
            size.width as f32 / window.scale_factor() as f32,
            size.height as f32 / window.scale_factor() as f32,
        );
        self.inspector = Some(inspector);
        self.inspector_window = Some(NativeInspector {
            access,
            surface,
            window: window.clone(),
            placement,
            pinned: geometry.pinned,
            mods: Default::default(),
            resize_edge: None,
            maximized: false,
            needs_frame: true.into(),
        });
        let native = self.inspector_window.as_mut().unwrap();
        native.placement.remember(&native.window, native.pinned);
        // Hidden windows do not receive WM_PAINT. Reveal only after a successful present.
        if self.redraw_inspector() {
            let native = self.inspector_window.as_mut().unwrap();
            native.access.revealed();
            window.set_visible(true);
            window.focus_window();
        } else {
            self.close_inspector();
            return Err("The inspector could not present its first frame".into());
        }
        Ok(())
    }

    pub(super) fn native_inspector_event(&mut self, id: WindowId, event: &WindowEvent) -> bool {
        if self
            .inspector_window
            .as_ref()
            .is_none_or(|native| native.window.id() != id)
        {
            return false;
        }
        let native = self.inspector_window.as_mut().unwrap();
        native.access.process_event(&native.window, event);
        if let Some(changed) = self
            .inspector
            .as_mut()
            .and_then(|inspector| inspector.spacing_shortcut(event, native.mods))
        {
            if changed {
                native.invalidate();
                self.request_redraw();
            }
            return true;
        }
        if matches!(event, WindowEvent::RedrawRequested) {
            self.redraw_inspector();
            return true;
        }
        let toggle = matches!(event, WindowEvent::KeyboardInput { event, is_synthetic, .. }
            if crate::inspector::shortcut_pressed(event.state, event.repeat, *is_synthetic) &&
            koru_keys::registry::get(koru_keys::ShortcutId::ToggleUiInspector).chord
                .matches(&event.logical_key, event.text.as_deref(), native.mods));
        if toggle && !self.inspector.as_mut().unwrap().shortcut_requests_close() {
            native.invalidate();
            self.sync_cursor_icon();
            self.request_redraw();
            return true;
        }
        if matches!(event, WindowEvent::CloseRequested) || toggle {
            self.close_inspector();
            self.sync_cursor_icon();
            self.request_redraw();
            return true;
        }
        let inspector = self.inspector.as_mut().unwrap();
        if let WindowEvent::CursorMoved { position, .. } = event {
            let scale = native.window.scale_factor() as f32;
            let size = native.window.inner_size();
            native.resize_edge = if resize::is_ours() {
                resize::edge(
                    size.width as f32 / scale,
                    size.height as f32 / scale,
                    position.x as f32 / scale,
                    position.y as f32 / scale,
                    native.window.is_maximized(),
                )
            } else {
                None
            };
            native.window.set_cursor(
                native
                    .resize_edge
                    .map(CursorIcon::from)
                    .unwrap_or(CursorIcon::Default),
            );
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) && let Some(edge) = native.resize_edge
        {
            let _ = native.window.drag_resize_window(edge);
            return true;
        }
        match event {
            WindowEvent::ModifiersChanged(mods) => native.mods = mods.state(),
            WindowEvent::Focused(false) => native.mods = Default::default(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                let maximized = native.window.is_maximized();
                if resize::is_ours() && native.maximized != maximized {
                    native.maximized = maximized;
                    frame::apply(&native.window, theme::p().panel_border, maximized);
                }
                let size = native.window.inner_size();
                if size.width > 0 && size.height > 0 {
                    if let Some(context) = self.context.as_mut() {
                        context.resize_surface(&mut native.surface, size.width, size.height);
                    }
                    inspector.panel_viewport(
                        size.width as f32 / native.window.scale_factor() as f32,
                        size.height as f32 / native.window.scale_factor() as f32,
                    );
                    native.access.invalidate_structure();
                    native.request_redraw();
                    native.placement.remember(&native.window, native.pinned);
                }
            }
            WindowEvent::Moved(_) => {
                native.placement.remember(&native.window, native.pinned);
            }
            _ => {}
        }
        let before = (
            inspector.selected,
            inspector.picking,
            inspector.capture_live(),
            inspector.spacing_rays,
        );
        let result = inspector.panel_event(event, native.window.scale_factor(), native.mods);
        let source_changed = before
            != (
                inspector.selected,
                inspector.picking,
                inspector.capture_live(),
                inspector.spacing_rays,
            );
        if result.drag_window {
            let _ = native.window.drag_window();
        }
        native.sync_pin(inspector.pinned);
        if inspector.dirty_semantics || result.structure_changed {
            native.access.invalidate_structure();
        }
        if result.reveal_source
            && let Some(live) = self.live.as_ref()
            && !live.window.has_focus()
        {
            if live.window.is_minimized() == Some(true) {
                live.window.set_minimized(false);
            }
            live.window.focus_window();
        }
        if result.close {
            self.close_inspector();
        }
        if result.redraw {
            if let Some(native) = self.inspector_window.as_ref() {
                native.request_redraw();
            }
            self.sync_cursor_icon();
            if source_changed || result.close {
                self.request_redraw();
            }
        }
        true
    }

    fn redraw_inspector(&mut self) -> bool {
        let (Some(inspector), Some(native)) =
            (self.inspector.as_mut(), self.inspector_window.as_mut())
        else {
            return false;
        };
        if !native.needs_frame.replace(false) {
            return true;
        }
        let (width, height) = (native.surface.config.width, native.surface.config.height);
        if native.window.is_minimized() == Some(true) || width == 0 || height == 0 {
            return false;
        }
        self.scene.reset();
        inspector.draw(&mut self.scene, Affine::scale(native.window.scale_factor()));
        if inspector.dirty_semantics {
            native.access.invalidate_structure();
            inspector.dirty_semantics = false;
        }
        let Some(context) = self.context.as_ref() else {
            return false;
        };
        let device = &context.devices[native.surface.dev_id];
        let Some(renderer) = self.renderers.get_mut(&native.surface.dev_id) else {
            return false;
        };
        koru_window::renderer::prepare_window_text(&mut self.scene);
        if let Err(error) = renderer.render_to_texture(
            &device.device,
            &device.queue,
            &self.scene,
            &native.surface.target_view,
            &vello::RenderParams {
                base_color: theme::p().panel_bg,
                width,
                height,
                antialiasing_method: koru_window::renderer::window_antialiasing(),
            },
        ) {
            tracing::error!("Inspector rendering failed: {error}");
            return false;
        }
        let Ok(frame) = native.surface.surface.get_current_texture() else {
            return false;
        };
        let mut encoder = device
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("inspector blit"),
            });
        native.surface.blitter.copy(
            &device.device,
            &mut encoder,
            &native.surface.target_view,
            &frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default()),
        );
        device.queue.submit([encoder.finish()]);
        frame.present();
        true
    }

    /// The panel's accessibility pass, from `about_to_wait`. Single-window
    /// rather than per-pane, otherwise [`App::service_inspector_accessibility`].
    pub(super) fn service_inspector_accessibility(&mut self) {
        let (Some(inspector), Some(native)) =
            (self.inspector.as_mut(), self.inspector_window.as_mut())
        else {
            return;
        };
        if inspector.dirty_semantics {
            native.access.invalidate_structure();
            inspector.dirty_semantics = false;
        }
        let wants = native.access.poll();
        if wants.snapshot {
            let size = native
                .window
                .inner_size()
                .to_logical::<f32>(native.window.scale_factor());
            let slot = native.access.slot();
            native
                .access
                .publish(&crate::accessibility::WindowSemantics {
                    window: slot,
                    label: koru_i18n::t(
                        crate::accessibility::ui_lang(),
                        koru_i18n::Key::InspectorTitle,
                    )
                    .into(),
                    scale: native.window.scale_factor(),
                    client: koru_draw::geometry::Rect::new(0.0, 0.0, size.width, size.height),
                    focus: inspector.focused(slot),
                    children: inspector.semantics(slot, 1.0),
                    scope: crate::accessibility::Scope::Whole,
                    retired: Vec::new(),
                });
        }
        let mut close = false;
        for action in native.access.resolve() {
            close |= inspector
                .accessibility_action(action.occurrence.part, action.action)
                .unwrap_or(false);
            native.sync_pin(inspector.pinned);
            native.request_redraw();
            if let Some(live) = self.live.as_ref() {
                live.window.request_redraw();
            }
        }
        if close {
            self.close_inspector();
            self.sync_cursor_icon();
        }
    }
}
