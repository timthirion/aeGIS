# Foundation: from empty repo to embeddable slippy map (native + web)

- **Status:** active
- **Last updated:** 2026-06-08
- **Last touched on:** M1.5 "first tile" — Chicago at z=10 fetched from
  OSM + rendered via the textured-quad pass in both targets

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

### M0 — Pixels native + web ✅ DONE

- [x] Cargo project: single package as both native bin and wasm `cdylib`
      + `rlib`. (Kept as one crate for M0; split into a workspace when
      the renderer grows.)
- [x] `wgpu` init (adapter / device / queue / surface) — builds and
      links natively. Implementation in `src/render.rs`; native driver
      in `src/lib.rs::run` (winit 0.29 event loop, `Arc<Window>` for
      the `Surface<'static>` lifetime), web driver in `src/web.rs`.
- [x] Fullscreen-triangle pass drawing a recognisable gradient
      (`src/shaders/clear.wgsl`).
- [x] `wasm-pack build --target web` succeeds (104 KB wasm after
      `wasm-opt`); `index.html` attaches to a `#aegis-host` element
      via `start(host_id)` and runs the same render. A
      `ResizeObserver` re-syncs the backing-store size on layout
      changes; an `rAF` loop drives `Renderer::render()` per frame.
- [x] WGSL covered by a `naga` validation test in `tests/shaders.rs`
      (no GPU needed; runs in plain `cargo test`).

**Done when:** the gradient shows in both a desktop window and a browser
tab; `cargo test` + `cargo clippy --all-targets -- -D warnings` +
`cargo check --target wasm32-unknown-unknown --lib` all green.
  _Build-verified 2026-06-08: native `cargo run` builds + spawns the
  window cleanly; `wasm-pack build --target web` clean; CI green;
  Pages deploy live at https://timthirion.github.io/aeGIS/.
  **Visual confirmation pending** — I can't drive a real browser /
  desktop window from this environment; the bindings + bundle build
  correctly but the embedder should open both targets once to confirm
  the gradient renders as expected._

**winit / wgpu version pin:** the renderer is on `winit 0.29` +
`pollster 0.3` (matches quasi's validated set). winit 0.30's
`ApplicationHandler` rework is the future but introduces complexity
on the wasm path for no incremental M0 benefit; bump deliberately
in a separate plan if/when needed.

**wgpu 29 gotchas worth pinning:**
- `Surface::get_current_texture()` returns `wgpu::CurrentSurfaceTexture`
  (enum: Success / Suboptimal / Outdated / Lost / Timeout / Occluded /
  Validation) — not `Result`. Reconfigure on Outdated/Lost.
- `RenderPassDescriptor` + `RenderPipelineDescriptor` carry
  `multiview_mask`, not `multiview`.
- `PipelineLayoutDescriptor` takes `immediate_size`, not
  `push_constant_ranges`.
- `Instance::new` takes the descriptor **by value**; the descriptor
  uses `::new_without_display_handle()` as its base for cross-target
  compatibility.

### M1 — Camera + viewport + Web Mercator math

- [x] `core::crs` — forward + inverse Spherical Mercator (`lonlat_to_world`
      / `world_to_lonlat`) + tile-coordinate math
      (`lonlat_to_tile_fractional` / `tile_to_lonlat_nw`). Tests pin a
      round-trip grid over [-180°, 180°] × [-85°, 85°] to within
      `1e-9` (37×171 = 6 327 points). Latitude clamps at
      `±85.05112877980659°` so the projection never returns ±∞.
- [x] `core::tile` — `TileId { z, x, y }` with `Copy + Eq + Hash`
      (LRU-key-ready). `TileId::from_lonlat` clamps to `[0, 2^z)`.
      Canonical fixtures pinned: z=0 has one tile, z=1's four
      quadrants around (0°, 0°) each fall into their expected tile,
      Chicago at z=10 = `(10, 262, 380)` and the corresponding OSM
      URL is `tile.openstreetmap.org/10/262/380.png`.
- [ ] `visible_tiles(camera, viewport) -> Vec<TileId>` — depends on
      the camera (next sub-item).
- [ ] `core::camera` — pan + zoom + wheel state; produces a
      `Mat4` for the WGSL projection uniform.
- [ ] Input glue: native `winit` mouse drag → camera pan; web canvas
      pointer events → same.
- [ ] Render the visible-tile rectangle outlines as a debug overlay
      (no tile fetch yet — proves the tile-selection math).

**Done when:** pan and zoom interactively in both targets show the
correct visible-tile grid as wireframe overlays at every zoom level.

### M1.5 — First tile (the "we have a map" stop) ✅ DONE

A scoped subset of M1+M2 the user explicitly asked for as a visible
checkpoint: fetch **one** raster tile centred on Chicago, render it
as a fullscreen quad with aspect-correct letterboxing. No camera, no
viewport math — just "the OSM tile of Chicago appears in the canvas."

- [x] `tile::fetch_tile_blocking` (native, `ehttp`) +
      `tile::fetch_tile_async` (web, `web_sys::fetch` +
      `wasm_bindgen_futures::spawn_local`) — see notes below on why
      ehttp's web path was dropped.
- [x] `tile::decode_png` — pure-Rust PNG decode via `image` crate's
      `png` feature (works on both targets).
- [x] `Renderer::set_tile(width, height, rgba)` — uploads the bytes
      as an `Rgba8UnormSrgb` texture, builds a fresh bind group,
      replaces the current tile. Idempotent; safe to call repeatedly.
- [x] `shaders/tile.wgsl` — textured fullscreen-triangle pass with
      an aspect-correction uniform so the 256×256 tile stays square
      regardless of canvas aspect (pillarbox / letterbox).
- [x] Surface format switched to **sRGB** so PNG-sourced sRGB bytes
      render with correct gamma end-to-end (GPU's auto sRGB→linear
      on texture read + linear→sRGB on surface write cancel out).
- [x] OSM tile source: `https://tile.openstreetmap.org/{z}/{x}/{y}.png`
      with a `User-Agent: aegis/0.0.1 (https://github.com/timthirion/aeGIS)`
      header (required by OSM's tile usage policy). Verified the
      response HTTP 200 + `Content-Type: image/png` via `curl` with
      the same header.
- [x] Naga validation for `tile.wgsl` in `tests/shaders.rs`.

**Done when:** the live Pages URL shows the Chicago metro at zoom 10,
letterboxed inside the viewport; `cargo run` shows the same tile in
a native window.
  _Build-verified 2026-06-08: native + wasm both clean, tests green,
  shader validation green. **Visual confirmation pending** — embedder
  should reload the live URL once the deploy lands and `cargo run`
  locally to confirm._

**Why ehttp on native but `web_sys` directly on web:** ehttp 0.5's
`fetch` requires a `Send + 'static` callback, which doesn't compose
with our `Rc<RefCell<Inner>>` web-state shape. On wasm we have a
single-threaded JS runtime, so `Send` is API-imposed bondage with no
upside. `web_sys::fetch_with_str` + `JsFuture` + `spawn_local` gives
the same fetch behaviour with no `Send` requirement, in ~20 lines.

**wgpu 29 deltas worth pinning in plan history:**
- `wgpu::SamplerDescriptor::mipmap_filter` is `MipmapFilterMode`
  (not `FilterMode` — drift from older wgpu majors).
- `PipelineLayoutDescriptor::bind_group_layouts` is `&[Option<&BGL>]`
  (the Option wrapper supports unbound group slots).

### M2 — Raster slippy map: tile fetch + render + attribution

- [x] `tile::TileId::osm_url` + `OSM_USER_AGENT` constant. OSM endpoint
      chosen as the M2 source for dev/low-volume traffic only; the
      Protomaps PMTiles path lands in plan 0004 (Phase 4).
- [x] Single-tile blocking fetch (native) + async fetch (web) —
      shipped in M1.5. The channel-driven multi-tile version comes
      next.
- [x] PNG decode → `wgpu::Texture` upload — shipped in M1.5 for one
      tile.
- [x] `shaders/tile.wgsl` textured-quad pass — shipped in M1.5 for
      one tile. The "one draw per visible tile" generalisation comes
      with the camera (M1 next).
- [ ] `core::tile::source` — `RasterTileSource` trait abstracting
      the URL builder; OSM is one impl, Protomaps another (Phase 4).
- [ ] Channel-driven multi-tile async fetcher with in-memory LRU
      cache keyed by `(source, z, x, y)`.
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

- **Tile source for M2:** ✅ Resolved 2026-06-08. M1.5 uses OSM's CDN
  (`tile.openstreetmap.org`) with the project-identifying `User-Agent:
  aegis/0.0.1 (https://github.com/timthirion/aeGIS)`. Acceptable for
  the dev/demo traffic from a single Pages URL; production-scale
  traffic moves to self-hosted Protomaps PMTiles in plan 0004
  (Phase 4) before we even risk crossing OSM's threshold.
- **Web HTTP client:** ✅ Resolved 2026-06-08. Native uses `ehttp`
  (which wraps `ureq`); web uses `web_sys::fetch` + `JsFuture` +
  `spawn_local` directly because ehttp's web path requires a
  `Send + 'static` callback that doesn't compose with the
  `Rc<RefCell<Inner>>` web-state shape (M1.5 notes).
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
