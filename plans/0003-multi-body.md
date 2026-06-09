# Multi-body globes: Mars, Moon, and the path to fictional worlds

- **Status:** proposed
- **Last updated:** 2026-06-09
- **Last touched on:** drafted alongside plan 0002 (search) during
  the post-Phase-9 cleanup; nothing implemented yet

## Goal

Let aeGIS render any spherical body — starting with Mars and the Moon
from NASA's public-domain tile pyramids — as a first-class basemap
alongside Earth. The renderer's camera math is already body-agnostic
(everything projects to a unit sphere), so the lift is a `Body`
abstraction, a second tile-grid projection (Equirectangular, not just
Mercator), a body switcher in the UI, and per-body chrome (fallback
texture, cap colors, vector-overlay visibility). Stretch: a path for
fictional worlds (Middle-earth, etc.) once the architecture is body-
parametric, gated on finding cleanly-licensed tile sources.

Built as ordered milestones (M0–M4). M0 is the architectural refactor
that unlocks everything else; M1–M3 are body integrations; M4 is
polish.

## Context

What exists today (commits up to `750edb1`):

- One implicit body throughout the renderer: Earth. Constants
  baked in (`CHICAGO_LONLAT`, `EARTH_JPG_BYTES` for the Blue
  Marble fallback, hard-coded polar-cap colours in `render.rs`).
- `BasemapMode` enum in `src/render.rs` with two variants — `Map`
  (Carto Voyager) and `Satellite` (Esri World Imagery). Both
  variants assume Earth, both assume Web Mercator XYZ.
- `TileProvider` enum in `src/tile.rs` — same `(z, x, y)`
  addressing for both providers because they share the Web
  Mercator pyramid; only the URL pattern differs.
- Tile shader in `src/shaders/tile.wgsl` — `world_to_lonlat_rad`
  inverts Web Mercator (the `atan(sinh(...))` form). Hard-coded.
- Vector overlay (country outlines from Natural Earth) is drawn
  unconditionally — meaningless on Mars or the Moon, but currently
  has no way to be disabled.
- Polar caps drawn unconditionally in `render.rs`, blue + white
  hard-coded.
- Bundled Blue Marble equirectangular Earth texture
  (`data/blue-marble/earth_4096x2048.jpg`, ~3 MB) embedded via
  `include_bytes!` and used as the under-tile fallback so the
  globe shows *something* before tiles stream in.

### New dependencies introduced in this plan

None planned. The work is structural: type-level refactors, new
shader variants for Equirectangular tile unprojection, and asset
additions for per-body fallback textures.

### Data sources

All NASA Trek imagery is **U.S. Government work, public domain**
worldwide. Tiles are CORS-wildcard (`Access-Control-Allow-Origin:
*` verified 2026-06-09).

**Critical architectural note:** NASA Trek uses **Equirectangular
(Plate Carrée)** tile grids, *not* Web Mercator. The URL path
contains `/EQ/` (e.g. `tiles/Mars/EQ/...`). At z=0 the world is
**2 tiles wide × 1 tile tall**, covering −180°..+180° longitude
and −90°..+90° latitude with no Mercator stretch. This is a
different pyramid shape from Earth's XYZ — Earth's z=0 is one
256×256 tile covering ~±85° lat with Mercator distortion. The
multi-body refactor has to teach `Camera::visible_tiles` and
`tile.wgsl` about this second pyramid; "just swap the URL" doesn't
work.

- **Mars (primary basemap):** Viking MDIM 2.1 Color Mosaic, 232
  m/px globally. URL:
  `https://trek.nasa.gov/tiles/Mars/EQ/Mars_Viking_MDIM21_ClrMosaic_global_232m/1.0.0/default/default028mm/{z}/{y}/{x}.jpg`.
  Recognisably "Mars-red" without being just a shaded relief.
- **Mars (alternate basemap):** MGS MOLA Color Hillshade, 463 m/px.
  URL pattern with `Mars_MGS_MOLA_ClrShade_merge_global_463m`.
  Topographic colour ramp — useful as a "terrain" toggle once the
  basemap-toggle UI generalises.
- **Moon (primary basemap):** LRO LROC WAC global mosaic, 303
  ppd (~118 m/px equatorial). URL:
  `https://trek.nasa.gov/tiles/Moon/EQ/LRO_WAC_Mosaic_Global_303ppd_v02/1.0.0/default/default028mm/{z}/{y}/{x}.jpg`.
- **Per-body fallback textures:** A small (≤ 2 MB) equirectangular
  JPEG per body, embedded via `include_bytes!`, shown beneath
  unfetched tiles. Sources are downscaled exports from the NASA
  Trek mosaics above, public domain. Bundling avoids first-paint
  blank-globe state and keeps the offline experience working.

**Middle-earth and other fictional worlds:** documented in M4's
open question; the architecture should make them addable, but
sourcing cleanly-licensed Tolkien-world tiles is a real obstacle.
Not in v1 scope.

## Design

### Body abstraction

A `Body` describes everything that varies between worlds. It's a
value type owned by the renderer, with one instance per supported
body bundled into a static slice.

```rust
pub struct Body {
    pub id: BodyId,                 // Earth / Mars / Moon / ...
    pub display_name: &'static str, // "Earth", "Mars", "Moon"

    /// Equatorial radius in metres. Used by future precision work
    /// (search-result distance, ellipsoidal upgrades) — the
    /// renderer itself treats every body as a unit sphere in 3D.
    pub equatorial_radius_m: f64,

    /// Which prime-meridian / coordinate convention this body uses.
    /// Earth: WGS84-style (PM at Greenwich). Mars: areocentric +180°
    /// = Olympus Mons longitude convention. Moon: ME (Mean Earth)
    /// frame. Stored as an enum so the search/labelling layer can
    /// format coords correctly.
    pub crs_convention: CrsConvention,

    /// Available basemaps for this body. At least one. The first
    /// entry is the default.
    pub basemaps: &'static [Basemap],

    /// Default camera position on a fresh load for this body.
    pub home: HomeView,

    /// Fallback equirectangular texture shown beneath unfetched
    /// tiles. Per-body so Mars doesn't briefly look like Earth.
    pub fallback_texture: &'static [u8],

    /// Polar-cap colours, sRGB. Earth: pale Arctic blue + warm
    /// Antarctic ice. Mars: dust red (matches the polar caps' actual
    /// ice + dust mix at high lat). Moon: neutral grey.
    pub cap_colors: CapColors,

    /// Whether the Natural Earth country-outline overlay should
    /// render. False for everything except Earth.
    pub show_political_overlays: bool,
}

pub struct Basemap {
    pub id: BasemapId,
    pub display_name: &'static str,        // "Map", "Satellite", ...
    pub provider: TileProvider,            // existing enum, extended
    pub max_z: u8,
    pub attribution_html: &'static str,    // rendered into the footer
}
```

`TileProvider` grows from a two-variant enum into a `{ projection,
url_template, max_z }` record so adding a new body's basemap is
data, not code:

```rust
pub enum TileProjection {
    WebMercator,        // Earth (Carto, Esri)
    Equirectangular,    // NASA Trek (Mars, Moon)
}

pub struct TileProvider {
    pub projection: TileProjection,
    pub url_template: &'static str,   // "https://.../{z}/{y}/{x}.jpg"
    pub max_z: u8,
}
```

### Tile-grid math

`Camera::visible_tiles_capped` currently assumes Web Mercator
(viewport → world rect → tile addresses). For an Equirectangular
pyramid:

- At zoom z, the world is `(2 * 2^z)` tiles wide and `2^z` tall
  (the 2:1 aspect ratio matches the −180°..+180° / −90°..+90°
  range). Earth's z=0 is 1×1; Mars/Moon's z=0 is 2×1.
- World coords for Equirectangular: `world.x ∈ [0, 1]` maps
  linearly to `lon ∈ [-180°, +180°]`; `world.y ∈ [0, 1]` maps
  linearly to `lat ∈ [+90°, -90°]` (north at top). **No
  Mercator stretch** — the shader's `world_to_lonlat_rad` needs
  a per-projection variant.

The cleanest split: `visible_tiles_capped` dispatches on the
basemap's `projection`. The Mercator path is the existing code;
the Equirectangular path computes tile addresses directly from
lon/lat without the `lonlat_to_world` Mercator distortion.

For the shader, two options:

1. **Branch in the shader** on a uniform projection flag. Tiny
   runtime cost; one pipeline.
2. **Compile two variants** of `tile.wgsl` and pick the pipeline
   per-basemap at upload time. Cleaner separation but doubles the
   pipeline count.

**Decision:** branch in the shader (option 1). The math is six
lines either way and a single pipeline is easier to reason about
when the camera path is shared.

### UI surface

The existing bottom-left segmented control (`#basemap-toggle`)
generalises from "Map ↔ Satellite" to a per-body basemap list. A
second control — top-left, mirroring the toggle visually —
switches the active body. Body switch fires the renderer's
existing `set_basemap_mode` flow (with a body change first), so
the dwell + fetch + draw paths don't change.

```text
┌──────────────────────────────────────────────────────────┐
│ aeGIS                                                    │
│                                                          │
│ ┌──────────────────┐                                     │
│ │ Earth Mars Moon  │                                     │
│ └──────────────────┘                                     │
│                                                          │
│              [ globe of currently-selected body ]        │
│                                                          │
│                                                          │
│ ┌────────────────┐                                       │
│ │ Map Satellite  │ ← becomes Color / Terrain on Mars     │
│ └────────────────┘                                       │
└──────────────────────────────────────────────────────────┘
```

Switching bodies snaps the camera to that body's `HomeView`, clears
the tile caches (or partitions them — see Open questions), and
re-renders the attribution panel.

### Camera defaults per body

- **Earth:** unchanged — Chicago at z=11, satellite (current
  default after plan 0002 lands).
- **Mars:** Olympus Mons (longitude 226.2°E / lat 18.65°N — but
  stored as the body-relevant convention internally) at z≈4, the
  largest volcano in the solar system. A photogenic landing.
- **Moon:** Mare Tranquillitatis / Apollo 11 landing site (lon
  23.47°E, lat 0.67°N) at z≈4.

## Milestones

### M0 — Body abstraction + Earth refactor (MAP-body-abstraction)

- [ ] `Body`, `Basemap`, `BodyId`, `BasemapId`, `TileProjection`,
      `HomeView`, `CapColors`, `CrsConvention` types in a new
      `src/body.rs`.
- [ ] Replace the `BasemapMode` enum with `(BodyId, BasemapId)`
      pairs everywhere it's used (renderer state, web getter /
      setter, basemap-toggle wiring).
- [ ] `TileProvider` becomes a record (`projection`,
      `url_template`, `max_z`); `tile_url` formats from the
      template.
- [ ] Static `EARTH: Body` with the two existing basemaps; nothing
      else changes about Earth's behaviour. The live demo looks
      identical post-M0.
- [ ] Test: `Body::all().iter().map(|b| b.basemaps.len()).sum() >=
      1` and every basemap's URL template has the expected
      `{z}/{x}/{y}` (or `{z}/{y}/{x}`) substitution points.

### M1 — Equirectangular tile projection (MAP-eq-projection)

- [ ] `Camera::visible_tiles_capped` dispatches on
      `body.basemaps[active].provider.projection`. New Equirectangular
      path computes tile addresses without Mercator stretch. Test
      that at z=2 with an Equirectangular Earth-equivalent body,
      the centre tile covers `(0°, 0°)`.
- [ ] `tile.wgsl` gains a uniform `projection_kind: u32` and the
      `world_to_lonlat_rad` function branches accordingly. Add a
      `caps_shader_validates`-style shader test.
- [ ] `Body::Earth` keeps `WebMercator`; nothing else changes
      visually. A dummy `Body::EarthEq` (test-only, not exposed)
      with the Equirectangular flag round-trips through the
      pipeline so the EQ path is exercised before Mars lands.

### M2 — Mars (MAP-mars)

- [ ] Add `MARS: Body` to `body.rs`: Viking Color Mosaic +
      MOLA Hillshade as two basemaps, Olympus Mons home view,
      bundled equirectangular fallback texture, dust-red cap
      colours, `show_political_overlays: false`.
- [ ] `data/mars/mars_fallback_2048x1024.jpg` (downscaled Viking
      mosaic export, ≤ 2 MB) committed to the repo with a small
      `data/mars/README.md` documenting source + license.
- [ ] Body switcher UI in `index.html` + wiring in `src/web.rs`
      that flips between Earth and Mars. Per-body basemap toggle
      updates its labels (e.g. "Color" / "Terrain" instead of
      "Map" / "Satellite").
- [ ] Attribution footer becomes a function of the current body
      + basemap (template-driven from `Basemap::attribution_html`).
- [ ] Reference render: a screenshot at
      `tests/visual/mars-olympus-mons.png` so future regressions are
      reviewable.

### M3 — Moon (MAP-moon)

- [ ] `MOON: Body` with LRO WAC global mosaic, Apollo 11 home
      view, bundled equirectangular fallback, neutral-grey cap
      colours. Mostly a data add — M0–M2 should be doing all the
      structural work.
- [ ] Reference render at `tests/visual/moon-tranquillitatis.png`.
- [ ] Body switcher now shows three options.

### M4 — Polish + a path for fictional worlds (MAP-multi-body-polish)

- [ ] **Camera default per body:** body switch sets the camera to
      that body's `HomeView` (a smooth fly-to if plan 0002 has
      shipped; an instant snap otherwise).
- [ ] **Per-body tile cache partition:** the existing `tiles` /
      `sat_tiles` HashMaps are keyed only by `TileId`; switching
      bodies needs them keyed by `(BodyId, BasemapId, TileId)` —
      or partitioned per-body-per-basemap — so Mars tiles don't
      bleed through when toggling back to Earth.
- [ ] **Per-body globe-tilt / axial obliquity (stretch):** Mars
      tilts 25.2°, Moon 1.5°, Earth 23.5°. If we add a "season"
      time slider, obliquity matters. Out of scope for v1; flag
      in the comment block on `Body`.
- [ ] **Fictional-world adapter:** document the data-shape a
      contributor would need to add a new body to `body.rs` —
      `Body` literal + tile-pyramid URL + fallback texture +
      license note in `data/<body>/README.md`. The architecture
      then supports anything; **shipping a Middle-earth body is
      gated on finding a tile source whose license fits the
      project's data policy** (see Open questions).

## Open questions

- **Middle-earth tile licensing.** Most fan-made Middle-earth
  maps online are derivative works of Tolkien's estate IP,
  operating under tolerance rather than a license you can rely
  on. Realistic options: (a) ship the architecture in M4,
  document the contributor path, but don't bundle a default
  Middle-earth body; (b) commission or contribute a CC-licensed
  Middle-earth-style map (large scope, separate plan); (c) point
  at a non-Tolkien fictional world that does have clean
  licensing (LotR-adjacent but not Tolkien, or one of the
  CC-BY-SA worlds on r/imaginarymaps). **Recommended resolution:**
  (a) for v1; revisit (b)/(c) as separate plans if there's
  appetite.
- **Cache partitioning vs eviction on body switch.** Keying the
  cache by `(BodyId, BasemapId, TileId)` keeps Mars + Earth tiles
  resident across switches at the cost of GPU memory. Eviction
  on switch is simpler but means re-streaming on every toggle.
  **Lean:** partition, capped at e.g. 256 MB total GPU residency
  with LRU eviction. Decide during M4 design.
- **Mars longitude convention.** Areocentric vs ographic, +East
  vs +West. NASA Trek uses +East / planetocentric (the modern
  IAU 2000 recommendation), but a lot of older maps use +West.
  The renderer can stay convention-agnostic; the
  search/labelling layer (plan 0002) needs to display the right
  one. Lean: store internally as +East, document, surface as
  +East in the UI for Mars unless feedback says otherwise.
- **WGS84-style ellipsoid for Mars / Moon?** Both bodies are
  significantly less spherical than Earth (Mars equatorial
  radius 3396 km vs polar 3376 km — 0.59% flattening, almost
  twice Earth's). A unit-sphere render is fine for v1; an
  ellipsoidal upgrade is the same plan as the Earth WGS84
  upgrade flagged in ROADMAP Phase 9.

## Done when

- The live demo lets the user switch between Earth, Mars, and
  Moon from a top-left control.
- Each body's default view loads with its bundled fallback
  texture immediately and streams tiles on dwell.
- The per-body basemap toggle shows the right options
  (Map/Satellite for Earth, Color/Terrain for Mars, single
  Mosaic for Moon).
- Country outlines render only on Earth.
- Polar caps render in body-appropriate colours.
- Attribution footer updates per body + basemap.
- All four milestones pass `cargo test --all-targets` and
  `cargo check --target wasm32-unknown-unknown --lib`.
- ROADMAP's "Phases" section gains a new entry pointing at this
  plan (probably as Phase 12).
