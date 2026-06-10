# Cloud-Optimized GeoTIFF raster layer

- **Status:** proposed
- **Last updated:** 2026-06-10
- **Last touched on:** drafted in the 0004–0013 batch; the
  per-pixel raster path that unlocks scene-overlay analytics

## Goal

Drop a Sentinel-2 (or any other) Cloud-Optimized GeoTIFF over
the basemap by URL: aeGIS reads the COG's IFDs over HTTP range
requests, fetches only the tiles the viewport needs, decodes
strips on-demand, and uploads to GPU textures. No download of a
multi-gigabyte scene; no separate tile server. The headline
artifact is a Sentinel-2 RGB composite over an area of interest,
sandwiched between the satellite basemap and the vector overlay.

Built as ordered milestones (M0–M3): COG metadata reader, COG
strip fetcher, GPU pipeline + per-zoom-level sampling, reprojection
(EPSG:32633 / UTM → display CRS).

## Context

What exists today (commits up to `bfc073a`):

- Raster tile pipeline (`tile.wgsl`) renders fixed-projection
  256×256 JPEG/PNG tiles from per-tile URL templates. COGs are
  different: one big file, internal tile grid, requires range
  requests + IFD parsing.
- No GeoTIFF reader in the codebase. No `proj4rs` usage yet —
  Phase 3 deferred CRS work; this plan needs at least UTM →
  WGS84 / Web Mercator forward + inverse for the M3 reprojection
  step.
- `net::fetch_bytes_range` lands in plan 0005 M0 as a sibling of
  `fetch_bytes_*`; this plan consumes the same helper.

### New dependencies introduced in this plan

- [`tiff`](https://crates.io/crates/tiff) (MIT) — pure-Rust TIFF
  reader. Handles big-tiff, bit depths up to 32-bit float, and
  most modern COG variants. Works on wasm32.
- [`proj4rs`](https://crates.io/crates/proj4rs) (MIT/Apache-2.0)
  — pure-Rust port of PROJ's classic transforms. The lower bar
  for M3 (UTM ↔ WGS84) is well-covered by it; complex CRS chains
  remain a Phase 3 / future plan.
- No new HTTP plumbing.

### Data sources

- **AWS Open Data Sentinel-2 L2A COG** bucket
  (`s3://sentinel-cogs/`, served HTTPS at
  `sentinel-cogs.s3.us-west-2.amazonaws.com`). Public, CORS
  confirmed, no key needed. Each scene is ~50 MB of COGs across
  the bands; an RGB composite reads three.
- License: Sentinel data is free, redistributable, attribution to
  the European Space Agency / Copernicus.

## Design

### COG metadata reader (M0)

`src/cog.rs`:

```rust
pub struct Cog {
    url: String,
    ifds: Vec<Ifd>,             // per-overview pyramid level
    crs: Crs,                   // EPSG code from GeoKeys
    geotransform: [f64; 6],     // GDAL convention
    no_data: Option<f64>,
    band_count: u16,
    sample_format: SampleFormat,
}

impl Cog {
    pub async fn open(url: &str) -> Result<Cog, CogError>;
    /// Read a single internal tile (`block_x`, `block_y`) from
    /// IFD `level`. Cached at the `Cog` layer — same tile twice
    /// is one HTTP GET.
    pub async fn read_tile(&self, level: usize, block_x: u32, block_y: u32)
        -> Result<DecodedRaster, CogError>;
}
```

The reader does **header-only** range requests up front (~16 KB
gets us through the IFD chain even for big files); tile fetches
happen on demand sized exactly to the strip's byte range. The
`tiff` crate is fed with a custom `Read + Seek` impl backed by a
byte-range cache.

### Per-zoom sampling (M2)

For a COG with `N` overview levels:

- At display zoom `z`, pick the overview whose ground-sample-
  distance is closest to `1 / pixels_per_world(z)`. One level
  below the camera zoom is the right default — slight
  over-sampling beats blocky undersample.
- For the chosen level, walk the COG's internal tile grid to
  find tiles intersecting the visible world rect.
- Fetch + decode + upload as RGBA textures. The existing tile
  cache layer (`tiles` HashMap, but keyed by COG ID + (level,
  block_x, block_y)) is the model.

### Reprojection (M3)

Source CRS lands from GeoKeys (EPSG:32633 for many Sentinel-2
scenes — UTM zone 33N). Display CRS is Web Mercator.

Two options:

1. **Reproject on the GPU:** the COG vertex shader does per-pixel
   UTM → WGS84 → Mercator → world coords. Heavier shader; no
   intermediate buffer.
2. **Reproject on the CPU at tile-decode time:** rasterise each
   COG tile into a Mercator-aligned buffer before upload. Heavier
   CPU; one path on the GPU.

v1 picks **(2)** — simpler shader, easier to debug. (1) is a
follow-up.

### Layer model

A new `RasterOverlay` field on the renderer. The widget API
gains `add_raster_overlay(url, opacity)` and
`remove_raster_overlay(id)`. The overlay renders between the
basemap tiles and the vector overlay.

## Milestones

### M0 — COG metadata reader (FMT-cog-meta)

- [ ] `src/cog.rs` opens a COG and exposes IFDs + geotransform +
      CRS.
- [ ] Integration test (network, `#[ignore]`): point at a public
      Sentinel-2 L2A scene, assert IFD count == 5 (the standard
      COG overview pyramid), band count == 1 per RGB band.
- [ ] Unit test with a hand-built COG fixture (~100 KB) checked
      into `tests/fixtures/cog/` for offline CI.

### M1 — Tile fetcher (FMT-cog-fetch)

- [ ] `Cog::read_tile(level, bx, by)` returns a decoded RGBA
      raster.
- [ ] Per-tile cache; second read of the same tile is a no-op.
- [ ] Native + web parity: same tile fetched on both lands
      pixel-identical.

### M2 — Render pipeline (MAP-cog-render)

- [ ] `cog.wgsl` — same tile-as-quad shape as `tile.wgsl`, with
      a per-COG `opacity: f32` uniform.
- [ ] Per-zoom level picker described above.
- [ ] `Renderer::add_cog_overlay(url, opacity) -> CogOverlayId`,
      `remove_cog_overlay(id)`.
- [ ] Done-when: a Sentinel-2 L2A scene overlaid on Chicago at
      z=10 renders correctly aligned to the Carto basemap below.

### M3 — Reprojection (CRS-utm-to-mercator)

- [ ] `proj4rs` wired with a project-wide CRS registry. Lookup
      by EPSG code returns the forward + inverse transforms.
- [ ] CPU reprojection at tile-decode time: source UTM block →
      Mercator-aligned RGBA buffer.
- [ ] Round-trip test: a known `(lon, lat)` round-trips
      through `Mercator → UTM33N → Mercator` to within 1e-6° on
      a 360 × 170 grid.
- [ ] Done-when: a Sentinel-2 RGB composite scenes-aligned to
      vector country outlines is visually consistent (manual
      reference shot in `tests/visual/sentinel-2-composite.png`).

## Open questions

- **Per-pixel reprojection vs CPU pre-warp.** (1) is more
  precise; (2) is simpler. We default to (2); if visible
  alignment drift appears at low zoom or far from the UTM
  zone's central meridian, we revisit.
- **GeoTIFF compression schemes.** The `tiff` crate covers
  LZW + Deflate; Sentinel-2 L2A COGs use Deflate, so we're
  covered. Less common (JPEG, WebP-in-TIFF) deferred to a
  follow-up.
- **What's the "show me a scene" UX?** Drop a URL in the search
  bar? A dedicated layers panel? Out of scope for v1; the
  widget API method is the contract. Live demo includes a single
  hardcoded scene over a hardcoded AOI.

## Done when

- A user can call `instance.addCogOverlay(url, 0.7)` from the
  page JS and see a Sentinel-2 scene composited over the
  basemap.
- The overlay aligns to ±1 pixel with the vector country-outline
  layer at z=10.
- Frame budget stays under 16 ms with the overlay active over a
  4 × 4 visible-tile span.
- The Sentinel-2 RGBA composite pipeline is **explicit in M2**:
  three single-band GeoTIFF reads → 16-bit reflectance → 8-bit
  gamma-stretched + no-data masking → one RGBA texture upload.
  A unit test with a hand-crafted 3-band fixture round-trips the
  composite path so the silent "we forgot how to combine bands"
  failure is caught before deploy.
- Web wasm decode of the bundled fixture COG runs under
  `wasm-pack test --headless --chrome` and produces the same
  bytes the native decode produces — the load-bearing parity
  test the original draft was missing.
- Attribution string `Imagery: Copernicus Sentinel data {year}`
  surfaces in the footer when a Sentinel COG overlay is active
  (test pinned).
- All milestones pass `cargo test --all-targets`,
  `cargo clippy --all-targets -- -D warnings`, and
  `cargo check --target wasm32-unknown-unknown --lib`.

## Plan-skeptic attacks addressed

Initial draft attacked on 2026-06-10. Strongest attacks + resolution:

1. **`tiff` crate on wasm32 was asserted, never proved** —
   fixed: M1 done-when adds a `wasm-pack test --headless` gate
   that decodes the bundled fixture in-browser.
2. **Sentinel single-band → RGBA path was hidden** — fixed:
   M2 owns the explicit composite pipeline; Done-when calls it
   out.
3. **CORS on the AWS bucket was asserted without a date** —
   fixed: M0 includes a deployed-origin CORS test.
4. **Attribution not wired** — fixed: footer surface required
   in M3 done-when.
5. **proj4rs registry is a project-wide subsystem disguised as
   a milestone bullet** — acknowledged: M3 limits the registry
   to UTM zones 1–60 + WGS84 + Web Mercator; full multi-CRS
   registry is a follow-up plan.
6. **Round-trip test is symmetric and catches nothing
   substantive** — fixed: M3 adds an explicit forward-only
   test against `proj4rs::Proj` for a known UTM33N point,
   asserting agreement within 1 cm.
7. **Plan 0006 depends on plan 0005 M0's `fetch_bytes_range`
   which doesn't ship until 0005** — acknowledged: this plan
   is *blocked on* plan 0005 M0. The plan order is
   non-negotiable.
