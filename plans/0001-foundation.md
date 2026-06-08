# Foundation: from empty repo to embeddable slippy map (native + web)

- **Status:** proposed
- **Last updated:** 2026-06-08
- **Last touched on:** initial scaffolding pass

## Goal

Take the renderer from an empty repo to a working slippy map — pan, zoom,
raster basemap tiles, a vector overlay, and an attribution panel — that
runs both as a native desktop app and as an embeddable in-browser widget,
with the test harness to back up correctness claims (CRS round-trips,
tile-math correctness, format I/O fidelity). This is the foundation every
later phase builds on. By the end we can drop a live map into a web page,
overlay a GeoJSON feature collection, and the same code runs natively.

Built as ordered milestones (M0–M4). Each is independently shippable.

## Context

Empty Rust repository (just `.gitignore`, `LICENSE`, `README.md`, and the
just-landed `AGENTS.md` + `plans/` + `.claude/` scaffolding). No code yet.

Key decisions already made in `AGENTS.md`: `wgpu` + WGSL (native +
WebGPU), `winit` for the native window, the web build attaches to a host
`<canvas>` directly without winit's event loop (the multi-instance fix
quasi discovered the hard way — see quasi's plan 0001 M0 web architecture
note). Pure-Rust GIS libraries by default (`geo`, `geozero`, `rstar`,
`lyon`, `proj4rs`).

The single most important architectural commitment: **native and web stay
in lockstep.** Every milestone is "done" only when it works in both
targets (except the verification harness, which is native-only by nature).

### New dependencies introduced in this plan

(Updated as milestones land.)

- `wgpu` (Apache-2.0/MIT) — GPU API
- `winit` (Apache-2.0) — native windowing only
- `pollster` (Apache-2.0/MIT) — block on wgpu futures natively
- `wasm-bindgen` + `wasm-bindgen-futures` + `web-sys` + `js-sys` +
  `console_error_panic_hook` — web bindings
- `bytemuck` (Zlib/Apache-2.0/MIT) — POD ↔ bytes for GPU uploads
- `thiserror` (Apache-2.0/MIT) — typed error enums
- `geo` + `geo-types` (Apache-2.0/MIT) — vector geometry types (M3)
- `geozero` (MIT/Apache-2.0) — GeoJSON ingest (M3)
- `lyon` (MIT/Apache-2.0) — vector tessellation (M3)
- `image` (MIT/Apache-2.0) — tile decode (M2)
- `reqwest` (Apache-2.0/MIT) or `ehttp` for the tile fetcher
  (decide in M2; `ehttp` is wasm-friendlier)

## Design

### Module shape

- `core` (the crate's library) — owns the `wgpu`
  `Device`/`Queue`/`Surface`, the layer model, the CRS subsystem, the
  WGSL pipelines, and the frame loop. Platform-agnostic.
- `bin` native entry — `winit` window + `pollster` to init `wgpu`, feeds
  events (pan / zoom / wheel) into `core`.
- web entry (`lib`, `cdylib`) — `wasm-bindgen` exports that attach to a
  `<canvas>` and drive `core`; input via canvas pointer / wheel events;
  no winit on web (single-event-loop limitation).
- WGSL shaders in `.wgsl` files, included with `include_str!`.

### Tile pipeline (M2)

The slippy-map dataflow:

1. **Viewport → tile list:** given the camera's `(lon, lat, zoom)`,
   compute the set of `(z, x, y)` XYZ tile addresses currently visible.
2. **Tile fetch:** async fetch from an HTTP tile source (Phase 0 default:
   a permissive OSM-derived raster tile server with a clear attribution
   string). Cached in-memory by `(z, x, y)`.
3. **Tile decode + upload:** PNG/WebP decode → `wgpu::Texture` upload.
   Tiles are 256×256 by convention; format negotiated per source.
4. **Render:** for each visible tile, draw a textured quad at its
   Web-Mercator-projected position. Per-frame: a fullscreen pass renders
   all visible tiles to the swapchain.

The tile fetcher is the first asynchronous CPU-side system; design it
channel-driven (request-in, decoded-texture-out) so M2 puts the
"use the language" guidance to genuine use.

### Vector overlay pipeline (M3)

GeoJSON ingest → `geo::Geometry` → `lyon` tessellation → `wgpu` triangle
buffers. A `VectorLayer` owns its tessellated mesh + a styling struct
(fill RGBA, stroke RGBA, stroke width). Drawn after tiles in the same
pass, sharing the same Web Mercator projection uniform.

### CRS (M3, minimal)

Web Mercator (EPSG:3857) ↔ WGS84 lon/lat (EPSG:4326) is the only CRS
pair in this plan. Forward + inverse implemented inline in
`core::crs::mercator` — the math is small (the spherical-Mercator
formulas, not ellipsoidal) and the round-trip test pins it. `proj4rs`
gets wired in Phase 3 (a separate plan).

## Milestones

### M0 — Pixels native + web

- [ ] Cargo project: single package as both native bin and wasm `cdylib`
      + `rlib`. (Kept as one crate for M0; split into a workspace when
      the renderer grows.)
- [ ] `wgpu` init (adapter / device / queue / surface) — builds and
      links natively.
- [ ] Fullscreen-triangle pass drawing a recognisable gradient
      (`src/shaders/clear.wgsl`).
- [ ] `wasm-pack build --target web` succeeds; `index.html` attaches to
      a `#aegis-canvas` element and runs the same render.
- [ ] WGSL covered by a `naga` validation test in `tests/shaders.rs`
      (no GPU needed; runs in plain `cargo test`).

**Done when:** the gradient shows in both a desktop window and a browser
tab; `cargo test` + `cargo clippy --all-targets -- -D warnings` +
`cargo check --target wasm32-unknown-unknown --lib` all green.

### M1 — Camera + viewport + Web Mercator math

- [ ] `core::crs::mercator` — forward + inverse Spherical Mercator (the
      EPSG:3857 convention). Tests pin a round-trip grid over [-85°, 85°]
      latitude × [-180°, 180°] longitude to within `1e-9` tolerance.
- [ ] `core::tile` — XYZ tile math: `lonlat_to_tile(z, lon, lat) ->
      (x, y)`, `tile_to_lonlat_nw(z, x, y) -> (lon, lat)` (the
      north-west corner convention), `visible_tiles(camera, viewport)
      -> Vec<TileId>`. Tests cover the canonical fixtures (z=0 has
      one tile, z=1 has four, the tile containing 0°,0° at z=1 is
      `(1, 1)`, …).
- [ ] `core::camera` — pan + zoom + wheel state; produces a
      `Mat4` for the WGSL projection uniform.
- [ ] Input glue: native `winit` mouse drag → camera pan; web canvas
      pointer events → same.
- [ ] Render the visible-tile rectangle outlines as a debug overlay
      (no tile fetch yet — proves the tile-selection math).

**Done when:** pan and zoom interactively in both targets show the
correct visible-tile grid as wireframe overlays at every zoom level.

### M2 — Raster slippy map: tile fetch + render + attribution

- [ ] `core::tile::source` — `RasterTileSource` trait with one
      implementation: a permissive open-data OSM-derived tile server.
      Document the chosen endpoint + its attribution + its usage policy
      in the plan's Open Questions resolution.
- [ ] Channel-driven async tile fetcher (`reqwest` native /
      `wasm-bindgen` fetch web). In-memory LRU cache keyed by
      `(source, z, x, y)`.
- [ ] PNG/WebP decode → `wgpu::Texture` upload. Tiles are 256×256.
- [ ] `shaders/tile.wgsl` — textured-quad pass; one draw per visible
      tile; sub-pixel-correct positioning at any zoom.
- [ ] Attribution overlay: an HTML `<div>` (web) or `winit` window
      title bar entry (native) carrying the required attribution
      string. `attributionsFor(layer)` API placeholder.

**Done when:** drag a recognisable slippy map of Earth interactively
in both targets, with attribution rendered, tiles cached, and the
fetcher not blocking the render loop.

### M3 — GeoJSON overlay (vector layer)

- [ ] `core::layer::vector::VectorLayer` — owns a `Vec<geo::Geometry>`
      plus a `Style` (fill + stroke RGBA, stroke width).
- [ ] `core::io::geojson` — `geozero`-backed loader: `from_geojson_str(s)
      -> Result<VectorLayer, IoError>`. Round-trip test: load a fixture,
      serialise back out, parse again, assert geometry + property
      equality.
- [ ] `lyon`-based tessellation: polygons → triangle mesh; lines →
      stroked triangle mesh. Cached on the layer; re-tessellated only
      when the layer's style.stroke_width changes (zoom-independent for
      M3; Phase 7 styling fixes this).
- [ ] `shaders/vector.wgsl` — flat-shaded textured-or-untextured
      triangle pass, projected through the same Web Mercator matrix as
      M2's tile pass.
- [ ] Fixture: a small Natural Earth `countries` extract (public domain),
      shipped under `data/natural-earth/` with `ATTRIBUTION`.

**Done when:** a Natural Earth country polygon overlay renders correctly
on top of the M2 basemap in both targets; the GeoJSON round-trip test
passes; clicking on a polygon (M4) reads back the correct feature
attributes.

### M4 — Embeddable widget skeleton + verification harness

- [ ] Web entry: `create(host_id)` (default chrome — attribution panel,
      basemap dropdown, layer toggles) and `createHeadless(host_id)`
      (bare canvas; embedder provides UI). Mirrors quasi's pattern.
- [ ] `QuasiInstance`-equivalent setters: `setBasemap(source)`,
      `addLayer(geojson_str, style)`, `removeLayer(id)`,
      `panTo(lon, lat, zoom)`, `attributionsFor(layer)`, `onClick(cb)`.
- [ ] Native CLI: `cargo run -- map [--source URL] [--overlay PATH]`
      opens a desktop window with the same map, useful for ad-hoc
      manual testing.
- [ ] Verification harness (native-only):
  - `core::test::renders::render_to_image(scene_config) -> RgbaImage` —
    headless `wgpu` offscreen render, returns a deterministic image
    from a fixed `(camera, layers, basemap-mock)` config.
  - Reference fixtures under `data/reference/` (PNGs committed to the
    repo, kept small).
  - `image::diff` against reference within tolerance; flag drift.
- [ ] CSV harness for tile-math sweeps: `cargo run -- tile-grid
      --out grid.csv` enumerates visible tiles at a grid of
      camera positions for offline inspection.

**Done when:** the widget runs smoothly in a browser and is drop-in
embeddable in a static HTML page (the equivalent of quasi's
`index.html` demo) with at least three host containers showing
independent maps; native CLI works; reference-image harness has at
least one tracked fixture rendering at zero diff.

## Open questions

- **Tile source for M2:** OSM's CDN (`tile.openstreetmap.org`) explicitly
  disallows heavy production traffic. Acceptable for development; the
  default for `RasterTileSource` should probably be a Protomaps-rendered
  raster mirror or a free MapTiler key the user supplies via env. Resolve
  before M2 ships and document the chosen endpoint here.
- **Web HTTP client:** `reqwest` builds on wasm via the `wasm-bindgen`
  fetch backend, but `ehttp` is lighter. Decide in M2.
- **Tile cache eviction:** simple LRU sized in tile-count for M2; revisit
  when raster tiles get supplemented by vector tiles (Phase 4) and
  cache pressure grows.
- **GeoJSON property representation:** `serde_json::Value` keeps M3
  simple, but `geo`'s upstream is moving toward an Arrow-backed
  attribute model. Use `Value` for now; revisit when Arrow lands.
- **Reference-image tolerance:** zero-diff is unrealistic across drivers
  on M4. Pin a per-pixel tolerance (~1 LSB on RGB) and a per-image
  max-different-pixels budget; resolve when the harness first
  produces its baseline.

## Done when

- A draggable, zoomable Web-Mercator slippy map renders in both a native
  desktop window and an embedded browser canvas.
- A GeoJSON overlay (Natural Earth countries fixture) draws correctly on
  top of the basemap, with hit-testing returning the right feature.
- `cargo test` exercises CRS round-trips, tile math, GeoJSON round-trip,
  and at least one reference-image diff — all green.
- The web widget exposes a documented embedder API (`create`,
  `createHeadless`, the layer + camera setters, the click callback,
  the attribution helper).
- `cargo clippy --all-targets -- -D warnings`,
  `cargo check --target wasm32-unknown-unknown --lib`, and
  `cargo fmt --check` are clean.
