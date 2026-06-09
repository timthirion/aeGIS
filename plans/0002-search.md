# Search: one box, coords + place queries

- **Status:** proposed
- **Last updated:** 2026-06-09
- **Last touched on:** drafted alongside plan 0003 (multi-body)
  during the post-Phase-9 cleanup; nothing implemented yet

## Goal

A single floating search bar that accepts either a coordinate
expression (`"41.87, -87.63"`, `"41°52′N 87°37′W"`, etc.) or a place
query (`"Topeka, Kansas"`, `"Eiffel Tower"`, `"Olympus Mons"` once
the multi-body plan lands), and flies the camera to the result.
Inputs that parse as coordinates skip the network entirely; everything
else hits an OSM-backed geocoder. The deliverable is the search UX
that turns aeGIS from "a globe you can spin" into "a globe you can
look something up on."

Built as ordered milestones (M0–M3). M0 + M1 are headless and unit-
testable; M2 + M3 are the user-facing surface.

## Context

What exists today (commits up to `750edb1`):

- Camera state in `src/camera.rs` — `center_lonlat: (f64, f64)`,
  `zoom: f64`, with `Camera::new(lon, lat, zoom)` as the entry point.
  Globeness drives the flat ↔ sphere blend (Phase 9).
- Renderer in `src/render.rs` — owns the `Camera`, takes
  `set_camera_center` / `set_camera_zoom`-shaped mutations through
  field access. No animation system today; every mutation is
  immediate.
- Web entry in `src/web.rs` — wires pointer / wheel input to camera
  mutation directly; no input dispatch abstraction yet.
- `index.html` — has a footer with attribution and a bottom-left
  basemap toggle. Top of the canvas is empty real estate.

No search bar exists; no geocoder client; no fly-to animation. The
search bar is the first piece of UI chrome beyond the basemap toggle,
so it sets a small precedent for how floating widgets get wired up
to the renderer.

### New dependencies introduced in this plan

- `regex` (MIT/Apache-2.0) — coordinate-format pattern matching
  (M0). Pure Rust, wasm-friendly, already a transitive dep of
  several existing crates so the binary cost is near-zero.
- No new Rust HTTP client needed — `ehttp` (native) and `web_sys::
  fetch_with_str` (web) are already in use for tile loads; the
  geocoder fetch reuses the same plumbing.

### Data sources

- **Geocoder (primary):** [Photon](https://photon.komoot.io)
  (Komoot). OSM-derived index, BSD-licensed server, CORS wildcard
  (`Access-Control-Allow-Origin: *` verified 2026-06-09 from a
  GitHub Pages origin), no API key. Returns GeoJSON
  `FeatureCollection` with `osm_type`, `osm_key`, `osm_value`,
  `country`, `state`, `city`, `name`, and a bounding box. Komoot
  asks for "reasonable use" — debounced autocomplete (250 ms)
  keeps a casual user well below any threshold. Attribution: "©
  OpenStreetMap contributors, search by Photon."
- **Geocoder (fallback):** [Nominatim](https://nominatim.
  openstreetmap.org) (OSMF-hosted). Same OSM dataset (ODbL), CORS
  wildcard, public-instance usage policy caps at 1 req/sec.
  Tighter rate limits make it the fallback rather than primary,
  but it's the canonical reference for self-hosting later.

Both indexes are ODbL — same as the OSM data underneath the Carto
basemap — so the data-source policy is satisfied with no fork-only
caveat.

## Design

### Module shape

A new `src/search.rs` module owns the pure logic (coord parsers +
geocoder client + result types). The renderer learns one new
method, `fly_to(target: SearchTarget)`, which kicks off a camera
animation (M3). Web wiring lives in `src/web.rs` — DOM input
binding, debounced fetch, autocomplete render — and HTML chrome
lives in `index.html`.

```text
src/
├── search.rs        ← new: parsers, geocoder client, SearchTarget
├── camera.rs        ← add fly-to interpolation state
├── render.rs        ← add Renderer::fly_to + per-frame tick
├── web.rs           ← add search-bar wiring + autocomplete render
└── lib.rs           ← export SearchTarget on the widget API

index.html
└── #aegis-search    ← floating top-centre input + dropdown
```

### Coord parser (M0)

`parse_coord(s: &str) -> Option<(f64, f64)>` (returns `(lon, lat)`,
not `(lat, lon)` — the rest of the codebase uses lon-first; the
parser converts at the boundary). Accepted forms, in order:

1. **Decimal pair with comma or whitespace:**
   `"41.87, -87.63"`, `"41.87 -87.63"`, `"-87.63,41.87"` (if the
   first value is out-of-range for latitude, assume lon-first).
2. **Decimal pair with hemisphere letters:**
   `"41.87°N 87.63°W"`, `"41.87N, 87.63W"`.
3. **DMS (degrees-minutes-seconds):**
   `"41°52′12″N 87°37′48″W"`, with `'` and `"` accepted in place of
   `′` and `″`, and the degree symbol optional.

Order matters because format 1 is the most ambiguous (no
hemisphere) — match the more-specific forms first. Each format gets
a regex + a small interpretation function; the parser tries them in
order and returns the first that yields valid lon/lat.

**Sanity:** lat ∈ [−90, 90], lon ∈ [−180, 180]. Out-of-range parses
return `None` so a typo doesn't silently fly the camera to nowhere.

### Geocoder client (M1)

`async fn geocode(query: &str, near: Option<(f64, f64)>) ->
Result<Vec<SearchResult>, GeocodeError>`.

Photon endpoint:

```text
https://photon.komoot.io/api/?q=<query>&limit=5&lang=en
  [&lat=<lat>&lon=<lon>]            ← if `near` is provided, ranks
                                      results closer to the camera
```

Response is GeoJSON; map each feature to:

```rust
pub struct SearchResult {
    pub name: String,           // e.g. "Topeka"
    pub context: String,        // e.g. "Kansas, USA"
    pub lonlat: (f64, f64),
    pub bbox: Option<[f64; 4]>, // (lon_min, lat_min, lon_max, lat_max)
    pub kind: ResultKind,       // City / Country / POI / Address / ...
}
```

`ResultKind` drives the target zoom (city → z=12, country → z=4,
POI → z=16, etc.). If `bbox` is present, prefer fitting the bbox
to the canvas over the per-kind default zoom.

**Native + web parity:** the same client logic runs in both targets;
only the HTTP transport differs (`ehttp` natively, `fetch` on web).
Same pattern as `tile::fetch_tile_*`.

**Failure modes:**

- Network error: surface a user-visible "couldn't reach geocoder"
  state in the dropdown; no console spew.
- Empty results: dropdown shows "no matches" rather than a stale
  list.
- Photon 503 / rate-limit (rare on the public instance but
  possible): fall back to Nominatim once per session, log the
  switch.

### UI surface (M2)

Floating `<div id="aegis-search">` pinned to the top centre of the
canvas, ~480 px wide on desktop, full-width on mobile. Input has a
debounced (250 ms) `input` handler that calls into the wasm
geocoder. Results render as a vertical list under the input:

```text
┌────────────────────────────────────────────────┐
│ 🔍  Topeka, Kansas                           ⌫ │
├────────────────────────────────────────────────┤
│ Topeka                                         │
│ Kansas, USA · city                             │
├────────────────────────────────────────────────┤
│ Topeka State Hospital Historic District        │
│ Kansas, USA · historic                         │
└────────────────────────────────────────────────┘
```

When the input parses as coordinates, the dropdown shows a single
synthetic "coordinate" entry — no network call — so the user sees
clearly that lat/lon was detected:

```text
┌────────────────────────────────────────────────┐
│ 🔍  41.87, -87.63                            ⌫ │
├────────────────────────────────────────────────┤
│ 41.87°N, 87.63°W                               │
│ coordinate · click or press Enter to fly       │
└────────────────────────────────────────────────┘
```

Keyboard: Enter selects the highlighted result (or the first one
if none highlighted); ↑/↓ navigate; Escape closes the dropdown.
Click on a result also selects it.

### Camera fly-to (M3)

The Renderer learns a small animation system. State:

```rust
struct FlyTo {
    start_lonlat: (f64, f64),
    start_zoom: f64,
    target_lonlat: (f64, f64),
    target_zoom: f64,
    started_at: f64,    // monotonic time, seconds
    duration: f64,      // seconds; scales with great-circle distance
}
```

Per frame, the renderer interpolates `center_lonlat` and `zoom`
between start and target using a smoothstep-eased parametric `t`.
Two-stage interpolation handles the "zoom out then back in" feel
that makes long flies legible:

- For the first half (t ∈ [0, 0.5]), zoom out to a fly-altitude
  picked from the great-circle distance (further → lower
  intermediate zoom; bounded above by the start/target max).
- For the second half (t ∈ [0.5, 1]), zoom into the target. Pan
  along the great circle the whole way.

`duration` scales with distance: short flies (< 1° apart) get
~0.4 s; antipodal flies get ~2.0 s. Tunable constants up front,
revisit after dogfooding.

Any user input (pan, zoom, basemap toggle) cancels the fly-to
mid-flight — animation should never feel like it's fighting the
user.

## Milestones

### M0 — Coord parser (UI-coord-parser)

- [ ] `parse_coord(&str) -> Option<(f64, f64)>` covering the three
      formats above. Unit tests for each format + each
      hemisphere + out-of-range rejection + the "first value
      out-of-range for lat → assume lon-first" rule.
- [ ] Tests use a small property-style table of (input, expected
      lon, expected lat). Round-trip a few known cities (Chicago,
      Tokyo, Sydney, Reykjavík) through several formats and assert
      they all land in the same tile at z=10.

### M1 — Geocoder client (UI-geocoder-client)

- [ ] `SearchResult`, `ResultKind`, `GeocodeError` types defined
      in `src/search.rs`.
- [ ] `geocode(query, near)` against Photon, parsing the GeoJSON
      response. Native uses `ehttp`, web uses `web_sys::fetch`.
- [ ] Nominatim fallback with a one-shot per-session switch on
      Photon 5xx / network error.
- [ ] Integration test (native, network-hitting, marked `#[ignore]`
      so it doesn't run on CI by default): query "Chicago" and
      assert one result lands in (lon −87.7 ± 0.5, lat 41.9 ± 0.5).

### M2 — Search-bar UI (UI-search-bar)

- [ ] `#aegis-search` element in `index.html` with input,
      dropdown, and styles. Matches the existing aeGIS chrome
      palette.
- [ ] `src/web.rs` wires the input → debounced parse-or-geocode →
      results-list render. Synthetic "coordinate" result rendered
      when parser succeeds.
- [ ] Keyboard handling: Enter / ↑ / ↓ / Escape.
- [ ] Visual reference: a screenshot at `tests/visual/search-bar.png`
      checked into the repo so future visual regressions are
      reviewable.

### M3 — Camera fly-to (UI-camera-fly-to)

- [ ] `FlyTo` state in `Renderer`; per-frame `tick_camera()`
      interpolates and clears state when `t >= 1`.
- [ ] Smoothstep eased pan + two-stage zoom (out → in) with
      duration scaling by great-circle distance.
- [ ] Cancellation: any pan / zoom / basemap toggle clears the
      `FlyTo`.
- [ ] `Renderer::fly_to(target_lonlat, target_zoom)` wired into
      the search-result selection path. Press Enter on "Chicago"
      from a globe view → camera glides to z=12 over Chicago.
- [ ] Unit test for the interpolation: at `t=0`, camera is at
      start; at `t=1`, camera is at target; great-circle midpoint
      at `t=0.5` is on the great-circle path.

## Open questions

- **Native search UI?** Native target currently has no DOM-backed
  chrome — the search bar would need a `winit`-side input handler
  + an immediate-mode renderer (egui? or roll our own). M2 lands
  web-only; native gets a `Renderer::search(query) -> Vec<Result>`
  method but no UI. **Resolved during M2 design:** ship web-only
  for the UI; native gets the headless API. Revisit when (if) a
  native UI surface lands.
- **POI category filter?** Photon supports filtering by `osm_tag`
  (e.g. only cities, only airports). Useful eventually; not in
  scope for v1. Open to revisit if the UX needs it.
- **Recents / history?** A list of recent searches is a small,
  obvious win. Out of scope for v1 — adding LocalStorage state
  has its own design surface (clear-on-fork-deployment, etc.).
  Revisit after M3.

## Done when

- Typing `"41.87, -87.63"` flies the camera smoothly to that
  point at z=12.
- Typing `"Topeka, Kansas"` shows ≥1 dropdown result; selecting
  one flies the camera to it at the result-kind's default zoom.
- Typing nonsense ("xyzzy") shows "no matches" in the dropdown.
- The geocoder fetch logs are reasonable: at most one request per
  250 ms during typing; no requests at all for coord-parseable
  input.
- All four milestones pass `cargo test --all-targets` and `cargo
  check --target wasm32-unknown-unknown --lib`.
- The live demo at `timthirion.github.io/aeGIS` has a working
  search bar visible top-centre.
