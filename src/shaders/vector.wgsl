// Vector overlay pass with flat ↔ sphere projection.
//
// Each vertex computes BOTH the flat-Mercator NDC and the spherical-
// projection NDC, then mixes between them by `camera.globeness`. At
// `globeness = 0` the result is identical to the original slippy-map
// projection; at `globeness = 1` the geometry sits on a 3D globe.
// In between, smoothstep.
//
// Backface handling: vertices on the far side of the sphere
// (positive rotated-Z means the camera is "behind" them) are discarded
// in the fragment shader, with a soft globeness gate so the discard
// only kicks in once we're substantially globe-shaped.

struct Camera {
    /// Camera centre in normalised Mercator (flat-path use).
    world_center: vec2<f32>,
    /// `TILE_PIXELS * 2^zoom` — display pixels per Mercator unit.
    pixels_per_world: f32,
    /// 0.0 = flat Mercator; 1.0 = full 3D globe; smoothstep in between.
    globeness: f32,
    /// `(canvas_width / 2, canvas_height / 2)` in physical pixels.
    canvas_half: vec2<f32>,
    /// Camera centre as (lon_rad, lat_rad) for the sphere rotation.
    center_lonlat_rad: vec2<f32>,
    /// Line colour (straight alpha; the pipeline does standard
    /// SRC_ALPHA blending).
    color: vec4<f32>,
    /// Sphere radius in NDC (tunable margin around the globe).
    /// WGSL pads the struct end to a multiple of 16 (the largest
    /// member's alignment) automatically — matches the Rust
    /// `VectorCameraUniform`'s 64-byte size. **Do not add a trailing
    /// `_pad: vec3<f32>` field** — vec3 alignment would push the
    /// struct size to 80 bytes and the browser would reject the
    /// 64-byte uniform buffer as "too small".
    globe_scale: f32,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct VsIn {
    @location(0) world: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    /// Rotated sphere Z — positive = front of sphere (visible),
    /// negative = back. Sent to the fragment shader for backface
    /// discard. Interpolates linearly across each line segment, so
    /// segments that cross the horizon get per-fragment culling.
    @location(0) sphere_depth: f32,
};

const PI: f32 = 3.14159265358979323846;

/// Inverse Spherical Mercator: normalised (x, y) → (lon_rad, lat_rad).
fn world_to_lonlat_rad(world: vec2<f32>) -> vec2<f32> {
    let lon_rad = world.x * 2.0 * PI - PI;
    let n = PI * (1.0 - 2.0 * world.y);
    let lat_rad = atan(sinh(n));
    return vec2<f32>(lon_rad, lat_rad);
}

/// `(lon, lat)` on a unit sphere → XYZ.
fn lonlat_to_sphere(lonlat: vec2<f32>) -> vec3<f32> {
    let lon = lonlat.x;
    let lat = lonlat.y;
    return vec3<f32>(cos(lat) * cos(lon), sin(lat), cos(lat) * sin(lon));
}

/// Rotate a sphere point so the camera's `center_lonlat` ends up at +Z.
/// Two-axis rotation: first by -cam_lon around Y, then by -cam_lat
/// around X.
fn rotate_to_camera(p: vec3<f32>, cam: vec2<f32>) -> vec3<f32> {
    let cl = cos(cam.x);
    let sl = sin(cam.x);
    // Yaw: rotate -cam_lon around Y (active rotation of the point).
    let x1 = cl * p.x + sl * p.z;
    let z1 = -sl * p.x + cl * p.z;
    let y1 = p.y;
    // Pitch: rotate -cam_lat around X.
    let cla = cos(cam.y);
    let sla = sin(cam.y);
    let y2 = cla * y1 + sla * z1;
    let z2 = -sla * y1 + cla * z1;
    return vec3<f32>(x1, y2, z2);
}

@vertex
fn vs_main(in: VsIn) -> VsOut {
    // ---- Flat-Mercator path (unchanged) ----
    let offset_px = (in.world - camera.world_center) * camera.pixels_per_world;
    let flat_ndc = vec2<f32>(
        offset_px.x / camera.canvas_half.x,
        -offset_px.y / camera.canvas_half.y,
    );

    // ---- Globe path ----
    let lonlat = world_to_lonlat_rad(in.world);
    let sphere = lonlat_to_sphere(lonlat);
    let rotated = rotate_to_camera(sphere, camera.center_lonlat_rad);
    // Aspect correction so the sphere stays round on non-square canvases.
    let aspect = camera.canvas_half.x / camera.canvas_half.y;
    let globe_ndc = vec2<f32>(
        rotated.x * camera.globe_scale / aspect,
        rotated.y * camera.globe_scale,
    );

    let ndc = mix(flat_ndc, globe_ndc, camera.globeness);

    var out: VsOut;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.sphere_depth = rotated.z;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Discard back-of-sphere fragments once we're substantially
    // globe-shaped. The 0.5 threshold lets the flat-and-mostly-flat
    // views ignore depth entirely.
    if (camera.globeness > 0.5 && in.sphere_depth < 0.0) {
        discard;
    }
    return camera.color;
}
