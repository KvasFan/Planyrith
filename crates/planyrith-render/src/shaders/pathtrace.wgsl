// Implements docs/06 §11.3: the path tracer of §11.1 on the graphics card.
// One invocation traces one pixel; each dispatch adds `count` paths per pixel
// to `accum`. Coordinates are relative to the camera origin (f32).

struct Params {
    o0: vec4<f32>,
    ox: vec4<f32>,
    oy: vec4<f32>,
    d0: vec4<f32>,
    dx: vec4<f32>,
    dy: vec4<f32>,
    // xyz direction, w radiance.
    sun: vec4<f32>,
    sky: vec4<f32>,
    horizon: vec4<f32>,
    below: vec4<f32>,
    // xyz color, w 1 when the background is shown.
    background: vec4<f32>,
    // x height, y 1 when there is a ground; z roughness.
    ground: vec4<f32>,
    ground_albedo: vec4<f32>,
    width: u32,
    height: u32,
    first: u32,
    count: u32,
    bounces: u32,
    nodes: u32,
    pad0: u32,
    pad1: u32,
};

struct Tri {
    p0: vec4<f32>,
    p1: vec4<f32>,
    p2: vec4<f32>,
    n0: vec4<f32>,
    n1: vec4<f32>,
    n2: vec4<f32>,
    // u0 v0 u1 v1
    uv01: vec4<f32>,
    // u2 v2, surface index (bits), unused
    uv2: vec4<f32>,
};

struct Node {
    lo: vec3<f32>,
    first: u32,
    hi: vec3<f32>,
    count: u32,
    right: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
};

struct Surf {
    albedo: vec3<f32>,
    roughness: f32,
    metallic: f32,
    opacity: f32,
    // Texels offset, or 0xffffffff for none; width; height.
    tex: u32,
    tex_w: u32,
    tex_h: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
};

@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> tris: array<Tri>;
@group(0) @binding(2) var<storage, read> nodes: array<Node>;
@group(0) @binding(3) var<storage, read> surfs: array<Surf>;
@group(0) @binding(4) var<storage, read> texels: array<u32>;
@group(0) @binding(5) var<storage, read_write> accum: array<vec4<f32>>;

const PI: f32 = 3.14159265358979;
const FIREFLY: f32 = 6.0;
const NONE: u32 = 0xffffffffu;
const FAR: f32 = 3.0e38;

var<private> rng_state: u32;

fn pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn rand() -> f32 {
    rng_state = pcg(rng_state);
    return f32(rng_state >> 8u) / 16777216.0;
}

struct Hit {
    t: f32,
    tri: u32,
    u: f32,
    v: f32,
};

fn hit_box(lo: vec3<f32>, hi: vec3<f32>, o: vec3<f32>, inv: vec3<f32>, limit: f32) -> bool {
    let t1 = (lo - o) * inv;
    let t2 = (hi - o) * inv;
    let tmin = min(t1, t2);
    let tmax = max(t1, t2);
    let near = max(max(tmin.x, tmin.y), tmin.z);
    let far = min(min(tmax.x, tmax.y), tmax.z);
    return far >= max(near, 0.0) && near < limit;
}

fn intersect(o: vec3<f32>, d: vec3<f32>) -> Hit {
    var best = Hit(FAR, NONE - 1u, 0.0, 0.0);
    var limit = FAR;
    if P.ground.y > 0.5 && abs(d.z) > 1e-12 {
        let t = (P.ground.x - o.z) / d.z;
        if t > 1e-4 && t < limit {
            limit = t;
            best = Hit(t, NONE, 0.0, 0.0);
        }
    }
    if P.nodes == 0u {
        return best;
    }
    let inv = 1.0 / d;
    var stack: array<u32, 64>;
    var depth = 1u;
    stack[0] = 0u;
    loop {
        if depth == 0u {
            break;
        }
        depth -= 1u;
        let node = nodes[stack[depth]];
        if !hit_box(node.lo, node.hi, o, inv, limit) {
            continue;
        }
        if node.count > 0u {
            for (var i = node.first; i < node.first + node.count; i += 1u) {
                let t = tris[i];
                let e1 = t.p1.xyz - t.p0.xyz;
                let e2 = t.p2.xyz - t.p0.xyz;
                let pv = cross(d, e2);
                let det = dot(e1, pv);
                if det < 1e-12 {
                    continue;
                }
                let idet = 1.0 / det;
                let tv = o - t.p0.xyz;
                let u = dot(tv, pv) * idet;
                if u < 0.0 || u > 1.0 {
                    continue;
                }
                let qv = cross(tv, e1);
                let v = dot(d, qv) * idet;
                if v < 0.0 || u + v > 1.0 {
                    continue;
                }
                let dist = dot(e2, qv) * idet;
                if dist > 1e-4 && dist < limit {
                    limit = dist;
                    best = Hit(dist, i, u, v);
                }
            }
        } else if depth + 2u <= 64u {
            stack[depth] = node.first;
            stack[depth + 1u] = node.right;
            depth += 2u;
        }
    }
    return best;
}

fn frame_t(n: vec3<f32>) -> vec3<f32> {
    var a = vec3<f32>(1.0, 0.0, 0.0);
    if abs(n.x) > 0.9 {
        a = vec3<f32>(0.0, 1.0, 0.0);
    }
    return normalize(cross(n, a));
}

fn environment(d: vec3<f32>) -> vec3<f32> {
    if d.z >= 0.0 {
        return mix(P.horizon.xyz, P.sky.xyz, pow(d.z, 0.5));
    }
    return P.below.xyz;
}

fn ggx(ndh: f32, a2: f32) -> f32 {
    let d = ndh * ndh * (a2 - 1.0) + 1.0;
    return a2 / (PI * d * d);
}

fn smith(ndv: f32, ndl: f32, r: f32) -> f32 {
    let k = (r + 1.0) * (r + 1.0) / 8.0;
    return (ndv / (ndv * (1.0 - k) + k)) * (ndl / (ndl * (1.0 - k) + k));
}

fn brdf(albedo: vec3<f32>, s: Surf, n: vec3<f32>, v: vec3<f32>, l: vec3<f32>) -> vec3<f32> {
    let ndl = dot(n, l);
    let ndv = max(dot(n, v), 1e-4);
    if ndl <= 0.0 {
        return vec3<f32>(0.0);
    }
    let r = clamp(s.roughness, 0.04, 1.0);
    let h = normalize(v + l);
    let f0 = mix(vec3<f32>(0.04), albedo, s.metallic);
    let vdh = max(dot(v, h), 0.0);
    let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - vdh, 5.0);
    let a2 = pow(r, 4.0);
    let specular = fresnel * ggx(max(dot(n, h), 0.0), a2) * smith(ndv, ndl, r) / (4.0 * ndv * ndl);
    let diffuse = (vec3<f32>(1.0) - fresnel) * (1.0 - s.metallic) * albedo / PI;
    return (diffuse + specular) * ndl;
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn sample_texture(s: Surf, uv: vec2<f32>) -> vec3<f32> {
    let u = uv.x - floor(uv.x);
    let v = 1.0 - (uv.y - floor(uv.y));
    let x = min(u32(u * f32(s.tex_w)), s.tex_w - 1u);
    let y = min(u32(v * f32(s.tex_h)), s.tex_h - 1u);
    let c = unpack4x8unorm(texels[s.tex + y * s.tex_w + x]);
    return vec3<f32>(srgb_to_linear(c.x), srgb_to_linear(c.y), srgb_to_linear(c.z));
}

fn trace(origin: vec3<f32>, dir: vec3<f32>) -> vec3<f32> {
    var radiance = vec3<f32>(0.0);
    var throughput = vec3<f32>(1.0);
    var o = origin;
    var d = dir;
    for (var bounce = 0u; bounce <= P.bounces + 1u; bounce += 1u) {
        let hit = intersect(o, d);
        if hit.tri == NONE - 1u {
            if bounce == 0u && P.background.w > 0.5 {
                radiance += throughput * P.background.xyz;
            } else {
                radiance += throughput * environment(d);
            }
            break;
        }
        let p = o + d * hit.t;
        var s: Surf;
        var n: vec3<f32>;
        var albedo: vec3<f32>;
        if hit.tri == NONE {
            s = Surf(P.ground_albedo.xyz, P.ground.z, 0.0, 1.0, NONE, 0u, 0u, 0u, 0u, 0u);
            n = vec3<f32>(0.0, 0.0, 1.0);
            albedo = s.albedo;
        } else {
            let t = tris[hit.tri];
            let w = 1.0 - hit.u - hit.v;
            s = surfs[bitcast<u32>(t.uv2.z)];
            let sn = t.n0.xyz * w + t.n1.xyz * hit.u + t.n2.xyz * hit.v;
            if dot(sn, sn) > 0.0 {
                n = normalize(sn);
            } else {
                n = normalize(cross(t.p1.xyz - t.p0.xyz, t.p2.xyz - t.p0.xyz));
            }
            albedo = s.albedo;
            if s.tex != NONE {
                let uv = t.uv01.xy * w + t.uv01.zw * hit.u + t.uv2.xy * hit.v;
                albedo = s.albedo * sample_texture(s, uv);
            }
        }
        if dot(n, d) > 0.0 {
            n = -n;
        }
        if s.opacity < 1.0 && rand() > s.opacity {
            o = p + d * 1e-2;
            continue;
        }
        let v = -d;
        let lift = p + n * 1e-2;
        if P.sun.w > 0.0 {
            let sun = P.sun.xyz;
            let st = frame_t(sun);
            let sb = cross(sun, st);
            let r = 0.008 * sqrt(rand());
            let a = 2.0 * PI * rand();
            let l = normalize(sun + st * (r * cos(a)) + sb * (r * sin(a)));
            if dot(n, l) > 0.0 && intersect(lift, l).tri == NONE - 1u {
                radiance += throughput * brdf(albedo, s, n, v, l) * P.sun.w;
            }
        }
        if bounce > P.bounces {
            break;
        }
        let rough = clamp(s.roughness, 0.04, 1.0);
        let share = clamp(0.5 * (1.0 - rough) + 0.5 * s.metallic, 0.1, 0.9);
        let t = frame_t(n);
        let b = cross(n, t);
        var next: vec3<f32>;
        if rand() < share {
            let a2 = pow(rough, 4.0);
            let u1 = rand();
            let u2 = rand();
            let cos_t = sqrt((1.0 - u1) / (1.0 + (a2 - 1.0) * u1));
            let sin_t = sqrt(max(1.0 - cos_t * cos_t, 0.0));
            let phi = 2.0 * PI * u2;
            let h = normalize(t * (sin_t * cos(phi)) + b * (sin_t * sin(phi)) + n * cos_t);
            let l = normalize(h * (2.0 * dot(v, h)) - v);
            let ndl = dot(n, l);
            if ndl <= 0.0 {
                break;
            }
            let f0 = mix(vec3<f32>(0.04), albedo, s.metallic);
            let vdh = max(dot(v, h), 1e-4);
            let fresnel = f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - vdh, 5.0);
            let ndv = max(dot(n, v), 1e-4);
            throughput *= fresnel * smith(ndv, ndl, rough) * vdh / (max(dot(n, h), 1e-4) * ndv) / share;
            next = l;
        } else {
            let u1 = rand();
            let u2 = rand();
            let r = sqrt(u1);
            let phi = 2.0 * PI * u2;
            next = normalize(t * (r * cos(phi)) + b * (r * sin(phi)) + n * sqrt(1.0 - u1));
            let f0 = mix(vec3<f32>(0.04), albedo, s.metallic);
            throughput *= albedo * (1.0 - s.metallic) * (vec3<f32>(1.0) - f0) / (1.0 - share);
        }
        if bounce >= 2u {
            let keep = clamp(max(max(throughput.x, throughput.y), throughput.z), 0.05, 0.95);
            if rand() > keep {
                break;
            }
            throughput /= keep;
        }
        o = lift;
        d = next;
    }
    return radiance;
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.width || id.y >= P.height {
        return;
    }
    let pixel = id.y * P.width + id.x;
    var sum = vec3<f32>(0.0);
    for (var k = P.first; k < P.first + P.count; k += 1u) {
        rng_state = pcg(pixel ^ pcg(k * 0x9e3779b9u + 0x632be5abu));
        let x = (f32(id.x) + rand()) / f32(P.width) * 2.0 - 1.0;
        let y = 1.0 - (f32(id.y) + rand()) / f32(P.height) * 2.0;
        let o = P.o0.xyz + P.ox.xyz * x + P.oy.xyz * y;
        let d = normalize(P.d0.xyz + P.dx.xyz * x + P.dy.xyz * y);
        let c = trace(o, d);
        sum += c * (FIREFLY / max(max(max(c.x, c.y), c.z), FIREFLY));
    }
    accum[pixel] += vec4<f32>(sum, 0.0);
}
