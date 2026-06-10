// Orbital trails — per-satellite polylines. Plan 0004 M3 / refined.
//
// LineList topology: pairs of consecutive vertices form one line
// segment. The CPU side builds one big buffer of (position, color)
// pairs covering every "eligible" satellite (small categories);
// the selected satellite gets a brighter colour. One draw call per
// frame.
//
// Same camera-occlusion logic as orbit.wgsl: vertices whose
// camera-to-position ray passes through Earth fail the visibility
// test and the fragment is discarded. Front-of-globe halves of
// orbits render; back-of-globe halves don't.

struct Camera {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    _pad0: f32,
};

@group(0) @binding(0) var<uniform> u: Camera;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) visibility: f32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = u.view_proj * vec4<f32>(in.position, 1.0);
    out.color = in.color;

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
    return in.color;
}
