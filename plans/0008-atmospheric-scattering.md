# Atmospheric scattering for the globe

- **Status:** proposed
- **Last updated:** 2026-06-10
- **Last touched on:** drafted in the 0004–0013 batch; the visual
  feature that separates aeGIS from "another slippy map"

## Goal

Render a physically-motivated atmosphere shell around the Earth
globe: Rayleigh scattering for the blue sky / red sunset,
optional Mie scattering for haze, sun-position dependent.
Most open-source web globes ship a flat-coloured "halo" or
nothing; a proper scattering pass — even a stylised one —
visibly elevates the production quality at low zoom. Pairs with
plan 0009 (solar terminator) since both share a sun-position
input.

Built as ordered milestones (M0–M3). M0 is the math (sun
position + scattering equations), M1 is the fragment shader,
M2 is the multi-body story (do Mars / Moon get atmospheres?),
M3 is the perf budget + tunables.

## Context

What exists today (commits up to `bfc073a`):

- The globe view (Phase 9 v1) renders a unit sphere with a
  back-hemisphere fragment cull. The atmosphere lives just past
  the limb — same camera, slightly larger sphere.
- No sun-position computation. Plan 0010 (time slider) lands
  the global time state this reads; for M0 we accept a hardcoded
  sun direction so this plan ships independently.
- WGSL shader infrastructure: shaders are `include_str!`'d
  inline. Adding a new fragment-only pass is the same pattern as
  the polar caps.

### New dependencies introduced in this plan

- None. Atmospheric scattering is a fragment-shader workload over
  procedural geometry.

## Design

### Sun position (M0)

`src/sun.rs`:

```rust
/// Approximate position of the sun in ECI (or any inertial frame)
/// for the given UTC `at` (seconds since UNIX epoch). Returns a
/// unit vector pointing from Earth's centre toward the sun.
pub fn sun_direction_at(at_unix_s: f64) -> [f64; 3];
```

We don't need ICRF-level precision; the visual effect tolerates
~0.5° of declination error. A low-order series gives us
sub-arcminute precision at trivial cost.

### Scattering model (M1)

We render a slightly larger transparent sphere around the Earth
(radius 1.025 — same scale as the atmospheric thickness on
real Earth). The fragment shader uses **single-scattering
Rayleigh + Mie** in the style of O'Neil 2005 (the lookup-table-
free variant), evaluated per-fragment.

Per fragment:

1. Ray from the camera through the fragment.
2. Intersect the planet sphere (front + back) and the atmosphere
   sphere.
3. Walk the ray segment from the atmosphere entry to the planet
   entry (or atmosphere exit if the ray misses the planet).
4. At a small number of sample points along the segment (8 in
   the v1, tunable), accumulate optical depth toward the sun and
   along the ray.
5. Combine with Rayleigh + Mie phase functions and the per-channel
   extinction coefficients.

The output is RGBA with low alpha (the atmosphere is mostly
clear); blended additively over the underlying sphere.

### Multi-body (M2)

Atmospheres ship per-body. The `Body` struct grows:

```rust
pub struct Atmosphere {
    pub planet_radius: f32,        // 1.0 in our normalised units
    pub atmosphere_radius: f32,    // 1.025 for Earth
    /// Rayleigh extinction per wavelength (R, G, B). Tuned per
    /// body — Earth ≈ (5.5e-6, 13e-6, 22.4e-6) in real units,
    /// remapped to our normalised sphere.
    pub rayleigh_beta: [f32; 3],
    pub mie_beta: f32,
}

pub fallback_atmosphere: Option<Atmosphere>,
```

Earth gets a tuned atmosphere; Mars gets a thin dust-tinted
atmosphere; Moon is `None` (no atmosphere → no pass).

### Tunables + perf (M3)

The 8-sample inner integration can be expensive at globe view
where many fragments traverse the atmosphere. A coarse pass at
half resolution + bilinear upsample is the common mitigation.
v1 ships full-resolution; if it exceeds 4 ms on a M1 MacBook
at z=2 (full-globe view), we add the half-res pass.

## Milestones

### M0 — Sun position (MAP-sun-direction)

- [ ] `sun_direction_at(unix_s)` matches USNO's solar position
      tables to within 0.1° on three reference dates: vernal
      equinox 2026, summer solstice 2026, an arbitrary date
      (2030-01-15T18:00:00Z).
- [ ] `Renderer::set_sun_direction(dir: [f32; 3])` lets callers
      override; plan 0010 wires the time-slider to this.

### M1 — Earth atmosphere shader (MAP-atmosphere-shader)

- [ ] `atmosphere.wgsl` with the O'Neil-style single-scattering
      integration.
- [ ] New render pipeline + sphere geometry (radius 1.025).
      Drawn after tiles, before vector + caps. Additive blend.
- [ ] Earth: visible blue halo at the limb, reddish gradient on
      the dawn/dusk terminator side. Verified by a screenshot
      comparison at z=2 with the sun at hardcoded
      `(1, 0.2, 0)` in body-fixed coords.
- [ ] Done-when: zoom out to z=2, the Earth has a sky-blue
      halo on the day side and a reddish glow at the terminator.

### M2 — Per-body atmospheres (MAP-atmosphere-multi-body)

- [ ] `Atmosphere` field on `Body`. Earth + Mars get tuned
      values; Moon is `None`.
- [ ] Mars's atmosphere is thin + dusty — `rayleigh_beta`
      shifted red, smaller atmosphere radius.
- [ ] Moon renders with no atmosphere pass at all.

### M3 — Perf budget (MAP-atmosphere-perf)

- [ ] Frame timing test at z=2, full-globe view, atmosphere on:
      < 4 ms additional shader time on a M1 MacBook.
- [ ] If over: half-res offscreen pass + bilinear composite.
      Acceptance is the same time bound; the implementation is
      what changes.

## Open questions

- **Pre-computed transmittance LUT?** O'Neil's per-fragment
  integration is sufficient for an 8-sample inner loop on
  modern hardware. Bruneton's LUT approach is significantly
  faster but adds a setup pass and texture management. Deferred.
- **Aerial perspective inside the atmosphere?** Beyond v1.
  Limited to the limb halo at low zoom.
- **Mars / Venus atmosphere precision.** Real Mars Rayleigh is
  tiny; the atmosphere is mostly Mie + dust. The v1 Mars look
  is "thin reddish halo," not radiometrically accurate.

## Done when

- The Earth globe at z=2 shows a sky-blue limb halo and a
  reddish dawn/dusk gradient that tracks the sun position.
- Mars shows a thin reddish halo at z=4.
- Moon shows no atmosphere.
- Atmosphere pass costs <4 ms at the worst camera position on
  a M1 MacBook (measurement printed in the test, not asserted).
- All milestones pass `cargo test --all-targets`,
  `cargo clippy --all-targets -- -D warnings`, and
  `cargo check --target wasm32-unknown-unknown --lib`.

## Plan-skeptic attacks addressed

Initial draft attacked on 2026-06-10. Resolution:

1. **Sun direction frame mismatch (ECI vs body-fixed)** —
   fixed: `sun_direction_at` returns body-fixed (ECEF for
   Earth). The internal ECI calculation rotates by GMST
   before the public API hands the vector back. Documented in
   doc comments + tested on three reference dates that pin
   ECEF direction (not just ECI).
2. **WGSL uniform alignment landmine** — fixed: M0 explicitly
   names the struct layout. `vec4<f32>` slot used (not
   `vec3<f32>`) — the sun direction packs into `vec4(x, y, z,
   _pad)` so the alignment trap from
   [`feedback_wgsl_struct_layout`](../../.claude/projects/-Users-tt-src-aegis/memory/feedback_wgsl_struct_layout.md)
   doesn't fire. CPU-side `#[repr(C)]` mirror gains an explicit
   `size_of` test per
   AGENTS.md.
3. **Atmosphere shell vs ellipsoid (plan 0012) conflict** —
   acknowledged: atmosphere radius is `equatorial_radius *
   1.025` until plan 0012 ships ellipsoidal vertex
   projection. After 0012 ships, the atmosphere is a
   *separate* ellipsoidal shell — that's a future revision
   flagged in Open questions.
4. **Sun-direction wiring owner ambiguous** — fixed: plan
   0009 (solar terminator) owns the `LayerTick` registration.
   This plan reads the same `Renderer::sun_direction` value;
   plan 0010 (time slider) provides the time the dir is
   computed from.
5. **"Printed not asserted" perf budget** — fixed: the
   measurement remains diagnostic but the criterion is now
   "if measured >4ms, the half-res path lands in this plan
   before close" — a forcing function, not just a hope.
