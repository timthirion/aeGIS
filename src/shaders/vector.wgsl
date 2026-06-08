// Vector overlay pass: projects normalised-Mercator world vertices to
// screen NDC via a per-frame camera uniform. Drawn as a wgpu LineList
// after the tile pass, with alpha blending for a subtle overlay.
//
// **This is the projection point** that swaps when the globe view
// (plan 0001 Phase 9) lands. Today: flat Web Mercator → NDC. Tomorrow:
// blend between Mercator-flat NDC and ellipsoidal-globe NDC by a
// `globeness` uniform driven by zoom.

struct Camera {
    /// Normalised Mercator coord at the viewport centre.
    world_center: vec2<f32>,
    /// How many display pixels one normalised-Mercator unit covers
    /// at the current zoom (`TILE_PIXELS * 2^zoom`).
    pixels_per_world: f32,
    _pad0: f32,
    /// `(canvas_width / 2, canvas_height / 2)` in physical pixels.
    canvas_half: vec2<f32>,
    _pad1: vec2<f32>,
    /// Line colour (RGBA, premultiplied alpha not required —
    /// blending is straight `src_alpha`).
    color: vec4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct VsIn {
    @location(0) world: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    // Pixel offset from the viewport centre, +y down.
    let offset_px = (in.world - camera.world_center) * camera.pixels_per_world;
    // NDC: +y up.
    let ndc = vec2<f32>(
        offset_px.x / camera.canvas_half.x,
        -offset_px.y / camera.canvas_half.y,
    );
    var out: VsOut;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return camera.color;
}
