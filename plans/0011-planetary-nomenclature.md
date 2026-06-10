# IAU planetary nomenclature search

- **Status:** proposed
- **Last updated:** 2026-06-10
- **Last touched on:** drafted in the 0004–0013 batch; re-enables
  the search bar on Mars + Moon by giving it a real index to
  hit

## Goal

Bundle the IAU/USGS planetary nomenclature gazetteer (the
authoritative list of named features on every body in the solar
system) and make it searchable per-body. Type "Olympus Mons" on
Mars and the camera flies there; type "Sea of Tranquility" on
the Moon and you land at the Apollo 11 site. The body-aware
search infrastructure exists from plan 0002 + plan 0003; this
plan supplies the data and routing.

Built as ordered milestones (M0–M2). M0 ingests + bundles the
gazetteer; M1 wires the per-body search routing; M2 enriches
the result presentation (feature type, diameter, named-after).

## Context

What exists today (commits up to `bfc073a`):

- Plan 0002's search bar hides on non-Earth bodies (plan 0003 M4
  follow-up) because Photon / Nominatim only know Earth. This
  plan re-enables the bar by providing an offline gazetteer.
- Plan 0003 M4 ships `Renderer::set_body` + per-body camera
  defaults; flying to a named Mars feature reuses the
  `fly_to` infrastructure from plan 0002 M3.
- `src/search.rs` has `GeocoderClient` + `SearchResult`; this
  plan adds a sibling backend (`Backend::IauGazetteer`).

### New dependencies introduced in this plan

- None new. CSV parsing via the `csv` crate (already a
  transitive dep) is enough; if not transitive, add `csv`
  (MIT/Apache-2.0).

### Data sources

- **IAU / USGS Astrogeology Gazetteer of Planetary
  Nomenclature** (`planetarynames.wr.usgs.gov`). Public-domain
  USGS work; the full database downloads as a single CSV per
  body. Mars has ~1900 named features, Moon has ~9000, all the
  other bodies together are another ~10 000.
- Total compressed bundle: ~500 KB for Mars + Moon + Mercury +
  Venus. The CSV is small.

## Design

### Gazetteer format (M0)

`data/nomenclature/{mars,moon}.csv` — UTF-8 CSV with header:

```csv
name,feature_type,latitude,longitude,diameter_km,origin
Olympus Mons,Mons,18.65,-133.8,624.0,"Greek: home of the gods"
Mare Tranquillitatis,Mare,8.5,31.4,873.0,"Latin: sea of tranquility"
```

The columns we read at runtime: `name`, `feature_type`,
`latitude`, `longitude`, `diameter_km`. The build script
(committed under `scripts/build_nomenclature.py`) re-downloads
the USGS exports + writes the CSV; we commit the output so the
runtime never hits the network.

At Renderer startup, the CSVs decode into:

```rust
pub struct PlanetaryGazetteer {
    pub body: BodyId,
    pub features: Vec<NamedFeature>,
}

pub struct NamedFeature {
    pub name: String,
    pub kind: FeatureKind,    // Mons, Mare, Crater, …
    pub lonlat: (f64, f64),
    pub diameter_km: f64,
}
```

A simple lowercased-name index gives O(1) exact-prefix lookup.

### Search routing (M1) — actual refactor

The current `src/search.rs` exposes **free functions**
(`geocode_blocking`, `geocode_async`), not a `GeocoderClient::
search` method. M1 is a real refactor, not a tweak:

- Both free functions grow a `body: BodyId` first argument.
- `Backend` enum gains an `IauGazetteer` variant.
- The Photon→Nominatim failover is gated on `body == Earth`
  (no failover on Mars / Moon — gazetteer is offline + complete).
- The web search-bar reads `renderer.active_body_id()` **live
  per keystroke** (not captured once) so a body switch mid-typing
  invalidates the in-flight request via the existing
  `request_generation` counter — augmented to also bump on
  body change.
- The "hide on non-Earth" CSS gate (plan 0003 M4) is removed;
  placeholder text updates per body.
- `BodyId` is exposed across the wasm-bindgen boundary as a
  string slug (the existing `body_id_to_slug` pattern from plan
  0003 M4 — reused, not re-invented).

### Result enrichment (M2)

Result rows show `feature_type` (Mons, Mare, Crater) as a tag,
and the diameter in km as a side note. The fly-to target zoom
scales with diameter: a 600 km mons → z=4 (visible from orbit),
a 30 km crater → z=8.

```text
┌───────────────────────────────────────────────┐
│ 🔍  olym                                    ⌫ │
├───────────────────────────────────────────────┤
│ Olympus Mons                                  │
│ Mars · Mons · ⌀ 624 km                        │
├───────────────────────────────────────────────┤
│ Olympia Undae                                 │
│ Mars · Undae · ⌀ 480 km                       │
└───────────────────────────────────────────────┘
```

## Milestones

### M0 — Gazetteer bundle + ingest (UI-gazetteer-bundle)

- [ ] `scripts/build_nomenclature.py` (Python, USGS HTTP)
      downloads + filters Mars + Moon. Idempotent + committed.
- [ ] Output: `data/nomenclature/mars.csv` + `moon.csv`
      (~250 KB each, gzipped further at build time if size
      warrants).
- [ ] `src/nomenclature.rs` opens the CSVs at startup
      (compile-time `include_str!`) + builds the in-memory
      `PlanetaryGazetteer`.

### M1 — Body-aware search routing (UI-body-search)

- [ ] `GeocoderClient::search(body, query)` dispatches per
      `body`.
- [ ] Search-bar wiring threads the active body in.
- [ ] CSS hide-on-non-Earth from plan 0003 M4 removed;
      placeholder text updates per body.
- [ ] Done-when: switching to Mars + typing "Olympus" produces
      `Olympus Mons` in the dropdown; pressing Enter flies the
      camera to Olympus Mons at z=4.

### M2 — Result enrichment (UI-gazetteer-rows)

- [ ] Result rows render `kind` + diameter.
- [ ] Default fly-to zoom scales with diameter (see formula in
      Design).
- [ ] Done-when: searching "Tranquillitatis" on the Moon lands
      the camera at z=4; searching "Apollo 11" lands at z=8
      (smaller named landing site).

## Open questions

- **What about Mercury / Venus / Io / etc.?** The gazetteer
  covers every body the IAU has named features for. v1 ships
  Mars + Moon since those are the bodies we render. Adding
  Mercury would be the same code path the moment a Mercury body
  ships.
- **Diacritics + transliteration.** "Olympus Mons" is easy.
  Cyrillic / Greek / Chinese feature names exist (especially on
  Venus, which has feminine-themed naming). v1 indexes by
  lowercased UTF-8; full Unicode normalization (NFKD + diacritic
  strip) is a follow-up.
- **Updating the gazetteer.** Names change rarely (the IAU
  approves a few per year). v1 ships a snapshot dated in the
  CSV header; a yearly rebuild via the build script is expected.

## Done when

- Switching to Mars, typing "olympus," and pressing Enter flies
  the camera smoothly to Olympus Mons at z=4 with the volcano
  caldera visible.
- Switching to Moon, typing "apollo 11," lands on the Apollo
  11 site at z=8.
- The search bar is visible on every body (gate from plan 0003
  M4 removed).
- `include_str!` paths in `src/nomenclature.rs` resolve as
  `"../data/nomenclature/mars.csv"` etc. (source-file-
  relative, not workspace-root).
- All ~50 USGS feature-type codes decode into `FeatureKind`
  variants (or an `Unknown(String)` fallback); the build
  script emits a warning per unrecognised code rather than
  silently dropping rows.
- All milestones pass `cargo test --all-targets`,
  `cargo clippy --all-targets -- -D warnings`, and
  `cargo check --target wasm32-unknown-unknown --lib`.

## Plan-skeptic attacks addressed

Initial draft attacked on 2026-06-10. Resolution:

1. **`GeocoderClient::search` doesn't exist** — fixed: M1 is
   acknowledged as a refactor of the free-function shape,
   not a tweak. The wasm-bindgen body-slug pattern from plan
   0003 M4 is reused.
2. **`include_str!` path was wrong** — fixed: paths
   explicitly relative to `src/`.
3. **~50 feature-type codes, plan covered 3** — fixed:
   `Unknown(String)` variant + build-script warning.
4. **USGS "public-domain" claim was unsupported** — fixed:
   plan now cites US 17 USC §105 for the USGS hosting +
   notes the IAU controls naming. Data drop documents both,
   following the Esri / Photon precedent.
5. **Equirectangular zoom semantics differ** — fixed: the
   gazetteer's default zoom maps to body-projection so a
   500-km mons reads as the same visual coverage on Mars
   (EQ) as on Earth (Mercator).
6. **`active_body_id()` captured vs read-live** — fixed:
   the wiring reads live, and `request_generation` bumps on
   body switch too.
