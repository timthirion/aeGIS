// Extruded-building pass (plan 0014 M1). Each vertex projects its
// normalised-Mercator world position to a unit-sphere point, then
// displaces along the per-building centroid normal by
// `height_world` so wall + top vertices ride at the right radius.
// The fragment combines an ambient floor + sun-direction Lambert
// against the per-face normal, plus a top-vs-wall tint so rooftops
// pop against the side walls. Depth-tested + writes depth so
// building-vs-building occlusion is correct in the single indexed
// draw call.

struct BuildingUniforms {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    strength: f32,            // smoothstep(14.0, 15.5) of camera zoom
    sun_dir: vec3<f32>,
    night_dim: f32,
    fill_color: vec4<f32>,    // top-face base colour (per-body tunable)
    wall_color: vec4<f32>,    // wall base colour (slightly darker for silhouette pop)
};

struct BuildingPerInstance {
    centroid_normal: vec3<f32>,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: BuildingUniforms;
@group(0) @binding(1) var<storage, read> per_building: array<BuildingPerInstance>;

const PI: f32 = 3.14159265358979323846;
/// Sub-millimetre radial bias in real-Earth units (`1e-6` of a unit
/// sphere ≈ 6 m, deliberately larger than the centimetre-scale
/// curvature delta within a building footprint) so the bases sit
/// just above the tile surface. Without this the depth test ties
/// with the basemap and the depth-write fails to commit.
const BASE_BIAS: f32 = 1.0e-6;

struct VsIn {
    @location(0) world: vec2<f32>,
    @location(1) height_world: f32,
    @location(2) building_idx: u32,
    @location(3) face_kind: u32,        // 0 = wall, 1 = top
    @location(4) normal: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) sphere: vec3<f32>,
    @location(1) visibility: f32,
    @location(2) world_normal: vec3<f32>,
    @location(3) @interpolate(flat) face_kind: u32,
};

/// Inverse Spherical Mercator: normalised `(x, y)` → `(lon, lat)` rad.
fn world_to_lonlat_rad(world: vec2<f32>) -> vec2<f32> {
    let lon_rad = world.x * 2.0 * PI - PI;
    let n = PI * (1.0 - 2.0 * world.y);
    let lat_rad = atan(sinh(n));
    return vec2<f32>(lon_rad, lat_rad);
}

/// `(lon, lat)` → XYZ on unit sphere with prime meridian at +Z.
fn lonlat_to_sphere(lonlat: vec2<f32>) -> vec3<f32> {
    let lon = lonlat.x;
    let lat = lonlat.y;
    return vec3<f32>(cos(lat) * sin(lon), sin(lat), cos(lat) * cos(lon));
}

@vertex
fn vs_main(in: VsIn) -> VsOut {
    let lonlat = world_to_lonlat_rad(in.world);
    let sphere_base = lonlat_to_sphere(lonlat);
    let n = per_building[in.building_idx].centroid_normal;
    // Extrude along the centroid normal by the per-vertex height
    // (0 for base verts, h_world for top + top-edge-wall verts).
    // The BASE_BIAS keeps building bases just above the tile
    // surface even when extruded height is zero.
    let displacement = in.height_world + BASE_BIAS;
    let sphere = sphere_base + n * displacement;

    var out: VsOut;
    out.clip = u.view_proj * vec4<f32>(sphere, 1.0);
    out.sphere = sphere;
    // Back-hemisphere cull works the same as for other body-surface
    // passes — the displacement is sub-1e-4 so the unit-sphere
    // dot-product is still a good proxy.
    out.visibility = dot(normalize(sphere), u.camera_pos) - 1.0;
    out.world_normal = in.normal;
    out.face_kind = in.face_kind;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (in.visibility < 0.0) {
        discard;
    }
    if (u.strength < 1e-3) {
        discard;
    }

    // Sun-direction Lambert. The walls' shading is the load-bearing
    // 3D cue in the top-down v1 — sun-facing walls should read
    // visibly brighter than shaded ones.
    let n = normalize(in.world_normal);
    let lambert = max(dot(n, u.sun_dir), 0.0);
    // Ambient floor so the shaded walls aren't pure black; full
    // Lambert at the bright end gives ~1.0 multiplier.
    let lit = 0.4 + 0.6 * lambert;

    // Top faces get a slightly brighter base colour than walls so
    // the rooftops pop against the ground. face_kind = 1 → top.
    let base = select(u.wall_color, u.fill_color, in.face_kind == 1u);

    let rgb = base.rgb * lit;
    return vec4<f32>(rgb * u.strength, base.a * u.strength);
}
