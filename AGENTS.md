# AGENTS.md

Guidance for AI agents working on this repository.

## Project Goal

aeGIS is an open-source GIS — a geographic information system you can pan,
zoom, query, and analyze entirely in a web browser. The end goal is a
self-hostable, embeddable, scriptable GIS whose data model and rendering are
shared between the desktop and the browser from one Rust codebase.

This repository is the **Rust** implementation. Its distinctive purpose: run
in the browser via WebAssembly (`wasm-pack`) so a hosted map can be embedded
in any page as a **live, interactive widget** — pan a Web Mercator tile map,
overlay a GeoJSON layer, query a feature — and the same code drives a native
desktop app for power-user workflows. Native and web are first-class
co-equal targets.

See `plans/ROADMAP.md` for direction and `plans/` for current,
machine-portable plans.

## Tech Stack

### Renderer + windowing (mirrors `quasi`)

- **Language:** Rust (edition 2021).
- **GPU:** [`wgpu`](https://wgpu.rs) — the renderer codes against **one API
  surface: WebGPU**, and one shading language: WGSL. `wgpu` *implements*
  WebGPU and maps it down to a native backend (Metal/Vulkan/DX12)
  automatically, or to WebGPU in the browser.
- **Shaders:** WGSL.
- **Windowing/input:** `winit` (native window); the web build attaches to
  a host `<canvas>` directly without winit's event loop.
- **Web packaging:** `wasm-bindgen` + `wasm-pack`.

### GIS data + algorithms (pure-Rust by default — wasm portability)

These are the libraries aeGIS commits to as its baseline. Adding a new
dependency outside this list earns a one-paragraph note in the introducing
plan's "Context" section.

- **Vector geometry:** [`geo`](https://github.com/georust/geo) +
  [`geo-types`](https://github.com/georust/geo) — types, predicates,
  algorithms (buffer, simplify, intersect, area).
- **Spatial index:** [`rstar`](https://github.com/georust/rstar) — pure-Rust
  R-tree, works on wasm.
- **Tessellation:** [`lyon`](https://github.com/nical/lyon) — vector → GPU
  triangles for polygon fills and stroked lines.
- **Format I/O:**
  - [`geozero`](https://github.com/georust/geozero) — the swiss-army knife;
    zero-copy readers for GeoJSON, FlatGeobuf, GeoPackage, MVT, GeoArrow.
  - [`geojson`](https://github.com/georust/geojson),
    [`flatgeobuf`](https://github.com/flatgeobuf/flatgeobuf),
    [`shapefile`](https://github.com/tmontaigu/shapefile-rs),
    [`gpx`](https://github.com/georust/gpx),
    [`osmpbf`](https://github.com/b-r-u/osmpbf) — format-specific paths
    when `geozero`'s universal reader isn't a fit.
  - [`mvt`](https://github.com/maplibre/maplibre-rs) for Mapbox Vector
    Tiles, [`pmtiles`](https://github.com/protomaps/PMTiles) for the
    PMTiles single-file vector-tile format (the primary self-hosted-
    basemap path).
- **Coordinate reference systems:**
  - [`proj4rs`](https://github.com/3liz/proj4rs) — pure-Rust port of PROJ;
    covers most production EPSG codes; **works on wasm**. This is the
    default.
  - [`proj`](https://github.com/georust/proj) — bindings to the C PROJ
    library; native-only escape hatch for the long tail.
- **Raster:** [`tiff`](https://github.com/image-rs/image-tiff),
  [`image`](https://github.com/image-rs/image), and the COG (Cloud
  Optimized GeoTIFF) reader path (TBD: build on `tiff` or pull in a
  dedicated COG crate).
- **Text rendering (cartographic labels):**
  [`glyphon`](https://github.com/grovesNL/glyphon) or
  [`cosmic-text`](https://github.com/pop-os/cosmic-text) — GPU text via
  `wgpu`, the hard part of any map renderer.

### Async + parallelism

`pollster` to block on `wgpu` futures natively; `wasm-bindgen-futures` on
web. CPU-side parallelism via `rayon` where it's a natural fit (parallel
tile decode, parallel reprojection, parallel index build). See "Use the
language" below.

## Scope: single API, no backend abstraction

This is a deliberate divergence worth stating plainly: **aeGIS (Rust) is
WebGPU-only at the API level, and has no GPU-backend abstraction layer.**
We write WebGPU/WGSL once and let `wgpu` choose the native backend.

- This is *not* "Metal-only" or "Vulkan-only" — running natively, `wgpu`
  still talks to Metal under the hood on macOS, Vulkan on Linux, etc. We
  just never write those APIs; the abstraction is `wgpu`'s job, not ours.
- We do **not** build a pluggable multi-backend system. Targeting a single
  API with a single shading language is precisely what makes the same
  source drop into a web app as a WebAssembly widget — that one-source-to-
  browser story is the reason this implementation exists, and a backend
  abstraction would work against it.

## Free + open map data sources

aeGIS exists to consume open data. The defaults the project will document,
test against, and ship integration paths for:

- **OpenStreetMap (ODbL)** — the universal vector base. Bulk extracts via
  [Geofabrik](https://download.geofabrik.de/); query API via Overpass.
  Raster tiles via OSM's own CDN are **local-dev only** — OSM's tile-
  usage policy blocks deployed apps (the live GitHub Pages build saw
  CORS denials + 503s). For deployed/demo use the live app pulls Carto's
  Voyager OSM-derived basemap (`a.basemaps.cartocdn.com`) which serves
  CORS-enabled tiles with no API key; for production self-host via
  PMTiles (below).
- **Protomaps (open data; MIT software, ODbL data)** — global vector
  basemap distributed as PMTiles. The recommended path for production
  vector basemaps because it self-hosts as a single file with no tile
  server.
- **Natural Earth (public domain)** — country / state / river / city
  vector + raster basemap data. Small enough to ship in-repo as test
  fixtures.
- **NASA Visible Earth — Blue Marble (public domain)** — global
  equirectangular Earth imagery. Bundled at 1024×512 (≈450 KB) as the
  always-on globe-view background; the basemap tiles overdraw it in
  their region.
- **USGS, NASA Earthdata, Copernicus Sentinel, NOAA** — open satellite
  imagery, DEMs, bathymetry, weather. Mostly accessed via STAC catalogs.
- **STAC** (SpatioTemporal Asset Catalog) — the open spec for indexing
  geospatial assets; the universal "where do I find recent imagery of X"
  layer.

**Attribution discipline.** Every basemap source carries a license string
that the UI must surface. OSM-derived layers require `© OpenStreetMap
contributors`; Protomaps adds itself; Natural Earth doesn't require it but
we credit anyway. The widget API must expose `attributionsFor(layer)` and
the default chrome must render it.

## Use the language

A secondary goal: exercise the breadth of Rust. When a design has multiple
reasonable shapes and one of them puts `async`, parallelism (`rayon`,
channels), traits, type-state, or lifetimes to genuine use, prefer that
shape — but **don't fake it**. A fundamentally sequential or GPU-bound
stage stays sequential. Architectural fit first; language breadth second.

This is CPU-side guidance. GPU work stays on the single WebGPU surface.

Concrete fits we expect:
- Async / channel-driven tile fetch + decode pipeline.
- `rayon` for parallel R-tree build and parallel reprojection of large
  vector layers.
- Type-state for the `LayerBuilder` pattern (the "you can't render a layer
  without a CRS" invariant becomes a compile-time guarantee).
- `thiserror` for a typed error hierarchy across the I/O / CRS / render
  boundaries.

## Build & Run

```bash
# Native
cargo run                              # desktop window
cargo test                             # unit tests
cargo clippy --all-targets             # lint
cargo fmt                              # format

# Web
wasm-pack build --target web           # produces pkg/ for the HTML harness
python3 -m http.server                 # then open http://localhost:8000/
```

Keep the native and web builds working in lockstep — a change that only
compiles natively is half-done. Guard platform-specific code with
`#[cfg(target_arch = "wasm32")]` / `#[cfg(not(target_arch = "wasm32"))]`.

## Architecture (intended)

- A core crate that owns the `wgpu` device/queue, the layer model, the
  CRS subsystem, and the WGSL rendering pipelines; platform-agnostic.
- Thin native (winit window) and web (canvas + `wasm-bindgen` exports)
  entry points that drive the core.
- A `data/` directory in the repo for small test fixtures (Natural Earth
  extracts, hand-rolled GeoJSON, single PMTiles tiles). Anything large
  (OSM regional extracts, satellite scenes) is fetched by `scripts/`
  on demand and gitignored.
- A `scripts/` directory for asset pipeline utilities that need a
  toolchain outside Rust (e.g. `pmtiles` CLI, `gdal_translate` for COG
  authoring). Each script ships with a README.

## Coding Style

- `rustfmt` defaults; keep `cargo clippy` clean (no warnings).
- snake_case items, CamelCase types, SCREAMING_SNAKE_CASE consts.
- Errors via `Result` with a typed error enum (`thiserror`); avoid
  `unwrap()` outside tests and clearly-infallible setup.
- Document public items with `///`; module overviews with `//!`.
- One responsibility per module; keep WGSL shaders in their own `.wgsl`
  files (include via `include_str!`) rather than inline string literals.
- All features must have automated tests where they can run off-GPU
  (CRS round-trips, geometry algorithms, format I/O, tile math).

## Testing

Automated tests are a **first-class, non-negotiable** priority. GIS bugs
are notoriously silent — a quarter-degree CRS offset, a swapped lon/lat,
a lost EPSG metadata field on round-trip — and the regression surface is
huge. The test suite is the project's defence.

**Rules of the road:**
- **No new module without tests.** Land code and its tests in the same
  change.
- **No drift from green.** `cargo test` (native) and `cargo check --target
  wasm32-unknown-unknown` both stay green at every commit. `cargo clippy
  --all-targets -- -D warnings` clean too.
- **Test what you can; document what you can't.** GPU pipeline validation
  needs hardware; say so explicitly and prefer landing a CPU-runnable
  regression alongside.

**Categories that have an obligatory test, in priority order:**
1. **CRS round-trips** — `EPSG:4326 ↔ EPSG:3857` (and back) over a grid of
   sample points. Symmetry to within float tolerance; documented epsilon.
   Lon/lat ordering is pinned by name, not by position, anywhere it
   crosses an API boundary.
2. **CPU↔GPU struct layout** — every uniform/storage struct used by WGSL
   gets a `size_of` / `offset_of` assertion. `vec3` forces 16-byte
   alignment; this class of bug fails only at runtime ("buffer too
   small") otherwise.
3. **WGSL parses + validates** — every `.wgsl` file is covered by a naga
   validation test (no GPU needed; runs in plain `cargo test`).
4. **Format I/O round-trips** — GeoJSON, FlatGeobuf, Shapefile,
   GeoTIFF, PMTiles tile reads. Property metadata + CRS metadata survives
   the round trip — this is the silent failure mode.
5. **Geometry math** — Web Mercator forward/inverse, tile XYZ ↔
   lon/lat/zoom, polygon area in known projections, R-tree neighbour
   queries against a known-answer fixture.
6. **Error paths** — every typed error variant gets a test that triggers
   it.

**Symptoms of suite rot to watch for:**
- A new file lands with zero tests "because it's just glue". Glue has
  bugs.
- Tests are commented out or marked `#[ignore]` "for now".
- A failing test gets its assertion loosened instead of the underlying
  behaviour fixed.
- The CRS round-trip suite skips an EPSG code we now ship in the UI.

## Data licensing discipline

Anything we ship in `data/` carries a `LICENSE` or `ATTRIBUTION` file
explaining its provenance and terms. Anything fetched at runtime against
an open service (OSM tiles, STAC catalogs) gets a runtime attribution
string surfaced in the UI. Plans that introduce a new data source must
document its license in the plan's "Context" section.

## Git Workflow

- Solo repo: commit directly to `main` and push freely.
- End commit messages with the standard Co-Authored-By trailer.
- Use `git mv` for moves/renames to preserve history.
