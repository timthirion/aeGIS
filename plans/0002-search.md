# Search: one box, coords + place queries

- **Status:** done
- **Last updated:** 2026-06-09
- **Last touched on:** M0–M3 all shipped in one session
  (`ad3cbe7`, `c7f5b79`, `c28ad15`, `b2de48d`) after the plan-
  skeptic-driven revision. See "Plan-skeptic attacks addressed".

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
  (M0). Pure Rust, wasm-friendly.
- `serde` + `serde_json` (MIT/Apache-2.0) — Photon response
  parsing (M1). `serde_json` is the canonical Rust JSON
  parser; already a transitive dep of much of the ecosystem.
- No new HTTP client — `ehttp` (native) and `web_sys::fetch_*`
  (web) already underpin the tile fetcher in `src/tile.rs`.
  **However**, the existing functions are image-decoders
  (`fetch_tile_blocking` / `fetch_tile_web` both call
  `decode_image` on the response body and return `DecodedTile`).
  M1 lands a sibling **`fetch_json_*` family** in a new
  `src/net.rs` module that does the same HTTP call shape but
  surfaces raw `Vec<u8>` (so the geocoder can `serde_json::
  from_slice` into a typed response). The image-fetcher stays
  unchanged; the new module gives both consumers a real shared
  HTTP surface to point at.

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
geocoder client + result types). A new `src/net.rs` module owns
the shared HTTP-with-Vec<u8>-body plumbing both the geocoder and
any future bytes-returning fetcher can use; the existing image-
decoding `tile::fetch_tile_*` keeps its specialised shape but now
sits on `net::fetch_bytes_*` under the hood (small refactor in
M1). The renderer learns two new methods — `fly_to(target_lonlat,
target_zoom)` and `fly_to_bbox(bbox_lonlat)` — which kick off a
camera animation (M3). Web wiring lives in `src/web.rs`; HTML
chrome lives in `index.html`.

```text
src/
├── net.rs           ← new: fetch_bytes_blocking (native) +
│                       fetch_bytes_async (web). Returns
│                       Result<Vec<u8>, NetError>. Single source
│                       of truth for HTTP shape across the crate.
├── search.rs        ← new: parsers, geocoder client, types
├── tile.rs          ← refactored: fetch_tile_* call net::*
├── camera.rs        ← add fly-to interpolation state (slerp)
├── render.rs        ← add Renderer::fly_to* + per-frame tick
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

**Position interpolation is slerp on the unit sphere, not lon/lat
lerp.** Linearly interpolating `(lon, lat)` is a rhumb line, not
a great circle — Reykjavík → Sydney via lon-lerp crosses the
equator instead of the pole. Per frame:

1. Convert `start_lonlat` and `target_lonlat` to unit-sphere
   vectors `p0`, `p1` (same `lonlat_to_sphere` the renderer
   already uses for the globe).
2. Compute the angle `Ω = acos(p0·p1)`. If `Ω < 1e-9`, fall
   through to start (no motion).
3. At parametric `t` (smoothstep-eased — `3t² − 2t³`),
   `p_t = (sin((1−t)·Ω) · p0 + sin(t·Ω) · p1) / sin(Ω)`.
4. Convert `p_t` back to `(lon, lat)` for the camera.

Two-stage zoom interpolation handles the "zoom out then back in"
feel that makes long flies legible:

- For the first half (t ∈ [0, 0.5]), ease zoom from `start_zoom`
  down to `intermediate_zoom`, where `intermediate_zoom` is
  derived from the great-circle arc length `Ω`: short flies stay
  near `min(start_zoom, target_zoom)`; antipodal flies drop all
  the way to globe view (`zoom ≈ 1.5`).
- For the second half (t ∈ [0.5, 1]), ease zoom from
  `intermediate_zoom` to `target_zoom`.

`duration` scales with `Ω`: 0.4 s for `Ω < π/180` (< 1° apart),
2.0 s for `Ω = π` (antipodal). Tunable constants up front,
revisit after dogfooding.

Any user input (pan, zoom, basemap toggle) cancels the fly-to
mid-flight — animation should never feel like it's fighting the
user.

### Bbox-fit (M3)

`Renderer::fly_to_bbox(bbox: [f64; 4])` is the bbox-aware variant
used when the geocoder returns a bbox (city, country, region —
anything bigger than a POI). It computes a target zoom that frames
the bbox in the current canvas, then defers to `fly_to`:

- **Flat path (globeness = 0):** `target_zoom = log2(min(canvas.x
  / bbox_world_width, canvas.y / bbox_world_height) / TILE_PIXELS)`
  — the standard slippy "fit bbox to canvas" formula.
- **Globe path (globeness > 0):** convert bbox corners to sphere
  vectors, compute the diameter of the bounding circular cap, and
  solve for the altitude that makes that cap subtend the camera's
  vertical FOV. Inverse of the existing `Camera::altitude`
  formula.

Both paths add a small margin (~10%) so the bbox doesn't sit
flush against the canvas edge.

## Milestones

### M0 — Coord parser (UI-coord-parser)

- [x] `parse_coord(&str) -> Option<(f64, f64)>` covering the three
      formats above. Unit tests for each format + each
      hemisphere + out-of-range rejection + the "first value
      out-of-range for lat → assume lon-first" rule.
- [x] Tests use a small property-style table of (input, expected
      lon, expected lat). Round-trip a few known cities (Chicago,
      Tokyo, Sydney, Reykjavík) through several formats and assert
      they all land in the same tile at z=10.

### M1 — Geocoder client (UI-geocoder-client)

- [x] New `src/net.rs`: `fetch_bytes_blocking(url)` (native, via
      `ehttp`) and `fetch_bytes_async(url, on_done)` (web, via
      `web_sys::fetch`). Returns `Result<Vec<u8>, NetError>` where
      `NetError` distinguishes transport vs HTTP-status vs
      decode-shape.
- [x] Refactor `tile::fetch_tile_blocking` / `fetch_tile_web` to
      call `net::fetch_bytes_*` and then `decode_image`. Behaviour
      unchanged; the existing tile path is the regression test
      for this refactor.
- [x] `SearchResult`, `ResultKind`, `GeocodeError` types in
      `src/search.rs`. `ResultKind` enum: `City | Country | Region
      | Address | Poi | Unknown` with `default_zoom() -> f64` mapping.
- [x] `geocode(query, near) -> Result<Vec<SearchResult>,
      GeocodeError>` against Photon via `net::fetch_bytes_*`,
      `serde_json::from_slice` into a typed Photon response struct,
      mapped to `SearchResult`.
- [x] Nominatim fallback: one-shot per-session switch on Photon
      5xx / transport error. The session-state lives in a
      `GeocoderClient` struct so tests can mock it.
- [x] Integration test (native, network-hitting, marked `#[ignore]`
      so it doesn't run on CI by default): query "Chicago" and
      assert one result has `name == "Chicago"`, `kind ==
      ResultKind::City`, and `lonlat` within
      (−87.7 ± 0.5, 41.9 ± 0.5).
- [x] Native + web parity test: both targets compile and the same
      `GeocoderClient` API exists on both. Web parity comes for
      free from the wasm32 `cargo check`; native is the integration
      test above.
- [x] README documents Photon's "reasonable use" wording (it's
      not an SLA), Nominatim's 1-req/sec public-instance cap, and
      the data-source policy memory entry. Same shape as the
      existing Esri-basemap-terms note.

### M2 — Search-bar UI (UI-search-bar)

- [x] `#aegis-search` element in `index.html` — input element
      only, no dropdown DOM. Styles use the same colour tokens as
      the existing chrome (footer `rgba(20, 22, 26, 0.85)`
      background, `#e6e8eb` text, `8px` border radius — copied
      from the current `#basemap-toggle` block). Pinned top-centre
      with `position: absolute; top: 16px; left: 50%; transform:
      translateX(-50%); width: min(480px, calc(100% - 32px))`.
- [x] `src/web.rs` creates the dropdown DOM via `web_sys` at
      startup (siblings of the input under `#aegis-search`), so
      Rust owns both the input listeners and the result list. No
      JS-side state.
- [x] **Debounce ownership: Rust-side.** On each `input` event,
      clear any pending timeout via
      `web_sys::Window::clear_timeout_with_handle`, then schedule
      a new one (250 ms) via `set_timeout_with_callback_and_timeout_and_arguments_0`.
      The closure runs `parse_coord` first; on `None`, it fires
      `geocoder.geocode(...)` and renders the result list when the
      future completes.
- [x] Synthetic coordinate entry: when `parse_coord` returns
      `Some((lon, lat))`, the dropdown shows a single row
      labelled `"<lat>°<N|S>, <lon>°<E|W>"` with the subtitle
      `"coordinate — click or press Enter to fly"`. No network
      call.
- [x] Keyboard handling: `keydown` listener on the input.
      `ArrowDown`/`ArrowUp` move highlight (wrap), `Enter` selects
      highlighted or first, `Escape` blurs and hides dropdown.
- [x] **Native target** (acknowledging the web-only UI split):
      `Renderer::search_and_fly_to(&str)` lands as a headless
      API — parses the query (coord or place), runs the geocoder
      blocking on native, picks the first result, kicks off the
      M3 fly-to. Native users get parity via that one method
      call; no `winit` keyboard input wiring in v1.
- [x] Manual reference shot at `tests/visual/search-bar.png`
      (committed for design review only — **not** a regression
      test until a pixel-diff harness exists; the existing
      `tests/shaders.rs` is the model for what a real regression
      test looks like and that bar isn't met here).

### M3 — Camera fly-to (UI-camera-fly-to)

- [x] `FlyTo` state in `Renderer`; per-frame `tick_camera()`
      interpolates and clears state when `t >= 1`.
- [x] **Slerp** for position (unit-sphere interpolation — not
      lon/lat lerp) using the formula in the Design section.
      Smoothstep ease on `t` for both position and zoom.
- [x] Two-stage zoom: zoom-out then zoom-in, with the
      intermediate zoom derived from the great-circle arc length
      (antipodal flies drop to ~zoom 1.5).
- [x] `duration` scales linearly with `Ω`: 0.4 s at `Ω = 0`,
      2.0 s at `Ω = π`. Both constants named in code so tuning
      is one-line.
- [x] Cancellation: any user input (pan, zoom, basemap toggle)
      clears `fly_to_state` to `None` immediately. Test the
      cancellation path in a unit test (set up a `FlyTo`, call
      `pan(1.0, 0.0, canvas)`, assert state is now `None`).
- [x] `Renderer::fly_to(target_lonlat, target_zoom)` and
      `Renderer::fly_to_bbox(bbox)` both wired into the
      search-result selection path. Result-kind picks one or the
      other (POI → `fly_to` with default zoom; everything with a
      bbox → `fly_to_bbox`).
- [x] Unit tests for slerp:
  - At `t=0`: camera position = start (within `1e-9`).
  - At `t=1`: camera position = target (within `1e-9`).
  - At `t=0.5`: interpolated 3D sphere point `p_mid` satisfies
    `|p_mid| = 1 ± 1e-9` (on the sphere) **and**
    `p_mid · (p0 × p1).normalized() = 0 ± 1e-9` (on the
    great-circle plane through `p0` and `p1`). These two
    conditions together pin the great-circle path — a rhumb-
    line midpoint fails the second.
  - Reykjavík (lon −21.9, lat 64.1) → Sydney (lon 151.2, lat
    −33.9): assert `lat(t=0.5) < 0` (the great circle goes
    through the south, not over the equator that a lon-lerp
    would produce).

## Open questions

- **Body-aware search vs Earth-only v1.** Plan 0003 (multi-body,
  also `proposed`) explicitly defers Mars-coordinate convention
  display to "the search/labelling layer" — i.e. this plan. But
  0003 is `proposed` and may not ship for weeks. **Decision for
  v1:** plan 0002 ships Earth-only. `SearchTarget` and
  `SearchResult` carry a bare `(lon, lat)`. When plan 0003
  lands, it owns the refactor that adds a `body: BodyId` field
  and the geocoder-skip-for-Mars logic. Plan 0003's M0
  explicitly absorbs that refactor scope (TODO: update plan 0003
  to reflect this).
- **Offline behavior on web.** A user with no network gets a
  parse-able coord input that works (M0 is offline) but a
  geocoder dropdown that hangs. v1 surfaces a 5-second timeout
  on the geocoder fetch and renders "geocoder unreachable" in
  the dropdown; deeper offline UX (cached results, query
  history) is out of scope. Open to revisit if the offline case
  matters more in practice than expected.
- **Photon Komoot ToS clarification.** Photon's hosted instance
  asks for "reasonable use" but provides no formal cap. If the
  live demo's traffic ever generates a complaint, the immediate
  fallback is self-hosting (Photon is BSD; `photon.komoot.io`
  docs the Docker image). Until then we cite the request shape
  (one request per 250 ms during typing, no autocomplete
  without input) in the README.
- **POI category filter.** Photon supports filtering by `osm_tag`.
  Useful eventually; not in scope for v1.
- **Recents / history.** A list of recent searches is a small,
  obvious win. Out of scope for v1 — LocalStorage state has its
  own design surface.

## Done when

- **Coord parser:** `parse_coord("41.87, -87.63")` returns
  `Some((-87.63, 41.87))` (lon-first). All three formats
  (decimal-comma, hemisphere-letter, DMS) round-trip the four
  test cities (Chicago, Tokyo, Sydney, Reykjavík) within
  `1e-6°`. Out-of-range input returns `None`.
- **Geocoder:** `GeocoderClient::geocode("Chicago", None)`
  returns a `Vec<SearchResult>` whose first element has `kind
  == ResultKind::City`, `name == "Chicago"`, and `lonlat`
  within `(−87.7 ± 0.5, 41.9 ± 0.5)`. (Integration test,
  `#[ignore]`'d on CI.)
- **Fly-to slerp:** at `t = 0.5` for a Reykjavík → Sydney fly,
  `lat < 0` — i.e. the path crosses the southern hemisphere,
  not the equator. (Unit test; pinned values in `t=0`, `t=1`,
  midpoint within `1e-9`.)
- **Fly-to bbox:** `fly_to_bbox(reykjavik_bbox)` lands the camera
  with the bbox occupying ≥80% and ≤95% of the canvas's
  shorter dimension. (Unit test; margins documented above.)
- **Cancellation:** mid-`fly_to`, a single `pan(1.0, 0.0,
  canvas)` clears the animation state to `None` on the same
  frame. (Unit test.)
- **Debounce:** a JS-side test scenario types
  `"C", "Ch", "Chi"` within 100 ms each and observes exactly
  one Photon request fire 250 ms after `"Chi"` (verified via
  a mock `GeocoderClient` that counts calls).
- **No-network UX:** with the geocoder unreachable, typing
  `"Topeka"` shows "geocoder unreachable" in the dropdown
  within 5 seconds. (Manual; covered by Open questions.)
- **Native parity:** `Renderer::search_and_fly_to("Chicago")`
  works on native — `cargo run` + that call flies the camera
  to Chicago without any web chrome.
- **The standard floor** (this is for every aeGIS commit, not
  a milestone-specific assertion): all four milestones pass
  `cargo test --all-targets` and `cargo check --target
  wasm32-unknown-unknown --lib`.
- **Live demo** at `timthirion.github.io/aeGIS` shows a working
  search bar top-centre; typing the four `Done when` queries
  above produces the expected behaviour.

## Plan-skeptic attacks addressed

(Recorded for the next reviewer — the initial draft was attacked
by `plan-skeptic` on 2026-06-09; relevant fixes folded into the
sections above.)

1. **JSON HTTP plumbing didn't exist** — added `src/net.rs` as
   the shared `Vec<u8>`-returning HTTP layer; `tile.rs` now
   sits on it. The "reuses existing plumbing" claim has a
   concrete module to point at.
2. **Lon/lat lerp is a rhumb line, not a great circle** — fly-
   to now uses slerp on the unit sphere. Tests pin the
   Reykjavík → Sydney case where lon-lerp would (incorrectly)
   cross the equator.
3. **Bbox-fit was hidden behind a `fly_to(target, zoom)`
   signature** — split out as `fly_to_bbox(bbox)` with both
   flat and globe zoom-fit paths spec'd.
4. **Native+web lockstep silently broken** — split made
   explicit: web owns the UI, native gets `Renderer::
   search_and_fly_to(&str)` as a first-class headless API.
   AGENTS.md gets an addendum (TODO during M2) noting that
   future UX may ship web-first when no comparable native
   surface exists.
5. **Visual-regression PNG was just a screenshot with no
   compare step** — milestone language downgraded: a manual
   reference shot is committed, but it's not a regression test
   until a perceptual-diff harness lands (separate plan).
6. **Plan 0002 ↔ 0003 inconsistency on body-aware search** —
   resolved by deciding plan 0002 ships Earth-only and plan
   0003's M0 absorbs the body-aware refactor. Open questions
   documents the decision.
7. **Done-when criteria weren't falsifiable** — every line in
   the new Done-when has a specific assertion or test name.
   The "smoothly" word is gone.
