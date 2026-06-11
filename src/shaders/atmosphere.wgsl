// Atmospheric scattering shell (plan 0008 M1). Single-scattering
// Rayleigh + Mie with per-pixel ray-march, additively blended over
// the planet sphere. Renders a procedurally tessellated sphere at
// `atmosphere_radius` slightly larger than the planet; fragments
// trace a ray from the camera through their world position,
// accumulate optical depth toward the sun + along the ray, and
// emit the scattered light.
//
// Geometry: front-face culled (BackFace winding so the back of the
// shell faces camera). One quad per (lon, lat) cell, no vertex
// buffer — same procedural pattern as `earth.wgsl`. At globe view
// the projected disk covers ~half the canvas; we ray-march only
// those fragments rather than every screen pixel.
//
// Strength: zoom-ramped 0..1 from the renderer so the halo fades
// out at street zoom along with the day/night dim (same window as
// `day_night_color`). Without that ramp the atmosphere wraps the
// camera with washed-out blue when you zoom inside the shell.

struct AtmosphereUniform {
    view_proj: mat4x4<f32>,
    // Camera in body-fixed coords. Length > 1 + thin atmosphere
    // for globe view, < 1 + atmosphere_radius at high zoom.
    camera_pos: vec3<f32>,
    planet_radius: f32,
    // Sun direction (unit) in body-fixed frame.
    sun_dir: vec3<f32>,
    atmosphere_radius: f32,
    // Per-wavelength Rayleigh extinction coefficient (R, G, B),
    // normalized so the integral along the unit-sphere planet
    // radius gives a reasonable optical depth.
    rayleigh_beta: vec3<f32>,
    sun_intensity: f32,
    // Mie extinction (typically wavelength-independent, but kept
    // as vec3 for tuning headroom — Mars uses a slightly tinted
    // Mie to read as dust haze).
    mie_beta: vec3<f32>,
    mie_g: f32,
    rayleigh_scale: f32,
    mie_scale: f32,
    // Zoom-driven 0..1 fade — full at globe view, 0 at street zoom.
    strength: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: AtmosphereUniform;

const PI: f32 = 3.14159265358979323846;
const HALF_PI: f32 = 1.5707963267948966;
const LAT_BANDS: u32 = 48u;
const LON_SEGMENTS: u32 = 96u;
const QUAD_VERTS: u32 = 6u;
const I_STEPS: i32 = 12;
const J_STEPS: i32 = 4;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let quad_idx = vi / QUAD_VERTS;
    let local_vi = vi % QUAD_VERTS;
    let lon_idx = quad_idx % LON_SEGMENTS;
    let lat_idx = quad_idx / LON_SEGMENTS;
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0),
    );
    let c = corners[local_vi];
    let lon_t = (f32(lon_idx) + c.x) / f32(LON_SEGMENTS);
    let lat_t = (f32(lat_idx) + c.y) / f32(LAT_BANDS);
    let lat = HALF_PI - lat_t * PI;
    let lon = lon_t * 2.0 * PI - PI;
    let unit = vec3<f32>(cos(lat) * sin(lon), sin(lat), cos(lat) * cos(lon));
    let world = unit * u.atmosphere_radius;
    var out: VsOut;
    out.clip = u.view_proj * vec4<f32>(world, 1.0);
    out.world_pos = world;
    return out;
}

/// Ray-sphere intersection. Sphere centered at origin, radius `sr`.
/// Returns (t_near, t_far) along ray `ro + t * rd`. If the ray
/// misses, returns (1e6, -1e6) so the caller's `t_near > t_far`
/// branch discards correctly.
fn rsi(ro: vec3<f32>, rd: vec3<f32>, sr: f32) -> vec2<f32> {
    let b = dot(rd, ro);
    let c = dot(ro, ro) - sr * sr;
    let d = b * b - c;
    if (d < 0.0) {
        return vec2<f32>(1e6, -1e6);
    }
    let sd = sqrt(d);
    return vec2<f32>(-b - sd, -b + sd);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (u.strength <= 0.0) {
        discard;
    }
    let ro = u.camera_pos;
    let rd = normalize(in.world_pos - ro);

    let atm_hit = rsi(ro, rd, u.atmosphere_radius);
    if (atm_hit.x > atm_hit.y) {
        discard;
    }
    let planet_hit = rsi(ro, rd, u.planet_radius);
    let planet_hits = planet_hit.y > planet_hit.x && planet_hit.x > 0.0;

    let t_near = max(atm_hit.x, 0.0);
    var t_far = atm_hit.y;
    if (planet_hits) {
        t_far = min(t_far, planet_hit.x);
    }
    if (t_near >= t_far) {
        discard;
    }

    // Phase functions — angular dependence of scattering on the
    // angle between view direction and sun direction.
    let mu = dot(rd, u.sun_dir);
    let mumu = mu * mu;
    let gg = u.mie_g * u.mie_g;
    let p_rlh = 3.0 / (16.0 * PI) * (1.0 + mumu);
    let mie_denom = pow(1.0 + gg - 2.0 * mu * u.mie_g, 1.5) * (2.0 + gg);
    let p_mie = 3.0 / (8.0 * PI) * ((1.0 - gg) * (mumu + 1.0)) / max(mie_denom, 1e-6);

    // Primary ray-march from atmosphere entry to atmosphere exit
    // (or planet surface, whichever comes first).
    let i_step = (t_far - t_near) / f32(I_STEPS);
    var i_t = t_near + i_step * 0.5;
    var total_rlh = vec3<f32>(0.0);
    var total_mie = vec3<f32>(0.0);
    var i_od_rlh = 0.0;
    var i_od_mie = 0.0;

    for (var i = 0; i < I_STEPS; i = i + 1) {
        let i_pos = ro + rd * i_t;
        let i_height = length(i_pos) - u.planet_radius;
        let i_od_step_rlh = exp(-i_height / u.rayleigh_scale) * i_step;
        let i_od_step_mie = exp(-i_height / u.mie_scale) * i_step;
        i_od_rlh = i_od_rlh + i_od_step_rlh;
        i_od_mie = i_od_mie + i_od_step_mie;

        // Earth's-shadow check: if the line from this sample
        // toward the sun pierces the planet first, the sample is
        // in shadow — skip its scattering contribution.
        let shadow_hit = rsi(i_pos, u.sun_dir, u.planet_radius);
        let in_shadow = shadow_hit.y > shadow_hit.x && shadow_hit.x > 0.0;

        if (!in_shadow) {
            // Secondary ray toward the sun — accumulate optical
            // depth from the sample point to the top of the
            // atmosphere along the sun direction.
            let j_hit = rsi(i_pos, u.sun_dir, u.atmosphere_radius);
            let j_step = j_hit.y / f32(J_STEPS);
            var j_t = j_step * 0.5;
            var j_od_rlh = 0.0;
            var j_od_mie = 0.0;
            for (var j = 0; j < J_STEPS; j = j + 1) {
                let j_pos = i_pos + u.sun_dir * j_t;
                let j_height = max(length(j_pos) - u.planet_radius, 0.0);
                j_od_rlh = j_od_rlh + exp(-j_height / u.rayleigh_scale) * j_step;
                j_od_mie = j_od_mie + exp(-j_height / u.mie_scale) * j_step;
                j_t = j_t + j_step;
            }

            // Transmittance: outscatter along both legs of the
            // light path (sun → sample, sample → camera).
            let attn = exp(
                -(u.mie_beta * (i_od_mie + j_od_mie)
                    + u.rayleigh_beta * (i_od_rlh + j_od_rlh))
            );
            total_rlh = total_rlh + i_od_step_rlh * attn;
            total_mie = total_mie + i_od_step_mie * attn;
        }

        i_t = i_t + i_step;
    }

    let color = u.sun_intensity
        * (p_rlh * u.rayleigh_beta * total_rlh + p_mie * u.mie_beta * total_mie);

    let alpha = u.strength;
    // Pre-multiplied alpha, additive-friendly blend.
    return vec4<f32>(color * alpha, alpha);
}
