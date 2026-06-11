// Vector overlay on a 3D sphere. Each vertex projects from
// (lon, lat) → unit-sphere XYZ → clip space via a per-frame view +
// perspective projection matrix.
//
// **One scene, one projection.** The "flat slippy map" view at high
// zoom emerges naturally from the perspective camera sitting close
// to the sphere surface (`altitude ≈ 0.02` at zoom 10); the globe
// view emerges from the camera being far enough out to see the whole
// sphere (`altitude ≈ 2.0` capped at low zoom). The transition is
// just camera position interpolating with zoom — no per-vertex blend.
//
// Backface culling: each vertex's sphere position has its dot product
// with the camera position passed to the fragment shader; a fragment
// is on the far hemisphere iff that dot is less than 1 (because for a
// unit sphere, the visible-hemisphere boundary is exactly where the
// sphere is tangent to the line-of-sight from the camera, i.e. where
// `vertex · camera_pos = vertex · vertex = 1`).

struct Camera {
    view_proj: mat4x4<f32>,
    // 3D position of the camera in sphere-coords (length > 1).
    position: vec3<f32>,
    _pad0: f32,
    // Line colour for this overlay.
    color: vec4<f32>,
    // Day/night state (plan 0009 M0).
    sun_dir: vec3<f32>,
    night_dim: f32,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct VsIn {
    @location(0) world: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) visibility: f32,
    @location(1) sphere: vec3<f32>,
};

const PI: f32 = 3.14159265358979323846;

/// Inverse Spherical Mercator: normalised (x, y) → (lon_rad, lat_rad).
fn world_to_lonlat_rad(world: vec2<f32>) -> vec2<f32> {
    let lon_rad = world.x * 2.0 * PI - PI;
    let n = PI * (1.0 - 2.0 * world.y);
    let lat_rad = atan(sinh(n));
    return vec2<f32>(lon_rad, lat_rad);
}

/// `(lon, lat)` on a unit sphere → XYZ with prime meridian at +Z.
fn lonlat_to_sphere(lonlat: vec2<f32>) -> vec3<f32> {
    let lon = lonlat.x;
    let lat = lonlat.y;
    return vec3<f32>(cos(lat) * sin(lon), sin(lat), cos(lat) * cos(lon));
}

/// Day/night dim + dawn/dusk warm tint multiplier (plan 0009 M0+M1).
/// See `day_night_color` in tile.wgsl for the same formula.
fn day_night_color(sphere: vec3<f32>, sun_dir: vec3<f32>, night_dim: f32) -> vec3<f32> {
    let cos_sun = dot(sphere, sun_dir);
    let day = smoothstep(0.0, 0.15, cos_sun);
    let dim = mix(night_dim, 1.0, day);
    let warm_rise = smoothstep(-0.05, 0.05, cos_sun);
    let warm_fall = 1.0 - smoothstep(0.05, 0.3, cos_sun);
    let warm = warm_rise * warm_fall * 0.6;
    let tint = mix(vec3<f32>(1.0, 1.0, 1.0), vec3<f32>(1.3, 0.8, 0.5), warm);
    return vec3<f32>(dim, dim, dim) * tint;
}

@vertex
fn vs_main(in: VsIn) -> VsOut {
    let lonlat = world_to_lonlat_rad(in.world);
    let sphere = lonlat_to_sphere(lonlat);
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(sphere, 1.0);
    // > 0 = front of sphere, < 0 = back. Linear interpolation across
    // segments handles the horizon-crossing case per-fragment.
    out.visibility = dot(sphere, camera.position) - 1.0;
    out.sphere = sphere;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (in.visibility < 0.0) {
        discard;
    }
    // Country outlines fade with the surface they sit on,
    // otherwise they float distractingly across the night side.
    let mult = day_night_color(normalize(in.sphere), camera.sun_dir, camera.night_dim);
    return vec4<f32>(camera.color.rgb * mult, camera.color.a);
}
