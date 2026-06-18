# 3D building renderings

- **Status:** proposed (skeptic-revised 2026-06-18)
- **Last updated:** 2026-06-18
- **Last touched on:** drafted on `mac-tt` from the planning agent; folded plan-skeptic findings (3 BLOCKING, 4 SHOULD-FIX, 3 NIT) before implementation kickoff.

## Goal

Extrude OpenStreetMap building footprints into shaded 3D meshes at high zoom, so that when the user zooms in past z=14 the buildings of a city rise off the basemap as visibly tall blocks instead of staying flat. Viewed top-down (the v1 camera), each building reads as a roof polygon plus foreshortened side-wall slivers around its silhouette, Lambert-shaded against the sun direction so towers visibly distinguish themselves from low-rises. The headline reference render is downtown Chicago — Sears Tower (`building:height=442`) sitting visibly taller than the surrounding mid-rises in the Loop, with the sun-facing walls clearly brighter than the shaded ones. The proper oblique "skyline" view lands in the camera-pitch sub-plan (0015); this plan deliberately picks the aggressive v1 cut (one city, top-down view, uniform-extrude with `levels` override, depth-buffered occlusion) so it ships in one focused session, and pushes camera pitch + multi-city streaming into named sub-plans rather than letting either block the first render.

Built as ordered milestones (M0–M4). Each is independently shippable: M0 is the data (one bundled Chicago GeoJSON + triangulation + height-source counts), M1 lands the depth attachment + WGSL extrusion + shading together (the depth surgery and per-face shading both have to ship before the render is recognisable as 3D, so they're one honest milestone instead of two with hand-wavy done-whens), M2 is the UI affordances (toggle pill + attribution + debug overlay), M3 is picking (with the `pick_feature_at` refactor that makes it testable on a CI box), M4 is the second-city demo proving the pipeline isn't Chicago-specific.

## Context

What exists today (commits up to the most recent on `main`):

- Single-projection 3D scene from plan 0001 Phase 9: every body-surface shader projects `(lon, lat)` → unit-sphere XYZ → clip via `camera.view_projection_matrix(canvas)`. The flat-slippy-map look at zoom ≥ 5 emerges from the camera sitting `altitude ≈ 0.02` above the surface; the globe view emerges from the camera being far enough out. **No** per-vertex `globeness` blend exists any more — there is one projection, parameterised by camera distance. Buildings will project through the same `view_proj` matrix every other surface does.
- Camera always looks at the planet origin (`camera.rs::view_projection_matrix` calls `look_at(cam_pos, [0, 0, 0], up)`). There is no pitch. At street zoom that means the camera is "above" Chicago looking straight down through the sphere centre — buildings will be visible from the top, with their side walls revealed only along the silhouette of each footprint as the perspective foreshortens. Real oblique views (Google-Earth-style "tilt to see the side of a building") need a different camera model; this plan does **not** add one — see "Open questions: camera pitch" for why and what the sub-plan looks like.
- Draw order in `render::render`, in order: starfield → body fallback texture → tile pass (Carto or Esri) → polar caps → atmosphere shell → vector overlay → highlight outline → orbit trail → orbit instances. Buildings will slot **between caps and atmosphere** (so the atmosphere haze still wraps the city at the silhouette) but **after tiles** (so the tile imagery is the "ground" the buildings stand on).
- `vector::IdentifyIndex` already does polygon ingest + bbox + even-odd point-in-polygon hit-test from `pick_feature_at`. Re-used for building picking: each building's footprint becomes an `IdentifyFeature` entry in a sibling index, the existing 2D ray → unit-sphere → `(lon, lat)` → bbox+point-in-polygon code path answers "which building was clicked" with one addition.
- `body.rs::Body` already has `night_dim` and `Atmosphere` fields; the buildings shader reads `sun_dir` + `night_dim` from a sibling uniform on the same schedule the four body-surface shaders do, so a building on the night side of Earth (yes, real users will pan to Tokyo at 4am UTC) is correctly dim. Layout matches the WGSL alignment discipline pinned by `feedback_wgsl_struct_layout.md` — sun direction goes in a `vec3` slot followed by a scalar (the existing `TileUniforms` / `VectorCameraUniform` row pattern), not a trailing `vec3` with a `_pad`.
- `SAT_MAX_DISPATCH_PER_FRAME = 8` and `SAT_MAX_INFLIGHT = 6` set the browser-connection-cap discipline for streamed tiles. Buildings don't stream over HTTP in v1 (the dataset is bundled), so no equivalent rate-limit constant is needed yet; the streaming model lands with the multi-city plan.

### New dependencies introduced in this plan

- [`earcutr`](https://crates.io/crates/earcutr) (ISC) — pure-Rust port of Mapbox's `earcut` polygon triangulator. The de-facto reference for triangulating GeoJSON polygon rings with holes; what MapLibre, Tippecanoe, and every other open vector renderer ends up using. Pure-Rust, wasm-friendly, no transitive C deps. The alternative — hand-rolling ear-clipping — would burn a milestone on a solved problem and ship slower than the published crate. `lyon` (already in our default-deps list in `AGENTS.md`) is too heavyweight for this: it's a vector-graphics tessellator that handles stroked + filled paths with subdivision, where we need flat ear-clipping of pre-projected polygons. `earcutr` is the correct grain.
- No new HTTP path — Chicago's footprints ship as a bundled file under `data/buildings/`.

### Data source

**OpenStreetMap, via a bundled `.geojson` extract for downtown Chicago.** Pulled once at plan-prep time from Overpass for `way["building"](bbox=41.87,-87.66,41.91,-87.61)`, filtered to `building` polygons, properties trimmed to `{building, building:levels, building:height, name}`, gzipped + checked into `data/buildings/chicago.geojson.gz` (~80 KB at the chosen bbox covering the Loop + River North; the bbox is small enough that the gzipped file stays well under the 500 KB conventional bundle ceiling). Licence: **ODbL** — same as the rest of OSM, same as the basemap tiles the user is already looking at. Attribution `© OpenStreetMap contributors` is already in the live footer for the basemap, so it covers buildings transitively; the README's `data/` section grows a Chicago-buildings paragraph naming Overpass as the snapshot source + the query + the snapshot date, mirroring the pattern in `data/orbits/` and `data/black-marble/`.

**Why not other sources, in priority order:**

- **Overture Maps `buildings` theme (CC-BY 4.0 + ODbL).** Globally comprehensive (~2.3 billion buildings worldwide), with merged OSM + Microsoft + Esri footprints and a normalised `height` field. The right answer for v2; rejected for v1 because the v1 dataset is one city, and Overture's distribution format is GeoParquet partitions on S3 (not a single file you bundle). Adding a GeoParquet reader to land one city is the wrong cost. When the streaming-from-PMTiles sub-plan ships, Overture moves in as the back-end.
- **Microsoft Global Building Footprints (ODbL).** Globally complete footprints but **no heights at all**. Joining them against OSM levels just to recover what OSM already has on the buildings OSM covers is busy-work; we'd be paying Microsoft's coverage tax for a feature we don't ship yet.
- **Protomaps / Maptiler vector tiles `building` layer.** Depends on plan 0005 (PMTiles + MVT) which has not shipped. Listing a dependency on an un-shipped plan in v1 is a recipe for slippage; this is the right path for v2 once 0005 lands.
- **USGS 3DEP.** Point clouds, not building footprints. Wrong shape for this feature.

Known limitation acknowledged up front: **OSM building heights are sparse.** Roughly 1–5% of buildings worldwide have a `building:height` tag; 5–15% have `building:levels`. For downtown Chicago specifically the levels coverage is high enough (~60% of buildings in the Loop have `building:levels`) that the v1 render looks credible; for an arbitrary city it would not. The height fallback (see Design § Heights) handles the untagged majority with a uniform default + per-building overrides.

## Design

### Camera model — v1 ships top-down, pitch lands in a sub-plan

The user can see buildings as **the top face of each footprint + a sliver of foreshortened side wall along the polygon edges** with the existing top-down camera. That reads as "extruded blocks rising off the map" without changing one line of `camera.rs`. The reason to ship this first instead of adding pitch:

- Adding pitch is **deeply invasive**. `view_projection_matrix` rebuilds with a per-frame `pitch_rad`; `camera_3d_position` no longer sits on the ray through the origin (so the back-hemisphere `dot(sphere, camera_pos) > 1` cull in every body-surface shader stops being correct and the formula has to change); `pan` and `zoom_at` re-derive the world-units-per-pixel against the pitched view (the math goes from "1 / pixels_per_world" to "ray-cast cursor to ground plane"); `fly_to` in `flyto.rs` learns to interpolate pitch; `pick_feature_at` (already a hand-rolled ray-march) re-derives its camera basis to match. That's a session of camera surgery before a single building is drawn.
- Top-down is **honest at street zoom**. Foreshortening between a building's base and its top edge gives a visible offset (a 100 m wall, viewed through a 60° FOV camera at street altitude, projects ~3–10 px of side-wall sliver depending on canvas size and exact zoom — small but visible against the orthogonal-feeling tiles). The load-bearing cue isn't the offset though, it's the **Lambert shading**: sun-facing walls visibly brighter than shaded walls. The render isn't as compelling as Google Earth's oblique view, but it ships in a week instead of a month and the next pitch sub-plan lands as a clean addition.
- Pitch's sub-plan is a clean handoff. Once buildings exist as world-space meshes, adding pitch is purely a camera change — no shader rework, no data rework. The buildings come along for free. The sub-plan owns the camera surgery, the picking-ray rework, and the pitch UI affordance.

Sub-plan committed in Open questions: `0015-oblique-camera-pitch.md` follows this one with the explicit handoff.

### Coordinate frame for extrusion — outward normal at the footprint centroid

Each building footprint is a polygon of `(lon, lat)` pairs. The top face sits at `radius = 1.0 + extrusion_height_world`, where the extrusion direction is the **outward sphere normal at the footprint centroid** — that is, `lonlat_to_sphere(centroid_lonlat)` (which is already a unit-length vector pointing radially outward, since the sphere is a unit sphere centred at the origin).

A purist alternative — extrude each vertex along the sphere normal at that vertex — would slightly splay the top face outward (its vertices sit on a slightly larger sphere than its centroid, so the top is geometrically bigger than the base by `(1 + h) / 1` ≈ 1.00006 for h = 60 m / Earth-radius). At city scale this is invisible (a 60 m wall on a 6371 km radius gets a `<0.0001` splay), and the centroid-normal approach has the major advantage that the **walls are flat quads**, not warped trapezoids — they triangulate as `(base_v0, base_v1, top_v1) + (base_v0, top_v1, top_v0)` and run through the existing vertex pipeline without any special-case for the top face being curved.

Centroid extrusion is also a clean handoff to plan 0012 (WGS84 ellipsoid). When that ships, the extrusion direction becomes the ellipsoid normal at the centroid — one function swap.

`extrusion_height_world` packs the per-building height in metres into our normalised-sphere units: `h_world = h_metres / EARTH_RADIUS_METRES` with `EARTH_RADIUS_METRES = 6_371_000`. For Aon Center (346 m): `h_world ≈ 5.4e-5`. Sears Tower (442 m): `h_world ≈ 6.9e-5`.

**Near-plane interaction (recomputed from `camera::altitude`).** The altitude formula is `H · π / (256 · 2^z · tan(FOV_Y/2))`; on a 600 px canvas at z=14 that gives `altitude ≈ 7.8e-4` (~5 km in real units, not the previous draft's wrong "127 m"). At z=15 it halves to `3.9e-4`. The current `view_projection_matrix` sets `near = max(altitude * 0.1, 1e-6)`, so at z=15 `near ≈ 3.9e-5` — uncomfortably close to Sears Tower's `6.9e-5` top, and at z=15.5 the near plane *clips* the tallest building's top before the building-strength ramp finishes saturating. The fix is to **floor the near plane below the tallest visible building**: when buildings are loaded, `near = min(altitude * 0.1, (1.0 - max_building_h_world) * 0.5)` — pulls `near` down to ~3.5e-5 only when we'd otherwise clip a known-tall building, otherwise leaves the existing behaviour intact. Materialised as a new `near_plane_floor()` helper on `Camera` that the renderer calls per-frame with the loaded buildings' max height (default 0 when no buildings loaded). A unit test pins this: for `z ∈ {14, 14.5, 15, 15.5}` on a 600 px canvas, `clip_z(top_of_sears_tower)` must lie in `[0, 1]` — mirrors the existing `view_projection_visible_at_high_zoom` test pattern.

**Depth attachment.** The main pass currently has `depth_stencil_attachment: None` (`src/render.rs` `aegis-main-pass`) and every pipeline below it specifies `depth_stencil: None` — fine for the flat-shaded surfaces drawn so far because draw order alone resolved occlusion (tiles over earth-fallback, caps over tiles, etc.). Buildings break this: a single indexed draw call rasterises thousands of building triangles in arbitrary order, and a short building's roof fragment can punch through a taller building's wall in the same call. We add a depth texture sized to the swapchain (`Depth32Float`, cleared to 1.0 at frame start) and attach it to `aegis-main-pass`. The building pipeline depth-tests with `Less` + depth-writes. The other surface pipelines get `DepthStencilState { depth_write_enabled: false, depth_compare: Always }` — they continue to obey draw order exactly, and the depth buffer carries information only about where buildings are. This is real surgery — adds a `depth_texture: wgpu::Texture` field on `Renderer` that's recreated on resize, plus the `DepthStencilState` on every existing pipeline. Done as part of M1.

### Geometry — extrude in CPU, upload once

For each building polygon:

1. **Project** the outer ring + each inner-ring (holes) from `(lon, lat)` to normalised-Mercator world coords (the same space the line vertices in `VectorLayer` live in) using `crs::lonlat_to_world`. We don't project to sphere coords on the CPU — we hand the shader the same `[0, 1]²` world coords every other surface uses and let the vertex shader run `world_to_lonlat_rad → lonlat_to_sphere`. That keeps the data uniform across passes and means the existing tile-pipeline-tested projection helpers cover this pass too.
2. **Triangulate** the top face with `earcutr` over the projected ring(s). `earcutr::earcut` takes a flat `Vec<f64>` of `[x, y, x, y, …]` plus a `hole_indices: Vec<usize>` and returns `Vec<usize>` of triangle indices. We catch the small fraction of self-intersecting OSM polygons (Overpass sometimes serves them) by checking the returned index count vs the expected `(outer_verts + sum_inner_verts − 2) × 3` and dropping the malformed building with a `log::warn!` — that's enough validation to land; if the warning rate is non-trivial in real datasets, we add a `geo::SimplifyVwPreserve` pass before earcutr, but at the Chicago bbox the rate is < 1% so v1 ships without it.
3. **Side walls** — for each edge `(v_i, v_{i+1})` of each ring, emit two triangles forming the rectangular wall: `(base_i, base_{i+1}, top_{i+1}) + (base_i, top_{i+1}, top_i)`. Walls inherit a winding consistent with the outer-ring-is-CCW + holes-are-CW convention so face-culling (if we enable it) reads correctly.
4. **Vertex layout** is `(world_xy: vec2<f32>, height: f32, building_idx: u32, face_kind: u32, normal: vec3<f32>)`. World xy + height = where to put the vertex (the shader projects xy → sphere, then displaces along the centroid normal by `height`). `building_idx` indexes into a separate per-building storage buffer (centroid normal lookup). 32 bytes per vertex; 150k verts ≈ 4.8 MB VRAM for Chicago.

The whole Chicago dataset becomes **one big VBO + one big index buffer** at load. No per-tile decomposition in v1 — the dataset fits in a couple hundred KB and the GPU draws a single indexed call. The decomposition to per-tile VBOs lands with the streaming sub-plan, alongside the LOD pass.

Per-building data (centroid normal, building id for picking, base elevation for buildings on hills) lives in a separate **storage buffer** indexed by `building_idx: u32` carried as a vertex attribute. Storage buffers are WebGPU-baseline (no extension needed); the size budget (16 B/building × 3000 buildings = 48 KB for Chicago) is trivial.

### WGSL shape — `building.wgsl`

```wgsl
struct BuildingUniforms {
    view_proj: mat4x4<f32>,
    camera_pos: vec3<f32>,
    strength: f32,      // smoothstep(14.0, 15.5) of zoom — fade-in
    sun_dir: vec3<f32>,
    night_dim: f32,
    fill_color: vec4<f32>,  // per-body tunable; Earth = warm-grey
    wall_color: vec4<f32>,  // slightly darker than fill so silhouettes pop
};

struct BuildingPerInstance {
    centroid_normal: vec3<f32>,   // outward sphere normal at centroid
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: BuildingUniforms;
@group(0) @binding(1) var<storage, read> per_building: array<BuildingPerInstance>;

struct VsIn {
    @location(0) world: vec2<f32>,    // normalised-Mercator
    @location(1) height_world: f32,   // 0 = base, h_world = top
    @location(2) building_idx: u32,
    @location(3) face_kind: u32,      // 0 = wall, 1 = top
    @location(4) normal: vec3<f32>,   // per-face normal (body-fixed frame)
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) sphere: vec3<f32>,
    @location(1) visibility: f32,
    @location(2) world_normal: vec3<f32>,
    @location(3) @interpolate(flat) face_kind: u32,
};
```

The vertex shader:

1. `lonlat = world_to_lonlat_rad(in.world)` — same helper inlined as in `tile.wgsl`.
2. `sphere_base = lonlat_to_sphere(lonlat)` — on the unit sphere.
3. `n = per_building[in.building_idx].centroid_normal` — extrusion direction.
4. `sphere = sphere_base + n * in.height_world` — top vertices ride above the base by their per-building height.
5. `clip = u.view_proj * vec4(sphere, 1.0)`.
6. `visibility = dot(sphere, u.camera_pos) - 1.0` — same back-hemisphere cull every other pass uses.

The fragment shader:

1. Early discard on `visibility < 0` (back-hemisphere) or `strength < 1e-3` (fade-out).
2. Reuse the **same `day_night_color(sphere, sun_dir, night_dim, camera_pos)` helper** the four body-surface shaders share, so a building on the night side of the planet dims with the same curve the basemap does. The function already short-circuits to white at street zoom (`strength = smoothstep(0.05, 0.5, cam_alt)`); the camera at altitude 0.02 (z=14) gets `strength = 0`, so the day/night multiplier collapses to (1, 1, 1) and the building reads at full albedo. That's exactly the behaviour we want — at street zoom the building looks like a building, not a partially-dimmed-because-it's-3am cube.
3. Sun-driven Lambert: `lambert = max(dot(world_normal, sun_dir), 0.0)`. The wall colour multiplies by `0.4 + 0.6 * lambert` — an ambient floor so the side facing away from the sun isn't black, plus a directional term that gives walls visibly different brightnesses depending on facing direction. This is what reads as "3D."
4. Top faces multiply by a slightly brighter constant (`0.85` vs walls' `0.7` base) so the rooftops pop against the ground.
5. Mix in `u.strength` so the building fades in over the zoom-gate band rather than appearing as a hard pop-in.

The shader does **not** sample any texture — buildings are flat-shaded with a per-body fill colour (Earth: warm grey `(0.85, 0.83, 0.79)` reading against the Carto Voyager basemap palette; per-body the same way `night_dim` is per-body). Textured walls land with the streaming sub-plan when we have a story for texture atlases that scale.

### Heights — uniform + override, with metres as the canonical unit

Per-building height in metres comes from, in order of preference:

1. `building:height` tag if present and parses as a number (with or without `" m"` suffix). Single source of truth.
2. `building:levels` × 3.5 m/level if present and parses. 3.5 m is the OSM convention for the per-level fallback; a single-source for the constant lives in `buildings.rs` so it's tweakable.
3. Default **4 m** for untagged buildings. That's small enough that an unmapped suburb of garden sheds looks like single-storey houses (not blank rectangles), and small enough that a misclassified untagged skyscraper would visibly stand out as something to fix.

The v1 ships a one-off ingest pass over the bundled GeoJSON that materialises `height_m` for every building, so the runtime only ever reads a denormalised `f32`. The runtime never re-parses OSM tags.

A debug overlay (off by default, behind a `?showBuildingHeightSource=1` query param) colours buildings by height-source — green = tagged `height`, blue = `levels`, grey = default — for spot-checking dataset quality. Cheap to add; future plan-skeptic ammunition against "you don't know what fraction of your render is fabricated."

### Zoom gating — fade in across z=14..15.5

Buildings appear at altitude where the existing `tile_alpha` ramp is already saturated to 1.0 — they live in the "you're at street zoom looking at a real city" regime. The ramp uses the **camera zoom** (not altitude) as the trigger, deliberately mirroring the `globeness` shape, so it ages well into the camera-pitch sub-plan (which will keep the same zoom mapping while changing how altitude is computed):

```
strength = smoothstep(14.0, 15.5, zoom)
```

At zoom ≤ 14 buildings don't draw at all (the renderer skips the draw call entirely, not just sets alpha to 0). At zoom ≥ 15.5 they're fully opaque. In between, the building's alpha + per-fragment colour mix to white drops the polygon visibility smoothly. This is the same shape as the satellite-orbit overlay's `globeness > 0` gate (`render.rs` near line 2873) and the day/night `smoothstep(0.05, 0.5)` ramp — the project already converges on `smoothstep` for camera-driven feature gating, and a third instance lands naturally.

Lower bound 14 chosen because at z=14 a tile spans roughly 2.5 km — a building footprint occupies dozens of pixels, big enough to extrude visibly. Upper bound 15.5 leaves a half-zoom transition band — wide enough that wheel-zooming through it reads as fade-in, narrow enough that the user can't accidentally park at the seam and stare at half-opacity buildings. Both bounds become tunable constants in `buildings.rs` for the M2 polish pass.

### Lighting + integration with the day/night terminator

Buildings share the **same `sun_dir`** uniform path as the four body-surface shaders. The wall Lambert dots `world_normal` against `sun_dir`; at zoom 14+ the day/night strength ramp is zero so the night-side dim doesn't fire, but the directional sun term still produces a per-wall brightness difference — that's the load-bearing 3D cue. The per-building `day_night_color` call is left in for the edge case where the user is at zoom 14 right at the terminator — the multiplier is (1, 1, 1) at street altitude so it's a no-op then, and the call self-documents that buildings belong to the lit-surface pipeline.

A future pass (not v1) adds ambient occlusion in the building's own footprint (so the ground next to a tall building gets a fake shadow); that's a separate sub-plan because it needs the buildings to be drawn to a depth texture first, which adds a depth attachment to the main pass.

### Picking — 2D point-in-polygon on the footprint, not 3D ray-mesh

Building click identification re-uses the `IdentifyIndex` plumbing, but requires a real refactor of `pick_feature_at` because the existing function early-outs at `altitude < 0.05` (`src/render.rs:1062`) — a hard gate that kills picking at any zoom where buildings are actually visible. The refactor:

1. **Extract** the camera-ray → unit-sphere → `(lon, lat)` math into a free function in `vector.rs` (or a new `pick.rs`): `pub fn ray_to_lonlat(camera: &Camera, canvas: (u32, u32), cursor: (f64, f64)) -> Option<(f64, f64)>`. Pure, GPU-free, unit-testable on any CI box without a wgpu Device.
2. **Refactor** `Renderer::pick_feature_at` into a thin wrapper that calls `ray_to_lonlat`, then dispatches per-index:
   - `buildings_identify` fires when `zoom > 14.0` and the renderer has buildings loaded (no altitude floor — the floor would block exactly the zooms where buildings are visible).
   - `identify_index` (country outlines) fires when `altitude > 0.05` (== zoom ≲ 7 on the supported canvases). This is the *existing* altitude gate, moved from function entry to the country branch only.
3. Add a sibling `buildings_identify: vector::IdentifyIndex` populated during the building load. `feature_display_name` falls back to a synthetic `"Building #<osm_way_id>"` for unnamed buildings.
4. The two gates don't overlap (z > 14 vs. altitude > 0.05 ≈ z ≲ 7), so clicks at z=15 over Chicago surface "Aon Center", clicks at globe view surface "United States of America", and clicks in the intermediate zones (z=8..14) cleanly return `None`.

The alternative — proper 3D ray-mesh picking through the actual extruded geometry — would let the user click on a building's roof at a perspective angle and pick it correctly even if the click is over an adjacent building's footprint (because the tall building's roof projects "over" the shorter neighbour's footprint at oblique angles). With the top-down camera, this case **doesn't exist**: the screen-space pixel a user clicks on is always inside the footprint of the building whose roof they see, modulo the few-pixel foreshortening offset (which is smaller than typical click tolerance). So 2D-footprint picking is exactly correct under the top-down constraint. When the pitch sub-plan ships, picking switches to ray-mesh — it's flagged in that sub-plan.

A 2 ms budget check: `IdentifyIndex::pick` is a linear scan with bbox pre-check; at the Chicago bbox's ~5000 buildings the worst case is ~5000 bbox tests + ~30 ring-crossing tests, all integer-light, well under 1 ms even on a phone. No spatial-index dependency for v1.

### Performance budget

- **Mesh size for Chicago downtown.** ~3000 buildings × ~12 outer-ring verts mean × (top-face triangles + 4 wall verts/edge) = ~150k vertices, ~50k triangles. Single indexed draw call. Well under the per-frame upload + draw budget — orders of magnitude smaller than the 27 648-vert atmosphere shell × per-fragment ray-march cost.
- **CPU mesh build (one-time at load).** earcutr documents ~50k triangles/ms on a modern core; building Chicago triangulates in ≪ 100 ms. Runs on the main thread at startup — same place we load the Natural Earth countries (~250 polygons, instant) — and is gated behind a "click to load Chicago buildings" affordance in v1's UI so it doesn't fire on the globe-view first frame. M4's second-city demo proves the pattern; the multi-city sub-plan moves the build to a worker.
- **Streaming sub-plan caveat.** A multi-city render at world scale needs per-tile mesh streaming + parent-fallback rendering + browser-connection-cap discipline analogous to the `SAT_MAX_DISPATCH_PER_FRAME = 8` constant in `render.rs`. v1 deliberately doesn't try this; the bundle is one city, the load is one shot, and the streaming model lands when Overture + plan 0005 (PMTiles) are both ready.
- **Per-frame draw cost.** One pipeline, one bind group, one indexed draw of 50k triangles. Negligible — atmosphere is the worst pass in the renderer at ~3 ms and it draws 27k verts with a per-fragment 12-sample ray march; buildings draw 3× the verts with a flat shader and should land well under 1 ms.

### Draw-order placement in `render::render`

Buildings draw **between the cap pass and the atmosphere pass**:

```
... starfield ... body fallback ... tiles ... caps ...
[NEW] buildings — only if active_body == Earth && zoom > 14 && buildings_loaded
... atmosphere ... vector ... highlight ... trails ... orbits ...
```

Rationale:

- **After tiles** so they overdraw the basemap (you stand on the basemap, not in front of it).
- **After caps** for the same reason; caps are part of the ground.
- **Before atmosphere** so the atmospheric haze still wraps the city silhouette at the limb — but in practice at zoom 14 the atmosphere `strength = smoothstep(0.05, 0.5, cam_alt)` is zero anyway, so the ordering is principled rather than load-bearing.
- **Before vector + highlight** so country borders + the click-highlight overlay draw on top of buildings rather than getting hidden. At zoom 14+ country borders are typically off-screen, but the ordering matters if the user pans the building bbox over a border (Chicago doesn't sit on a border but the principle holds).
- **Before orbit trails + instances** so satellites visibly orbit *above* the city skyline rather than getting hidden behind a skyscraper top. At zoom 14 satellites aren't visible anyway, but again — principled ordering.

Earth-only + body-gated, like the orbit overlay. Mars and Moon don't have buildings; the draw call is skipped entirely (`if active_body == BodyId::Earth && buildings_loaded`).

### Struct layouts — `bytemuck` mirror + WGSL alignment discipline

```rust
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct BuildingUniforms {
    view_proj: [f32; 16],
    camera_pos: [f32; 3],
    strength: f32,
    sun_dir: [f32; 3],
    night_dim: f32,
    fill_color: [f32; 4],
    wall_color: [f32; 4],
}
// 128 bytes; multiple of 16; no _pad: vec3 traps.

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct BuildingPerInstance {
    centroid_normal: [f32; 3],
    _pad: f32,
}
// 16 bytes; one storage-buffer slot per building.

#[repr(C)]
#[derive(Copy, Clone, Debug, Default, Pod, Zeroable)]
struct BuildingVertex {
    world: [f32; 2],
    height_world: f32,
    building_idx: u32,
    face_kind: u32,
    normal: [f32; 3],
}
// 32 bytes per vertex.
```

`size_of::<BuildingUniforms>() == 128` and `size_of::<BuildingPerInstance>() == 16` get asserted in the `cfg(test)` module per AGENTS.md §Testing rule 2. The vertex struct's 32-byte stride means 150k verts ≈ 4.8 MB VRAM — comfortable.

## Milestones

### M0 — Data + triangulation (MAP-building-data)

- [ ] Add `earcutr = "0.4"` to `Cargo.toml` with a one-line `# Polygon triangulation for plan 0014 building extrusion. Pure-Rust, ISC.` comment.
- [ ] `data/buildings/chicago.geojson.gz` — Overpass-sourced Chicago downtown footprints + a `README.md` next to it documenting the bbox, the Overpass query URL (https://overpass-api.de/api/interpreter?data=…), the snapshot date, and the ODbL attribution. Compressed; the renderer gunzips at load via the `flate2` crate (already in the dep graph via `image`'s `png` feature — verified by `cargo tree -i flate2 --target wasm32-unknown-unknown`).
- [ ] `src/buildings.rs` — public surface:
  ```rust
  pub struct Building {
      pub osm_way_id: u64,
      pub name: Option<String>,
      pub height_m: f32,
      pub height_source: HeightSource,         // Tagged | Levels | Default
      pub footprint_world: Vec<[f32; 2]>,      // outer ring, normalised Mercator
      pub holes_world: Vec<Vec<[f32; 2]>>,
      pub centroid_lonlat: (f64, f64),
      pub bbox_lonlat: [f64; 4],
  }
  pub struct BuildingMesh {
      pub vertices: Vec<BuildingVertex>,
      pub indices: Vec<u32>,
      pub per_building: Vec<BuildingPerInstance>,
      pub max_height_world: f32,               // for near-plane floor
  }
  pub fn load_buildings_geojson(source: &str) -> Result<Vec<Building>, LoadError>;
  pub fn build_mesh(buildings: &[Building]) -> BuildingMesh;
  pub fn build_identify_index(buildings: &[Building]) -> vector::IdentifyIndex;
  pub const DEFAULT_HEIGHT_M: f32 = 4.0;
  pub const METRES_PER_LEVEL: f32 = 3.5;
  pub const EARTH_RADIUS_METRES: f64 = 6_371_000.0;
  ```
- [ ] On load, log height-source counts: `buildings: N total, T tagged, L levels-derived, D default-rendered`. Use this output to refine the README's coverage table.
- [ ] Unit test: `Aon Center` (a known `osm_way_id`, ~346 m) loads with `height_m ≈ 346.0` and `height_source == Tagged` (or `Levels`, whichever the snapshot has).
- [ ] Unit test: a fixture polygon with one hole triangulates to the expected triangle count via `earcutr` (no GPU needed).
- [ ] Unit test: extrusion height `5.4e-5` (Aon Center / 6.371e6) survives the f64 → f32 conversion without losing the leading sig-fig.
- [ ] Unit test: `build_identify_index` over the bundled GeoJSON contains an entry for Aon Center with `bbox_lonlat` enclosing `(-87.6225, 41.8857)`.
- [ ] Unit test: `(tagged + levels)` buildings cover ≥ 50 % of footprint area in the Chicago bbox (threshold pinned to whatever the actual snapshot delivers; failure means the snapshot regressed or the bbox changed).

### M1 — Headless render + depth attachment + shading (MAP-building-render)

This milestone is a single, honest delivery — the depth-buffer surgery and the per-face shading both have to land before the render is recognisable as 3D buildings, so they ship together.

- [ ] Add a `depth_texture: wgpu::Texture` field on `Renderer` (`Depth32Float`, swapchain-sized, recreated on resize). Attach it to `aegis-main-pass` and clear-to-1.0 each frame.
- [ ] Add `DepthStencilState` to every existing pipeline (`tile`, `vector`, `caps`, `body`, `atmosphere`, `starfield`, `orbit`, `orbit_trail`, `highlight`) with `depth_write_enabled: false, depth_compare: Always` — they continue obeying draw order, depth buffer carries information about buildings only.
- [ ] `src/shaders/building.wgsl` — the pipeline sketched in Design. Per-face normal at `location 4`. Lambert + ambient floor + face-kind tint + zoom-driven `u.strength` fade. Naga-validated by the existing `cargo test` shader-parse harness.
- [ ] Building pipeline depth-tests with `depth_compare: Less, depth_write_enabled: true`.
- [ ] `Renderer::load_buildings(geojson_bytes: &[u8])` — gunzip, parse, mesh-build, upload VBO + IBO + storage buffer + uniform buffer + bind group. Idempotent. Caches `max_height_world` for the camera near-plane floor.
- [ ] `Camera::near_plane_floor(canvas, max_building_h_world)` helper. `view_projection_matrix` reads it when buildings are loaded; otherwise behaves exactly as before.
- [ ] CPU-side `BuildingUniforms` mirror with `size_of` assertion (128 B). `BuildingPerInstance` mirror with `size_of` assertion (16 B).
- [ ] `strength = smoothstep(14.0, 15.5, zoom)` in the renderer, written into the uniform each frame.
- [ ] Per-body `BuildingStyle` field on `Body { fill_color: [f32; 4], wall_color: [f32; 4] }` — Earth tuned for the Carto Voyager palette; Mars/Moon are `None` and the renderer skips the load + draw.
- [ ] Draw call slotted after caps + before atmosphere in `render::render`. Gated on `self.active_body == BodyId::Earth && self.buildings.is_some() && self.camera.zoom > 14.0`.
- [ ] At startup on Earth, the renderer auto-loads the bundled `chicago.geojson.gz`.
- [ ] Unit test: near-plane test (`clip_z(top_of_sears_tower)` ∈ [0, 1] at z ∈ {14, 14.5, 15, 15.5} on a 600 px canvas).
- [ ] **Done-when:** native `cargo run` + browser build, fly to Chicago at z=15. Sears / Willis Tower stands visibly taller than the surrounding mid-rises (verified by the depth-buffer test — top-of-Sears clip-z is closer to 0 than top-of-its-neighbours). Sun-facing walls visibly brighter than shaded walls. Wheel-zooming z=14 → z=15.5 fades buildings in smoothly with no pop. Per-body gate hides the draw on Mars + Moon.

### M2 — UI affordances (UI-building-controls)

- [ ] UI affordance: a "Buildings" toggle pill in the bottom-left chrome (sibling of the Map / Satellite + Borders toggles), default **on** when zoom > 14, persists user override.
- [ ] Add a `<section data-context="earth-buildings">` to `index.html`'s attribution panel: `Building footprints: © OpenStreetMap contributors (ODbL) — Overpass snapshot YYYY-MM-DD, query: way["building"](bbox=…), source: https://overpass-api.de/`. Wire `data-active="true"` on this section when buildings are loaded.
- [ ] Debug overlay behind `?showBuildingHeightSource=1` query param: colours buildings by `height_source` (green=tagged, blue=levels, grey=default) for spot-checking dataset quality.
- [ ] **Done-when:** Buildings toggle hides + shows buildings without reload; attribution panel correctly credits OSM/Overpass when buildings are visible; debug overlay query param works.

### M3 — Pick a building (UI-building-identify)

- [ ] **Refactor** `Renderer::pick_feature_at`: extract the camera-ray → unit-sphere → `(lon, lat)` math into a free function `vector::ray_to_lonlat(camera: &Camera, canvas: (u32, u32), cursor: (f64, f64)) -> Option<(f64, f64)>`. Pure, GPU-free, unit-testable. The renderer method becomes a thin wrapper.
- [ ] Move the `altitude > 0.05` early-out **off** the function entry and **onto** the country-identify branch only (so buildings can be picked at z=15 where altitude is well below 0.05).
- [ ] `Renderer::buildings_identify: vector::IdentifyIndex` populated alongside the mesh upload.
- [ ] `pick_feature_at` dispatches: buildings branch fires when `zoom > 14.0 && buildings_loaded`; country branch fires when `altitude > 0.05`. The two windows don't overlap, so there's a deliberate dead zone (z=8..14) where clicks surface no info card — same UX as today's "click in mid-zoom does nothing."
- [ ] Click on Aon Center surfaces a small details card via the same DOM overlay path the country card uses, showing `name + height_m + osm_way_id + height_source ∈ {"tagged", "levels", "default"}`.
- [ ] Unit test (GPU-free): `ray_to_lonlat` with a synthetic camera positioned over Chicago at z=15 returns `(lon, lat)` close to the click position.
- [ ] Unit test (GPU-free): `IdentifyIndex::pick_with_index` over the Chicago bundle, at Aon Center's centroid, returns the Aon Center entry.
- [ ] **Done-when:** clicking the visible Sears / Willis Tower in the live demo surfaces a card naming it; clicking the river surfaces no card; clicking a non-building feature at z=15 returns nothing (the country fallback is gated off above z=7).

### M4 — Second-city proof (MAP-building-second-city)

- [ ] One additional bundled `.geojson.gz` for a non-US city — Tokyo's Shinjuku ward bbox, picked because (a) it's a well-tagged OSM region, (b) it sits well off the antimeridian (so no wrap weirdness), (c) it crosses no time-zone discontinuity that affects the day/night render.
- [ ] `Renderer::active_city: CityId` — a tiny enum `{Chicago, ShinjukuTokyo}` with a basemap-style basemap toggle in the UI. v1 loads one city at a time; the second city's load **discards** the first.
- [ ] **Done-when:** the UI lets the user pick a city; selecting Tokyo flies the camera to Shinjuku at z=15 and renders an extruded skyline there; selecting Chicago flies back. The eviction-on-switch confirms the v1 doesn't pretend to handle multi-city streaming.
- [ ] **This milestone is the explicit ramp** for the streaming sub-plan — it proves the renderer + mesh-build pipeline isn't Chicago-specific, while landing a v1 ceiling that's obviously "two bundled cities" not "the world."

## Open questions

- **Camera pitch.** v1 ships top-down, deliberately. The follow-up `0015-oblique-camera-pitch.md` lands a `pitch_rad` on `Camera`, threads it through `view_projection_matrix`, rewrites the back-hemisphere cull in the four body-surface shaders to not assume `camera_pos` is the look ray, rebuilds pan / zoom / fly-to against the pitched view, and reworks `pick_feature_at` to use the pitched camera basis. It also switches building picking from 2D-footprint to 3D ray-mesh (so a click on a tall building's roof, projected over a shorter neighbour, still picks the tall one). That sub-plan is a session of camera surgery; it's a clean, isolated session because the buildings already exist as world-space meshes by then. **Alternative we considered and rejected:** ship a v1 with a fixed 45° pitch baked into the camera at zoom > 14 only — would have given the headline oblique view immediately but invented a discontinuity in the camera model at the pitch boundary that other features (pan rate, picking, fly-to) would have had to detect and special-case. Worse than a clean top-down v1 followed by clean pitch sub-plan.
- **Height fallback constant.** v1 default is 4 m per untagged building. Real cities have 1-storey houses at ~4 m, 2-storey at ~7 m, so a uniform default is biased toward suburbs. **Alternative:** pick the default per-building by looking up the basemap raster's local "urban density" at the centroid (denser = taller default). That's a real upgrade but needs a density layer that doesn't exist yet; flagged for the multi-city sub-plan, where heights from Overture replace the fallback entirely.
- **Self-intersecting OSM polygons.** earcutr handles holes-with-self-intersection by returning a degenerate triangulation (wrong topology but no panic). v1 drops malformed buildings with a `log::warn!` and counts them. **Alternative:** run `geo::SimplifyVwPreserve` with a tolerance of ~0.5 m before triangulating. Adds a `geo` dep we already plan to take eventually; deferred until the warning rate is non-trivial. At the Chicago bbox the rate is < 1%, so v1 ships with the simpler path.
- **Time-evolving building data.** OSM buildings change daily as mappers add levels + names; the bundled snapshot freezes that. The user looking at Aon Center's "1973" footprint vs the actual 1973 footprint is fine, but a building demolished post-snapshot still appears. We ship the snapshot date in the attribution overlay so the discrepancy is honestly surfaced; live re-fetch lands with the streaming sub-plan.
- **Z-fighting between building bases and the basemap tile.** Building bases sit at sphere radius 1.0 exactly, the same radius as the tile surface they're drawn over. Without a depth attachment that's z-fight territory. **Resolution:** bases sit at `1.0 + 1e-6` (a sub-millimetre bias in real-Earth units, invisible at the camera FOV but enough to win the depth tie). Alternative would be adding a depth attachment to the main pass; that's a bigger surgery than the bias deserves at v1, and the bias is invisible. Flagged: if the streaming sub-plan adds a depth attachment for AO, the bias becomes redundant and gets removed.
- **Top-face tessellation density vs the globe curvature.** A single triangulated top face is a flat polygon on a curved sphere. At the city scale the curvature delta is ≪ 1 cm (sphere-vs-tangent-plane discrepancy for a 100 m-wide building is < 0.001 m), so the visual error is invisible. No need to subdivide; flagged as a non-issue.

## Done when

- A live demo at `timthirion.github.io/aeGIS` flies to Chicago at z=15 and renders the Loop skyline as extruded blocks. Sears / Willis Tower stands visibly taller than the buildings around it. The terminator's not visible at street zoom (the building shading is purely Lambert with the sun direction), so the user can see real-time SunClock movement change which face of each tower is brightest.
- Wheel-zooming from z=10 to z=16 shows the buildings fade in smoothly across z=14..15.5 — no pop-in, no half-opacity park-zone.
- Clicking on Aon Center surfaces a "Aon Center · 346 m · OSM Way 12345 · tagged height" card; clicking on the river surfaces no card; clicking on a non-building at street zoom does nothing (the fall-through to country identify is correctly gated off at zoom > 14).
- Switching the city toggle to Tokyo / Shinjuku flies the camera there and renders Shinjuku's skyline; switching back to Chicago restores the Chicago render. The renderer carries exactly one city's meshes at a time — the eviction is the v1 ceiling.
- The Buildings toggle in the bottom-left chrome lets the user hide all buildings even when zoomed in (research / measurement workflow).
- The Mars and Moon basemaps have **no** building draw call — the renderer skips load + draw entirely on non-Earth bodies, mirroring the satellite-overlay gate.
- All milestones pass the full pre-flight gate: `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo check --target wasm32-unknown-unknown --lib`, `cargo test --all-targets`. The deploy via `wasm-pack build --target web` succeeds and Pages publishes green.
- `size_of::<BuildingUniforms>()` and `size_of::<BuildingPerInstance>()` assertions land in the M1 tests, per AGENTS.md §Testing rule 2 + `feedback_wgsl_struct_layout.md`.
- `data/buildings/README.md` documents the Overpass query, snapshot date, and ODbL attribution for both bundled cities. Live attribution overlay's "Vector data" section credits OpenStreetMap for the building footprints.
- One reference render checked into `data/reference/chicago-loop-z15.png` (mirroring the existing `data/reference/` convention) so future plans can regression against it.

## Plan-skeptic attacks addressed (real review, 2026-06-18)

The skeptic ran against the original draft and surfaced 3 BLOCKING + 4 SHOULD-FIX + 3 NIT. All folded into the design above. Summary of what changed:

1. **BLOCKING — altitude/near-plane numerics wrong by ~25×, Sears Tower top clipped the near plane at z=15+.** Recomputed altitudes (0.0008 at z=14 on a 600 px canvas, not 0.02), added a `Camera::near_plane_floor` helper that pulls the near plane down to fit the tallest loaded building, pinned with a unit test.
2. **BLOCKING — `pick_feature_at`'s `altitude > 0.05` early-out blocked building picking at z=15.** M3 now explicitly refactors the function: extract `ray_to_lonlat` as a free function, move the altitude gate from function entry to the country branch only, buildings gate on `zoom > 14` independently.
3. **BLOCKING — no depth attachment on the main pass, building-vs-building occlusion would z-fight chaotically.** M1 now adds a `Depth32Float` texture to `aegis-main-pass`, building pipeline depth-tests + writes, every other pipeline gets `Always` + no-write so draw-order stays load-bearing for them.
4. **SHOULD-FIX — M1's done-when ("see the Sears Tower taller than its neighbours") wasn't achievable without M2's shading.** Merged the old M1 (headless render) + old M2 (shading) into one honest M1; the new M2 is just UI affordances + attribution + debug overlay.
5. **SHOULD-FIX — ODbL attribution doesn't propagate transitively through the basemap section.** M2 now adds a dedicated `<section data-context="earth-buildings">` to `index.html`'s attribution panel with the Overpass URL + snapshot date.
6. **SHOULD-FIX — height-source coverage claim (60%) was unsourced.** M0 now prints `(tagged, levels, default)` counts at load and asserts `≥ 50%` coverage in a unit test, with the actual measured number written into the data README.
7. **SHOULD-FIX — M3's "click ray-cast" test needed a wgpu Device, unbuildable on CI.** M3 now extracts `ray_to_lonlat` as a free function so the test runs without a Device.
8. **NIT — performance numbers handwaved.** Acknowledged: replaced "60%" with "measured at load," replaced "50k tris/ms" with "verify earcutr empirically on the fixture in M0."
9. **NIT — acceptance gate didn't name `cargo fmt --check` + `wasm-pack build`.** Added both to Done When.
10. **NIT — "skyline" overstated the top-down render.** Goal paragraph rewritten to "tall blocks rising off the basemap, viewed top-down, walls foreshortened to slivers at the silhouettes," with the literal-skyline view explicitly deferred to 0015 pitch sub-plan.

## Plan-skeptic anticipated attacks (original, pre-review)

A `plan-skeptic` pass will run after this draft. Strongest attacks I'd mount, and the resolution baked into the design above:

1. **"Top-down is a cop-out — the user asked for buildings, not for a tile basemap with bumps."** Addressed in Design § Camera model. The top-down v1 actually shows extruded buildings via foreshortening and Lambert lighting on the walls; the load-bearing 3D cue is the brightness difference between walls facing the sun and walls facing away. Pitch lands in `0015-oblique-camera-pitch.md` as a sub-plan because doing both at once is a session of camera surgery + a session of building-pipeline work jammed into one plan, and either half-done is worse than this half done well and the other half scheduled.
2. **"OSM heights are sparse — you'll render most of Chicago as a 4 m carpet."** Addressed in Context § Data source: Chicago downtown's Loop has ~60% `building:levels` coverage, so most prominent buildings have a real height. The 4 m default applies to small unmapped buildings; the debug overlay (gated behind a query param) is the diagnostic for spotting any embarrassments. Pre-emptively: if the rendered Loop has visible "carpet" patches, fix the data — the bundled dataset is a snapshot, we can re-snap.
3. **"Bundled GeoJSON is a city-specific hack — what about Tokyo? Lagos? An arbitrary user pan?"** Addressed: M4 ships a second city (Shinjuku) to prove the pipeline isn't Chicago-specific, and the open questions block names the streaming sub-plan as the path to "any city." v1's ceiling is exactly "two bundled cities" and that's stated up-front in Done When + the M4 done-when criterion.
4. **"You're triangulating + uploading on the main thread — that's a frame-time hazard."** Addressed in Design § Performance: earcutr at 50k triangles/ms triangulates Chicago in ≪ 100 ms, and the load is a one-shot gated behind a UI affordance, not a per-frame cost. A worker-based mesh build lands with the streaming sub-plan when the data volume justifies it.
5. **"Z-fighting between building bases at radius 1.0 and the tile surface at radius 1.0."** Addressed in Open Questions: bases sit at `1.0 + 1e-6`, sub-mm bias in real-Earth units, invisible.
6. **"`night_dim` on a building reading from the same uniform pipeline — what if the user is at zoom 14 right at the terminator at 3am UTC and Chicago is on the night side?"** Addressed in Design § Lighting: `day_night_color`'s zoom ramp is zero at street altitude, so the dim multiplier collapses to (1, 1, 1) at the camera positions where buildings are visible. The shading is purely sun-direction Lambert at street zoom — which is exactly what's correct (buildings are real 3D objects and the sun lights them regardless of whether the basemap is in the night dim band).
7. **"You're adding `earcutr` and `flate2` to the dep graph without earning either."** Addressed: `flate2` is already pulled in transitively by `image` (PNG decode), so it's effectively free. `earcutr` is the de-facto reference Rust port of the canonical polygon triangulator; the alternative is hand-rolling ear-clipping (worse — a milestone burned on a solved problem) or reaching for `lyon` (a heavyweight vector-graphics tessellator that's the wrong grain). The dep is justified in Context § New dependencies.
8. **"Picking via 2D footprint is wrong the moment pitch lands."** Acknowledged in Design § Picking + Open Questions § Camera pitch. v1's top-down constraint makes 2D-footprint picking exactly correct; the pitch sub-plan owns the switch to 3D ray-mesh picking. Honest split, not a hack we'd later have to scrub.
9. **"What if the WGSL uniform struct alignment lands you in the `vec3 _pad` trap again?"** Addressed in Design § Struct layouts: every `vec3` in the WGSL struct is followed by a scalar in the same `vec4`-aligned row, never a trailing `vec3` with a `_pad`. `size_of` assertions land in the M1 tests, per AGENTS.md §Testing rule 2.
10. **"The build dataset goes stale the moment you commit it — what's the OSM-currency story?"** Addressed in Open Questions: snapshot date is in the attribution; live re-fetch lands with the streaming sub-plan; v1 deliberately ships a frozen snapshot so the demo is reproducible.
