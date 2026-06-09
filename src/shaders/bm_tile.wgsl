// Blue Marble tile pass — streams NASA GIBS imagery onto the unit
// sphere. Same tessellated-quad pattern as `tile.wgsl`, but the
// projection is **equirectangular** (EPSG:4326) instead of Web
// Mercator: per-vertex (lon, lat) interpolates *linearly* between
// the tile's degree bounds, then projects to the sphere via the
// same `lonlat_to_sphere` convention used everywhere else.
//
// The Carto-side `tile.wgsl` does inverse Mercator on a normalised
// world rect. That math doesn't apply here — equirectangular tiles
// are degree-aligned and reach the poles, so the inverse projection
// is just a `radians()` away.
//
// Backface culling is the per-fragment sphere-vs-camera dot product,
// matching the existing tile / vector / cap / earth conventions.

struct Uniforms {
    view_proj: mat4x4<f32>,
    // 3D camera position (used for backface culling).
    camera_pos: vec3<f32>,
    // Per-frame zoom-driven fade applied to output alpha. Lets the
    // renderer cross-fade the streamed satellite imagery in without
    // a hard pop when the cache lands a tile mid-frame.
    tile_alpha: f32,
    // Tile's geographic bounds in **degrees**:
    // (lon_min, lat_min, lon_max, lat_max).
    lon_lat_bounds: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tile_tex: texture_2d<f32>;
@group(0) @binding(2) var tile_sampler: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) visibility: f32,
};

const PI: f32 = 3.14159265358979323846;
const DEG_TO_RAD: f32 = 0.017453292519943295;
const GRID: u32 = 32u;
const QUAD_VERTS: u32 = 6u;

/// `(lon, lat)` in radians on a unit sphere → XYZ. Same convention
/// as the Carto-tile / vector / cap / earth shaders: prime meridian
/// at +Z, north pole at +Y, 90°E at +X.
fn lonlat_to_sphere(lon: f32, lat: f32) -> vec3<f32> {
    return vec3<f32>(cos(lat) * sin(lon), sin(lat), cos(lat) * cos(lon));
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let quad_idx = vi / QUAD_VERTS;
    let local_vi = vi % QUAD_VERTS;
    let qx = quad_idx % GRID;
    let qy = quad_idx / GRID;
    var quad_uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let local_uv = quad_uvs[local_vi];
    let tile_uv = vec2<f32>(
        (f32(qx) + local_uv.x) / f32(GRID),
        (f32(qy) + local_uv.y) / f32(GRID),
    );

    // Interpolate lon (left → right) and lat (top = lat_max → bottom
    // = lat_min) across the tile's degree bounds.
    let lon_deg = mix(u.lon_lat_bounds.x, u.lon_lat_bounds.z, tile_uv.x);
    let lat_deg = mix(u.lon_lat_bounds.w, u.lon_lat_bounds.y, tile_uv.y);
    let sphere = lonlat_to_sphere(lon_deg * DEG_TO_RAD, lat_deg * DEG_TO_RAD);

    var out: VsOut;
    out.clip = u.view_proj * vec4<f32>(sphere, 1.0);
    out.uv = tile_uv;
    out.visibility = dot(sphere, u.camera_pos) - 1.0;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (in.visibility < 0.0) {
        discard;
    }
    let sample = textureSample(tile_tex, tile_sampler, in.uv);
    return vec4<f32>(sample.rgb, sample.a * u.tile_alpha);
}
