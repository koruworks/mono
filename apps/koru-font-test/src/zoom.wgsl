// SPDX-License-Identifier: MPL-2.0

struct Settings { origin: vec2<u32>, factor: u32, padding: u32, gamma: f32, cutoff: f32, coverage_enabled: u32, padding2: u32 }
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> settings: Settings;

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    return vec4<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fragment(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = settings.origin + vec2<u32>(position.xy) / settings.factor;
    let last = textureDimensions(source) - vec2<u32>(1u);
    // Integer textureLoad: no sampler, interpolation, new hinting or reshaping.
    let color = textureLoad(source, vec2<i32>(min(pixel, last)), 0);
    if settings.coverage_enabled == 0u { return color; }
    // Vello 0.8 writes straight RGBA: remap coverage, preserving ink color.
    var coverage = pow(clamp(color.a, 0.0, 1.0), settings.gamma);
    if color.a < settings.cutoff { coverage = 0.0; }
    return vec4<f32>(mix(vec3<f32>(1.0), color.rgb, coverage), 1.0);
}
