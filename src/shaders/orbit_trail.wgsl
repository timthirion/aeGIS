// Orbital trail — a polyline of 3D positions sampled along one
// orbital period of the selected satellite. Drawn as a LineStrip.
// Plan 0004 M3.
//
// Same camera-occlusion logic as orbit.wgsl: vertices whose
// camera-to-position ray passes through Earth get a `visibility < 0.5`
// flag the fragment shader treats as a discard. That gives the
// classic "front half of the orbit visible, back half hidden" look.

struct Camera {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    _pad0: f32,
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) visibility: f32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = u.view_proj * vec4<f32>(in.position, 1.0);

    let c = u.camera_pos;
    let p = in.position;
    let seg = p - c;
    let seg_len = length(seg);
    let d = seg / seg_len;
    let t_star = -dot(c, d);
    var min_dist2: f32;
    if (t_star < 0.0) {
        min_dist2 = dot(c, c);
    } else if (t_star > seg_len) {
        min_dist2 = dot(p, p);
    } else {
        min_dist2 = dot(c, c) - t_star * t_star;
    }
    out.visibility = select(1.0, 0.0, min_dist2 < 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (in.visibility < 0.5) {
        discard;
    }
    return u.color;
}
