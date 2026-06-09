// Tessellated tile pass using the same single-projection 3D scene
// as `vector.wgsl`. Each tile is an 8×8 grid (384 verts), each vertex
// projects its (lon, lat) → unit-sphere XYZ → clip via the camera's
// view-projection matrix. At high zoom (camera near sphere surface)
// the tile renders almost flat; at low zoom the tile curves to wrap
// the globe.
//
// Backface culling is per-fragment via the sphere-vs-camera dot
// product, same convention as the vector pass.

struct Uniforms {
    view_proj: mat4x4<f32>,
    // 3D camera position (used for backface culling).
    camera_pos: vec3<f32>,
    _pad0: f32,
    // Tile's world rect in normalised Mercator: (xmin, ymin, xmax, ymax).
    world_rect: vec4<f32>,
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
// `GRID × GRID` quads per tile. A z=0 tile covers the entire globe;
// at GRID=8 the silhouette degenerates to a ~16-facet polygon
// because the tile mesh sits on the same sphere positions as the
// underlying Earth-texture sphere and wins the depth tie. GRID=32
// → 1024 quads (2048 tris) per tile, smooth at the globe scale and
// still cheap at high zoom where each tile covers a tiny region.
const GRID: u32 = 32u;
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
    let world = mix(u.world_rect.xy, u.world_rect.zw, tile_uv);
    let lonlat = world_to_lonlat_rad(world);
    let sphere = lonlat_to_sphere(lonlat);
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
    return textureSample(tile_tex, tile_sampler, in.uv);
}
