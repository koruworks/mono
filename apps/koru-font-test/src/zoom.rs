// SPDX-License-Identifier: MPL-2.0

//! Magnify the completed native-resolution texture, never the font geometry.
use vello::wgpu;
use winit::{
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
};

pub struct Zoom {
    pub factor: u32,
    origin: [f64; 2],
    cursor: [f64; 2],
    modifiers: ModifiersState,
    dragging: bool,
}

impl Default for Zoom {
    fn default() -> Self {
        Self {
            factor: 1,
            origin: [0.0; 2],
            cursor: [0.0; 2],
            modifiers: ModifiersState::empty(),
            dragging: false,
        }
    }
}

impl Zoom {
    fn clamp(&mut self, size: [u32; 2]) {
        for (axis, extent) in size.into_iter().enumerate() {
            self.origin[axis] = self.origin[axis].clamp(
                0.0,
                (extent as f64 - extent as f64 / self.factor as f64).max(0.0),
            );
        }
    }

    fn change(&mut self, factor: u32, size: [u32; 2]) {
        let next = factor.clamp(1, 32);
        for axis in 0..2 {
            let source = self.origin[axis].floor() + self.cursor[axis] / self.factor as f64;
            self.origin[axis] = source - self.cursor[axis] / next as f64;
        }
        self.factor = next;
        self.clamp(size);
        if next == 1 {
            self.dragging = false;
        }
    }

    /// Returns true for inspection input that should not reach the editor.
    pub fn event(&mut self, event: &WindowEvent, size: [u32; 2]) -> bool {
        match event {
            WindowEvent::ModifiersChanged(state) => self.modifiers = state.state(),
            WindowEvent::Focused(false) => {
                self.dragging = false;
                self.modifiers = ModifiersState::empty();
            }
            WindowEvent::Resized(_) => self.clamp(size),
            WindowEvent::CursorMoved { position, .. } => {
                let next = [position.x, position.y];
                if self.dragging {
                    for axis in 0..2 {
                        self.origin[axis] -= (next[axis] - self.cursor[axis]) / self.factor as f64;
                    }
                    self.clamp(size);
                }
                self.cursor = next;
                return self.factor > 1;
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } if self.factor > 1 => {
                self.dragging = *state == ElementState::Pressed;
                return true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64 * 48.0,
                    MouseScrollDelta::PixelDelta(p) => p.y,
                };
                if self.modifiers.control_key() {
                    if dy != 0.0 {
                        self.change(
                            if dy > 0.0 {
                                self.factor * 2
                            } else {
                                self.factor / 2
                            },
                            size,
                        );
                    }
                    return true;
                }
                if self.factor > 1 {
                    self.origin[1] -= dy / self.factor as f64;
                    self.clamp(size);
                    return true;
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let code = event.physical_key;
                let ctrl = self.modifiers.control_key();
                let zoom_key = ctrl
                    && matches!(
                        code,
                        PhysicalKey::Code(
                            KeyCode::Equal
                                | KeyCode::Minus
                                | KeyCode::Digit0
                                | KeyCode::NumpadAdd
                                | KeyCode::NumpadSubtract
                                | KeyCode::Numpad0
                        )
                    );
                let reset = self.factor > 1 && code == PhysicalKey::Code(KeyCode::Escape);
                if zoom_key || reset {
                    if event.state == ElementState::Pressed {
                        let next = match code {
                            PhysicalKey::Code(KeyCode::Equal | KeyCode::NumpadAdd) => {
                                self.factor * 2
                            }
                            PhysicalKey::Code(KeyCode::Minus | KeyCode::NumpadSubtract) => {
                                self.factor / 2
                            }
                            _ => 1,
                        };
                        self.change(next, size);
                    }
                    return true;
                }
                if self.factor > 1
                    && !matches!(
                        code,
                        PhysicalKey::Code(
                            KeyCode::F3
                                | KeyCode::F4
                                | KeyCode::F5
                                | KeyCode::F6
                                | KeyCode::F7
                                | KeyCode::F8
                                | KeyCode::F9
                                | KeyCode::F10
                                | KeyCode::F11
                        )
                    )
                {
                    return true;
                }
            }
            WindowEvent::Ime(_) if self.factor > 1 => return true,
            _ => {}
        }
        false
    }
}

pub struct PixelBlitter {
    pipeline: wgpu::RenderPipeline,
    settings: wgpu::Buffer,
}

impl PixelBlitter {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Exact pixel magnifier"),
            source: wgpu::ShaderSource::Wgsl(include_str!("zoom.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Exact pixel magnifier"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let settings = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Pixel magnifier settings"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { pipeline, settings }
    }

    pub fn copy(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        zoom: &Zoom,
        coverage: crate::coverage::Coverage,
    ) {
        let values = [
            zoom.origin[0].floor() as u32,
            zoom.origin[1].floor() as u32,
            zoom.factor,
            0,
            coverage.gamma.to_bits(),
            coverage.cutoff.to_bits(),
            u32::from(coverage.active()),
            0,
        ];
        let bytes: Vec<u8> = values.into_iter().flat_map(u32::to_le_bytes).collect();
        queue.write_buffer(&self.settings, 0, &bytes);
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Pixel magnifier inputs"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.settings.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Pixel magnifier"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bindings, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests;
