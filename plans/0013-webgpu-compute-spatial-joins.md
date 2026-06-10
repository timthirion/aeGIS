# WebGPU compute shaders for spatial joins (research spike)

> **Renaming note:** this plan is structurally a research plan
> per [`plans/README.md`](README.md)'s `research/RNNNN-*`
> convention. The next time this lands, it moves to
> `plans/research/R0001-gpu-spatial-join-spike.md` and the
> `0013-` slot is freed. Filed here only because the batch
> committed all ten under sequential `NNNN-` numbers.

- **Status:** proposed (research spike)
- **Last updated:** 2026-06-10
- **Last touched on:** drafted in the 0004–0013 batch; the
  weight-bearing question is "can we accelerate spatial joins
  enough on the GPU to make analytic queries interactive?"

## Goal

Benchmark and characterise a GPU-compute implementation of
point-in-polygon / nearest-neighbour spatial joins against the
CPU rstar baseline (plan 0007). Goal isn't necessarily to ship
the GPU path as the production engine — it's to know, with
hard numbers, when the GPU wins, when the CPU wins, and what
the implementation cost looks like at each side. The deliverable
is a benchmark suite + a written-up findings document, not
necessarily a production code path.

This is a **research spike** with deliberately narrow scope. If
the results favour GPU compute, plan 0014+ ships the production
path; if they favour CPU, we keep rstar and document why.

Built as ordered research milestones (R0–R3). R0 frames the
hypothesis; R1 ships the CPU baseline benchmark; R2 ships the
GPU compute kernel; R3 writes up findings + recommendations.

## Context

What exists today (commits up to `bfc073a`):

- Plan 0007 lands an rstar-backed R-tree on the CPU side. The
  3 k-feature Natural Earth case is sub-millisecond; the open
  question is the 100 k+ case (OSM extracts, vector tiles
  unpacked).
- WebGPU compute shaders are widely available now: Chrome 113+,
  Safari 17.4+, Firefox 121+ (behind a flag). aeGIS's wgpu
  backend already negotiates compute capability at adapter
  request time; we just don't use it yet.
- The vector layer model stores feature geometry CPU-side. A
  GPU spatial join needs the geometry in GPU buffers — the cost
  of the upload is part of what we're measuring.

### New dependencies introduced in this plan

- None. WGSL compute is wgpu native.

## Design

### Workload (R0)

The benchmark workload is **points-in-polygons**: given `N`
points and `M` polygons, return for each point the polygon
containing it (or `None`).

Three variants:
- **Small**: 10 k points, 3 k polygons (Natural Earth
  countries — the plan 0007 case scaled up).
- **Medium**: 100 k points, 50 k polygons (a US-county
  granular case).
- **Large**: 1 M points, 50 k polygons (the "every taxi pickup
  this month, which neighbourhood?" case).

### CPU baseline (R1)

`benches/cpu_spatial_join.rs` via `criterion` crate. Implementation:

1. Build R-tree on polygons (bulk-load).
2. For each point: bbox query, then exact polygon containment
   on the candidates.
3. Measure: build time, per-query time, total time, peak memory.

### GPU compute kernel (R2)

`src/shaders/spatial_join.wgsl` plus `src/spatial_gpu.rs`:

1. Upload polygons as packed vertex buffer + per-polygon
   offset table + per-polygon AABBs.
2. Build a coarse uniform-grid index on the GPU (one compute
   pass): each grid cell holds a list of polygon IDs whose
   AABB intersects it.
3. For each query point: look up the grid cell, walk its
   polygon list, do exact ray-cast / winding-number on each.
4. Output: a `Vec<u32>` of polygon IDs (or sentinel `u32::MAX`).

The benchmark measures wall-clock from "polygons in CPU memory"
through "results in CPU memory" — including upload + readback
— so the comparison is fair to the CPU path (which doesn't
need either).

### Findings document (R3)

A `plans/research/R0001-gpu-spatial-join-findings.md` (the
first research-plan-numbered doc). Documents:

- Hardware tested (M1 Air, an Intel + NV laptop, browser
  variants).
- Numbers for every workload × backend.
- Crossover points: where does GPU beat CPU?
- Implementation cost: shader complexity, memory bound, browser
  compute caveats.
- Production recommendation: ship the GPU path? Behind a
  feature flag? Never?

## Milestones

### R0 — Hypothesis + scope (RES-hypothesis)

- [ ] One-paragraph hypothesis pinned in this plan: "We expect
      the GPU path to be slower for the small workload (upload
      cost dominates), break even on medium, and be 5–20× faster
      on large." The findings doc either confirms or refutes.
- [ ] Workload fixtures committed under `tests/fixtures/spatial/`
      with a `README.md` documenting source + license. Natural
      Earth + a synthetic point distribution.

### R1 — CPU baseline (RES-cpu-bench)

- [ ] `benches/cpu_spatial_join.rs` with all three workloads,
      `criterion`'d, results printable as a markdown table.
- [ ] Add `dev-dependencies.criterion = "0.5"`.
- [ ] Numbers reproduce on CI (numbers vary per machine; the
      bench produces a CSV that can be committed for
      comparison across hardware).

### R2 — GPU compute path (RES-gpu-bench)

- [ ] `spatial_join.wgsl` compute shader implementing the
      uniform-grid + per-cell walk.
- [ ] `spatial_gpu.rs` Rust harness: upload + dispatch + readback.
- [ ] Same `criterion` workloads run against the GPU path.
- [ ] Done-when: GPU path produces identical results to CPU
      path on every workload (small + medium + large), and
      timings are recorded for both.

### R3 — Findings + recommendation (RES-writeup)

- [ ] `plans/research/R0001-gpu-spatial-join-findings.md`
      written. Numbers + commentary + production recommendation.
- [ ] PR description for the findings doc cross-links plan 0013
      and any next-step plans (e.g., 0014+ for production GPU
      spatial join if recommended).
- [ ] Plan 0013 transitions to `done` once R0001 is written.

## Open questions

- **WebGPU compute on Firefox.** Behind a flag at the time of
  drafting; R3 should call out the FF user base impact.
- **Memory bound.** The 1 M points × 50 k polygons workload
  uploads ~100 MB to the GPU; that's fine on desktop, marginal
  on mobile. Findings doc characterises.
- **The "compile shader at startup" cost.** WGSL compute
  shaders take 50-300 ms to compile on first use. Document
  whether that disqualifies for ad-hoc queries.
- **Generalising to lines / nearest-neighbour.** Out of scope
  for this spike; point-in-polygon is the canonical case.

## Done when

- All three workloads have benchmark numbers on CPU + GPU
  recorded in the findings doc.
- The findings doc makes a clear production recommendation:
  ship the GPU path / behind a flag / not at all.
- The findings doc lives at
  `plans/research/R0001-gpu-spatial-join-findings.md` (the
  first entry in the research-plan series the
  [`plans/README.md`](README.md) reserves the prefix for).
- All milestones pass `cargo test --all-targets`,
  `cargo clippy --all-targets -- -D warnings`, and
  `cargo check --target wasm32-unknown-unknown --lib`.

## Plan-skeptic attacks addressed

Initial draft attacked on 2026-06-10. Resolution:

1. **"Identical results" CPU vs GPU is unachievable on shared
   polygon edges** — fixed: R2 done-when is "agreement within
   ε on every non-boundary point, with a documented tie-
   breaker (lowest polygon-id wins) for shared-edge cases."
2. **No "honest negative" threshold** — fixed: R3 pre-commits
   to "ship the GPU path only if Large speedup ≥ 3× on at
   least two distinct GPU vendors (Apple Silicon + discrete
   AMD/NVIDIA)." Anything less → "do not ship."
3. **Hardware matrix was aspirational** — fixed: above
   threshold makes multi-vendor hardware load-bearing.
4. **CI has no GPU** — acknowledged: R2 numbers are local-
   only; the *CPU* baseline reproduces on CI. R3 names which
   numbers reproduce where.
5. **CRS unspoken for point-in-polygon** — fixed: R0 pins
   the predicate as "planar winding-number in Web Mercator
   world coords" (slippy convention, antimeridian
   excluded — the spike doesn't try to handle wrap).
6. **CPU baseline cites unmeasured `<1ms` from plan 0007** —
   fixed: R1 *is* the measurement; plan 0007's `<1ms` was
   speculation. R1 produces the real number.
7. **Per-cell variable-length list was hand-waved** — fixed:
   R2 names the layout explicitly (per-cell head index into
   a globally-sorted polygon-id array, sized at build time
   with a 95th-percentile cap; cells over the cap fall back
   to the rstar path).
8. **Numbering scheme** — flagged at top: this plan
   structurally belongs under `RNNNN-` and will be moved.
