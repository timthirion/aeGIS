// Tessellated tile pass — wraps each raster tile onto the globe at
// low zoom, sits flat at high zoom, smoothly interpolates between
// the two by `camera.globeness`.
//
// Geometry: 8×8 grid of quads per tile (9×9 corner vertices = 384
// `LineList`-style vertices drawn unindexed). At globeness=0 the
// projection collapses to the same NDC quad the original M1.5
// "fullscreen-triangle aspect" shader produced; at globeness=1 each
// vertex sits on a sphere patch.
//
// Backface culling is per-fragment: the vertex shader passes the
// rotated sphere depth; the fragment discards when the rotated point
// is on the far side of the sphere and we're substantially globe-
// shaped.

struct Uniforms {
    /// (x_min, y_min, x_max, y_max) of the tile in normalised
    /// Mercator world coords.
    world_rect: vec4<f32>,
    /// Camera centre in normalised Mercator (flat-path use).
    world_center: vec2<f32>,
    /// `TILE_PIXELS * 2^zoom`.
    pixels_per_world: f32,
    /// 0 = flat; 1 = full 3D globe; smoothstep in between.
    globeness: f32,
    /// `(canvas_width / 2, canvas_height / 2)` in physical pixels.
    canvas_half: vec2<f32>,
    /// Camera centre as (lon_rad, lat_rad) — for sphere rotation.
    center_lonlat_rad: vec2<f32>,
    /// Sphere radius in NDC (tunable margin around the globe).
    /// See `vector.wgsl` for the matching `_pad: vec3<f32>` warning —
    /// WGSL pads the struct end to 64 bytes automatically.
    globe_scale: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tile_tex: texture_2d<f32>;
@group(0) @binding(2) var tile_sampler: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) sphere_depth: f32,
};

const PI: f32 = 3.14159265358979323846;
const GRID: u32 = 8u;
const QUAD_VERTS: u32 = 6u;

fn world_to_lonlat_rad(world: vec2<f32>) -> vec2<f32> {
    let lon_rad = world.x * 2.0 * PI - PI;
    let n = PI * (1.0 - 2.0 * world.y);
    let lat_rad = atan(sinh(n));
    return vec2<f32>(lon_rad, lat_rad);
}

fn lonlat_to_sphere(lonlat: vec2<f32>) -> vec3<f32> {
    let lon = lonlat.x;
    let lat = lonlat.y;
    return vec3<f32>(cos(lat) * cos(lon), sin(lat), cos(lat) * sin(lon));
}

fn rotate_to_camera(p: vec3<f32>, cam: vec2<f32>) -> vec3<f32> {
    // Same rotation as vector.wgsl — bring `cam` to (0, 0, 1).
    let cl = cos(cam.x);
    let sl = sin(cam.x);
    let x1 = cl * p.x + sl * p.z;
    let z1 = -sl * p.x + cl * p.z;
    let y1 = p.y;
    let cla = cos(cam.y);
    let sla = sin(cam.y);
    let y2 = cla * y1 + sla * z1;
    let z2 = -sla * y1 + cla * z1;
    return vec3<f32>(x1, y2, z2);
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Unpack: which 8×8 cell are we in, and which of its 6 verts.
    let quad_idx = vi / QUAD_VERTS;
    let local_vi = vi % QUAD_VERTS;
    let qx = quad_idx % GRID;
    let qy = quad_idx / GRID;
    // 6-vertex CCW quad (matches the old single-quad pattern).
    var quad_uvs = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let local_uv = quad_uvs[local_vi];
    // Tile-local UV in [0, 1].
    let tile_uv = vec2<f32>(
        (f32(qx) + local_uv.x) / f32(GRID),
        (f32(qy) + local_uv.y) / f32(GRID),
    );
    // Vertex position in normalised Mercator world coords.
    let world = mix(u.world_rect.xy, u.world_rect.zw, tile_uv);

    // ---- Flat-Mercator path ----
    let offset_px = (world - u.world_center) * u.pixels_per_world;
    let flat_ndc = vec2<f32>(
        offset_px.x / u.canvas_half.x,
        -offset_px.y / u.canvas_half.y,
    );

    // ---- Globe path ----
    let lonlat = world_to_lonlat_rad(world);
    let sphere = lonlat_to_sphere(lonlat);
    let rotated = rotate_to_camera(sphere, u.center_lonlat_rad);
    let aspect = u.canvas_half.x / u.canvas_half.y;
    let globe_ndc = vec2<f32>(
        rotated.x * u.globe_scale / aspect,
        rotated.y * u.globe_scale,
    );

    let ndc = mix(flat_ndc, globe_ndc, u.globeness);

    var out: VsOut;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    // PNG row 0 = north edge of tile = tile_uv.y = 0. WebGPU sampling
    // expects (u, v) with v=0 at the texture top, so the UV is just
    // tile_uv.
    out.uv = tile_uv;
    out.sphere_depth = rotated.z;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (u.globeness > 0.5 && in.sphere_depth < 0.0) {
        discard;
    }
    return textureSample(tile_tex, tile_sampler, in.uv);
}
