// Fullscreen-triangle clear pass: paints a diagonal gradient so we can
// verify "pixels reach the screen" in both targets before any GIS code
// lands. Plan 0001 M0.

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Fullscreen triangle in NDC — no vertex buffer required.
    let xy = vec2<f32>(
        f32((vi << 1u) & 2u),
        f32(vi & 2u),
    );
    var out: VsOut;
    out.clip = vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
    out.uv = xy;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Diagonal aeGIS-blue gradient.
    return vec4<f32>(in.uv.x * 0.2, in.uv.y * 0.5, 0.4 + in.uv.x * 0.3, 1.0);
}
