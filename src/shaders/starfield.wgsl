// Procedural starfield, rendered fullscreen before everything else
// so the lit globe + atmosphere overdraw it where they cover the
// view. Stars are hashed against the world-space view direction so
// they stay fixed in the celestial sphere as the user pans the
// camera around Earth.
//
// The fullscreen quad has no vertex buffer (six clip-space verts
// emitted procedurally). The fragment shader reconstructs the
// world ray for each pixel from the camera basis (`forward`, `right`,
// `up`) derived analytically from `camera_pos` — no inverse-matrix
// upload needed.

struct StarfieldUniform {
    // Camera position in body-fixed coords (same convention as the
    // other surface shaders). `forward = −normalize(camera_pos)`
    // because the camera always looks at the origin.
    camera_pos: vec3<f32>,
    /// Canvas aspect ratio (width / height). Used to undo the
    /// projection's horizontal stretch when reconstructing rays.
    aspect: f32,
    /// "Up hint" — `(0, 1, 0)` everywhere except near the poles
    /// where the renderer switches to `(0, 0, 1)` so the cross
    /// product producing `right` doesn't degenerate.
    up_hint: vec3<f32>,
    /// Zoom-driven 0..1 fade. Stars are most visible at globe view
    /// where there's empty sky around the planet; at street zoom
    /// the globe fills the canvas and stars aren't visible anyway
    /// — gating on strength lets the fragment shader early-out
    /// and skip the per-pixel hash work.
    strength: f32,
};

@group(0) @binding(0) var<uniform> u: StarfieldUniform;

const PI: f32 = 3.14159265358979323846;
/// Half the perspective camera's vertical FOV (60° / 2). Mirrors
/// the `60.0_f32.to_radians()` in `Camera::view_projection_matrix`.
const FOV_HALF_Y: f32 = 0.5235987755982988;
/// Cells across the equal-area celestial-sphere parameterisation.
/// Final star count ≈ density² × 2 × (1 − threshold) ≈ 1080 with
/// the params below. Visually reads as a generous-but-not-busy
/// star field at globe view.
const STAR_DENSITY: f32 = 60.0;
/// Hash threshold above which a cell holds a star. 0.85 = ~15% of
/// cells generate a star.
const STAR_THRESHOLD: f32 = 0.85;
/// Sharper exponential gives smaller stars; 60 ≈ 1/STAR_DENSITY
/// per pixel diameter. Tunable.
const STAR_FALLOFF: f32 = 60.0;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    // Two triangles covering the entire clip-space rect.
    var pos = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>( 1.0, -1.0), vec2<f32>(-1.0,  1.0),
        vec2<f32>(-1.0,  1.0), vec2<f32>( 1.0, -1.0), vec2<f32>( 1.0,  1.0),
    );
    let p = pos[vi];
    var out: VsOut;
    // depth = 1.0 puts the quad on the far plane — there's no
    // depth test enabled here, but render order makes this draw
    // first and everything else overwrites where it covers.
    out.clip = vec4<f32>(p, 1.0, 1.0);
    out.ndc = p;
    return out;
}

fn hash21(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

fn hash22(p: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(
        fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453),
        fract(sin(dot(p, vec2<f32>(269.5, 183.3))) * 43758.5453),
    );
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (u.strength <= 0.0) {
        discard;
    }

    // Camera basis. Camera always looks at the origin, so
    // forward = −normalize(camera_pos). `right` and `up` follow
    // from the cross-product convention used by `look_at` in
    // camera.rs.
    let forward = -normalize(u.camera_pos);
    let right = normalize(cross(forward, u.up_hint));
    let up = cross(right, forward);

    // Reconstruct the world-space ray through this pixel by
    // adding the right + up components scaled by NDC × tan(FOV/2).
    // Mirrors the projection that produced the matrix in
    // `Camera::view_projection_matrix`.
    let tan_half = tan(FOV_HALF_Y);
    let dir = normalize(
        forward
        + right * (in.ndc.x * u.aspect * tan_half)
        + up * (in.ndc.y * tan_half)
    );

    // Equal-area parameterisation of the celestial sphere:
    // (lon/π, sin(lat)). Both axes range over [-1, 1] and a
    // uniform grid in this space gives uniform-area cells on the
    // sphere — much better than raw lat/lon, which would bunch
    // stars near the poles.
    let p = vec2<f32>(atan2(dir.x, dir.z) / PI, dir.y) * STAR_DENSITY;
    let cell = floor(p);
    let local = p - cell;

    var color = vec3<f32>(0.0);
    for (var dx: i32 = -1; dx <= 1; dx = dx + 1) {
        for (var dy: i32 = -1; dy <= 1; dy = dy + 1) {
            let n = vec2<f32>(f32(dx), f32(dy));
            let neighbor = cell + n;
            let pos_in_cell = n + hash22(neighbor);
            let dist = length(local - pos_in_cell);

            let exists_hash = hash21(neighbor + vec2<f32>(5.7, 3.1));
            let exists = step(STAR_THRESHOLD, exists_hash);
            // Rare bright stars: the top 3% of "exists" hashes get
            // an extra 2× brightness so a few stand out.
            let bright_boost = 1.0 + step(0.97, exists_hash) * 1.5;
            let intensity = exists * bright_boost * exp(-dist * STAR_FALLOFF);

            // Subtle blue/yellow tint by another hash slot.
            let color_h = hash21(neighbor + vec2<f32>(11.0, 17.0));
            let star_color = mix(
                vec3<f32>(1.0, 0.92, 0.86),
                vec3<f32>(0.88, 0.95, 1.0),
                color_h,
            );
            color = color + star_color * intensity;
        }
    }

    return vec4<f32>(color * u.strength, u.strength);
}
