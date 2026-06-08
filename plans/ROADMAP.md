# aeGIS Roadmap (Rust)

## Mission

Build an open-source GIS that runs **entirely in a web browser** —
pan, zoom, query, overlay, analyze — driven by the same Rust codebase
that powers a native desktop app. Every feature is chosen to be
correct, measurable, and explainable; the project's defence against
the silent-bug-friendly GIS surface (lost CRS metadata, swapped
lon/lat, mis-projected tiles) is a test suite that exercises the
math first and the renderer second.

This Rust implementation has one defining constraint that shapes
everything: **it runs in the browser.** Via `wgpu` → WebGPU and
`wasm-pack`, the same GIS that drives a native desktop window also
drops into a web page as a **live, interactive widget**. A reader
can pan a Web Mercator basemap, toggle a GeoJSON overlay, click a
feature for attributes — without recompiling anything for a separate
web target. That interactivity is the differentiator and the reason
this implementation exists.

Design bias:
- **Correctness over features** — a result we can defend against a
  reference (a known geometry, a published projection, a fixture
  fixture render).
- **Measurability** — CRS round-trip error, tile-pixel accuracy,
  query latency are first-class.
- **One source, two targets** — native and web stay in lockstep;
  WebGPU is both the delivery vehicle and the rendering substrate.
- **Open data first** — OpenStreetMap, Natural Earth, Protomaps,
  STAC. Every basemap source we depend on is one we (or anyone) can
  self-host.

## Where we are today

Empty Rust repository scaffolded with project conventions
(`AGENTS.md`), the planning discipline (`plans/`), the agent +
skill scaffolding (`.claude/`), and an initial foundation plan
([`0001-foundation.md`](0001-foundation.md)). No code shipped yet.

## Plan + milestone conventions

See [`plans/README.md`](README.md) for the full convention. One
`plans/NNNN-*.md` per concrete piece of work, zero-padded and
globally incrementing (next free number: `0002`). Milestones use
track prefixes (`MAP-`, `CRS-`, `FMT-`, `IDX-`, `UI-`) +
semantic slugs.

## Phases

Phases are roughly ordered; boundaries are soft. Each becomes one
or more `plans/NNNN-*.md` as work starts.

### Phase 0 — Foundation: pixels on screen, native + web

`wgpu` device/queue, a fullscreen pass, and a render loop that
runs both in a native `winit` window and on an HTML canvas via
`wasm-pack`. Proves the dual-target pipeline before any GIS
complexity. ([Plan 0001](0001-foundation.md).)

### Phase 1 — A slippy map: Web Mercator + XYZ tiles

The canonical "you have a map" milestone. Implement Web Mercator
(forward + inverse), the XYZ tile addressing math, pan + zoom +
wheel input, an async tile fetcher (raster tiles from an open
basemap source), GPU upload + cache, and the per-frame tile
selector that picks visible tiles for the current viewport.

Publishable artifact: a draggable map of Earth in a browser,
attribution rendered in the corner, pulling raster tiles from a
permissive open source. ([Plan 0001](0001-foundation.md) M2.)

### Phase 2 — Vector data + GeoJSON overlay

GeoJSON ingest (via `geozero`), `geo` geometry types, vector →
GPU tessellation (`lyon`), and a layer model that lets a caller
drop a GeoJSON feature collection over the basemap. The first
test of the layer / styling system.

Publishable artifact: a Natural Earth country polygon overlay
rendered over the Phase 1 slippy map, with hover / click readout
of feature attributes.

### Phase 3 — CRS: proj4rs wired through the layer pipeline

`proj4rs` integrated so layers in non-Web-Mercator CRSes (e.g.
EPSG:4326 raw, EPSG:27700 UK National Grid, an Albers Equal Area
for a US-focused view) reproject correctly into the map's display
CRS. The CRS round-trip test suite is the safety net; a "lost
EPSG metadata on round-trip" regression catches as a test failure
long before it ships.

Publishable artifact: the same Natural Earth countries layer
re-rendered in Albers Equal Area, with the basemap (or a graticule)
correctly distorted for the projection.

### Phase 4 — Vector tiles: PMTiles + MVT

PMTiles single-file basemap reader; MVT decode + tessellation; the
ability to point the renderer at a `.pmtiles` file (locally or via
HTTP range requests) and get a self-hosted vector basemap with no
tile server.

Publishable artifact: a Protomaps vector basemap rendered live in
the browser from a single PMTiles file fetched over HTTP, with
styling rules that re-color roads and parks.

### Phase 5 — Raster: GeoTIFF + Cloud-Optimized GeoTIFF

Raster layer model; GeoTIFF reader; COG range-request fetcher;
GPU upload + sampling; reprojection (if source CRS ≠ display CRS).
Aimed at making it trivial to drop a Sentinel scene or a USGS DEM
over the basemap.

Publishable artifact: a Sentinel-2 RGB composite over an area of
interest, layered under the Phase 2 vector overlay, with the
basemap visible underneath.

### Phase 6 — Spatial index + queries

`rstar`-backed R-tree built on layer load, hit-testing for
click-to-identify, nearest-neighbour and bbox queries surfaced via
the widget API. Enables actually-interactive maps (not just
display).

### Phase 7 — Styling system

A declarative style spec (MapLibre-style JSON, or our own DSL —
TBD by an early plan in this phase) that drives layer rendering
parameters per zoom level. The point where aeGIS becomes a
recognisable mapping toolkit, not a hardcoded demo.

### Phase 8 — Embeddable widget API

Package the renderer with `wasm-pack` into the public widget API:
`create(host_id, opts)`, `setBasemap`, `addLayer`, `on(event,
callback)`, `attributionsFor(layer)`. The closing milestone of the
v1 trajectory — the surface a blog post or third-party app
actually consumes.

### Phase 9 — Globe view: flat → spherical zoom-out

The Google-Earth / MapLibre-globe affordance. Implement both a flat
Web Mercator vertex projection and an ellipsoidal (WGS84) globe
projection in WGSL, and **interpolate** between them based on the
current zoom level — continuous, single codepath, no separate "globe
mode." At zoom ≲ 5 the user sees a recognisable Earth; at zoom ≳ 7
they see flat Mercator; in between, the projection smoothly tweens.

Reference: MapLibre GL JS's globe-view implementation
(`globe-projection` PR + blog post). The trick is parameterising the
projection by a single `globeness ∈ [0, 1]` uniform driven by zoom.

Tile selection has to grow up too — at globe zoom, the visible
half-sphere implies a different set of tiles than a flat rectangular
viewport. Frustum culling against the ellipsoid + horizon-clipping
become real.

Publishable artifact: zoom continuously from a Mercator street-level
view all the way out to a rotating globe, with the basemap tile
fetch keeping up the whole way.

### Phase 10 — Orbital overlay: live satellite positions

Once the globe view lands, the natural payoff: render the known
satellite catalog as moving points in a thin shell above the
ellipsoid.

- **Data source:** [Celestrak](https://celestrak.org) — public-
  domain TLE (Two-Line Element) catalogs for ~30k tracked objects:
  Starlink, ISS, GPS / GLONASS / Galileo / BeiDou constellations,
  weather satellites, debris.
- **Propagator:** [`sgp4`](https://crates.io/crates/sgp4) — pure-
  Rust SGP4/SDP4 implementation, the canonical TLE propagator;
  works on wasm.
- **Rendering:** instanced point cloud + (optionally) orbital-path
  polylines, both as a shell around the globe. Time controls
  (real-time / accelerated / scrub) drive the SGP4 evaluation.

Publishable artifact: live Starlink shell rotating around an aeGIS
globe, with hover-to-identify and an orbital-track overlay.

This phase also introduces "things in the atmosphere" as a first-
class concept — DEM-draped terrain (Phase 11+) would extend it.

## Active plans

- [`0001-foundation.md`](0001-foundation.md) — Foundation:
  pixels native + web, Web Mercator slippy map, GeoJSON overlay,
  verification harness, embeddable widget skeleton.

## Done

(nothing yet)
