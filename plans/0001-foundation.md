# Foundation: from empty repo to embeddable slippy map (native + web)

- **Status:** active
- **Last updated:** 2026-06-08
- **Last touched on:** M3 vector overlay — Natural Earth countries
  drawn as alpha-blended line list over the basemap, projecting through
  a per-frame camera uniform that's the swap point for the globe view

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

### M1 — Camera + viewport + Web Mercator math ✅ DONE

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
- [x] `Camera::visible_tiles(canvas) -> Vec<TileId>` — selects tiles
      at `round(zoom)`, clamps indices to `[0, n-1]` per axis. Tests
      pin "z=0 has exactly one tile" + "Chicago z=10 800×600 viewport
      contains the Chicago tile and a reasonable [4, 16] count."
      **Limitation:** antimeridian-wrap not handled — documented as
      deferred to a wrap-aware iteration.
- [x] `core::camera` — `pan(dx, dy)` (mouse-drag convention),
      `zoom_at(delta, cursor, canvas)` (wheel-around-cursor; the
      world point under the cursor stays pinned). `tile_ndc_rect`
      gives the renderer per-tile screen quads at any fractional
      zoom. The `Mat4` projection uniform turned out unnecessary —
      the renderer reads `tile_ndc_rect` directly per tile.
- [x] Input glue: native `winit` left-mouse-drag → `camera.pan`,
      mouse-wheel → `camera.zoom_at`. Web pointer events
      (`pointerdown`/`move`/`up`/`cancel`/`leave`) → same, with
      `WheelEvent.delta_y` inverted so scroll-down = zoom-out
      (matches every browser map's convention).
- [ ] ~~Render the visible-tile rectangle outlines as a debug overlay
      (no tile fetch yet — proves the tile-selection math).~~ Skipped:
      M1/M2 lands the visible-tile-selection + actual-tile-rendering
      together (the wireframe debug overlay would only have been
      useful before fetches worked, which is no longer the state we
      pass through).

**Done when:** pan and zoom interactively in both targets show the
correct visible-tile grid. _Confirmed buildable 2026-06-08: native +
wasm clean, 22 tests passing, wasm bundle 312 KB after wasm-opt.
**Visual confirmation pending** — embedder should drag + scroll on
the live URL to confirm the interaction feels right._

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
      shipped in M1.5.
- [x] PNG decode → `wgpu::Texture` upload — generalised from M1.5's
      one-tile path to a keyed `HashMap<TileId, TileBinding>` in the
      multi-tile renderer (M1/M2 commit).
- [x] `shaders/tile.wgsl` textured-quad pass — generalised to take a
      per-tile NDC-rect uniform; one draw call per visible tile.
- [x] Channel-driven multi-tile async fetcher — `Renderer` owns an
      `mpsc::channel<(TileId, Result<DecodedTile, String>)>`; each
      visible tile not yet loaded triggers a background fetch
      (`std::thread::spawn` native / `wasm_bindgen_futures::spawn_local`
      web) whose closure posts the decoded result back. `requested`
      HashSet de-dupes in-flight requests. `drain_completed_fetches`
      uploads results each frame.
- [ ] LRU eviction by tile-count — deferred. Tiles accumulate
      indefinitely for now; at zoom 10 over the metro Chicago area
      the working set is small (~15 tiles), and even pan-around-the-
      city stays well under a few hundred. Eviction lands when M3
      (vector overlay) increases per-tile memory or when zoom-in /
      zoom-out behaviour produces growth across multiple zoom levels.
- [ ] `core::tile::source` — `RasterTileSource` trait abstracting
      the URL builder; OSM is one impl, Protomaps another (Phase 4).
      Deferred until the second source actually exists.
- [ ] Attribution overlay: an HTML `<div>` (web) or `winit` window
      title bar entry (native) carrying the required attribution
      string. `attributionsFor(layer)` API placeholder.

**Done when:** drag a recognisable slippy map of Earth interactively
in both targets, with attribution rendered, tiles cached, and the
fetcher not blocking the render loop.

### M3 — GeoJSON overlay (vector layer)

- [x] `core::vector::VectorLayer` — pre-projected `Vec<[f32; 2]>` in
      normalised Mercator world coords, laid out for a wgpu `LineList`.
      Pure data + a single owned vertex buffer; the `Style` / per-
      layer attribute work is deferred to Phase 7 (styling system) so
      M3 stays focused on getting pixels on screen.
- [x] `core::vector::load_geojson_lines` — `geojson`-backed loader
      walking `LineString` / `MultiLineString` / `Polygon` /
      `MultiPolygon` / `GeometryCollection`. Each `(lon, lat)` projects
      through Spherical Mercator at load time, so the GPU just sees
      world coords. Tests pin: empty FC, N-point linestring → N-1
      segments, polygon ring auto-closure (with + without explicit
      last-coord-duplicate), point-types skipped, equator/prime →
      world centre (0.5, 0.5).
- [ ] ~~`lyon`-based tessellation~~ — deferred. LineList + 1-px lines
      give a clean country-outline look for M3; lyon-stroked thick
      lines / polygon fills land when Phase 7 introduces the styling
      system that actually controls stroke weight + fill colour.
- [x] `shaders/vector.wgsl` — per-vertex projection through a
      `VectorCameraUniform { world_center, pixels_per_world,
      canvas_half, color }`. **This is the projection point that
      swaps when the globe view lands** — same vertex data, new
      shader math interpolating between flat Mercator NDC and
      ellipsoidal-globe NDC by a `globeness` uniform.
- [x] Fixture: Natural Earth 110m countries
      (`data/natural-earth/countries.geojson`, 712 KB, public domain),
      with `ATTRIBUTION.md` covering provenance + refresh command.
- [x] Renderer wiring: separate `vector_pipeline` (LineList topology,
      `BlendState::ALPHA_BLENDING`), one shared bind-group with the
      per-frame `VectorCameraUniform`. Drawn last so the overlay
      sits on top of the basemap tiles.
- [x] Loading: native reads `data/natural-earth/countries.geojson`
      from the working directory at startup (best-effort — logs a
      warning if missing rather than panicking). Web fetches it via
      `web_sys::fetch` once the canvas is attached; arrival is
      asynchronous so the basemap loads first.

**Done when:** country borders render in coral-orange over the basemap
in both targets; pre-flight green; the wasm bundle stays under 500 KB.
  _Build-verified 2026-06-08: native + wasm clean, 31 lib tests + 3
  shader tests passing, wasm bundle 408 KB after wasm-opt.
  **Visual confirmation pending** — embedder should reload the live
  URL and zoom out to see the country outlines materialise as the
  basemap zoom range supports the 110m feature scale._

**What M3 deliberately doesn't ship** (per the scope-trim above):
- Filled polygons (just outlines for now)
- Hover / click feature interrogation (M4 work — needs the embedder
  API surface to expose it)
- Multiple layers, per-layer styling, vector-tile sources (Phase 4 / 7)
- Tessellation via `lyon` (deferred until styling needs thick strokes)

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
