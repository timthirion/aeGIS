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
    // other surface shaders).
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
    /// Sun direction in body-fixed coords — same value the
    /// surface + atmosphere shaders read. The fragment renders a
    /// small disc + halo at this direction so the day/night work
    /// has a visible source.
    sun_dir: vec3<f32>,
    _pad0: f32,
    /// Camera look target in body-fixed coords. `forward =
    /// normalize(look_target − camera_pos)`. At zero pitch this
    /// is the surface point under the camera centre — collinear
    /// with the origin from the camera, so the math reduces to
    /// the pre-pitch `−normalize(camera_pos)`. At non-zero pitch
    /// the camera is off the radial axis and this keeps the
    /// basis honest. `target` itself is a reserved keyword in
    /// WGSL — hence the `look_target` rename.
    look_target: vec3<f32>,
    _pad1: f32,
};

@group(0) @binding(0) var<uniform> u: StarfieldUniform;

const PI: f32 = 3.14159265358979323846;
/// Half the perspective camera's vertical FOV (60° / 2). Mirrors
/// the `60.0_f32.to_radians()` in `Camera::view_projection_matrix`.
const FOV_HALF_Y: f32 = 0.5235987755982988;
/// Cells per radian on the celestial sphere. Direct angular
/// parameterisation `(lon, lat)` keeps cell aspect square at
/// low latitudes so stars look round in screen space — the
/// earlier `(lon/π, sin lat)` packing made them stretch
/// horizontally by ~3× the vertical extent.
///
/// Total cells ≈ 2π² × density² ≈ 7900 here; at 15 % survival
/// rate that's ~1200 visible stars on the celestial sphere.
const STAR_DENSITY: f32 = 20.0;
/// Hash threshold above which a cell holds a star. 0.85 = ~15 %
/// of cells generate a star.
const STAR_THRESHOLD: f32 = 0.85;
/// Linear-falloff radius in cell units. Star covers up to
/// `1 / STAR_INV_RADIUS = ~0.06` of a cell, ≈ 0.18° on sky →
/// roughly 1–2 screen pixels at a 1000-px-wide canvas.
const STAR_INV_RADIUS: f32 = 16.0;

/// `cos(angular_radius)` for the sun disc + halo. Real sun is
/// 0.27° angular radius; we exaggerate to ~0.8° for the core and
/// ~6° for the halo so the source reads clearly at globe view
/// without being a dot lost in the starfield.
const SUN_CORE_COS: f32 = 0.9999;
const SUN_HALO_COS: f32 = 0.9945;

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

    // Camera basis from the look target — matches what
    // `view_projection_matrix` builds + what `pick_feature_at`
    // ray-marches through. Reduces to `−normalize(camera_pos)`
    // at pitch=0 (target is the surface point on the line
    // through origin from camera) and stays correct under
    // non-zero pitch.
    let forward = normalize(u.look_target - u.camera_pos);
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

    // Direct angular parameterisation `(lon, lat)`. Stars look
    // isotropic at low latitudes — the previous `(lon/π, sin lat)`
    // packing stretched them horizontally by ~3× because the two
    // axes spanned different arc lengths under the same density.
    // Near-pole longitude bunching still happens but reads as
    // "more stars near zenith," which looks natural.
    let p = vec2<f32>(atan2(dir.x, dir.z), asin(dir.y)) * STAR_DENSITY;
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

            // Wide brightness range like a real night sky: most
            // stars dim, occasional medium, rare very bright.
            // Earlier flat curve made every star look the same.
            let bright_boost =
                0.35
                + step(0.92, exists_hash) * 1.0
                + step(0.98, exists_hash) * 2.5;

            // Hard-cutoff quadratic falloff instead of the previous
            // exponential. exp() has a long tail that reads as a
            // soft halo — "smeared" stars. `max(0, 1 − d/r)²`
            // goes cleanly to zero outside the star, giving the
            // pin-prick look.
            let radius_factor = max(0.0, 1.0 - dist * STAR_INV_RADIUS);
            let intensity = exists * bright_boost * radius_factor * radius_factor;

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

    // Sun glyph — small disc + soft halo. Only renders when the
    // sun isn't behind the planet from the camera's point of view:
    // closest approach of the camera-to-sun ray to origin gives a
    // clean occlusion test without spawning a second draw call.
    let cs = dot(u.camera_pos, u.sun_dir);
    let cc = dot(u.camera_pos, u.camera_pos);
    // Sun ray hits planet iff its closest-approach distance < 1
    // AND that closest approach is in front of the camera.
    let sun_occluded = cs < 0.0 && (cc - cs * cs) < 1.0;
    if (!sun_occluded) {
        let cos_to_sun = dot(dir, u.sun_dir);
        let core = step(SUN_CORE_COS, cos_to_sun);
        let halo_t = clamp(
            (cos_to_sun - SUN_HALO_COS) / max(SUN_CORE_COS - SUN_HALO_COS, 1e-6),
            0.0,
            1.0,
        );
        let halo = halo_t * halo_t;
        let sun_color = vec3<f32>(1.0, 0.96, 0.78);
        let sun_intensity = core * 1.8 + halo * 0.5;
        color = color + sun_color * sun_intensity;
    }

    return vec4<f32>(color * u.strength, u.strength);
}
