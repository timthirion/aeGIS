# Time slider + animatable layers

- **Status:** proposed
- **Last updated:** 2026-06-10
- **Last touched on:** drafted in the 0004–0013 batch; foundational
  for satellite-orbit replay, solar-terminator daylight, and any
  layer with a time dimension

## Goal

A single source of truth for "what time is it in the simulation"
plus a UI to control it and an API for layers to subscribe.
Satellite orbits (plan 0004) read this to propagate; the
solar terminator (plan 0009) reads it to rotate the sun;
future time-varying vector / raster layers consume the same
events. Default mode is real time; the slider also scrubs,
accelerates, and pauses.

Built as ordered milestones (M0–M2). M0 is the state + API;
M1 is the UI; M2 is the layer-subscription model.

## Context

What exists today (commits up to `bfc073a`):

- No global time concept in the renderer. The fly-to animation
  (plan 0002 M3) reads `web_sys::Performance::now()` /
  `std::time::Instant` directly per frame.
- Plans 0004, 0008, 0009 all consume the time signal this plan
  produces.
- The web chrome has top-bar (body switcher) and bottom-left
  (basemap toggle); the time slider is a new chrome zone.

### New dependencies introduced in this plan

- None. Time is a `f64` UNIX-seconds value passed around the
  renderer; no crates needed.

## Design

### State (M0)

`src/clock.rs`:

```rust
pub struct SimClock {
    /// Wall-clock seconds since UNIX epoch at the last `step`.
    last_real_unix_s: f64,
    /// Simulation time in UNIX seconds.
    pub sim_unix_s: f64,
    /// Playback rate. 1.0 = real time, 0.0 = paused, 60.0 =
    /// 60× real time.
    pub rate: f64,
}

impl SimClock {
    pub fn new_now(now_real_unix_s: f64) -> SimClock;
    pub fn step(&mut self, now_real_unix_s: f64);
    pub fn set_sim(&mut self, target_unix_s: f64);
    pub fn set_rate(&mut self, rate: f64);
}
```

The renderer owns one `SimClock`. Every frame:

```rust
let now = ... // performance.now() / Instant::now()
self.clock.step(now);
let sim_t = self.clock.sim_unix_s;
// pass sim_t to satellite propagation, sun-direction calc, etc.
```

### Subscriptions (M2)

Layer authors register a `Box<dyn LayerTick>`. The trait
**takes a context struct** so layers can access the GPU queue +
device they need to rewrite uniforms / instance buffers — the
original "tick(&mut self, sim_unix_s)" signature is
unimplementable because a layer can't get `&wgpu::Queue` from
a renderer-owned box during its own tick:

```rust
pub struct TickContext<'a> {
    pub sim_unix_s: f64,
    pub queue: &'a wgpu::Queue,
    pub device: &'a wgpu::Device,
}

pub trait LayerTick {
    fn tick(&mut self, cx: TickContext<'_>);
}

impl Renderer {
    pub fn register_layer_tick(&mut self, tick: Box<dyn LayerTick>)
        -> LayerTickId;
    pub fn unregister_layer_tick(&mut self, id: LayerTickId);
}
```

Satellite orbits (plan 0004) register a tick that rewrites the
instance buffer through `cx.queue`; the solar terminator (plan
0009) registers one that rewrites the sun-direction uniform.
Body switch calls `unregister_layer_tick` for the previous
body's tickers so `time_consumers_count()` reflects the active
body only.

### Time scale

`sim_unix_s` is **UTC seconds** (UNIX time). sgp4 propagation
needs UT1 ≈ UTC ± 1 s — acceptable for visualisation. Wall-
clock advancement uses `Instant::now()` (native) /
`performance.now()` (web) — both **monotonic** — and converts
the delta into `sim_unix_s` advancement. The plan does *not*
re-sample the system clock per tick, so an NTP correction or
sleep/wake doesn't inject jumps.

### UI (M1)

A bottom-centre chrome zone (mirrors the search bar at the
top-centre). Three controls:

```
┌─────────────────────────────────────────────────────────┐
│ ⏮  2026-06-10 03:47:21 UTC  ⏯  ▶▶ 1×  ⏭                 │
└─────────────────────────────────────────────────────────┘
```

- Play/pause toggle (`⏯`)
- Date/time readout (formatted from `sim_unix_s`)
- Rate cycle (`1×` → `10×` → `60×` → `1×` …)
- Skip-to-now button (`⏭`) — sets sim to wall-clock now
- A horizontal scrubber slider spanning ±24 hours from "now"

UI hides itself when nothing on screen consumes time (i.e., no
satellite layer, no atmospheric terminator visible). Wired via
a `Renderer::time_consumers_count()` accessor.

## Milestones

### M0 — Clock state + API (UI-sim-clock)

- [ ] `src/clock.rs` with `SimClock`.
- [ ] `Renderer::clock(&self)` accessor + `set_sim_time` /
      `set_rate` mutators.
- [ ] Per-frame `clock.step(now)` in the render path.
- [ ] Unit test: at `rate = 60.0`, 1 wall-clock second advances
      the sim by 60 seconds within `1e-9`.

### M1 — Time slider UI (UI-time-slider)

- [ ] `#aegis-time-slider` element in `index.html` with the
      five controls above. Uses the same chrome palette.
- [ ] `src/web.rs` wires play/pause + rate + skip-to-now +
      scrubber to the `SimClock` API.
- [ ] Visibility gate: hidden until `time_consumers_count() > 0`.
- [ ] Done-when: a user can scrub the slider and watch the
      solar terminator (plan 0009) move across Earth in real
      time.

### M2 — Layer subscriptions (UI-layer-tick)

- [ ] `LayerTick` trait + `register_layer_tick`.
- [ ] Plan 0004's satellite layer becomes the first registered
      ticker; plan 0009's sun-direction layer the second.
- [ ] Done-when: registering the satellite layer makes the
      time-slider UI appear; scrubbing moves the ISS along its
      orbit.

## Open questions

- **Subscription vs polling.** The trait shape is "renderer
  calls per-frame." An alternative is layers polling
  `renderer.sim_time()`. Trait is more cohesive; polling is
  simpler. Lean trait.
- **Time-zone display in the UI.** v1 is UTC only. Local time
  is a follow-up.
- **Saving sim state in the URL.** Bookmarking a `?t=...&rate=...`
  is a nice-to-have; out of scope for v1. Surfaced as an open
  question because it shapes how the URL state plan reads the
  clock.

## Done when

- The bottom-centre chrome shows a working time slider that
  controls a running solar terminator (when plan 0009 ships)
  and the satellite-orbit overlay (when plan 0004 ships).
- The UI hides itself on bodies / configurations where no
  time-consuming layer is active.
- Native target: a `--sim-rate` CLI flag controls playback rate
  + a `--sim-start "YYYY-MM-DDTHH:MM:SSZ"` flag sets the start
  time. The full slider UI is web-only; native gets the
  headless control surface so plan 0004 / 0009 still ship on
  `cargo run`.
- All milestones pass `cargo test --all-targets`,
  `cargo clippy --all-targets -- -D warnings`, and
  `cargo check --target wasm32-unknown-unknown --lib`.

## Plan-skeptic attacks addressed

Initial draft attacked on 2026-06-10. Resolution:

1. **`LayerTick::tick(&mut self, f64)` was unimplementable** —
   fixed: signature takes a `TickContext` with `&Queue` +
   `&Device`. Caller now has the GPU handles needed to write
   uniforms.
2. **No `unregister`** — fixed: added with
   `LayerTickId` returned by registration.
3. **Native build had no UI** — fixed: native ships
   `--sim-rate` + `--sim-start` CLI flags. Full slider UI
   stays web-only.
4. **`sim_unix_s` time-scale was unspecified** — fixed: UTC,
   documented. UT1 / TAI offsets are ignored at this layer
   (acceptable for visualisation; sgp4 tolerates).
5. **`step()` wasn't monotonic-safe** — fixed: internal delta
   uses `Instant::now()` / `performance.now()`, not wall-
   clock UNIX. NTP corrections and sleep don't inject jumps.
6. **f64 precision for sgp4 at far-future epochs** —
   acknowledged: sgp4 internally splits into TLE-epoch +
   minutes-since-epoch; plan 0004 owns that split, this plan
   just hands off a `f64` UTC second value.
