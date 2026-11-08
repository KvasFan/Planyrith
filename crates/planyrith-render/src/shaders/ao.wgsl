// Screen-space ambient occlusion of the realistic look (docs/06 §4.1):
// positions come back from the opaque faces' depth through the inverse
// projection (eye-relative world millimeters), normals from neighboring
// positions; twelve samples in a disk rotated per 4×4 pixel cell, then a
// 4×4 average that multiplies the frame.

struct Ao {
    // Clip space to eye-relative world coordinates.
    eye_from_clip: mat4x4<f32>,
    // x: radius (mm), y: strength 0..1, z, w: target size in pixels.
    params: vec4<f32>,
};

// The faces' depth as the bytes of its float (`fs_ao_depth`); 0: none.
@group(0) @binding(0) var depth: texture_2d<f32>;
@group(0) @binding(1) var<uniform> ao: Ao;

@group(0) @binding(2) var occlusion: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(uv * 2.0 - 1.0, 0.0, 1.0);
}

fn size() -> vec2<i32> {
    return vec2<i32>(i32(ao.params.z), i32(ao.params.w));
}

// The eye-relative position under pixel `px` at depth `z`.
fn unproject(px: vec2<i32>, z: f32) -> vec3<f32> {
    let uv = (vec2<f32>(px) + 0.5) / vec2<f32>(size());
    let p = ao.eye_from_clip * vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, z, 1.0);
    return p.xyz / p.w;
}

// The depth stored under `px`.
fn depth_at(px: vec2<i32>) -> f32 {
    let b = vec4<u32>(round(textureLoad(depth, px, 0) * 255.0));
    return bitcast<f32>(b.x | (b.y << 8u) | (b.z << 16u) | (b.w << 24u));
}

// The position under `px`; w = 0 where no face was drawn (reverse Z: 0).
fn position(px: vec2<i32>) -> vec4<f32> {
    let q = clamp(px, vec2<i32>(0), size() - vec2<i32>(1));
    let z = depth_at(q);
    if (z <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(unproject(q, z), 1.0);
}

// The shorter of the two one-sided differences (a step at a silhouette
// would tilt the normal).
fn delta(c: vec3<f32>, a: vec4<f32>, b: vec4<f32>) -> vec3<f32> {
    let da = a.xyz - c;
    let db = c - b.xyz;
    if (a.w == 0.0) {
        return db;
    }
    if (b.w == 0.0 || dot(da, da) < dot(db, db)) {
        return da;
    }
    return db;
}

@fragment
fn fs_ao(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let px = vec2<i32>(pos.xy);
    let c = position(px);
    if (c.w == 0.0) {
        return vec4<f32>(1.0);
    }
    let dx = delta(c.xyz, position(px + vec2<i32>(1, 0)), position(px - vec2<i32>(1, 0)));
    let dy = delta(c.xyz, position(px + vec2<i32>(0, 1)), position(px - vec2<i32>(0, 1)));
    var n = normalize(cross(dx, dy));
    if (dot(n, -c.xyz) < 0.0) {
        n = -n;
    }
    let radius = ao.params.x;
    // Millimeters per pixel at this depth.
    let pixel = max(length(unproject(px + vec2<i32>(1, 0), depth_at(px)) - c.xyz), 1e-4);
    let reach = clamp(radius / pixel, 2.0, 48.0);
    let cell = f32((px.x & 3) * 4 + (px.y & 3));
    let turn = cell * 0.39269908;
    var occluded = 0.0;
    for (var i = 0; i < 12; i++) {
        let angle = f32(i) * 2.3999632 + turn;
        let r = reach * sqrt((f32(i) + 0.5) / 12.0);
        let s = position(px + vec2<i32>(round(vec2<f32>(cos(angle), sin(angle)) * r)));
        if (s.w == 0.0) {
            continue;
        }
        let v = s.xyz - c.xyz;
        let d = length(v);
        if (d < 1e-3) {
            continue;
        }
        occluded += max(dot(n, v) / d - 0.1, 0.0) * clamp(1.0 - d / radius, 0.0, 1.0);
    }
    // A face half hidden by a wall at its foot loses about a third of its
    // light at full strength.
    let a = clamp(1.0 - ao.params.y * occluded * 0.25, 0.0, 1.0);
    return vec4<f32>(a, a, a, 1.0);
}

// The 4×4 average of the occlusion (removes the rotation pattern); blended
// as a factor onto the frame.
@fragment
fn fs_apply(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let px = vec2<i32>(pos.xy);
    let dims = vec2<i32>(textureDimensions(occlusion));
    var sum = 0.0;
    for (var y = -2; y < 2; y++) {
        for (var x = -2; x < 2; x++) {
            let q = clamp(px + vec2<i32>(x, y), vec2<i32>(0), dims - vec2<i32>(1));
            sum += textureLoad(occlusion, q, 0).r;
        }
    }
    let a = sum / 16.0;
    return vec4<f32>(a, a, a, 1.0);
}
