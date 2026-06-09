// Polar caps that fill the spherical gaps Web Mercator can't tile.
//
// The slippy basemap stops at |lat| = MERCATOR_LAT_MAX (≈85.051°)
// because the projection's Y stretches toward infinity at the pole.
// Without the caps, the top and bottom of the sphere are bare
// background pixels — visible whenever the camera looks over a pole.
//
// Each cap is a triangle fan around its pole: a pole vertex plus
// `RING_VERTS` ring vertices at the cap-edge latitude. Two draws per
// frame (north + south); the uniform's `pole_sign` (+1 north, −1
// south) and `color` differentiate them. The geometry is generated
// from `vertex_index` so the only buffer this pipeline binds is the
// uniform.

struct CapUniforms {
    view_proj: mat4x4<f32>,
    // 3D camera position in sphere-coords (length > 1). Used for the
    // back-hemisphere discard in the fragment shader, same convention
    // as `tile.wgsl` / `vector.wgsl`.
    camera_pos: vec3<f32>,
    pole_sign: f32,
    // Solid cap colour (rgba, straight-alpha).
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: CapUniforms;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) visibility: f32,
};

const PI: f32 = 3.14159265358979323846;
// Web Mercator's pole-side clamp. The cap covers the spherical band
// between this latitude and the pole — exactly the gap the tile
// pyramid leaves uncovered.
const CAP_EDGE_LAT_RAD: f32 = 1.4844222297453324; // 85.05112877980659° in radians
const RING_VERTS: u32 = 64u;

/// Sphere position from (lon, lat). Prime meridian at +Z — matches
/// `lonlat_to_sphere` in tile.wgsl / vector.wgsl. See
/// feedback-sphere-convention in memory for the why.
fn lonlat_to_sphere(lon: f32, lat: f32) -> vec3<f32> {
    return vec3<f32>(cos(lat) * sin(lon), sin(lat), cos(lat) * cos(lon));
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Each triangle in the fan is (pole, ring[i], ring[i+1]).
    let tri = vi / 3u;
    let corner = vi % 3u;

    var lon: f32;
    var lat: f32;
    if (corner == 0u) {
        // Pole vertex — longitude is degenerate at the pole; any value
        // collapses to the same sphere point.
        lon = 0.0;
        lat = u.pole_sign * (PI * 0.5);
    } else {
        let ring_idx = tri + (corner - 1u);
        let theta = 2.0 * PI * f32(ring_idx) / f32(RING_VERTS);
        lon = theta - PI; // [-π, π]
        lat = u.pole_sign * CAP_EDGE_LAT_RAD;
    }

    let sphere = lonlat_to_sphere(lon, lat);
    var out: VsOut;
    out.clip = u.view_proj * vec4<f32>(sphere, 1.0);
    // > 0 = front hemisphere, < 0 = back. Matches the tile/vector
    // convention so cap fragments behind the globe are discarded.
    out.visibility = dot(sphere, u.camera_pos) - 1.0;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (in.visibility < 0.0) {
        discard;
    }
    return u.color;
}
