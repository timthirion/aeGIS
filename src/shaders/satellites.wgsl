// Satellite orbital tracks — drawn as line segments around the globe.
//
// Each vertex carries a 3D position (already in sphere-radius units,
// with the same +Y-north / +Z-prime-meridian convention as the rest
// of the crate) and an RGB colour bundled per-orbit on the CPU side.
//
// No back-hemisphere discard: by design we render the *whole* orbit
// ring around the globe so the user can see the orbital plane even
// when half of it is behind the planet. If overlapping front+back
// arcs become visually noisy we can add a depth pass later and let
// the sphere occlude them.

struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = camera.view_proj * vec4<f32>(in.pos, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Moderately translucent so two overlapping rings don't punch out
    // the basemap underneath.
    return vec4<f32>(in.color, 0.75);
}
