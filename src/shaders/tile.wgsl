// Textured-quad pass: samples a single raster tile and draws it as a
// fullscreen-triangle scaled to preserve the tile's aspect ratio.
//
// M1.5 — the "first tile" stop on plan 0001's slope toward a full
// slippy map. M2 generalises this to N visible tiles, projected through
// a Web Mercator camera matrix.

struct Uniforms {
    // (sx, sy): per-axis NDC scale applied to the fullscreen triangle
    // so the tile preserves its 1:1 aspect ratio inside the canvas.
    // The unused tile of NDC outside the scaled quad gets the clear
    // colour (LoadOp::Clear) — letterbox / pillarbox.
    scale: vec2<f32>,
    _padding: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tile_tex: texture_2d<f32>;
@group(0) @binding(2) var tile_sampler: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Same fullscreen-triangle trick as `clear.wgsl` — xy ∈ {(0,0),
    // (2,0), (0,2)}.
    let xy = vec2<f32>(
        f32((vi << 1u) & 2u),
        f32(vi & 2u),
    );
    var out: VsOut;
    let ndc = (xy * 2.0 - 1.0) * u.scale;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    // UVs: the tile is stored top-down (PNG row 0 = north edge), and
    // WebGPU's NDC y is +up. Mapping `(1 - xy.y)` puts the texture's
    // top row at clip-space y = +1 (top of screen).
    out.uv = vec2<f32>(xy.x, 1.0 - xy.y);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(tile_tex, tile_sampler, in.uv);
}
