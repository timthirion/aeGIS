# plans/

Living planning documents for aeGIS (Rust), versioned in the repo so work
can move between machines without losing state.

## Why these live in the repo

A plan is the hand-off: anyone — human or agent — picking the repo up on
another machine should be able to read the active plan and continue
without reconstructing context. Keep plans current as you work; a ticked
checkbox here is the source of truth for "what's done." Commit plan
updates alongside the code they describe.

## Layout

- `ROADMAP.md` — the high-level, phased direction. The north star.
- `NNNN-short-slug.md` — one document per concrete piece of work,
  zero-padded and incrementing (`0001-`, `0002-`, …). The number is
  ordering, not priority.
- `research/` — research plans (hypotheses, experimental designs, paper
  roadmaps) with their own `RNNNN-*` numbering. Created when the first
  research plan lands.

## Plan document template

```markdown
# <Title>

- **Status:** proposed | active | blocked | done | abandoned
- **Last updated:** YYYY-MM-DD
- **Last touched on:** <machine / context, so the next session knows where it ran>

## Goal
One paragraph: what this delivers and why it matters for the roadmap.

## Context
What exists today, relevant files, constraints, prior decisions. New
dependencies get a one-paragraph note here (why this crate, what the
alternatives were, what its license is).

## Design
The approach. Struct sketches, WGSL/pipeline shapes, CRS / format
trade-offs considered.

## Steps
- [ ] Concrete, checkable tasks in order. Tick as you go.

## Open questions
Unresolved decisions. Resolve and record the answer rather than deleting.

## Done when
The acceptance criteria — tests, reference renders, fixture files.
```

## Conventions

- Update **Status** and **Last updated** every working session.
- Resolve an open question in-doc (with the answer) rather than dropping
  it.
- When a plan is `done`, leave it as a record and link it from
  `ROADMAP.md`.
- GIS work that ships visual output should cite a reference (a known
  fixture render, a published map, a metric) so correctness is
  verifiable on any machine.
- Native and web builds are both first-class: a plan isn't done until
  it works in both targets (unless explicitly native-only, e.g. heavy
  asset-pipeline tools that depend on GDAL/PROJ C bindings).
- Plans that introduce a new data source document its license in
  "Context" — see `AGENTS.md` data-licensing discipline.

## Milestone naming convention

Within a plan, milestones use a **track prefix + short semantic slug**:

- `MAP-<topic>` for map-rendering milestones (vector + raster
  rendering, tile pipelines, label placement).
- `CRS-<topic>` for coordinate-reference-system work
  (`CRS-mercator`, `CRS-proj4rs-wire`).
- `FMT-<topic>` for format I/O (`FMT-geojson`, `FMT-pmtiles`,
  `FMT-cog`).
- `IDX-<topic>` for spatial index / query (`IDX-rtree-build`,
  `IDX-nearest`).
- `UI-<topic>` for interactive UI affordances (`UI-pan-zoom`,
  `UI-layer-toggle`, `UI-attribution`).

Sequencing within a plan comes from the order of checkboxes in the
plan doc; cross-plan ordering is the ROADMAP's job. Slugs carry no
ordinal — `MAP-labels` doesn't imply it happened after `MAP-tiles`,
only that both belong to the map-rendering track.

Pick clear topical names up front. Renaming a milestone after work
starts pollutes the git log; if scope drifts, split rather than rename.
