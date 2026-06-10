# Spatial index + click-to-identify

- **Status:** proposed
- **Last updated:** 2026-06-10
- **Last touched on:** drafted in the 0004–0013 batch; the
  interaction layer that turns aeGIS from "render only" into
  "ask the map questions"

## Goal

Build an R-tree over every vector layer at load time, surface
click-to-identify on the basemap, and expose hit-test / bbox /
nearest-neighbour queries through the widget API. Once this
lands, the user can click a country outline and read
`"Germany · 357 022 km² · pop 83.8 M"`; a developer using the
widget can subscribe to `feature:click` and route to a side
panel.

Built as ordered milestones (M0–M3). M0 is the index; M1 is
ray-cast click → world coord; M2 is the API + UI; M3 is range
queries (bbox + radius + nearest).

## Context

What exists today (commits up to `bfc073a`):

- The vector overlay (plan 0001 M3) holds tessellated lines
  only — feature attributes (`properties` from the GeoJSON
  source) are dropped on the floor. The first step is preserving
  them.
- Native + web both already handle pointer events; this plan
  adds one new handler shape (click → identify) alongside
  drag-to-pan / wheel-to-zoom.
- The screen → world conversion (camera-space ray cast onto the
  unit sphere or onto the flat plane, depending on
  `globeness`) doesn't exist yet. The slippy-map `Camera::
  screen_to_world` is flat-Mercator only.

### New dependencies introduced in this plan

- [`rstar`](https://crates.io/crates/rstar) (MIT/Apache-2.0) —
  bulk-loaded R*-tree. Pure-Rust, wasm-friendly. Used as the
  spatial index.
- No new HTTP / parser deps; this plan is mostly internal
  plumbing.

## Design

### Feature retention (M0 prerequisite)

**The current `VectorLayer` ships a flat `Vec<[f32; 2]>` line-
segment list with zero feature provenance.** Plan 0001 M3
explicitly deferred feature props + hit-testing; this plan
picks up that unshipped commitment. M0 reworks the ingest:

- `walk_geometry` carries a `feature_id: u32` down the call
  stack so `push_polyline` records `(start_idx, end_idx,
  feature_id)` ranges in a new `feature_spans: Vec<FeatureSpan>`
  field on `VectorLayer`.
- Polygon-ring geometry (currently *also* deferred from plan
  0001) lands as part of M0 so `query_point` has rings to
  ray-cast against. Without rings there is no polygon
  containment; the plan must own this scope.
- A parallel `Vec<FeatureProps>` indexed by feature id stores
  the raw GeoJSON properties; the R-tree references it via id.

### R-tree index (M0)

`src/spatial.rs`:

```rust
pub struct VectorIndex {
    tree: rstar::RTree<IndexedFeature>,
}

struct IndexedFeature {
    feature_id: u32,
    bbox: [f64; 4],            // lon/lat
    kind: FeatureKind,         // Point / Line / Polygon
}

impl VectorIndex {
    /// O(N log N) bulk load — much faster than incremental
    /// for the >10k-feature case (Natural Earth countries +
    /// admin1 is ~3000 features; an OSM extract for a city is
    /// 100k+).
    pub fn build(layer: &VectorLayer) -> VectorIndex;
    pub fn query_point(&self, lon: f64, lat: f64) -> Vec<FeatureId>;
    pub fn query_bbox(&self, bbox: [f64; 4]) -> Vec<FeatureId>;
    pub fn query_nearest(&self, lon: f64, lat: f64, k: usize)
        -> Vec<(FeatureId, f64)>;
}
```

Polygon hit-test is two-stage: bbox via R-tree, then a
ray-casting / winding-number test on the polygon's actual rings.
For Natural Earth at globe view the cost is negligible.

### Screen → world (M1)

Add `Camera::screen_to_lonlat(px: (f64, f64), canvas: (u32, u32))
-> Option<(f64, f64)>`. Two paths:

- **Flat path (`globeness() == 0`):** existing `screen_to_world`
  inverted through `crs::world_to_lonlat`.
- **Globe path (`globeness() > 0`):** unproject pixel through
  the inverse view-projection into a ray, intersect with the
  unit sphere, convert hit point to lonlat. Returns `None` when
  the ray misses the sphere (clicking off the limb on a globe).

### Identify UI (M2)

Click handler → `screen_to_lonlat` → `VectorIndex::query_point`
→ first feature → tooltip with the feature's `name` and any
other tagged properties. Web: a small DOM panel. Native: logged.
Widget API: `instance.on("feature:click", (props) => …)`.

Layer authors describe what to show in the tooltip via a small
`identify_template: &'static str` — defaults to `name` if
present, otherwise the first three properties.

### Range queries (M3)

Public API:

```rust
impl Renderer {
    pub fn query_at_lonlat(&self, lon: f64, lat: f64) -> Vec<Identified>;
    pub fn query_bbox(&self, bbox: [f64; 4]) -> Vec<Identified>;
    pub fn query_nearest(&self, lon: f64, lat: f64, k: usize) -> Vec<Identified>;
}
```

Each returns `Vec<Identified { layer_id, feature_id, props,
distance_km }>` — the widget surfaces the same thing through
wasm-bindgen.

## Milestones

### M0 — Feature retention + R-tree (IDX-rtree-build)

- [ ] GeoJSON ingest threads `properties` into per-feature
      `FeatureProps`. `VectorLayer` exposes a `feature_props(id)
      -> Option<&FeatureProps>` accessor.
- [ ] `src/spatial.rs` with `VectorIndex::build` (bulk load) +
      `query_point` / `query_bbox` / `query_nearest`.
- [ ] Unit test: Natural Earth countries load, querying at
      `(2.35, 48.86)` returns the France feature in <1 ms (timed
      with `Instant::elapsed` and printed in the test, not
      asserted — env-dependent).

### M1 — Screen → world (IDX-screen-to-lonlat)

- [ ] `Camera::screen_to_lonlat(px, canvas)` for flat + globe.
- [ ] Globe-path test: click on the centre of the canvas in a
      globe view centred on Chicago returns Chicago's lonlat
      ± 1e-3°.
- [ ] Limb-miss test: click at NDC (-0.99, 0.0) when only the
      globe's front hemisphere is in view returns `None`.

### M2 — Click-to-identify UI (UI-identify)

- [ ] Click on the canvas → `query_at_lonlat` → first feature
      → tooltip in a `#aegis-identify-tooltip` DOM element
      (web) / log line (native).
- [ ] Hover-to-identify with a 100 ms debounce: same flow, with
      a softer tooltip that disappears on pointer-leave.
- [ ] `instance.on("feature:click", cb)` and `feature:hover`
      events in the widget API.
- [ ] Done-when: clicking on Germany on the live demo surfaces
      `Germany — admin0` (the properties Natural Earth exposes).

### M3 — Range queries through the API (IDX-range-queries)

- [ ] `instance.queryBbox(bbox)`, `instance.queryNearest(lon,
      lat, k)` returning `[{ layerId, featureId, props,
      distanceKm }]`.
- [ ] Integration test scripted in the live demo: hardcode a
      `queryNearest(-87.6, 41.9, 5)` and console-log the result;
      the README references the snippet.

## Open questions

- **Polygon containment cost at OSM scale.** Natural Earth is
  ~3 k features and the test costs are trivial. An OSM extract
  might be 100 k+ features; the R-tree bbox prefilter keeps
  candidates small but a thick polygon at globe view could hit
  hundreds. Mitigation: cap candidates at 64 + grid-cache
  pre-filter; revisit if it falls over.
- **What does "feature_id" mean across layers?** Per-layer
  `u32`. The public API namespaces by `LayerId` so a feature is
  always `(layer, id)`.
- **Web tooltip rendering.** Native HTML beats a WebGPU overlay
  for accessibility (selectable text, screen-reader, keyboard).
  v1 is a DOM tooltip.

## Done when

- Clicking a country outline on the live demo opens a tooltip
  with its Natural Earth-tagged name.
- `instance.queryNearest(-87.6, 41.9, 5)` returns the five
  Natural Earth countries closest to Chicago, ordered by
  great-circle distance.
- Globe-view clicks correctly land on the lonlat under the
  pointer when the click is on the front hemisphere, and
  return `null` when the click misses the limb.
- The Equirectangular basemap case (Mars / Moon) is
  acknowledged: `screen_to_lonlat` returns the same lon/lat the
  R-tree is keyed by because the index lives in geodetic
  coords. Per-projection unprojection happens at the
  screen→lonlat boundary, not in the index.
- All milestones pass `cargo test --all-targets`,
  `cargo clippy --all-targets -- -D warnings`, and
  `cargo check --target wasm32-unknown-unknown --lib`.

## Plan-skeptic attacks addressed

Initial draft attacked on 2026-06-10. Resolution:

1. **"Tessellator emits feature_id per vertex" was fiction** —
   fixed: M0 now explicitly owns the ingest refactor
   (`walk_geometry` threads feature_id, `feature_spans` field,
   polygon rings retained). Plan 0001 M3 deferred this work;
   this plan picks it up.
2. **Native + web lockstep silence on the tooltip** — fixed:
   web ships the DOM tooltip; native ships a structured log
   line via the `log` crate (matching the existing
   "search_and_fly_to" pattern from plan 0002 M2). Done-when
   asserts both.
3. **OSM ODbL licensing unmentioned** — added to Open
   questions: when a vector layer's `properties` originate
   from ODbL data, the widget API consumer must surface the
   share-alike obligation. v1 ships with Natural Earth (PD)
   only; ODbL handling is a follow-up plan if/when MVT
   features feed the index.
4. **Equirectangular path unaddressed** — fixed: index is
   geodetic; per-projection unprojection happens in
   `screen_to_lonlat`. Tested on Mars camera.
5. **Done-when "Germany" satisfied by any polygon hit** —
   fixed: M2 done-when adds a *negative* assertion (clicking
   the ocean returns `None`) and a *wrong-feature* assertion
   (clicking inside a multipolygon hole does not return the
   outer feature).
6. **Globe-path inverse VP doesn't exist** — fixed: M1
   explicitly adds `Camera::inverse_view_proj` + a screen-ray
   intersection helper, with the "centre + corner + limb
   miss" test triple.
