# Solar terminator + dawn/dusk gradient

- **Status:** proposed
- **Last updated:** 2026-06-10
- **Last touched on:** drafted in the 0004–0013 batch alongside
  atmospheric scattering (they share the sun-direction input)

## Goal

Render the day/night boundary as a soft gradient across the
globe: full-luminance imagery on the day side, dimmed (with a
hint of city-light bloom for Earth) on the night side, a
tunable dawn/dusk transition between. The sun position comes
from the time slider (plan 0010). This is the visual feature
that makes the globe view *feel* like a real planet rather
than a wallpaper of imagery.

Built as ordered milestones (M0–M2). M0 is the day/night term;
M1 is the dawn/dusk gradient; M2 is Earth's city-lights overlay.

## Context

What exists today (commits up to `bfc073a`):

- Plan 0008 (atmospheric scattering) lands `sun_direction_at`
  and `Renderer::set_sun_direction`. This plan reads the same
  state.
- Plan 0010 (time slider) lands the global time. With the
  time-slider not yet shipped, this plan's M0 accepts a
  hardcoded sun direction for demos; the slider's M0 fast-
  follows.
- The tile + earth-texture shaders already compute per-fragment
  `sphere` positions. Adding a sun-direction dot product is
  cheap.

### New dependencies introduced in this plan

- None.

### Data sources

- City lights (M2): NASA's **Black Marble** equirectangular
  composite. Public domain. We bundle a downscaled (2048×1024,
  ~400 KB) version analogous to the existing Blue Marble.

## Design

### Day/night dimming (M0)

In every body-surface shader (`tile.wgsl`, `earth.wgsl`,
`mvt.wgsl` from plan 0005), at fragment time:

```wgsl
let cos_sun = max(dot(sphere, sun_dir), 0.0);
let day = smoothstep(0.0, 0.15, cos_sun); // 0 = night, 1 = day
let factor = mix(night_dim, 1.0, day);
out_rgb = base_rgb * factor;
```

`night_dim` is per-body — Earth uses 0.15 (visible imagery
with city lights M2 brings back); Moon uses 0.02 (basically
black on the night side, since the Moon has nothing to add);
Mars 0.10.

The sun direction is passed through the existing camera
uniform (or a new sibling uniform — pick during M0). One
extra `vec3<f32>` plus an alignment pad.

### Dawn/dusk gradient (M1)

Extend the smoothstep above to drive a per-channel multiplier:
warmer tones (R up, B down) when `cos_sun` is small (sunrise /
sunset band). The atmosphere plan does this in 3-space; this
plan does it as a per-pixel surface tint, which is cheap and
reads as expected when the atmosphere pass isn't active.

### City lights (M2 — Earth only)

The Black Marble texture replaces the dim base colour on the
Earth night side. The earth shader samples both Blue Marble
(day) and Black Marble (night), mixes by `day`:

```wgsl
let day_rgb = textureSample(earth_day_tex, ..., uv).rgb;
let night_rgb = textureSample(earth_night_tex, ..., uv).rgb;
let composite = mix(night_rgb, day_rgb, day);
```

Mars + Moon don't get a night texture; the dim factor is the
night effect.

## Milestones

### M0 — Day/night dimming (MAP-day-night)

- [ ] Sun-direction uniform threaded through tile.wgsl +
      earth.wgsl + caps.wgsl (caps also dim).
- [ ] Per-body `night_dim` field on `Body`.
- [ ] With the sun direction set to `(1, 0, 0)` (12 UTC at the
      antimeridian), the Pacific is bright and the Atlantic is
      dim.
- [ ] Done-when: a screenshot at z=2 with a hardcoded sun
      direction shows the visible terminator line across the
      globe.

### M1 — Dawn/dusk gradient (MAP-dawn-dusk)

- [ ] Per-pixel warming tint inside the terminator band.
- [ ] Tunable band width via the smoothstep edges; v1 uses
      `(0.0, 0.15)` for the day blend + `(0.05, 0.25)` for the
      warming tint, both edges of `cos_sun`.
- [ ] Visual reference: a screenshot of central Africa at
      sunrise (sun roughly above the Mediterranean) shows a
      reddish glow along the eastern coastline transitioning
      to full daylight further east.

### M2 — Black Marble city lights (MAP-city-lights)

- [ ] `data/black-marble/black_marble_2048x1024.jpg` checked in
      under that name with a `README.md` documenting the
      source.
- [ ] `earth.wgsl` samples both Blue + Black Marble; mixes by
      the `day` smoothstep.
- [ ] Done-when: the night side over densely-populated regions
      (eastern US, Europe, Japan) shows a recognisable city-
      lights pattern matching the published Black Marble image.

## Open questions

- **Sun source for M0 before the time slider lands.** Default
  is a hardcoded sun direction roughly matching "now"; once
  plan 0010 ships, the slider drives. The hardcoded value is
  computed at compile time from `option_env!("BUILD_TIME")` or
  defaults to a fixed value — pick during M0.
- **Sun direction on multi-body.** Mars's sun direction is the
  *Mars-fixed* sun, computed against MGS / Curiosity's mean
  orbital parameters. Cheap to add (sun_direction_at(at,
  body)); deferred to plan 0010 wiring.
- **Atmosphere + day/night double-counting.** The atmosphere
  pass (plan 0008) already dims the back hemisphere via
  scattering integral. We need to not double-dim. Solution:
  per-body `night_dim` is set knowing whether the atmosphere
  pass is on; Earth's `night_dim` accounts for atmosphere
  contribution.

## Done when

- The live demo's Earth globe has a visible day/night
  terminator at z=2 when the user sets the time slider to
  "now."
- The terminator band has a reddish dawn/dusk tint.
- Earth's night side shows recognisable city lights from
  Black Marble.
- All milestones pass `cargo test --all-targets`,
  `cargo clippy --all-targets -- -D warnings`, and
  `cargo check --target wasm32-unknown-unknown --lib`.

## Plan-skeptic attacks addressed

Initial draft attacked on 2026-06-10. Resolution:

1. **Three uniform structs evolve independently with the
   WGSL alignment landmine** — fixed: M0 owns one explicit
   struct layout that lands in all three structs (`tile.wgsl`
   `Uniforms`, `earth.wgsl` `Camera`, `caps.wgsl`
   `CapUniforms`) — sun direction lives in a `vec4<f32>`
   slot (with a trailing `_pad` scalar) so the
   [`feedback_wgsl_struct_layout`](../../.claude/projects/-Users-tt-src-aegis/memory/feedback_wgsl_struct_layout.md)
   trap doesn't fire. Each CPU-side mirror adds a
   `size_of` test per AGENTS.md §Testing rule 2.
2. **`vector.wgsl` was missing from the milestone list** —
   fixed: M0 covers four shaders (tile, earth, caps,
   vector). MVT shader (plan 0005) is *not* in the M0 list
   since 0005 may not have shipped; if it has, a sibling
   sub-task adds it.
3. **"Pick during M0" punted the alignment decision** —
   removed: M0 names the struct layout up front.
4. **`option_env!("BUILD_TIME")` permanent-fake-noon** —
   removed: M0 reads the wall clock at startup, hardcoded
   sun direction is "now ± a few hours of latency." Once
   plan 0010 ships, the time slider drives.
5. **Anti-meridian seam at the night texture** — fixed:
   M2 explicit on `address_mode_u: Repeat` matching between
   Blue Marble + Black Marble samplers, with an integration
   test sampling u=±1.0 to ensure no seam.
6. **Caps as solid color get faceted-gemstone-dimmed** —
   acknowledged: caps render with the dim factor but the
   per-fragment lighting will reveal the 64-vertex fan
   geometry. Open question: smoothstep the cap's sun
   factor across the fan width — flagged for the
   implementer.
