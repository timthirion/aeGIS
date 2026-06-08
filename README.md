# aeGIS

[![CI](https://github.com/timthirion/aeGIS/actions/workflows/ci.yml/badge.svg)](https://github.com/timthirion/aeGIS/actions/workflows/ci.yml)
[![Live demo](https://img.shields.io/badge/live-demo-brightgreen.svg)](https://timthirion.github.io/aeGIS/)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust 2021](https://img.shields.io/badge/rust-2021_edition-orange.svg)](https://doc.rust-lang.org/edition-guide/rust-2021/index.html)
[![WebGPU](https://img.shields.io/badge/runs_in-WebGPU-purple.svg)](https://wgpu.rs)

> Open-source GIS in Rust — pan, zoom, query a map in the browser or
> natively, from one codebase.

## What is aeGIS?

A Rust geographic information system that targets **WebGPU** — so the same
code runs natively (Metal / Vulkan / DX12 via [`wgpu`](https://wgpu.rs))
and inside the browser. One codebase, one shading language
([WGSL](https://www.w3.org/TR/WGSL/)), one set of tests.

The point of the project: a GIS you can self-host, embed in a web page as
a live widget, and grow into a full analysis toolkit — without giving up
the rigour (a typed CRS subsystem, round-trip-tested format I/O, a
verification harness) that production GIS work demands.

aeGIS is built on free + open foundations:
[OpenStreetMap](https://www.openstreetmap.org/),
[Protomaps](https://protomaps.com/),
[Natural Earth](https://www.naturalearthdata.com/),
[Celestrak](https://celestrak.org/), and the Rust geospatial ecosystem
([`geo`](https://github.com/georust/geo),
[`geozero`](https://github.com/georust/geozero),
[`rstar`](https://github.com/georust/rstar),
[`lyon`](https://github.com/nical/lyon),
[`proj4rs`](https://github.com/3liz/proj4rs)).

## Status

Live at [**timthirion.github.io/aeGIS**](https://timthirion.github.io/aeGIS/).

The foundation phase (plan
[`0001-foundation.md`](plans/0001-foundation.md)) has shipped
through M3:

- Interactive Web Mercator slippy map (drag-to-pan,
  wheel-zoom-around-cursor)
- Multi-zoom tile rendering with parent-tile prefetch for smooth
  zoom transitions
- Async tile cache (channel-driven; native `std::thread`, web
  `spawn_local`)
- Country-outline overlay from the bundled Natural Earth 1:110m
  dataset

Plus **Phase 9 v1 (globe view)** — zooming out smoothly transitions
the flat Mercator map into a rotating 3D globe (single `globeness`
uniform interpolating between the two projections in WGSL). Country
outlines + OSM imagery both wrap the sphere; backface discard hides
the far side. Zoom back in and you're on a normal slippy map.

See [`plans/ROADMAP.md`](plans/ROADMAP.md) for direction.

## Direction

The plan is to grow aeGIS from a slippy raster map → vector overlays
(done) → multi-CRS reprojection → vector tiles → raster (Cloud-
Optimized GeoTIFF) → spatial index + queries → a declarative styling
system → an embeddable widget API → a Google-Earth-style globe view
with a smooth flat-to-spherical zoom-out (v1 done) → live satellite-
orbit overlays driven by the Celestrak TLE catalog.

Each phase becomes a `plans/NNNN-*.md` file with concrete milestones,
tests, and a reference fixture so anyone (human or agent) can pick up
the work on another machine.

## Quick start

(M0 in progress — the commands below light up as milestones land.)

```sh
# Native
cargo run                              # desktop map window
cargo test                             # unit + WGSL validation tests

# Web
wasm-pack build --target web           # produces pkg/
python3 -m http.server                 # then open http://localhost:8000/
```

## Architecture

A core renderer library owns the `wgpu` device/queue, the layer model,
the CRS subsystem, and the WGSL pipelines; thin native (winit) and web
(canvas + `wasm-bindgen`) entry points drive it. The native and web
builds stay in lockstep — a change that only compiles on one target
is half-done.

Coding conventions, the testing doctrine, and the dependency policy
live in [`AGENTS.md`](AGENTS.md). The planning discipline lives in
[`plans/README.md`](plans/README.md).

## License

[Apache-2.0](LICENSE).

Map data carries the licenses of its sources. OSM-derived layers
require `© OpenStreetMap contributors`; Protomaps adds itself;
Natural Earth is public domain. The widget API surfaces these
attributions via `attributionsFor(layer)`; the default UI chrome
renders them.
