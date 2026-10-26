// Faces and edges of the Planyrith viewport (docs/06 §3–§5).
//
// One uniform block per draw (dynamic offset): the camera-relative
// clip-from-local matrix, the normal matrix, colors, lighting and the ids the
// picking pass writes. Coordinates are container-local f32 (docs/06 §2).

struct Draw {
    clip_from_local: mat4x4<f32>,
    // Columns of the local-to-world normal matrix (inverse transpose).
    normal_x: vec4<f32>,
    normal_y: vec4<f32>,
    normal_z: vec4<f32>,
    // Linear base color (material, style default or edge color); a = opacity.
    color: vec4<f32>,
    // rgb: selection tint, a: tint amount.
    tint: vec4<f32>,
    // xyz: world direction toward the light, w: ambient share.
    light: vec4<f32>,
    // rgb: background color, a: fade toward it (outside the edit context).
    fade: vec4<f32>,
    // x, y: viewport size in pixels; z: edge width px; w: profile width px.
    viewport: vec4<f32>,
    // The eye in local coordinates (w = 1) or the direction toward it (w = 0).
    local_eye: vec4<f32>,
    // x: item id + 1; y: flags (1 textured, 2 unlit, 4 hide soft edges,
    // 8 dashed, 16 color by axis, 32 color by material); z, w: unused.
    ids: vec4<u32>,
    // Fog: start, end, near, far (mm along the view).
    fog: vec4<f32>,
    // Fog color; a: 0 off, 1 perspective depth (clip w), 2 parallel depth.
    fog_color: vec4<f32>,
    // Section plane in local coordinates (n, d): fragments with
    // n·p + d > 0 are cut away when ids.y has CLIPPED (docs/06 §3.1 step 2).
    section: vec4<f32>,
    // Light clip space from local coordinates (shadow map, docs/06 §6).
    light_clip: mat4x4<f32>,
    // x: shadows on; y: lit intensity; z: shadowed intensity; w: depth bias.
    shadow: vec4<f32>,
};

// Distance along the view of a clip position (fog).
fn view_depth(clip: vec4<f32>) -> f32 {
    if (draw.fog_color.a > 1.5) {
        // Parallel views map depth linearly: z = (far - d) / (far - near).
        return draw.fog.w - clip.z / clip.w * (draw.fog.w - draw.fog.z);
    }
    return clip.w;
}

fn fogged(rgb: vec3<f32>, depth: f32) -> vec3<f32> {
    if (draw.fog_color.a < 0.5) {
        return rgb;
    }
    let f = clamp((depth - draw.fog.x) / max(draw.fog.y - draw.fog.x, 1e-3), 0.0, 1.0);
    return mix(rgb, draw.fog_color.rgb, f);
}

@group(0) @binding(0) var<uniform> draw: Draw;
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var tex_sampler: sampler;
@group(2) @binding(0) var shadow_map: texture_depth_2d;
@group(2) @binding(1) var shadow_sampler: sampler_comparison;

const TEXTURED: u32 = 1u;
const UNLIT: u32 = 2u;
const HIDE_SOFT: u32 = 4u;
const DASHED: u32 = 8u;
const BY_AXIS: u32 = 16u;
const BY_MATERIAL: u32 = 32u;
const CLIPPED: u32 = 64u;
const SHADOW_CATCHER: u32 = 128u;

// The section plane cuts this local point away.
fn cut_away(local: vec3<f32>) -> bool {
    return (draw.ids.y & CLIPPED) != 0u && dot(draw.section.xyz, local) + draw.section.w > 1e-3;
}

// 1 lit, 0 in shadow: 3×3 percentage-closer filtering of the sun's depth.
fn sun_visibility(light: vec4<f32>) -> f32 {
    if (draw.shadow.x < 0.5) {
        return 1.0;
    }
    let ndc = light.xyz / light.w;
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || ndc.z > 1.0) {
        return 1.0;
    }
    let texel = 1.0 / vec2<f32>(textureDimensions(shadow_map));
    var sum = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            sum += textureSampleCompareLevel(
                shadow_map,
                shadow_sampler,
                uv + vec2<f32>(f32(x), f32(y)) * texel,
                ndc.z - draw.shadow.w,
            );
        }
    }
    return sum / 9.0;
}

// Edge flag bits (io::render_mesh::edge_flag_bits) plus face presence.
const EDGE_SOFT: u32 = 2u;
const EDGE_SMOOTH: u32 = 4u;
const EDGE_HAS_FACE0: u32 = 256u;
const EDGE_HAS_FACE1: u32 = 512u;

// ----- faces -----------------------------------------------------------------------------

struct FaceIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) face: u32,
};

struct FaceOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) @interpolate(flat) face: u32,
    @location(3) depth: f32,
    @location(4) local: vec3<f32>,
    @location(5) light: vec4<f32>,
};

@vertex
fn vs_face(input: FaceIn) -> FaceOut {
    var out: FaceOut;
    out.clip = draw.clip_from_local * vec4<f32>(input.position, 1.0);
    out.local = input.position;
    out.light = draw.light_clip * vec4<f32>(input.position, 1.0);
    let normal_matrix = mat3x3<f32>(draw.normal_x.xyz, draw.normal_y.xyz, draw.normal_z.xyz);
    out.normal = normal_matrix * input.normal;
    out.uv = input.uv;
    out.face = input.face;
    out.depth = view_depth(out.clip);
    return out;
}

fn shade(base: vec4<f32>, normal: vec3<f32>, visibility: f32) -> vec4<f32> {
    var lit = 1.0;
    if ((draw.ids.y & UNLIT) == 0u) {
        let n = normalize(normal);
        let diffuse = max(dot(n, draw.light.xyz), 0.0);
        if (draw.shadow.x > 0.5) {
            // The sun: shadowed intensity everywhere, the lit one in light.
            lit = draw.shadow.z + (draw.shadow.y - draw.shadow.z) * diffuse * visibility;
        } else {
            lit = draw.light.w + (1.0 - draw.light.w) * diffuse;
        }
    }
    var rgb = base.rgb * lit;
    rgb = mix(rgb, draw.tint.rgb, draw.tint.a);
    rgb = mix(rgb, draw.fade.rgb, draw.fade.a);
    return vec4<f32>(rgb, base.a);
}

@fragment
fn fs_face(input: FaceOut) -> @location(0) vec4<f32> {
    if (cut_away(input.local)) {
        discard;
    }
    let visibility = sun_visibility(input.light);
    if ((draw.ids.y & SHADOW_CATCHER) != 0u) {
        // The ground: only its shadow shows.
        let amount = (1.0 - visibility) * (1.0 - draw.shadow.z / max(draw.shadow.y, 1e-3));
        return vec4<f32>(0.0, 0.0, 0.0, clamp(amount, 0.0, 1.0) * 0.8);
    }
    var base = draw.color;
    if ((draw.ids.y & TEXTURED) != 0u) {
        // Texture rows run top to bottom; face-frame v runs upward.
        base = base * textureSample(tex, tex_sampler, vec2<f32>(input.uv.x, 1.0 - input.uv.y));
    }
    let lit = shade(base, input.normal, visibility);
    return vec4<f32>(fogged(lit.rgb, input.depth), lit.a);
}

@fragment
fn fs_face_id(input: FaceOut) -> @location(0) vec2<u32> {
    if (cut_away(input.local)) {
        discard;
    }
    return vec2<u32>(draw.ids.x, input.face + 1u);
}

// The sun's depth (draws whose clip_from_local is the light's).
@fragment
fn fs_shadow(input: FaceOut) {
    if (cut_away(input.local)) {
        discard;
    }
}

// ----- edges -----------------------------------------------------------------------------

struct EdgeIn {
    @builtin(vertex_index) corner: u32,
    @location(0) a: vec3<f32>,
    @location(1) b: vec3<f32>,
    @location(2) n0: vec3<f32>,
    @location(3) n1: vec3<f32>,
    @location(4) flags: u32,
    @location(5) index: u32,
    @location(6) material_color: vec4<f32>,
};

struct EdgeOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Distance along the edge in pixels (dashes).
    @location(1) along: f32,
    @location(2) @interpolate(flat) index: u32,
    @location(3) depth: f32,
    @location(4) local: vec3<f32>,
};

// Keeps a clip-space point in front of the eye (perspective views).
fn in_front(p: vec4<f32>, q: vec4<f32>) -> vec4<f32> {
    let eps = 1e-6;
    if (p.w >= eps) {
        return p;
    }
    let t = (eps - p.w) / (q.w - p.w);
    return mix(p, q, t);
}

fn axis_color(direction: vec3<f32>, fallback: vec4<f32>) -> vec4<f32> {
    let d = abs(normalize(direction));
    if (d.x > 0.9999) {
        return vec4<f32>(0.8, 0.05, 0.05, 1.0);
    }
    if (d.y > 0.9999) {
        return vec4<f32>(0.05, 0.6, 0.05, 1.0);
    }
    if (d.z > 0.9999) {
        return vec4<f32>(0.05, 0.1, 0.8, 1.0);
    }
    return fallback;
}

@vertex
fn vs_edge(input: EdgeIn) -> EdgeOut {
    var out: EdgeOut;
    out.index = input.index;

    // Profile edges (docs/06 §5): the two faces turn opposite ways toward
    // the eye, or there is only one face. The sign of n·v is the same in
    // local and world space for any affine transform.
    let mid = (input.a + input.b) * 0.5;
    var view = draw.local_eye.xyz;
    if (draw.local_eye.w > 0.5) {
        view = draw.local_eye.xyz - mid;
    }
    let has0 = (input.flags & EDGE_HAS_FACE0) != 0u;
    let has1 = (input.flags & EDGE_HAS_FACE1) != 0u;
    var profile = !(has0 && has1);
    if (has0 && has1) {
        profile = (dot(input.n0, view) > 0.0) != (dot(input.n1, view) > 0.0);
    }
    let soft = (input.flags & (EDGE_SOFT | EDGE_SMOOTH)) != 0u;
    var width = draw.viewport.z;
    if (profile && draw.viewport.w > 0.0) {
        width = max(width, draw.viewport.w);
    }
    if (soft && !profile && (draw.ids.y & HIDE_SOFT) != 0u) {
        width = 0.0;
    }

    var p = draw.clip_from_local * vec4<f32>(input.a, 1.0);
    var q = draw.clip_from_local * vec4<f32>(input.b, 1.0);
    let p2 = in_front(p, q);
    let q2 = in_front(q, p);
    p = p2;
    q = q2;
    let half_size = draw.viewport.xy * 0.5;
    let sp = p.xy / p.w * half_size;
    let sq = q.xy / q.w * half_size;
    var dir = sq - sp;
    let length_px = length(dir);
    if (length_px < 1e-6) {
        dir = vec2<f32>(1.0, 0.0);
    } else {
        dir = dir / length_px;
    }
    let perp = vec2<f32>(-dir.y, dir.x);

    // Corners: 0 (a, -), 1 (a, +), 2 (b, +), 3 (a, -), 4 (b, +), 5 (b, -).
    let at_b = input.corner == 2u || input.corner == 4u || input.corner == 5u;
    var side = -1.0;
    if (input.corner == 1u || input.corner == 2u || input.corner == 4u) {
        side = 1.0;
    }
    var base = p;
    var along = 0.0;
    var extend = -0.5;
    out.local = input.a;
    if (at_b) {
        base = q;
        along = length_px;
        extend = 0.5;
        out.local = input.b;
    }
    // Half a pixel past each end so joints close.
    let offset_px = perp * side * width * 0.5 + dir * extend * min(width, 1.0);
    let offset_ndc = offset_px / half_size;
    var clip = base + vec4<f32>(offset_ndc * base.w, 0.0, 0.0);
    // Edges sit on their faces (docs/06 §3.1 step 5): a small pull toward
    // the eye in reversed Z.
    out.depth = view_depth(clip);
    clip.z = clip.z * 1.0004 + 1e-7 * clip.w;
    out.clip = clip;
    out.along = along;

    var color = draw.color;
    if ((draw.ids.y & BY_MATERIAL) != 0u) {
        color = input.material_color;
    }
    if ((draw.ids.y & BY_AXIS) != 0u) {
        let world = mat3x3<f32>(draw.normal_x.xyz, draw.normal_y.xyz, draw.normal_z.xyz);
        // The normal matrix's inverse transpose keeps axis-aligned edges
        // axis-aligned for the transforms groups use (rotations, scales).
        color = axis_color(world * (input.b - input.a), color);
    }
    let rgb = mix(mix(color.rgb, draw.tint.rgb, draw.tint.a), draw.fade.rgb, draw.fade.a);
    out.color = vec4<f32>(rgb, color.a);
    return out;
}

@fragment
fn fs_edge(input: EdgeOut) -> @location(0) vec4<f32> {
    if (cut_away(input.local)) {
        discard;
    }
    if ((draw.ids.y & DASHED) != 0u && fract(input.along / 8.0) > 0.5) {
        discard;
    }
    return vec4<f32>(fogged(input.color.rgb, input.depth), input.color.a);
}

@fragment
fn fs_edge_id(input: EdgeOut) -> @location(0) vec2<u32> {
    if (cut_away(input.local)) {
        discard;
    }
    return vec2<u32>(draw.ids.x, 0x80000000u | (input.index + 1u));
}

// ----- sky (docs/06 §3.1, style background) ----------------------------------------------

struct SkyOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

// One triangle covering the viewport.
@vertex
fn vs_sky(@builtin(vertex_index) i: u32) -> SkyOut {
    let x = f32((i << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(i & 2u) * 2.0 - 1.0;
    var out: SkyOut;
    out.clip = vec4<f32>(x, y, 0.0, 1.0);
    out.ndc = vec2<f32>(x, y);
    return out;
}

// The view ray through the pixel is light.xyz (forward) + ndc.x · normal_x
// (right · tan(fov/2) · aspect) + ndc.y · normal_y (up · tan(fov/2)). Above
// the horizon the background color (fade) turns into the sky color (color)
// with height; below it the ground color (tint, a = opacity) covers it.
@fragment
fn fs_sky(input: SkyOut) -> @location(0) vec4<f32> {
    let dir = normalize(draw.light.xyz + input.ndc.x * draw.normal_x.xyz + input.ndc.y * draw.normal_y.xyz);
    let horizon = draw.fade.rgb;
    if (dir.z >= 0.0) {
        let h = 1.0 - clamp(dir.z / 0.45, 0.0, 1.0);
        return vec4<f32>(mix(horizon, draw.color.rgb, 1.0 - h * h), 1.0);
    }
    return vec4<f32>(mix(horizon, draw.tint.rgb, draw.tint.a), 1.0);
}

// ----- watermarks (docs/06 §3.1, style watermarks) -----------------------------------------

struct MarkOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// A screen rectangle: viewport = NDC (x0, y0, x1, y1), tint = UV (u0, v0,
// u1, v1) with v0 at the top edge.
@vertex
fn vs_mark(@builtin(vertex_index) i: u32) -> MarkOut {
    // Corners: 0 (0,0) 1 (1,0) 2 (1,1) 3 (0,0) 4 (1,1) 5 (0,1); (0,0) is the
    // bottom left.
    var corner = vec2<f32>(0.0, 0.0);
    if (i == 1u || i == 2u || i == 4u) {
        corner.x = 1.0;
    }
    if (i == 2u || i == 4u || i == 5u) {
        corner.y = 1.0;
    }
    var out: MarkOut;
    let xy = mix(draw.viewport.xy, draw.viewport.zw, corner);
    out.clip = vec4<f32>(xy, 0.0, 1.0);
    out.uv = vec2<f32>(mix(draw.tint.x, draw.tint.z, corner.x), mix(draw.tint.w, draw.tint.y, corner.y));
    return out;
}

@fragment
fn fs_mark(input: MarkOut) -> @location(0) vec4<f32> {
    let texel = textureSample(tex, tex_sampler, input.uv);
    return vec4<f32>(texel.rgb, texel.a * draw.color.a);
}
