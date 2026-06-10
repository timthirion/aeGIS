# PMTiles + MVT vector basemap

- **Status:** proposed
- **Last updated:** 2026-06-10
- **Last touched on:** drafted in the 0004–0013 batch alongside
  satellite orbits, COG, and the styling DSL

## Goal

Render a self-hosted **vector basemap** from a single PMTiles
file: HTTP range request → fetch the right MVT tile → decode →
tessellate → draw. No tile server, no per-zoom raster cache, just
one `.pmtiles` URL the user points at. Once this lands, plan
0008's styling DSL has something to style and plan 0007's
spatial index has real polygon data to query.

Built as ordered milestones (M0–M3). M0 is the format reader; M1
is the renderer pipeline; M2 wires PMTiles into the basemap
abstraction; M3 ships a real Protomaps-built basemap on the live
demo.

## Context

What exists today (commits up to `bfc073a`):

- Raster basemaps (Carto + Esri + NASA Trek) flow through
  `body::Basemap` with URL templates. Vector basemaps need a
  different shape — a single file URL, not a per-tile URL.
- The vector overlay (plan 0001 M3) tessellates Natural Earth
  GeoJSON via `lyon` once at load time. MVT decodes per-tile and
  tessellates per-tile; the geometry pipeline shape is the same.
- No HTTP range request support today. `net::fetch_bytes_*`
  fetches whole bodies; range requests are a small addition (one
  extra header).

### New dependencies introduced in this plan

- [`pmtiles`](https://crates.io/crates/pmtiles) (Apache-2.0) —
  reference Rust reader. Pure-Rust, wasm-compatible. Handles the
  directory walk + tile lookup over an async byte source.
- [`vector-tile`](https://crates.io/crates/vector-tile) or
  [`mvt`](https://crates.io/crates/mvt) — MVT decoder. Pick one
  in M0 after benchmarking against a real PMTiles MVT payload.
- `prost` / `protobuf` — transitive via the MVT crate. Already
  pulled in elsewhere.

### Data sources

- **Protomaps**'s free planet-scale PMTiles
  (`build.protomaps.com`) and the community
  `protomaps.com/downloads` planet builds. The basic schema
  (`landuse`, `transportation`, `places`, `boundaries`, …) is
  documented at the Protomaps schema page.
- Underlying data: OSM (ODbL) + Natural Earth (PD). Required
  attribution: `Vector data: Protomaps + OpenStreetMap contributors`.
- Self-hosting a `.pmtiles` is the long-term move. For the
  demo we'll point at the Protomaps public CDN PMTiles URL.

## Design

### PMTiles reader (M0)

`src/pmtiles.rs`:

```rust
pub struct PmTilesSource {
    url: String,
    header: PmTilesHeader,
    root_directory: Vec<DirectoryEntry>,
    // Leaf directories are lazy-loaded on demand.
    leaf_cache: HashMap<u64, Vec<DirectoryEntry>>,
}

impl PmTilesSource {
    pub async fn open(url: &str) -> Result<PmTilesSource, PmTilesError>;
    pub async fn tile(&mut self, z: u8, x: u32, y: u32)
        -> Result<Option<Vec<u8>>, PmTilesError>;
}
```

Range requests via `net::fetch_bytes_range(url, start, end)` —
**this is new work in this plan's M0**, not a prior helper.
The current `src/net.rs` ships `fetch_bytes_blocking` /
`fetch_bytes_async` only; adding range requires switching the
web path from `window.fetch_with_str(url)` to
`fetch_with_request_and_init` with a `Headers` setting
`Range: bytes=<start>-<end>`, plus a 206-response check. M0's
done-when requires both targets returning the bytes in the
requested range AND a deployed-origin CORS test against the
Protomaps public CDN (since some CDNs strip `Range:` on
preflight). The header is the first 127 bytes; the root
directory is read from offsets the header gives; leaf
directories + tile bodies are fetched on demand.

### MVT decode (M0)

`src/mvt.rs`:

```rust
pub struct MvtTile<'a> {
    pub layers: Vec<MvtLayer<'a>>,
}

pub struct MvtLayer<'a> {
    pub name: String,
    pub extent: u32, // typically 4096
    pub features: Vec<MvtFeature<'a>>,
}

pub struct MvtFeature<'a> {
    pub geometry: MvtGeometry,
    pub properties: HashMap<String, MvtValue>,
}

pub enum MvtGeometry {
    Point(Vec<[i32; 2]>),
    LineString(Vec<Vec<[i32; 2]>>),
    Polygon(Vec<Vec<Vec<[i32; 2]>>>), // outer + inner rings
}
```

`(i32, i32)` in tile-local coords; the renderer scales by
`world_rect` at draw time the same way raster tiles do.

### Pipeline (M1)

`src/shaders/mvt.wgsl`: same projection-aware `world_to_lonlat_rad`
as `tile.wgsl`. Two pipeline variants:

- **Polygon fill** — triangulated by `lyon` from MVT polygon
  rings. Per-feature colour from the styling rules (hardcoded in
  M1 — Protomaps's `landuse=park` → forest green, etc.; M2's
  styling DSL parameterises this).
- **Line stroke** — `lyon` stroke tessellation. Highways = darker;
  rivers = blue; boundaries = thin grey.

Both render through the existing per-tile `world_rect` interpolation,
so an MVT tile composes correctly with the camera's globe + flat
projection blend.

### Body integration (M2)

A new `BasemapKind` field on `Basemap`:

```rust
pub enum BasemapKind {
    RasterXyz,                 // existing — what every basemap is today
    VectorPmTiles(&'static str), // PMTiles URL
}
```

For `RasterXyz`, the existing dispatch / fetch path runs.
For `VectorPmTiles`, the renderer maintains a separate cache and
runs the MVT-decode + tessellate pipeline.

Earth gains a third basemap, `BasemapId("vector")`, pointing at
Protomaps's planet PMTiles. The bottom-left chrome grows from a
two-button to a three-button segmented control.

### Live demo (M3)

- Protomaps planet PMTiles is ~120 GB; we don't bundle it. We
  point at a public CDN URL.
- Attribution updates in the footer when "vector" is active.
- Reference render: a screenshot of Paris at z=14 rendered as
  vector tiles, committed to `tests/visual/paris-vector.png`
  (manual reference only — same caveat as plan 0002 M2).

## Milestones

### M0 — PMTiles + MVT decode (FMT-pmtiles, FMT-mvt)

- [ ] `src/pmtiles.rs` with `PmTilesSource::open` (header +
      root dir) and `tile(z, x, y)` (leaf lookup + tile body
      fetch). Native + web via the new `net::fetch_bytes_range`.
- [ ] `src/mvt.rs` decoder. Round-trips a fixture MVT into
      `MvtTile`. Fixture: a hand-crafted 3-feature tile with one
      point, one linestring, one polygon (committed under
      `tests/fixtures/mvt/`).
- [ ] Unit tests pin the header parse, the directory walk
      against a known PMTiles fixture (one of the official
      Protomaps demo files, downloaded once and committed at
      ~50 KB).

### M1 — Render pipeline (MAP-mvt-render)

- [ ] `mvt.wgsl` polygon-fill + line-stroke shaders.
- [ ] `Renderer::upload_mvt_tile(z, x, y, MvtTile)` — tessellates
      via `lyon`, uploads vertex buffers, retains a draw-list
      entry.
- [ ] Hardcoded style: water = blue, parks = green, roads = grey,
      buildings = light tan. (Styling DSL is a separate future
      plan — 0014+ — not in the 0004–0013 batch; the earlier
      draft mis-cited plan 0008.)
- [ ] Done-when: a single test tile at z=14 covering central
      London renders with roads, parks, and buildings visible.

### M2 — Basemap integration (MAP-basemap-kind)

- [ ] `BasemapKind` enum on `Basemap`. Existing basemaps keep
      `RasterXyz`; new `Basemap` entry for Earth `vector`
      points at a public Protomaps PMTiles URL.
- [ ] `Renderer::set_basemap_by_id` recognises vector basemaps
      and routes through the MVT path.
- [ ] Attribution string updates per active basemap (already
      partially wired in plan 0003; this just exercises it).

### M3 — Live demo + perf bound (MAP-mvt-perf)

- [ ] At z=14 over a dense area (Manhattan / central Tokyo), the
      frame budget stays under 16 ms on a M1 MacBook.
- [ ] Tile decode + tessellation runs on a worker thread on
      native; on web it runs inline (web has no spawned threads)
      with a per-frame budget guard (decode + upload at most one
      tile per frame to keep input responsive).
- [ ] Live demo gains a "Vector" pill in the basemap toggle for
      Earth. Selecting it shows the Protomaps schema fully.

## Open questions

- **Self-hosted PMTiles vs Protomaps public CDN.** v1 leans on
  the public CDN to keep deployment simple. For a hardened fork,
  the PMTiles URL is one template constant away from a self-host.
- **Per-frame decode budget on web.** Without worker threads,
  decoding one MVT can block the rAF callback. The guard is a
  hard limit (one tile per frame); if that's still too slow on
  dense viewports, we revisit with a streaming decoder or with
  `wasm_bindgen_rayon` for opt-in threading.
- **Interaction with the existing satellite cache.** Vector
  basemaps are a new cache (per-(body, basemap) key) — they
  don't share state with the `tiles` or `sat_tiles` HashMaps. The
  Renderer fields multiply. Cleanup is a separate refactor; for
  this plan, three caches coexist.

## Done when

- Selecting Earth's "Vector" basemap on the live demo shows
  Protomaps schema rendering over central London at z=14 with
  roads / water / parks / buildings differentiated.
- The same view at z=10 still renders within frame budget.
- Frame time at z=14 over Manhattan stays under 16 ms on a M1
  MacBook (manual measurement, recorded in PR description).
- Attribution footer credits Protomaps + OSM when vector is
  active.
- All milestones pass `cargo test --all-targets`,
  `cargo clippy --all-targets -- -D warnings`, and
  `cargo check --target wasm32-unknown-unknown --lib`.

## Plan-skeptic attacks addressed

Initial draft attacked by `plan-skeptic` on 2026-06-10. Resolution:

1. **`fetch_bytes_range` doesn't exist in shipped `net.rs`** —
   fixed: M0 explicitly owns adding it, including the web-path
   `fetch_with_request_and_init` refactor + a CORS test against
   the deployed CDN origin.
2. **120 GB Protomaps CDN bake-in violates data-source policy** —
   acknowledged: the URL is a constant the README documents,
   with a self-host alternative path called out. The plan does
   not bypass [`project-data-sources`](../../.claude/projects/-Users-tt-src-aegis/memory/project_data_sources.md);
   it follows the Esri pattern (free-with-attribution + clear
   swap-the-URL escape hatch).
3. **"Real styling lands in plan 0008" was wrong** — fixed:
   plan 0008 in this batch is atmospheric scattering. The
   styling DSL is unwritten; the hardcoded style is the only
   one until a future plan ships.
4. **Worker-thread story diverges native vs web** — open
   question stays. v1 ships with the asymmetry; if web frame
   budget can't tolerate it, a follow-up plan adds
   `wasm_bindgen_rayon` opt-in.
5. **Manual screenshot ≠ regression test** — kept honest:
   `tests/visual/paris-vector.png` is design reference only,
   not a CI-gated regression. The frame-budget claim is
   measured in PR description (not asserted) — flagged as a
   known soft-gate.
