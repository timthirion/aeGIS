# Live satellite-orbit overlay

- **Status:** done
- **Last updated:** 2026-06-10
- **Last touched on:** all five milestones (M0–M4) shipped in a
  single focused session: `30b9bad` (M0), `ae7f73e` + `62d8c5e`
  (M1), `99ee550` (M2), `58f73d7` (M3), `877bef4` (M4).

## Goal

Render the publicly-tracked Earth satellite catalog as a moving
point cloud above the globe — Starlink shells, the ISS, GNSS
constellations, weather satellites, debris — propagated from
Celestrak TLEs through the SGP4/SDP4 model and updated against
real or scrubbed time. Hover identifies one; click fixes the
camera to it. This is the headline payoff for the globe view
that Phase 9 shipped: aeGIS goes from "you can spin a planet" to
"you can watch things orbit it in real time."

Built as ordered milestones (M0–M4). Each is independently
shippable: M0 is the math (no rendering), M1 the headless render,
M2 the categories + filtering, M3 the orbit-trail polylines, M4
hover/click identification.

## Context

What exists today (commits up to `bfc073a`):

- Globe view in `src/render.rs` with the camera locked to a unit
  sphere; satellite-orbit work treats every body as a unit sphere
  too, so the math reuses what the renderer already does.
- The time-slider plan ([`0010-time-slider.md`](0010-time-slider.md))
  lands the global "current time" state this plan reads.
- No existing point-cloud pipeline. The vector overlay (plan 0001
  M3) is a LineList; instanced points are a separate pipeline.
- No HTTP fetching of non-tile resources outside the geocoder /
  GeoJSON paths. The TLE fetch reuses `src/net.rs` from plan 0002 M1.

### New dependencies introduced in this plan

- [`sgp4`](https://crates.io/crates/sgp4) (MIT/Apache-2.0) — the
  canonical Rust SGP4/SDP4 propagator. Pure-Rust, wasm-friendly.
- No new HTTP client — `net::fetch_bytes_*` already covers TLE
  text downloads.

### Data sources

- **Celestrak** (`celestrak.org/NORAD/elements/`) — public-domain
  Two-Line Element sets. Hundreds of categorised files
  (`stations.txt`, `starlink.txt`, `gnss.txt`, `last-30-days.txt`,
  …) refreshed every few hours. CORS confirmed
  `Access-Control-Allow-Origin: *` from a GitHub Pages origin.
  Attribution in the footer: `Orbital elements: CelesTrak`.
- TLEs are observational data, not subject to copyright. Public
  domain.

## Design

### Math (M0)

`src/orbit.rs`:

```rust
pub struct Tle { pub name: String, pub line1: String, pub line2: String }

pub fn parse_tles(text: &str) -> Vec<Tle>;

/// One TLE prepared for repeated propagation. Owns the SGP4 elements.
pub struct Satellite {
    pub name: String,
    pub norad_id: u32,
    pub epoch_julian: f64,
    elements: sgp4::Elements,
    pub category: Category,
}

pub enum Category {
    Iss, Starlink, Gnss, Weather, Debris, Other,
}

pub fn satellites_from_tles(tles: &[Tle], category: Category) -> Vec<Satellite>;

/// Propagate to UTC `at` (seconds since UNIX epoch) and return
/// the satellite's position in TEME (true equator, mean equinox)
/// km. The renderer rotates this to the body-fixed frame.
pub fn propagate(sat: &Satellite, at_unix_s: f64) -> [f64; 3];
```

### Coordinate frames

TEME → ECEF rotation: `θ = GMST(at)`. The renderer's globe is in
the body-fixed frame (Greenwich at +Z); a single Z-axis rotation
by `-θ` puts the TEME satellite where it should appear. ECEF →
lon/lat for ground-tracking labels. Both transforms live in
`src/orbit.rs` with their own unit tests pinned against a known
reference (e.g. ISS at a known epoch).

### Native TLE source

Native uses the same `net::fetch_bytes_blocking` web does
(`celestrak.org` works from both targets). We also commit a
single TLE fixture (`data/orbits/iss-fixture.txt`, ~200 bytes —
the ISS group at a frozen epoch) under
[`project_data_sources`](../../.claude/projects/-Users-tt-src-aegis/memory/project_data_sources.md)
so the M0 / M1 tests run offline and so a network-down native
launch isn't a blank globe.

### Stable selection identity

Satellite identity is **NORAD catalog number** (parsed from the
TLE), not the position in the propagation buffer. M2's category
refresh re-sorts; M4's pick texture writes the NORAD id (u32);
the hover/click handler looks up `satellites_by_norad: HashMap<u32, usize>`
to find the current buffer index. A TLE refresh between hover
and click doesn't mis-identify.

### Render pipeline (M1)

`src/shaders/orbit.wgsl`:

- One instance per satellite. Vertex buffer carries instance data
  (3D position + category index + age-since-epoch in days for the
  "TLE staleness" tint).
- Vertex shader projects through the same `view_proj` matrix the
  tile pipeline uses. No backface culling — points behind the
  globe should *not* render (orbital points behind the Earth are
  occluded by the Earth, not the orbit shell).
- Geometry: each satellite renders as a billboarded square at
  fixed pixel size (e.g. 4 px). Category determines colour.
- Depth test on against the globe surface, so satellites behind
  the Earth occlude correctly.

The renderer holds `satellites: Vec<Satellite>` and a per-frame
`positions: Vec<[f32; 3]>` instance buffer it rewrites each frame.
Propagation runs CPU-side on every frame; with ~10k satellites
that's ~5 ms of `sgp4::propagate` per frame on a modern laptop.
If that becomes a bottleneck, fall through to "propagate every
N frames, interpolate between" — flagged in Open questions.

### Categories + filtering (M2)

A new UI control: checkboxes (or pills) for each `Category`.
Toggling rebuilds the instance buffer to include only enabled
categories. Default: ISS + Starlink + GNSS on; weather + debris
off (debris alone is ~25 k objects).

### Orbit trails (M3)

For a *selected* satellite (M4), draw the orbit as a polyline
sampled at N points (~128) across one orbital period. Uses the
existing vector pipeline with a body-specific colour. Trail
recomputes when the selection changes or when the TLE refreshes.

### Hover + click (M4)

GPU-side hit-test: the orbit shader writes the satellite index
to a 32-bit pick texture in a second colour attachment. On
pointer move + click, read back the pixel under the cursor (web:
`copyTextureToBuffer` + map_read; native: the same pattern).
Hover surfaces a tooltip; click selects + draws the trail.

## Milestones

### M0 — Math (MAP-orbit-math)

- [x] `src/orbit.rs` with `Tle`, `Satellite`, `Category`,
      `parse_tles`, `satellites_from_tles`, `propagate`,
      `teme_to_ecef`, `gmst_from_unix`.
- [x] Unit test: a known ISS TLE propagates to the published
      position (`{lat, lon, alt}` from celestrak.org's lookup
      page) within 1 km at the TLE epoch + 1 hour.
- [x] Unit test: `gmst_from_unix(2000-01-01T12:00:00Z)` matches
      the IAU reference value within 1e-6 radians.
- [x] Unit test: round-trip `teme → ecef → lonlat` for the same
      ISS position lands within 0.001° of the Celestrak lookup.

### M1 — Headless render (MAP-orbit-render)

- [x] `orbit.wgsl` shader + pipeline; instance buffer + per-frame
      rewrite.
- [x] `Renderer::load_satellites(category, tle_text)`: parse,
      prep, store. Called from the web fetcher.
- [x] Web entry point downloads `celestrak.org/NORAD/elements/
      gp.php?GROUP=stations&FORMAT=tle` (the ISS group, ~25 sats)
      at startup and feeds into `load_satellites`.
- [x] Camera at z=2 with the time-slider stopped: at least one
      visible orbiting dot (the ISS). On native + web.

### M2 — Categories + filtering (UI-orbit-categories)

- [x] Categories tab in the bottom-left chrome (sibling of the
      basemap toggle) — Stations / Starlink / GNSS / Weather /
      Debris pills.
- [x] Each category fetches its TLE group on first toggle-on,
      caches in memory.
- [x] Render budget guard: if total visible satellites > 12 000,
      log a warning and skip propagation of the lowest-priority
      category until the count drops below 8 000.
- [x] Done-when: turning Starlink on drops ~6 000 dots into the
      view, all moving, total frame time stays under 16 ms on a
      M1 MacBook.

### M3 — Orbit trails (MAP-orbit-trails)

- [x] `Satellite::orbital_period_minutes()` from the TLE's mean
      motion.
- [x] `Satellite::trail_points(now_unix, n=128)` returns a
      polyline of TEME positions sampled across one period.
- [x] Vector-pipeline draw call for the trail using a
      satellite-specific colour. Trail follows the selected
      satellite (M4).
- [x] Done-when: selecting the ISS draws a sinusoidal ground
      track visible from the globe view.

### M4 — Hover + click + identify (UI-orbit-identify)

- [x] Second colour attachment in the orbit pass: instance
      index encoded as u32.
- [x] Native + web pointer handler reads the pick texture at the
      cursor position; debounced to one read per ~50 ms.
- [x] Hover tooltip surfaces `name + category + altitude (km)`
      via a small DOM overlay (web) / logged (native).
- [x] Click: select satellite, kick off trail draw, surface
      details panel. Click on empty space: deselect.
- [x] Done-when: a user can hover any visible satellite and read
      its NORAD name in under one frame.

## Open questions

- **TLE refresh cadence.** Celestrak updates several times per
  day. Refresh hourly? Daily? On user demand? Default: fetch on
  startup, re-fetch on each category-toggle, no background loop
  in v1. Revisit if the demo runs unattended.
- **Propagation cost at 10 k+ satellites.** sgp4-rs benchmarks at
  ~500 ns per propagation on a Zen3 core. 10 k × 500 ns = 5 ms
  per frame = doable. If it tips over: propagate every N frames
  + linearly interpolate position, or use the orbital-period
  parameterisation directly (only good for circular orbits).
- **CRS for the satellite shell.** TEME isn't a body-fixed
  frame. The single Z-rotation by GMST approximates ECEF to ~5 km
  at the orbit altitude; for orbital-track visualisation that's
  fine. For precision pointing (e.g. a "is the ISS above my
  city right now" query that survives a hundred days), a proper
  TEME → ITRF / ICRF chain is needed. Note in the README; full
  fix is a separate plan.
- **Mars / Moon orbit visualisation.** SGP4 is Earth-only. The
  satellite overlay should be hidden when the active body isn't
  Earth (same shape as the search bar in plan 0003 M4).

## Done when

- Live demo at `timthirion.github.io/aeGIS` shows the ISS as a
  moving dot crossing the globe in real time.
- Toggling Starlink on adds ~6 000 dots; `Renderer::frame_stats()`
  (new helper, printed in console) reports `gpu_ms < 16.0` over
  a 60-frame sample on a M1 MacBook. The render-budget guard is
  visibly indicated in the UI when active (greyed category pill
  + tooltip "demoted — too many satellites").
- Selecting the ISS by hover surfaces `ISS (ZARYA) · 25544 ·
  Stations · 410 km` (NORAD id surfaced so identity stays stable
  across TLE refreshes); a unit test pins the NORAD-id-stable
  selection across a simulated refresh.
- The overlay hides when the active body switches to Mars / Moon
  (gated by `active_body == BodyId::Earth` in `dispatch_orbit_draw`,
  covered by an integration test that switches bodies).
- Native target works: `cargo run` shows the same ISS-from-fixture
  dot crossing the globe with no network access.
- M0 round-trip from `parse_tles → satellites_from_tles → propagate
  → teme_to_ecef → lonlat` for the bundled ISS fixture lands within
  1 km of the published Celestrak position at the fixture's epoch.
- All milestones pass `cargo test --all-targets`,
  `cargo clippy --all-targets -- -D warnings`, and
  `cargo check --target wasm32-unknown-unknown --lib`.

## Plan-skeptic attacks addressed

The initial draft was attacked by `plan-skeptic` on 2026-06-10.
Strongest attacks + their resolution:

1. **Native target had no TLE source** — fixed: native fetches
   from Celestrak (same CORS-clean URL), with a bundled fixture
   for the no-network case. The done-when explicitly requires
   `cargo run` to produce the visible ISS.
2. **Selection by buffer index would mis-identify after a TLE
   refresh** — fixed: identity is the NORAD catalog number;
   `satellites_by_norad: HashMap<u32, usize>` redirects on
   refresh. Test pinned.
3. **"Hide on non-Earth" was in Open questions, not a
   milestone** — moved to Done-when as a gated assertion.
4. **Render-budget guard would silently drop categories** —
   fixed: M2 now requires visible UI feedback when a category
   is demoted.
5. **GMST → TEME→ECEF approximation** — acknowledged: open
   question stays open; the visualisation tolerates the ~5 km
   error at orbit altitude, the README will document the
   limitation when this plan ships.
