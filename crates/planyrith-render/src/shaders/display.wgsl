// The frame for UIs that composite in gamma space (egui): the sRGB color
// target read back as linear values and written, sRGB-encoded, into an
// `Rgba8Unorm` texture. A separate pass rather than a second view format,
// which WebGL2 and some GL drivers lack (docs/06 §1, docs/11 §4.2).

@group(0) @binding(0) var color: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One triangle covering the target.
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

fn encode(c: f32) -> f32 {
    if c <= 0.0031308 {
        return c * 12.92;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(color, vec2<i32>(pos.xy), 0);
    return vec4<f32>(encode(c.r), encode(c.g), encode(c.b), c.a);
}
