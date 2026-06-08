# aeGIS

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

Early. The repo carries the project conventions
([`AGENTS.md`](AGENTS.md)), the planning discipline
([`plans/`](plans/)), the agent + skill scaffolding
([`.claude/`](.claude/)), and a foundation plan
([`plans/0001-foundation.md`](plans/0001-foundation.md)) whose first
milestone (`M0` — pixels on screen, native + web) is in progress.

See [`plans/ROADMAP.md`](plans/ROADMAP.md) for direction.

## Direction

The plan is to grow aeGIS from a slippy raster map → vector overlays →
multi-CRS reprojection → vector tiles → raster (Cloud-Optimized GeoTIFF)
→ spatial index + queries → a declarative styling system → an
embeddable widget API → a Google-Earth-style globe view with a smooth
flat-to-spherical zoom-out → live satellite-orbit overlays driven by
the Celestrak TLE catalog.

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
