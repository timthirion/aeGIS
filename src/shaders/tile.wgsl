// Textured-quad pass: samples one raster tile and draws it at a
// per-tile rectangle in screen-NDC. One draw call per visible tile;
// the rect comes from the `Camera::tile_ndc_rect` for the camera's
// current pan/zoom state.
//
// Phase 9 (globe view): the per-tile uniform also carries a
// `globeness` value derived from camera zoom. The fragment shader
// fades the tile alpha by `(1 - globeness)` so the basemap
// smoothly disappears as the 3D globe takes over (the country-
// outline vector overlay, projected to the sphere, becomes the
// sole content of the low-zoom view).

struct Uniforms {
    /// Tile quad in NDC: (x_min, y_min, x_max, y_max).
    rect: vec4<f32>,
    /// 0.0 = flat Mercator (tiles fully visible); 1.0 = full globe
    /// (tiles fully faded out). Smoothstep'd CPU-side from zoom.
    globeness: f32,
    _pad: vec3<f32>,
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
    // 6-vertex quad as two triangles (CCW winding).
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let p = positions[vi];
    var out: VsOut;
    let ndc = mix(u.rect.xy, u.rect.zw, p);
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    // PNG row 0 = top-of-tile; map it to NDC y_max (top of quad).
    out.uv = vec2<f32>(p.x, 1.0 - p.y);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let sample = textureSample(tile_tex, tile_sampler, in.uv);
    let fade = 1.0 - u.globeness;
    // Straight-alpha output; the pipeline does SRC_ALPHA blending.
    return vec4<f32>(sample.rgb, sample.a * fade);
}
