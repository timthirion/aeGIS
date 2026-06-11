// Earth texture sphere — Blue Marble equirectangular imagery sampled
// onto a full unit sphere. Drawn before any tile (so loaded tiles
// overdraw the texture in their region) and before the polar caps
// and vector overlays.
//
// Geometry is generated procedurally from `vertex_index`: LAT_BANDS ×
// LON_SEGMENTS quads tessellate the sphere, each quad emitted as two
// triangles (6 verts). No vertex buffer bound.
//
// Backface culling uses the same sphere-vs-camera dot product as
// `tile.wgsl` / `vector.wgsl` so fragments behind the globe are
// discarded.

struct Camera {
    view_proj: mat4x4<f32>,
    // 3D camera position in sphere-coords (length > 1).
    position: vec3<f32>,
    _pad0: f32,
    // Day/night state (plan 0009 M0). See tile.wgsl for the
    // conventions; this shader uses the same dim formula.
    sun_dir: vec3<f32>,
    night_dim: f32,
};

@group(0) @binding(0) var<uniform> camera: Camera;
@group(0) @binding(1) var earth_tex: texture_2d<f32>;
@group(0) @binding(2) var earth_sampler: sampler;
// City-lights texture (plan 0009 M2). Earth binds NASA Black Marble;
// other bodies bind a 1×1 black pixel so the BGL is uniform —
// sampling black there reduces to the M0 + M1 behaviour cleanly.
@group(0) @binding(3) var night_tex: texture_2d<f32>;

const PI: f32 = 3.14159265358979323846;
const HALF_PI: f32 = 1.5707963267948966;
const LAT_BANDS: u32 = 64u;
const LON_SEGMENTS: u32 = 128u;
const QUAD_VERTS: u32 = 6u;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) visibility: f32,
    @location(2) sphere: vec3<f32>,
};

/// `(lon, lat)` on a unit sphere → XYZ with prime meridian at +Z.
/// Matches the convention in tile.wgsl / vector.wgsl / caps.wgsl.
fn lonlat_to_sphere(lon: f32, lat: f32) -> vec3<f32> {
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
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let quad_idx = vi / QUAD_VERTS;
    let local_vi = vi % QUAD_VERTS;
    let lon_idx = quad_idx % LON_SEGMENTS;
    let lat_idx = quad_idx / LON_SEGMENTS;

    // Quad corners as (dlon, dlat) in [0, 1] — two triangles spanning
    // the (lon, lat) quad. Winding gives the outward-facing normal so
    // the camera-side faces render front.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0),
    );
    let c = corners[local_vi];

    // lat band 0 sits at the north pole; band `LAT_BANDS` at south.
    let lat_t = (f32(lat_idx) + c.y) / f32(LAT_BANDS);
    let lon_t = (f32(lon_idx) + c.x) / f32(LON_SEGMENTS);

    let lat = HALF_PI - lat_t * PI;       // +π/2 → −π/2
    let lon = lon_t * 2.0 * PI - PI;      // −π   → +π

    let sphere = lonlat_to_sphere(lon, lat);
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(sphere, 1.0);
    // Equirectangular UVs: u = lon → [0, 1], v = north → 0 (image is
    // top-down with north pole at the first row).
    out.uv = vec2<f32>(lon_t, lat_t);
    out.visibility = dot(sphere, camera.position) - 1.0;
    out.sphere = sphere;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (in.visibility < 0.0) {
        discard;
    }
    let day_rgb = textureSample(earth_tex, earth_sampler, in.uv).rgb;
    let night_rgb = textureSample(night_tex, earth_sampler, in.uv).rgb;
    let sphere_n = normalize(in.sphere);
    let cos_sun = dot(sphere_n, camera.sun_dir);
    let day = smoothstep(0.0, 0.15, cos_sun);
    // Dawn/dusk warm tint on the day side. Same coefficients as
    // `day_night_color`; inlined here because Earth's composite
    // splits the day + night terms rather than applying a single
    // multiplier.
    let warm_rise = smoothstep(-0.05, 0.05, cos_sun);
    let warm_fall = 1.0 - smoothstep(0.05, 0.3, cos_sun);
    let warm = warm_rise * warm_fall * 0.6;
    let warm_tint = mix(vec3<f32>(1.0, 1.0, 1.0), vec3<f32>(1.3, 0.8, 0.5), warm);
    let day_term = day_rgb * warm_tint;
    // Night side: city lights at full brightness on top of a very
    // faint copy of the day surface. Black Marble already encodes
    // both lights + dim oceans/continents so adding `day_rgb *
    // night_dim` would brighten the whole night side too much —
    // keep it small. Boost the city-lights term so it's clearly
    // visible against the dimmed surface.
    let night_term = night_rgb * 1.3 + day_rgb * camera.night_dim;
    let composite_full = mix(night_term, day_term, day);
    // Zoom ramp — at street zoom the user wants the basemap at
    // full brightness, so blend back to the unmodified day texture.
    // Same `(0.05, 0.5)` window as `day_night_color` in the other
    // shaders so the four passes ramp in lock-step and we don't
    // get flickery seams when tiles overdraw the globe.
    let cam_alt = length(camera.position) - 1.0;
    let strength = smoothstep(0.05, 0.5, cam_alt);
    let composite = mix(day_rgb, composite_full, strength);
    return vec4<f32>(composite, 1.0);
}
