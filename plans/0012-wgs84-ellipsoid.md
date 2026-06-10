# WGS84 ellipsoidal upgrade

- **Status:** proposed
- **Last updated:** 2026-06-10
- **Last touched on:** drafted in the 0004–0013 batch; the
  precision lift that turns aeGIS from "approximate" to
  "GIS-grade"

## Goal

Replace the unit-sphere approximation with a proper WGS84
ellipsoid (and per-body equivalents): equatorial radius
6 378 137 m, flattening 1/298.257 223 563. Vertices project
through the ellipsoidal forward transform; tile, vector, COG
overlays all stay aligned to ±1 pixel against authoritative
references (Natural Earth coastlines, USGS satellite-track
predictions). This is the upgrade that lets aeGIS make
correctness claims that survive a published-reference test.

Built as ordered research-spike milestones (M0–M3). M0 frames
the math + measures the existing sphere error; M1 lands the
WGS84 forward + inverse + round-trip tests; M2 plumbs the
ellipsoid through the camera + shaders; M3 ships per-body
ellipsoid parameters.

## Context

What exists today (commits up to `bfc073a`):

- The renderer treats every body as a unit sphere. Lon/lat
  vertices project to `(cos(lat)·sin(lon), sin(lat),
  cos(lat)·cos(lon))` — the spherical projection.
- Earth's actual flattening is 0.335 % (polar radius 6 356 752 m
  vs equatorial 6 378 137 m). At z=12 over 60° N latitude that's
  ~7 km of north-south position error from spherical assumption
  — visible as misalignment between Carto tiles (built against
  WGS84) and overlaid Natural Earth lines.
- Mars has 0.59 % flattening; the Moon 0.12 %. Per-body parameters
  matter once we make ellipsoid claims.

### New dependencies introduced in this plan

- None. The math is closed-form; `proj4rs` (already planned for
  plan 0006) covers any complex CRS chain we'd want to use as a
  cross-check.

## Design

### M0 — Measure current error

A research-style M0: instrument the existing sphere code,
generate a 360 × 170 grid of ground-truth WGS84 positions
(via `proj4rs`), measure pixel offset at each grid point at
z=10. Output a table that quantifies "how wrong is the sphere?"
across latitude. This grounds the M1+ work in a number that
matters; without M0 we ship a "more accurate" system without
knowing what we made better.

### Ellipsoid math (M1)

```rust
pub struct Ellipsoid {
    pub equatorial_radius_m: f64,
    pub flattening: f64,
}

impl Ellipsoid {
    pub const WGS84: Ellipsoid = Ellipsoid {
        equatorial_radius_m: 6_378_137.0,
        flattening: 1.0 / 298.257_223_563,
    };

    pub fn polar_radius_m(&self) -> f64;
    pub fn first_eccentricity_squared(&self) -> f64;
    pub fn radius_of_curvature_n(&self, lat_rad: f64) -> f64;

    /// Geodetic (lon, lat, h) → ECEF (x, y, z) in metres.
    /// h is height above ellipsoid in metres (0 for surface).
    pub fn geodetic_to_ecef(&self, lon_rad: f64, lat_rad: f64, h_m: f64)
        -> [f64; 3];

    /// ECEF → geodetic. Iterative (Bowring); converges in 2-3
    /// iterations to f64 precision.
    pub fn ecef_to_geodetic(&self, ecef: [f64; 3])
        -> (f64, f64, f64);
}
```

Round-trip tests pin the precision: 360×170 grid round-trips
through `geodetic → ecef → geodetic` to within 1e-9 radians
(~6 mm at Earth's surface) on every grid point.

### Camera + shaders (M2)

The renderer normalises everything to a unit sphere for
rendering — there's no need to change the GPU side to "real"
ECEF metres. What changes is the **vertex projection**: instead
of using the spherical `lonlat_to_sphere`, we use a normalised
ellipsoidal projection:

```wgsl
fn lonlat_to_ellipsoid(lonlat: vec2<f32>, ellipsoid_n: vec2<f32>) -> vec3<f32> {
    let f = ellipsoid_n.x;   // first eccentricity²
    let lat = lonlat.y;
    let lon = lonlat.x;
    let n = 1.0 / sqrt(1.0 - f * sin(lat) * sin(lat));
    return vec3<f32>(
        n * cos(lat) * sin(lon),
        n * (1.0 - f) * sin(lat),  // polar radius / equatorial scaling
        n * cos(lat) * cos(lon),
    );
}
```

The `ellipsoid_n` uniform carries `(eccentricity², 1/flattening)`
— two scalars added to the body uniform. The shader's existing
backface cull math still works because the surface is convex.

### Per-body parameters (M3)

`Body` grows an `ellipsoid: Ellipsoid` field. Earth uses WGS84;
Mars uses IAU Mars 2000 (a = 3 396 200 m, f = 1 / 169.8); Moon
uses a near-spherical IAU Moon (a = 1 738 100 m, f = 0.001 25).
The renderer reads `body.ellipsoid` and writes the uniform
per draw call.

## Milestones

### M0 — Baseline error measurement (CRS-sphere-error)

- [ ] `cargo run --bin measure-sphere-error` or a `#[ignore]`d
      test that emits a CSV of `(lat, lon, sphere_offset_m,
      ellipsoidal_offset_m)` over a 360 × 170 grid at z=10.
- [ ] Report: max sphere error, location of max, RMS over the
      grid. Numbers committed in this plan as the v1 baseline.

### M1 — Ellipsoid math (CRS-wgs84-math)

- [ ] `src/ellipsoid.rs` with the struct + constants + forward
      + inverse.
- [ ] Round-trip test: 360 × 170 grid through `geodetic_to_ecef`
      → `ecef_to_geodetic` lands within 1e-9 rad of identity.
- [ ] Cross-check against `proj4rs::Proj::transform`: random
      grid of 1 000 points, our `geodetic_to_ecef` agrees with
      proj's `EPSG:4978` to within 1 cm.

### M2 — Camera + shaders (CRS-ellipsoid-render)

- [ ] Vertex shaders gain `ellipsoid_n` uniform + the
      `lonlat_to_ellipsoid` function in place of the spherical
      version. Pipelines touched: tile.wgsl, earth.wgsl,
      caps.wgsl, vector.wgsl, mvt.wgsl (if plan 0005 has shipped).
- [ ] Renderer writes the active body's ellipsoid parameters
      into the uniform per frame.
- [ ] Done-when: re-running the M0 measurement post-implementation
      shows the offsets drop to <1 m on average.

### M3 — Per-body ellipsoids (CRS-multi-body-ellipsoids)

- [ ] `Body::ellipsoid` field populated for Earth / Mars / Moon.
- [ ] Done-when: at z=4 on Mars, the Viking color mosaic aligns
      to ±1 pixel with overlaid feature positions from the
      gazetteer (plan 0011's M2).

## Open questions

- **GPU precision (f32) at large radii.** Real ECEF metres are
  ~6.4 M; f32 has 24 bits of mantissa, so the floor is ~0.4 m
  at that scale. We normalise to the unit sphere (radius 1)
  before the GPU sees anything, so we keep f32 precision at
  better than 6 µm. Documented; no f64 path needed in shaders.
- **Camera altitude math.** The slippy-map altitude formula in
  `Camera::altitude` assumes a spherical Earth; on an ellipsoid
  the altitude at a given zoom varies slightly with latitude.
  For the visual experience, the existing formula stays — the
  shift is sub-pixel at every zoom. Open until we can measure
  otherwise.
- **Antimeridian wrap.** Out of scope; tracked in ROADMAP Phase
  9's "known limitations." This plan doesn't make it worse.

## Done when

- The M0 measurement re-run after M2 shows ellipsoidal offsets
  drop from ~7 km at 60° N to <1 m everywhere.
- Round-trip ellipsoidal forward + inverse tests pin precision
  to 1e-9 radians.
- Multi-body parameters land in `Body::ellipsoid`; Mars + Moon
  reuse the same pipeline.
- All milestones pass `cargo test --all-targets`,
  `cargo clippy --all-targets -- -D warnings`, and
  `cargo check --target wasm32-unknown-unknown --lib`.

## Plan-skeptic attacks addressed

Initial draft attacked on 2026-06-10. Resolution:

1. **Headline "Carto vs Natural Earth ±1 px" is unreachable
   without reprojecting raster pixels** — fixed: scope
   tightened. The deliverable is "vector overlays + raster
   tiles project through the same ellipsoidal projection,
   so vector-vs-vector alignment improves." Carto's baked
   Mercator texels stay where they were; the gain is when
   vector / COG / MVT layers overlay. Done-when reworded.
2. **Measurement is circular (`proj4rs` vs our ellipsoid)** —
   fixed: M1 adds a *third-party* reference — round-trip
   through GeographicLib's published WGS84 test vectors
   (the canonical NGA test suite), bundled offline as
   `tests/fixtures/wgs84-test-vectors.csv`.
3. **Pole singularity in the 360 × 170 grid** — fixed:
   grid is 360 × 168 (excluding ±90° rows); explicit pole
   tests use the Bowring-degenerate-case branch and assert
   no NaN.
4. **`Camera::camera_3d_position` stays spherical** —
   fixed: M2 explicitly updates the camera position to
   ellipsoidal too, with the cap-test recalibrated.
5. **WGSL alignment for `ellipsoid_n` uniform** — fixed:
   M2 names the layout up front (a `vec4<f32>` with
   `(eccentricity², 1-f, _pad, _pad)`), with the explicit
   `size_of` assertion per
   [`feedback_wgsl_struct_layout`](../../.claude/projects/-Users-tt-src-aegis/memory/feedback_wgsl_struct_layout.md).
6. **proj4rs::4978 (ECEF) is in plan 0006's deferred
   bucket** — acknowledged: M1's `proj4rs` cross-check is
   conditional on 0006 expanding the registry, or we lean
   on the GeographicLib test vectors instead. Either path
   ships.
