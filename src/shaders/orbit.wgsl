// Instanced satellite-point renderer. Plan 0004 M1.
//
// One draw call per category (Stations / Starlink / GNSS / …);
// the instance buffer carries one (position, NORAD id) per
// satellite, propagated CPU-side every frame.
//
// Per-instance, the vertex shader (a) projects the satellite's
// world position to clip space, (b) billboards two triangles
// around that point at a fixed device-pixel size, and (c)
// occludes the point when Earth is between camera and satellite
// (the ray from camera to satellite intersects the unit sphere).

struct Camera {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    _pad0: f32,
    viewport_px: vec2<f32>,
    _pad1: vec2<f32>,
};

struct Instance {
    // World position in renderer body-fixed coords (1.0 == one
    // Earth radius). Comes straight from
    // `orbit::propagate_render_space`.
    @location(0) world_pos: vec3<f32>,
    // Per-instance sRGB colour (already converted to linear
    // CPU-side via the existing srgb8_to_linear helper).
    @location(1) color: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) discard_flag: f32,
};

@group(0) @binding(0) var<uniform> u: Camera;

// Point footprint in device pixels. 5 px reads as a clear dot
// without dominating the globe.
const POINT_SIZE_PX: f32 = 5.0;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    // Occlusion: does the camera → satellite segment intersect
    // the unit-radius Earth?
    let c = u.camera_pos;
    let p = inst.world_pos;
    let seg = p - c;
    let seg_len = length(seg);
    let d = seg / seg_len;
    let t_star = -dot(c, d);
    var min_dist2: f32;
    if (t_star < 0.0) {
        // Closest approach is behind the camera — satellite ahead.
        min_dist2 = dot(c, c);
    } else if (t_star > seg_len) {
        // Closest approach is past the satellite.
        min_dist2 = dot(p, p);
    } else {
        // Closest approach is inside the segment.
        min_dist2 = dot(c, c) - t_star * t_star;
    }
    let occluded = min_dist2 < 1.0;

    // Project the world point through the camera matrix.
    let center_clip = u.view_proj * vec4<f32>(p, 1.0);

    // Two triangles spanning a unit square — winding picks
    // canonical CCW order.
    var offsets = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>( 1.0,  1.0),
        vec2<f32>(-1.0,  1.0),
    );
    let corner = offsets[vi];
    // Pixel → clip-space conversion. `viewport_px` is in device
    // pixels; the factor of 2 normalises NDC (-1..+1 range). Scale
    // by `clip.w` so the point stays the same device-pixel size
    // regardless of perspective depth — standard billboard trick.
    let px_to_clip = vec2<f32>(2.0 / u.viewport_px.x, 2.0 / u.viewport_px.y);
    let offset_clip = corner * POINT_SIZE_PX * px_to_clip * center_clip.w;

    var out: VsOut;
    out.clip = vec4<f32>(
        center_clip.xy + offset_clip,
        center_clip.zw,
    );
    out.color = inst.color;
    out.discard_flag = select(0.0, 1.0, occluded);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (in.discard_flag > 0.5) {
        discard;
    }
    return vec4<f32>(in.color, 1.0);
}
