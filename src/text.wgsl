struct Resolution {
    size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u_resolution: Resolution;
@group(0) @binding(1) var u_atlas: texture_2d<f32>;
@group(0) @binding(2) var u_sampler: sampler;

struct VsIn {
    // Per-vertex: unit-square corner (0..1, 0..1)
    @location(0) corner: vec2<f32>,
    // Per-instance:
    @location(1) pos: vec2<f32>,
    @location(2) size: vec2<f32>,
    @location(3) uv_min: vec2<f32>,
    @location(4) uv_max: vec2<f32>,
    @location(5) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs(in: VsIn) -> VsOut {
    var out: VsOut;
    let px = in.pos + in.corner * in.size;
    let ndc_x = (px.x / u_resolution.size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (px.y / u_resolution.size.y) * 2.0;
    out.clip = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = mix(in.uv_min, in.uv_max, in.corner);
    out.color = in.color;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    // R8 atlas: glyph alpha mask
    let mask = textureSample(u_atlas, u_sampler, in.uv).r;
    return vec4<f32>(in.color.rgb, in.color.a * mask);
}
